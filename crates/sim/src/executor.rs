use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use groundhog_rpc_api_types::block::ChainTip;
use groundhog_rpc_client::GroundhogApiClient;
use tokio::sync::Semaphore;
use tokio::task::{Id as TaskId, JoinError, JoinSet};

use crate::error::SimError;
use crate::event::{EventWriter, ResultEvent, WorkloadEvent};
use crate::manifest::{SimulationManifest, WorkloadKind};
use crate::metrics::RunMetricsBuilder;
use crate::result::ChainTipObservation;
use crate::scheduler::{
    PhaseStart, ScheduledActorTick, ScheduledSimulationEvent, select_workload_for_tick,
};
use crate::util::{duration_ms, json_string, unix_ms, workload_name};

const DEFAULT_GLOBAL_IN_FLIGHT_LIMIT: usize = 1_024;
const DEFAULT_PER_ACTOR_IN_FLIGHT_LIMIT: usize = 1;

/// Minimal RPC surface used by the Phase 1 simulator executor.
pub trait SimRpcClient {
    fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send;
}

impl<T> SimRpcClient for T
where
    T: GroundhogApiClient + Send + Sync,
{
    #[allow(clippy::manual_async_fn)]
    fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
        async {
            GroundhogApiClient::chain_tip(self)
                .await
                .map_err(|error| error.to_string())
        }
    }
}

/// Execute a deterministic simulation schedule against a typed RPC client.
pub async fn run_schedule<C>(
    client: C,
    manifest: &SimulationManifest,
    schedule: &[ScheduledSimulationEvent],
    phase_starts: &[PhaseStart],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let run_start = Instant::now();
    let mut next_phase = phase_starts.iter().peekable();
    let mut tasks = JoinSet::new();
    let mut task_contexts = HashMap::new();
    let global_in_flight = Arc::new(Semaphore::new(DEFAULT_GLOBAL_IN_FLIGHT_LIMIT));
    let actor_in_flight = actor_semaphores(phase_starts);
    let mut in_flight = 0_u64;

    for event in schedule {
        in_flight -=
            drain_completed_workloads(&mut tasks, &mut task_contexts, event_writer, metrics)
                .await?;
        while let Some(phase) =
            next_phase.next_if(|phase| phase.simulation_time_ms <= event.simulation_time_ms())
        {
            sleep_until_sim_time(run_start, phase.simulation_time_ms).await;
            write_phase_started(event_writer, phase).await?;
        }
        match event {
            ScheduledSimulationEvent::ActorTick(tick) => {
                sleep_until_sim_time(run_start, tick.simulation_time_ms).await;
                let workload = select_workload_for_tick(manifest, tick)?;
                if let Some(skipped) = dispatch_workload(
                    client.clone(),
                    tick.clone(),
                    workload,
                    &mut tasks,
                    &mut task_contexts,
                    global_in_flight.clone(),
                    actor_in_flight
                        .get(tick.actor_id as usize)
                        .ok_or(SimError::Executor("actor semaphore missing".into()))?
                        .clone(),
                )? {
                    record_completion(skipped, event_writer, metrics).await?;
                } else {
                    in_flight += 1;
                }
            }
            ScheduledSimulationEvent::Chaos(_) => {}
        }
    }
    for phase in next_phase {
        sleep_until_sim_time(run_start, phase.simulation_time_ms).await;
        write_phase_started(event_writer, phase).await?;
    }
    while in_flight > 0 {
        let joined = tasks
            .join_next_with_id()
            .await
            .ok_or(SimError::Executor("workload task set closed early".into()))?;
        let completion = join_workload(joined, &mut task_contexts)??;
        record_completion(completion, event_writer, metrics).await?;
        in_flight -= 1;
    }
    Ok(())
}

fn actor_semaphores(phase_starts: &[PhaseStart]) -> Vec<Arc<Semaphore>> {
    let max_users = phase_starts
        .iter()
        .map(|phase| phase.users)
        .max()
        .unwrap_or_default();
    (0..max_users)
        .map(|_| Arc::new(Semaphore::new(DEFAULT_PER_ACTOR_IN_FLIGHT_LIMIT)))
        .collect()
}

