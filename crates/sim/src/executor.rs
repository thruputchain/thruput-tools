use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use groundhog_rpc_api_types::{
    block::ChainTip, runtime_manifest::RuntimeManifest, transaction::TxHash,
};
use groundhog_rpc_client::GroundhogApiClient;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{Id as TaskId, JoinError, JoinSet};

use crate::actor::ActorState;
use crate::error::SimError;
use crate::event::{EventWriter, ResultEvent, WorkloadEvent};
use crate::manifest::{SimulationManifest, WorkloadKind};
use crate::metrics::RunMetricsBuilder;
use crate::result::ChainTipObservation;
use crate::scheduler::{
    PhaseStart, ScheduledActorTick, ScheduledSimulationEvent, select_workload_for_tick,
};
use crate::transaction::{
    BuiltSubmitTransaction, build_wallet_submit_smoke_transaction,
    runtime_dev_wallet_action_entrypoint,
};
use crate::util::{duration_ms, json_string, unix_ms, workload_name};

const DEFAULT_GLOBAL_IN_FLIGHT_LIMIT: usize = 1_024;
// Actor state is mutated only when a workload completion is recorded. Keep the
// per-actor limit at one unless nonce/state ownership is redesigned.
const DEFAULT_PER_ACTOR_IN_FLIGHT_LIMIT: usize = 1;

/// Minimal RPC surface used by the Phase 1 simulator executor.
pub trait SimRpcClient {
    fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send;
    fn send_raw_transaction(
        &self,
        tx_hex: String,
    ) -> impl Future<Output = Result<TxHash, String>> + Send;
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

    #[allow(clippy::manual_async_fn)]
    fn send_raw_transaction(
        &self,
        tx_hex: String,
    ) -> impl Future<Output = Result<TxHash, String>> + Send {
        async {
            GroundhogApiClient::send_raw_transaction(self, tx_hex)
                .await
                .map_err(|error| error.to_string())
        }
    }
}

/// Inputs for executing a generated schedule.
pub struct ScheduleExecution<'a> {
    pub manifest: &'a SimulationManifest,
    pub runtime_manifest: &'a RuntimeManifest,
    pub schedule: &'a [ScheduledSimulationEvent],
    pub phase_starts: &'a [PhaseStart],
    pub actor_states: &'a mut [ActorState],
    pub event_writer: &'a mut EventWriter,
    pub metrics: &'a mut RunMetricsBuilder,
}