fn dispatch_workload<C>(
    client: C,
    tick: ScheduledActorTick,
    workload: WorkloadKind,
    tasks: &mut JoinSet<Result<WorkloadCompletion, SimError>>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
    global_in_flight: Arc<Semaphore>,
    actor_in_flight: Arc<Semaphore>,
) -> Result<Option<WorkloadCompletion>, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let global_permit = match global_in_flight.try_acquire_owned() {
        Ok(permit) => permit,
        Err(error) => {
            return Ok(Some(backpressure_completion(
                &tick,
                workload,
                "global",
                &error.to_string(),
            )?));
        }
    };
    let actor_permit = match actor_in_flight.try_acquire_owned() {
        Ok(permit) => permit,
        Err(error) => {
            drop(global_permit);
            return Ok(Some(backpressure_completion(
                &tick,
                workload,
                "actor",
                &error.to_string(),
            )?));
        }
    };
    let context = WorkloadTaskContext {
        actor_id: tick.actor_id,
        workload,
    };

    let abort = tasks.spawn(async move {
        let _permits = (global_permit, actor_permit);
        match workload {
            WorkloadKind::ChainTip => execute_chain_tip(client, &tick).await,
            _ => Err(SimError::UnsupportedRuntimeWorkloads(workload_name(
                workload,
            ))),
        }
    });
    task_contexts.insert(abort.id(), context);

    Ok(None)
}

async fn drain_completed_workloads(
    tasks: &mut JoinSet<Result<WorkloadCompletion, SimError>>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<u64, SimError> {
    let mut drained = 0_u64;
    while let Some(joined) = tasks.try_join_next_with_id() {
        record_completion(
            join_workload(joined, task_contexts)??,
            event_writer,
            metrics,
        )
        .await?;
        drained += 1;
    }
    Ok(drained)
}

fn join_workload(
    joined: Result<(TaskId, Result<WorkloadCompletion, SimError>), JoinError>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
) -> Result<Result<WorkloadCompletion, SimError>, SimError> {
    match joined {
        Ok((task_id, completion)) => {
            task_contexts.remove(&task_id);
            Ok(completion)
        }
        Err(error) => {
            let context = task_contexts
                .remove(&error.id())
                .ok_or_else(|| SimError::Executor(format!("missing task context for {error}")))?;
            if error.is_panic() {
                return Err(SimError::WorkloadPanic {
                    actor_id: context.actor_id,
                    workload: context.workload,
                    payload: panic_payload(error),
                });
            }
            Err(SimError::Executor(format!(
                "workload task for actor {} {:?} was cancelled: {error}",
                context.actor_id, context.workload
            )))
        }
    }
}

fn panic_payload(error: JoinError) -> String {
    let payload = error.into_panic();
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).into()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".into()
    }
}

async fn record_completion(
    completion: WorkloadCompletion,
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<(), SimError> {
    if let Some(error_class) = completion.error_class {
        if completion.skipped {
            metrics.record_skipped(completion.workload, error_class);
        } else {
            metrics.record_failure(completion.workload, error_class, completion.latency_ms);
        }
    } else {
        metrics.record_success(completion.workload, completion.latency_ms);
    }
    if let Some(observation) = completion.chain_tip {
        metrics.record_chain_tip(observation);
    }
    event_writer.write(&completion.event).await?;
    Ok(())
}

fn backpressure_completion(
    tick: &ScheduledActorTick,
    workload: WorkloadKind,
    scope: &str,
    error: &str,
) -> Result<WorkloadCompletion, SimError> {
    let now = unix_ms()?.to_string();
    Ok(WorkloadCompletion {
        event: ResultEvent::WorkloadSkipped(WorkloadEvent {
            actor_id: tick.actor_id,
            workload,
            scheduled_time_ms: tick.simulation_time_ms,
            started_at_unix_ms: now.clone(),
            completed_at_unix_ms: now,
            latency_ms: 0,
            rpc_method: rpc_method(workload),
            chain_height: None,
            chain_block_hash: None,
            error_class: Some("simulator-backpressure".into()),
            error: Some(format!("{scope} in-flight limit reached: {error}")),
        }),
        workload,
        latency_ms: 0,
        error_class: Some("simulator-backpressure".into()),
        skipped: true,
        chain_tip: None,
    })
}

async fn write_phase_started(
    event_writer: &mut EventWriter,
    phase: &PhaseStart,
) -> Result<(), SimError> {
    event_writer
        .write(&ResultEvent::PhaseStarted {
            phase_index: phase.phase_index,
            simulation_time_ms: phase.simulation_time_ms,
            duration_ms: phase.duration_ms,
            users: phase.users,
            rate_per_user_millihz: phase.rate_per_user_millihz,
        })
        .await?;
    Ok(())
}

async fn execute_chain_tip<C>(
    client: C,
    tick: &ScheduledActorTick,
) -> Result<WorkloadCompletion, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let response = client.chain_tip().await;
    let latency_ms = duration_ms(started.elapsed());
    let completed_at_unix_ms = unix_ms()?.to_string();

    match response {
        Ok(tip) => {
            let height = *tip.height;
            let block_hash = json_string(&tip.block_hash, "chainBlockHash")?;
            let chain_tip = ChainTipObservation {
                simulation_time_ms: tick.simulation_time_ms,
                height,
                block_hash: block_hash.clone(),
            };
            Ok(WorkloadCompletion {
                workload: WorkloadKind::ChainTip,
                latency_ms,
                error_class: None,
                chain_tip: Some(chain_tip),
                event: ResultEvent::WorkloadCompleted(WorkloadEvent {
                    actor_id: tick.actor_id,
                    workload: WorkloadKind::ChainTip,
                    scheduled_time_ms: tick.simulation_time_ms,
                    started_at_unix_ms,
                    completed_at_unix_ms,
                    latency_ms,
                    rpc_method: "groundhog_chainTip",
                    chain_height: Some(height),
                    chain_block_hash: Some(block_hash),
                    error_class: None,
                    error: None,
                }),
                skipped: false,
            })
        }
        Err(error) => Ok(WorkloadCompletion {
            workload: WorkloadKind::ChainTip,
            latency_ms,
            error_class: Some("rpc-error".into()),
            chain_tip: None,
            event: ResultEvent::WorkloadFailed(WorkloadEvent {
                actor_id: tick.actor_id,
                workload: WorkloadKind::ChainTip,
                scheduled_time_ms: tick.simulation_time_ms,
                started_at_unix_ms,
                completed_at_unix_ms,
                latency_ms,
                rpc_method: "groundhog_chainTip",
                chain_height: None,
                chain_block_hash: None,
                error_class: Some("rpc-error".into()),
                error: Some(error),
            }),
            skipped: false,
        }),
    }
}

struct WorkloadCompletion {
    event: ResultEvent,
    workload: WorkloadKind,
    latency_ms: u64,
    error_class: Option<String>,
    skipped: bool,
    chain_tip: Option<ChainTipObservation>,
}

#[derive(Debug, Clone, Copy)]
struct WorkloadTaskContext {
    actor_id: u32,
    workload: WorkloadKind,
}

async fn sleep_until_sim_time(run_start: Instant, simulation_time_ms: u64) {
    let target = Duration::from_millis(simulation_time_ms);
    let elapsed = run_start.elapsed();
    if let Some(remaining) = target.checked_sub(elapsed) {
        tokio::time::sleep(remaining).await;
    }
}

fn rpc_method(workload: WorkloadKind) -> &'static str {
    match workload {
        WorkloadKind::ChainTip => "groundhog_chainTip",
        WorkloadKind::WalletSubmitSmoke => "groundhog_sendRawTransaction",
        WorkloadKind::ReceiptPoll => "groundhog_getTransactionReceipt",
        WorkloadKind::StatusPoll => "groundhog_getTransactionStatus",
        WorkloadKind::EventsQuery => "groundhog_getEvents",
        WorkloadKind::StorageRead => "groundhog_getStorageAt",
        WorkloadKind::ViewCall => "groundhog_view",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::event::EventWriter;
    use crate::manifest::SimulationManifest;
    use crate::metrics::RunMetricsBuilder;
    use crate::scheduler::{generate_schedule, phase_starts};

    #[derive(Clone)]
    struct PanickingClient;

    impl SimRpcClient for PanickingClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { panic!("simulated chain-tip panic") }
        }
    }

    #[tokio::test]
    async fn run_schedule_reports_workload_panics_with_context() {
        let manifest = manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();

        let err = run_schedule(
            PanickingClient,
            &manifest,
            &schedule,
            &starts,
            &mut event_writer,
            &mut metrics,
        )
        .await
        .expect_err("panic should be surfaced");

        assert!(matches!(
            err,
            SimError::WorkloadPanic {
                actor_id: 0,
                workload: WorkloadKind::ChainTip,
                ref payload,
            } if payload == "simulated chain-tip panic"
        ));
    }

    fn manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}]
        }))
        .expect("manifest fixture should parse")
    }
}