/// Execute a deterministic simulation schedule against a typed RPC client.
#[allow(clippy::too_many_lines)]
pub async fn run_schedule<C>(client: C, input: ScheduleExecution<'_>) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let ScheduleExecution {
        manifest,
        runtime_manifest,
        schedule,
        phase_starts,
        actor_states,
        event_writer,
        metrics,
    } = input;
    let wallet_entrypoint = runtime_dev_wallet_action_entrypoint(runtime_manifest)?;
    let run_start = Instant::now();
    let mut next_phase = phase_starts.iter().peekable();
    let mut tasks = JoinSet::new();
    let mut task_contexts = HashMap::new();
    let global_in_flight = Arc::new(Semaphore::new(DEFAULT_GLOBAL_IN_FLIGHT_LIMIT));
    let actor_in_flight = actor_semaphores(phase_starts);
    let mut in_flight = 0_u64;

    for event in schedule {
        in_flight -= drain_completed_workloads(
            &mut tasks,
            &mut task_contexts,
            actor_states,
            event_writer,
            metrics,
        )
        .await?;
        while let Some(phase) =
            next_phase.next_if(|phase| phase.simulation_time_ms <= event.simulation_time_ms())
        {
            sleep_until_sim_time_draining(
                run_start,
                phase.simulation_time_ms,
                &mut tasks,
                &mut task_contexts,
                event_writer,
                metrics,
                actor_states,
                &mut in_flight,
            )
            .await?;
            write_phase_started(event_writer, phase).await?;
        }
        match event {
            ScheduledSimulationEvent::ActorTick(tick) => {
                sleep_until_sim_time_draining(
                    run_start,
                    tick.simulation_time_ms,
                    &mut tasks,
                    &mut task_contexts,
                    event_writer,
                    metrics,
                    actor_states,
                    &mut in_flight,
                )
                .await?;
                let workload = select_workload_for_tick(manifest, tick)?;
                let actor_index = tick.actor_id as usize;
                if actor_states.get(actor_index).is_none() {
                    return Err(SimError::Executor("actor state missing".into()));
                }
                let permits = match acquire_workload_permits(
                    tick,
                    workload,
                    global_in_flight.clone(),
                    actor_in_flight
                        .get(actor_index)
                        .ok_or(SimError::Executor("actor semaphore missing".into()))?
                        .clone(),
                )? {
                    Ok(permits) => permits,
                    Err(skipped) => {
                        record_completion(skipped, actor_states, event_writer, metrics).await?;
                        continue;
                    }
                };
                let prepared = prepare_workload(
                    actor_states
                        .get_mut(actor_index)
                        .ok_or(SimError::Executor("actor state missing".into()))?,
                    workload,
                    wallet_entrypoint,
                )?;
                dispatch_workload(
                    client.clone(),
                    tick.clone(),
                    workload,
                    prepared,
                    permits,
                    &mut tasks,
                    &mut task_contexts,
                );
                in_flight += 1;
            }
            ScheduledSimulationEvent::Chaos(_) => {}
        }
    }
    for phase in next_phase {
        sleep_until_sim_time_draining(
            run_start,
            phase.simulation_time_ms,
            &mut tasks,
            &mut task_contexts,
            event_writer,
            metrics,
            actor_states,
            &mut in_flight,
        )
        .await?;
        write_phase_started(event_writer, phase).await?;
    }
    while in_flight > 0 {
        let joined = tasks
            .join_next_with_id()
            .await
            .ok_or(SimError::Executor("workload task set closed early".into()))?;
        let completion = join_workload(joined, &mut task_contexts)??;
        record_completion(completion, actor_states, event_writer, metrics).await?;
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

fn acquire_workload_permits(
    tick: &ScheduledActorTick,
    workload: WorkloadKind,
    global_in_flight: Arc<Semaphore>,
    actor_in_flight: Arc<Semaphore>,
) -> Result<Result<WorkloadPermits, WorkloadCompletion>, SimError> {
    let global_permit = match global_in_flight.try_acquire_owned() {
        Ok(permit) => permit,
        Err(error) => {
            return Ok(Err(backpressure_completion(
                tick,
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
            return Ok(Err(backpressure_completion(
                tick,
                workload,
                "actor",
                &error.to_string(),
            )?));
        }
    };
    Ok(Ok(WorkloadPermits {
        _global: global_permit,
        _actor: actor_permit,
    }))
}
fn prepare_workload(
    actor: &mut ActorState,
    workload: WorkloadKind,
    wallet_entrypoint: groundhog_xdr::contract::ContractEntrypointID,
) -> Result<PreparedWorkload, SimError> {
    match workload {
        WorkloadKind::ChainTip => Ok(PreparedWorkload::ChainTip),
        WorkloadKind::WalletSubmitSmoke => Ok(PreparedWorkload::WalletSubmitSmoke(
            build_wallet_submit_smoke_transaction(actor, wallet_entrypoint)?,
        )),
        _ => Err(SimError::UnsupportedRuntimeWorkloads(workload_name(
            workload,
        ))),
    }
}

fn dispatch_workload<C>(
    client: C,
    tick: ScheduledActorTick,
    workload: WorkloadKind,
    prepared: PreparedWorkload,
    permits: WorkloadPermits,
    tasks: &mut JoinSet<Result<WorkloadCompletion, SimError>>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
) where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let context = WorkloadTaskContext {
        actor_id: tick.actor_id,
        workload,
    };

    let abort = tasks.spawn(async move {
        let _permits = permits;
        match prepared {
            PreparedWorkload::ChainTip => execute_chain_tip(client, &tick).await,
            PreparedWorkload::WalletSubmitSmoke(tx) => {
                execute_wallet_submit_smoke(client, &tick, tx).await
            }
        }
    });
    task_contexts.insert(abort.id(), context);
}

async fn drain_completed_workloads(
    tasks: &mut JoinSet<Result<WorkloadCompletion, SimError>>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<u64, SimError> {
    let mut drained = 0_u64;
    while let Some(joined) = tasks.try_join_next_with_id() {
        record_completion(
            join_workload(joined, task_contexts)??,
            actor_states,
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
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<(), SimError> {
    if let Some(submitted) = completion.submitted_transaction {
        let actor = actor_states
            .get_mut(submitted.actor_id as usize)
            .ok_or(SimError::Executor("actor state missing".into()))?;
        actor.record_submitted_tx(submitted.tx_hash, submitted.scheduled_time_ms)?;
    }
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
        event: ResultEvent::WorkloadSkipped {
            event: WorkloadEvent {
                actor_id: tick.actor_id,
                workload,
                scheduled_time_ms: tick.simulation_time_ms,
                started_at_unix_ms: now.clone(),
                completed_at_unix_ms: now,
                latency_ms: 0,
                rpc_method: rpc_method(workload),
                chain_height: None,
                chain_block_hash: None,
                tx_hash: None,
                tx_nonce: None,
                error_class: Some("simulator-backpressure".into()),
                error: Some(format!("{scope} in-flight limit reached: {error}")),
            },
        },
        workload,
        latency_ms: 0,
        error_class: Some("simulator-backpressure".into()),
        skipped: true,
        chain_tip: None,
        submitted_transaction: None,
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
                event: ResultEvent::WorkloadCompleted {
                    event: WorkloadEvent {
                        actor_id: tick.actor_id,
                        workload: WorkloadKind::ChainTip,
                        scheduled_time_ms: tick.simulation_time_ms,
                        started_at_unix_ms,
                        completed_at_unix_ms,
                        latency_ms,
                        rpc_method: "groundhog_chainTip",
                        chain_height: Some(height),
                        chain_block_hash: Some(block_hash),
                        tx_hash: None,
                        tx_nonce: None,
                        error_class: None,
                        error: None,
                    },
                },
                skipped: false,
                submitted_transaction: None,
            })
        }
        Err(error) => Ok(WorkloadCompletion {
            workload: WorkloadKind::ChainTip,
            latency_ms,
            error_class: Some("rpc-error".into()),
            chain_tip: None,
            event: ResultEvent::WorkloadFailed {
                event: WorkloadEvent {
                    actor_id: tick.actor_id,
                    workload: WorkloadKind::ChainTip,
                    scheduled_time_ms: tick.simulation_time_ms,
                    started_at_unix_ms,
                    completed_at_unix_ms,
                    latency_ms,
                    rpc_method: "groundhog_chainTip",
                    chain_height: None,
                    chain_block_hash: None,
                    tx_hash: None,
                    tx_nonce: None,
                    error_class: Some("rpc-error".into()),
                    error: Some(error),
                },
            },
            skipped: false,
            submitted_transaction: None,
        }),
    }
}

async fn execute_wallet_submit_smoke<C>(
    client: C,
    tick: &ScheduledActorTick,
    tx: BuiltSubmitTransaction,
) -> Result<WorkloadCompletion, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let response = client.send_raw_transaction(tx.tx_hex).await;
    let latency_ms = duration_ms(started.elapsed());
    let completed_at_unix_ms = unix_ms()?.to_string();
    let tx_nonce = Some(tx.nonce.to_string());

    match response {
        Ok(tx_hash) => {
            let tx_hash_json = json_string(&tx_hash, "txHash")?;
            Ok(WorkloadCompletion {
                workload: WorkloadKind::WalletSubmitSmoke,
                latency_ms,
                error_class: None,
                chain_tip: None,
                submitted_transaction: Some(SubmittedTransaction {
                    actor_id: tick.actor_id,
                    tx_hash,
                    scheduled_time_ms: tick.simulation_time_ms,
                }),
                event: ResultEvent::WorkloadCompleted {
                    event: WorkloadEvent {
                        actor_id: tick.actor_id,
                        workload: WorkloadKind::WalletSubmitSmoke,
                        scheduled_time_ms: tick.simulation_time_ms,
                        started_at_unix_ms,
                        completed_at_unix_ms,
                        latency_ms,
                        rpc_method: "groundhog_sendRawTransaction",
                        chain_height: None,
                        chain_block_hash: None,
                        tx_hash: Some(tx_hash_json),
                        tx_nonce,
                        error_class: None,
                        error: None,
                    },
                },
                skipped: false,
            })
        }
        Err(error) => Ok(WorkloadCompletion {
            workload: WorkloadKind::WalletSubmitSmoke,
            latency_ms,
            error_class: Some("rpc-error".into()),
            chain_tip: None,
            submitted_transaction: None,
            event: ResultEvent::WorkloadFailed {
                event: WorkloadEvent {
                    actor_id: tick.actor_id,
                    workload: WorkloadKind::WalletSubmitSmoke,
                    scheduled_time_ms: tick.simulation_time_ms,
                    started_at_unix_ms,
                    completed_at_unix_ms,
                    latency_ms,
                    rpc_method: "groundhog_sendRawTransaction",
                    chain_height: None,
                    chain_block_hash: None,
                    tx_hash: None,
                    tx_nonce,
                    error_class: Some("rpc-error".into()),
                    error: Some(error),
                },
            },
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
    submitted_transaction: Option<SubmittedTransaction>,
}

#[derive(Debug, Clone, Copy)]
struct SubmittedTransaction {
    actor_id: u32,
    tx_hash: TxHash,
    scheduled_time_ms: u64,
}

struct WorkloadPermits {
    _global: OwnedSemaphorePermit,
    _actor: OwnedSemaphorePermit,
}

enum PreparedWorkload {
    ChainTip,
    WalletSubmitSmoke(BuiltSubmitTransaction),
}

#[derive(Debug, Clone, Copy)]
struct WorkloadTaskContext {
    actor_id: u32,
    workload: WorkloadKind,
}

#[allow(clippy::too_many_arguments)]
async fn sleep_until_sim_time_draining(
    run_start: Instant,
    simulation_time_ms: u64,
    tasks: &mut JoinSet<Result<WorkloadCompletion, SimError>>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
    actor_states: &mut [ActorState],
    in_flight: &mut u64,
) -> Result<(), SimError> {
    loop {
        let target = Duration::from_millis(simulation_time_ms);
        let elapsed = run_start.elapsed();
        let Some(remaining) = target.checked_sub(elapsed) else {
            return Ok(());
        };

        if tasks.is_empty() {
            tokio::time::sleep(remaining).await;
            return Ok(());
        }

        tokio::select! {
            () = tokio::time::sleep(remaining) => return Ok(()),
            joined = tasks.join_next_with_id() => {
                let joined = joined.ok_or(SimError::Executor("workload task set closed early".into()))?;
                let completion = join_workload(joined, task_contexts)??;
                record_completion(completion, actor_states, event_writer, metrics).await?;
                *in_flight = in_flight
                    .checked_sub(1)
                    .ok_or(SimError::Executor("in-flight workload count underflowed".into()))?;
            }
        }
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
    use groundhog_rpc_api_types::{
        BlockHeight, block::ChainTip, hex::Hex0x, runtime_manifest::RuntimeManifest,
    };
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::{ContractAddress, SHA256Hash};
    use serde_json::json;
    use std::sync::Mutex;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::event::EventWriter;
    use crate::manifest::SimulationManifest;
    use crate::metrics::RunMetricsBuilder;
    use crate::nonce::NonceNamespace;
    use crate::runtime::SimAccount;
    use crate::scheduler::{generate_schedule, phase_starts};

    #[derive(Clone)]
    struct PanickingClient;

    #[derive(Clone)]
    struct DelayedClient;

    #[derive(Clone)]
    struct SubmitClient {
        submitted: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone)]
    struct FailingSubmitClient {
        submitted: Arc<Mutex<Vec<String>>>,
    }

    impl SimRpcClient for PanickingClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { panic!("simulated chain-tip panic") }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            async { panic!("simulated submit panic") }
        }
    }

    impl SimRpcClient for DelayedClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok(ChainTip {
                    height: BlockHeight::from(7),
                    block_hash: Hex0x(SHA256Hash::from([0x07; 32])),
                })
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            async { Ok(TxHash(Hex0x(SHA256Hash::from([0x55; 32])))) }
        }
    }

    impl SimRpcClient for SubmitClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { Err("chain tip should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            let submitted = self.submitted.clone();
            async move {
                submitted
                    .lock()
                    .expect("submitted transaction list should lock")
                    .push(tx_hex);
                Ok(TxHash(Hex0x(SHA256Hash::from([0x5a; 32]))))
            }
        }
    }

    impl SimRpcClient for FailingSubmitClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { Err("chain tip should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            let submitted = self.submitted.clone();
            async move {
                submitted
                    .lock()
                    .expect("submitted transaction list should lock")
                    .push(tx_hex);
                Err("submit failed".into())
            }
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
        let mut actors = actor_states();

        let err = run_schedule(
            PanickingClient,
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
            },
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

    #[tokio::test]
    async fn run_schedule_drains_completions_while_waiting_for_next_phase() {
        let manifest = manifest();
        let schedule = vec![ScheduledSimulationEvent::ActorTick(ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 0,
        })];
        let phase_starts = vec![
            PhaseStart {
                phase_index: 0,
                simulation_time_ms: 0,
                duration_ms: 20,
                users: 1,
                rate_per_user_millihz: 50_000,
            },
            PhaseStart {
                phase_index: 1,
                simulation_time_ms: 20,
                duration_ms: 1,
                users: 1,
                rate_per_user_millihz: 1_000,
            },
        ];
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();

        run_schedule(
            DelayedClient,
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &phase_starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
            },
        )
        .await
        .expect("schedule should run");

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let kinds: Vec<_> = events
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .expect("event should parse")
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .expect("event kind should be present")
                    .to_owned()
            })
            .collect();

        assert_eq!(
            kinds,
            vec!["phase-started", "workload-completed", "phase-started"]
        );
    }

    #[tokio::test]
    async fn run_schedule_executes_wallet_submit_smoke_and_updates_actor_state() {
        let manifest = wallet_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let submitted = Arc::new(Mutex::new(Vec::new()));

        run_schedule(
            SubmitClient {
                submitted: submitted.clone(),
            },
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
            },
        )
        .await
        .expect("wallet submit schedule should run");

        assert_eq!(actors[0].next_nonce_sequence, 1);
        assert_eq!(
            actors[0].last_submitted_tx_hash,
            Some(TxHash(Hex0x(SHA256Hash::from([0x5a; 32]))))
        );
        assert_eq!(actors[0].last_submitted_scheduled_time_ms, Some(0));
        assert_eq!(
            submitted
                .lock()
                .expect("submitted transaction list should lock")
                .len(),
            1
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::WalletSubmitSmoke].completed,
            1
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        assert!(events.contains("\"workload\":\"wallet-submit-smoke\""));
        assert!(events.contains(
            "\"txHash\":\"0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a\""
        ));
        assert!(events.contains("\"txNonce\":\"4294967296\""));
    }

    #[tokio::test]
    async fn failed_wallet_submit_does_not_advance_actor_nonce() {
        let manifest = wallet_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let submitted = Arc::new(Mutex::new(Vec::new()));

        run_schedule(
            FailingSubmitClient {
                submitted: submitted.clone(),
            },
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
            },
        )
        .await
        .expect("failed submit should be recorded as workload failure");

        assert_eq!(actors[0].next_nonce_sequence, 0);
        assert_eq!(actors[0].last_submitted_tx_hash, None);
        assert_eq!(
            submitted
                .lock()
                .expect("submitted transaction list should lock")
                .len(),
            1
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::WalletSubmitSmoke].failed,
            1
        );
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

    fn wallet_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-wallet-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "wallet-submit-smoke", "weight": 1}]
        }))
        .expect("manifest fixture should parse")
    }

    fn actor_states() -> Vec<ActorState> {
        vec![ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("executor-test-seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x99; 32])).0,
            },
            NonceNamespace::new(1).expect("namespace should fit"),
        )]
    }

    fn runtime_manifest() -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x11),
            "producer": {
                "binary": "hogd-dev",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
                "indexer": "127.0.0.1:1001"
            },
            "bootstrapAccountDerivation": {
                "scheme": "groundhog-bootstrap-account-v1",
                "seed": "seed"
            },
            "bootstrapAccounts": [],
            "bootstrap": {
                "devWallet": {
                    "contentHash": prefixed_hex(0x33),
                    "deployedCodeHash": prefixed_hex(0x44),
                    "artifactHint": "./dev-wallet.wasm",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "doActionEntrypoint": "0x646f416374696f6e"
                },
                "storageCostModel": {
                    "hash": prefixed_hex(0x55),
                    "baseDataItemCost": "1",
                    "bytestringCostPerByte": "1",
                    "baseContractDeploymentCost": "1",
                    "contractDeploymentCostPerByte": "1",
                    "authPerKeyCost": "1",
                    "rcqCostPerEntry": "1",
                    "replayCacheCostPerEntry": "1",
                    "balanceCost": "1"
                }
            }
        }))
        .expect("runtime manifest fixture should parse")
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
