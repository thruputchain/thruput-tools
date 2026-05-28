use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use groundhog_rpc_api_types::{
    block::ChainTip,
    hex::{Hex0x, HexBytes},
    receipt::{ExecStatus, TransactionReceipt},
    runtime_manifest::RuntimeManifest,
    transaction::TxHash,
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
};
use groundhog_rpc_client::GroundhogApiClient;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{Id as TaskId, JoinError, JoinSet};

use crate::actor::{ActorState, DrainedPendingReceipt, ReceiptPollTarget};
use crate::contract::PreparedViewCall;
use crate::error::SimError;
use crate::event::{ChaosTransitionEvent, EventWriter, ResultEvent, WorkloadEvent};
use crate::manifest::{ChaosEventKind, ChaosEventSpec, SimulationManifest, WorkloadKind};
use crate::metrics::RunMetricsBuilder;
use crate::result::{ChainTipObservation, ChaosEventRecord, ChaosEventState};
use crate::scheduler::{
    PhaseStart, ScheduledActorTick, ScheduledSimulationEvent, select_workload_for_tick,
};
use crate::target::ChaosController;
use crate::transaction::{
    BuiltSubmitTransaction, build_wallet_submit_smoke_transaction,
    runtime_dev_wallet_action_entrypoint,
};
use crate::util::{duration_ms, json_string, unix_ms, workload_name};

const DEFAULT_GLOBAL_IN_FLIGHT_LIMIT: usize = 1_024;
// Actor state is mutated only when a workload completion is recorded. Keep the
// per-actor limit at one unless nonce/state ownership is redesigned.
const DEFAULT_PER_ACTOR_IN_FLIGHT_LIMIT: usize = 1;
pub const DEFAULT_RECEIPT_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL: Duration = Duration::from_millis(250);

/// Minimal RPC surface used by the Phase 1 simulator executor.
pub trait SimRpcClient {
    fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send;
    fn send_raw_transaction(
        &self,
        tx_hex: String,
    ) -> impl Future<Output = Result<TxHash, String>> + Send;
    /// Return the committed receipt for a transaction hash, or `None` when it is not indexed yet.
    fn transaction_receipt(
        &self,
        _tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
        async { Err("transaction receipt RPC is not implemented by this client".into()) }
    }
    fn view(
        &self,
        _request: ViewRequest,
    ) -> impl Future<Output = Result<ViewResponse, String>> + Send {
        async { Err("view RPC is not implemented by this client".into()) }
    }
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

    #[allow(clippy::manual_async_fn)]
    fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
        async move {
            GroundhogApiClient::get_transaction_receipt(self, tx_hash)
                .await
                .map_err(|error| error.to_string())
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn view(
        &self,
        request: ViewRequest,
    ) -> impl Future<Output = Result<ViewResponse, String>> + Send {
        async move {
            GroundhogApiClient::view(self, request)
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
    pub view_calls: &'a [PreparedViewCall],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScheduleRunReport {
    pub chaos_failed: bool,
}

/// Execute a deterministic simulation schedule against a typed RPC client.
pub async fn run_schedule<C>(client: C, input: ScheduleExecution<'_>) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let mut chaos_events = Vec::new();
    let view_calls = Vec::new();
    let input = ScheduleExecution {
        view_calls: &view_calls,
        ..input
    };
    run_schedule_with_chaos(client, input, None, &mut chaos_events)
        .await
        .map(|_| ())
}

/// Execute a deterministic simulation schedule with an optional target chaos controller.
#[allow(clippy::too_many_lines)]
pub async fn run_schedule_with_chaos<C>(
    client: C,
    input: ScheduleExecution<'_>,
    chaos_controller: Option<&dyn ChaosController>,
    chaos_events: &mut Vec<ChaosEventRecord>,
) -> Result<ScheduleRunReport, SimError>
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
        view_calls,
    } = input;
    let wallet_entrypoint = runtime_dev_wallet_action_entrypoint(runtime_manifest)?;
    let run_start = Instant::now();
    let mut next_phase = phase_starts.iter().peekable();
    let mut tasks = JoinSet::new();
    let mut task_contexts = HashMap::new();
    let global_in_flight = Arc::new(Semaphore::new(DEFAULT_GLOBAL_IN_FLIGHT_LIMIT));
    let actor_in_flight = actor_semaphores(phase_starts);
    let mut in_flight = 0_u64;
    let mut chaos_failed = false;

    for event in schedule {
        let drained = drain_completed_workloads(
            &mut tasks,
            &mut task_contexts,
            actor_states,
            event_writer,
            metrics,
        )
        .await?;
        in_flight = in_flight.checked_sub(drained).ok_or(SimError::Executor(
            "in-flight workload count underflowed".into(),
        ))?;
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
                    view_calls,
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
            ScheduledSimulationEvent::Chaos(chaos) => {
                sleep_until_sim_time_draining(
                    run_start,
                    chaos.simulation_time_ms,
                    &mut tasks,
                    &mut task_contexts,
                    event_writer,
                    metrics,
                    actor_states,
                    &mut in_flight,
                )
                .await?;
                let event_spec =
                    manifest
                        .chaos
                        .get(chaos.chaos_index)
                        .ok_or(SimError::Executor(
                            "scheduled chaos index missing from manifest".into(),
                        ))?;
                chaos_failed = execute_scheduled_chaos(
                    event_spec,
                    chaos.chaos_index,
                    chaos.simulation_time_ms,
                    chaos_controller,
                    &mut tasks,
                    &mut task_contexts,
                    actor_states,
                    event_writer,
                    metrics,
                    chaos_events,
                    &mut in_flight,
                )
                .await?;
                if chaos_failed {
                    break;
                }
            }
        }
    }
    if !chaos_failed {
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
    }
    while in_flight > 0 {
        let joined = tasks
            .join_next_with_id()
            .await
            .ok_or(SimError::Executor("workload task set closed early".into()))?;
        let completion = join_workload(joined, &mut task_contexts)??;
        record_completion(completion, actor_states, event_writer, metrics).await?;
        in_flight = in_flight.checked_sub(1).ok_or(SimError::Executor(
            "in-flight workload count underflowed".into(),
        ))?;
    }
    Ok(ScheduleRunReport { chaos_failed })
}

/// Poll pending receipt queues once the scheduled load has stopped.
///
/// This prevents tail-end submissions from being classified as timed out just
/// because no scheduled receipt-poll tick remained after the submit.
pub async fn drain_pending_receipt_polls<C>(
    client: C,
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
    simulation_time_ms: u64,
    max_wait: Duration,
    retry_interval: Duration,
) -> Result<u64, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let mut drained = 0_u64;
    let deadline = Instant::now()
        .checked_add(max_wait)
        .unwrap_or_else(Instant::now);

    loop {
        for actor_index in 0..actor_states.len() {
            while let Some(target) = actor_states[actor_index].receipt_poll_target() {
                let tick = ScheduledActorTick {
                    phase_index: 0,
                    actor_id: actor_states[actor_index].actor_id,
                    actor_sequence: u64::from(actor_states[actor_index].next_nonce_sequence),
                    simulation_time_ms,
                };
                let completion = execute_receipt_poll(
                    client.clone(),
                    &tick,
                    PreparedReceiptPoll {
                        target: Some(target),
                    },
                )
                .await?;
                let found_receipt = matches!(
                    completion.receipt_poll,
                    Some(ReceiptPollCompletion { present: true })
                );
                record_completion(completion, actor_states, event_writer, metrics).await?;
                drained += 1;
                if !found_receipt {
                    break;
                }
            }
        }
        if actor_states
            .iter()
            .all(|actor| actor.receipt_poll_target().is_none())
        {
            return Ok(drained);
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(drained);
        }
        tokio::time::sleep(retry_interval.min(remaining)).await;
    }
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
    view_calls: &[PreparedViewCall],
) -> Result<PreparedWorkload, SimError> {
    match workload {
        WorkloadKind::ChainTip => Ok(PreparedWorkload::ChainTip),
        WorkloadKind::WalletSubmitSmoke => Ok(PreparedWorkload::WalletSubmitSmoke(
            build_wallet_submit_smoke_transaction(actor, wallet_entrypoint)?,
        )),
        WorkloadKind::ReceiptPoll => Ok(PreparedWorkload::ReceiptPoll(PreparedReceiptPoll {
            target: actor.receipt_poll_target(),
        })),
        WorkloadKind::ViewCall => {
            let view = view_calls.first().ok_or_else(|| {
                SimError::Executor("view-call workload requires at least one prepared view".into())
            })?;
            Ok(PreparedWorkload::ViewCall(PreparedViewWorkload {
                view: view.clone(),
                caller: actor.wallet_address,
            }))
        }
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
        scheduled_time_ms: tick.simulation_time_ms,
    };

    let abort = tasks.spawn(async move {
        let _permits = permits;
        match prepared {
            PreparedWorkload::ChainTip => execute_chain_tip(client, &tick).await,
            PreparedWorkload::WalletSubmitSmoke(tx) => {
                execute_wallet_submit_smoke(client, &tick, tx).await
            }
            PreparedWorkload::ReceiptPoll(poll) => execute_receipt_poll(client, &tick, poll).await,
            PreparedWorkload::ViewCall(view) => execute_view_call(client, &tick, view).await,
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
        actor.record_submitted_tx(
            submitted.tx_hash,
            submitted.scheduled_time_ms,
            submitted.completed_at,
        )?;
    }
    if let Some(observed) = completion.observed_receipt {
        let actor = actor_states
            .get_mut(observed.actor_id as usize)
            .ok_or(SimError::Executor("actor state missing".into()))?;
        actor.record_receipt(observed.receipt);
    }
    if let Some(receipt_poll) = completion.receipt_poll {
        metrics.record_receipt_poll(receipt_poll.present);
    }
    if let Some(latency_ms) = completion.submit_to_receipt_latency_ms {
        metrics.record_submit_to_receipt_latency(completion.workload, latency_ms);
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
                receipt_present: None,
                receipt_status: None,
                receipt_status_detail: None,
                submit_to_receipt_latency_ms: None,
                view_result: None,
                view_return_data: None,
                view_abort_data: None,
                view_event_count: None,
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
        observed_receipt: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn execute_scheduled_chaos(
    event_spec: &ChaosEventSpec,
    chaos_index: usize,
    simulation_time_ms: u64,
    chaos_controller: Option<&dyn ChaosController>,
    tasks: &mut JoinSet<Result<WorkloadCompletion, SimError>>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
    chaos_events: &mut Vec<ChaosEventRecord>,
    in_flight: &mut u64,
) -> Result<bool, SimError> {
    let (scheduled_started, scheduled_started_unix) = transition_start()?;
    write_chaos_transition(
        event_writer,
        chaos_events,
        ChaosTransitionInput {
            chaos_index,
            chaos_kind: event_spec.kind,
            state: ChaosEventState::Scheduled,
            simulation_time_ms,
            started: scheduled_started,
            started_at_unix_ms: scheduled_started_unix,
            completed: None,
            detail: None,
        },
    )
    .await?;

    let Some(controller) = chaos_controller else {
        let (failed_started, failed_started_unix) = transition_start()?;
        write_chaos_transition(
            event_writer,
            chaos_events,
            ChaosTransitionInput {
                chaos_index,
                chaos_kind: event_spec.kind,
                state: ChaosEventState::Failed,
                simulation_time_ms,
                started: failed_started,
                started_at_unix_ms: failed_started_unix,
                completed: Some(Instant::now()),
                detail: Some("target provider does not expose a chaos controller".into()),
            },
        )
        .await?;
        return Ok(true);
    };

    tokio::task::yield_now().await;
    let drained =
        drain_completed_workloads(tasks, task_contexts, actor_states, event_writer, metrics)
            .await?;
    *in_flight = in_flight.checked_sub(drained).ok_or(SimError::Executor(
        "in-flight workload count underflowed".into(),
    ))?;
    let aborted_contexts = abort_in_flight_for_restart(tasks, task_contexts, in_flight).await?;

    let (started, started_unix) = transition_start()?;
    write_chaos_transition(
        event_writer,
        chaos_events,
        ChaosTransitionInput {
            chaos_index,
            chaos_kind: event_spec.kind,
            state: ChaosEventState::Started,
            simulation_time_ms,
            started,
            started_at_unix_ms: started_unix.clone(),
            completed: None,
            detail: None,
        },
    )
    .await?;

    match controller.execute(event_spec).await {
        Ok(outcome) => {
            // We abort before restart to stop dispatch against a dying target, but
            // record restart-loss diagnostics only after recovery is confirmed.
            record_aborted_for_restart(
                aborted_contexts,
                actor_states,
                event_writer,
                metrics,
                simulation_time_ms,
            )
            .await?;
            record_restart_lost_receipts(actor_states, event_writer, metrics, simulation_time_ms)
                .await?;
            let completed = Instant::now();
            write_chaos_transition(
                event_writer,
                chaos_events,
                ChaosTransitionInput {
                    chaos_index,
                    chaos_kind: event_spec.kind,
                    state: ChaosEventState::Completed,
                    simulation_time_ms,
                    started,
                    started_at_unix_ms: started_unix.clone(),
                    completed: Some(completed),
                    detail: outcome.detail.clone(),
                },
            )
            .await?;
            if outcome.state == ChaosEventState::Recovered {
                write_chaos_transition(
                    event_writer,
                    chaos_events,
                    ChaosTransitionInput {
                        chaos_index,
                        chaos_kind: event_spec.kind,
                        state: ChaosEventState::Recovered,
                        simulation_time_ms,
                        started,
                        started_at_unix_ms: started_unix,
                        completed: Some(Instant::now()),
                        detail: outcome.detail,
                    },
                )
                .await?;
            }
            Ok(false)
        }
        Err(error) => {
            record_aborted_for_failed_chaos(
                aborted_contexts,
                actor_states,
                event_writer,
                metrics,
                simulation_time_ms,
            )
            .await?;
            write_chaos_transition(
                event_writer,
                chaos_events,
                ChaosTransitionInput {
                    chaos_index,
                    chaos_kind: event_spec.kind,
                    state: ChaosEventState::Failed,
                    simulation_time_ms,
                    started,
                    started_at_unix_ms: started_unix,
                    completed: Some(Instant::now()),
                    detail: Some(error.to_string()),
                },
            )
            .await?;
            Ok(true)
        }
    }
}

async fn abort_in_flight_for_restart(
    tasks: &mut JoinSet<Result<WorkloadCompletion, SimError>>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
    in_flight: &mut u64,
) -> Result<Vec<WorkloadTaskContext>, SimError> {
    let contexts: Vec<_> = task_contexts.drain().map(|(_, context)| context).collect();
    if contexts.is_empty() {
        *in_flight = 0;
        return Ok(contexts);
    }

    tasks.abort_all();
    while tasks.join_next_with_id().await.is_some() {}
    *in_flight = 0;
    Ok(contexts)
}

async fn record_aborted_for_restart(
    contexts: Vec<WorkloadTaskContext>,
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
    simulation_time_ms: u64,
) -> Result<(), SimError> {
    for context in contexts {
        if context.workload == WorkloadKind::ReceiptPoll {
            continue;
        }
        if context.workload == WorkloadKind::WalletSubmitSmoke {
            let actor = actor_states
                .get_mut(context.actor_id as usize)
                .ok_or(SimError::Executor("actor state missing".into()))?;
            actor.record_uncertain_submit_nonce()?;
        }
        record_completion(
            target_restarted_workload_completion(context, simulation_time_ms)?,
            actor_states,
            event_writer,
            metrics,
        )
        .await?;
    }
    Ok(())
}

async fn record_aborted_for_failed_chaos(
    contexts: Vec<WorkloadTaskContext>,
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
    simulation_time_ms: u64,
) -> Result<(), SimError> {
    for context in contexts {
        record_completion(
            chaos_failed_workload_completion(context, simulation_time_ms)?,
            actor_states,
            event_writer,
            metrics,
        )
        .await?;
    }
    Ok(())
}

async fn record_restart_lost_receipts(
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
    simulation_time_ms: u64,
) -> Result<(), SimError> {
    let drained: Vec<_> = actor_states
        .iter_mut()
        .flat_map(ActorState::drain_pending_receipts)
        .collect();
    for receipt in drained {
        record_completion(
            target_restarted_receipt_completion(receipt, simulation_time_ms)?,
            actor_states,
            event_writer,
            metrics,
        )
        .await?;
    }
    Ok(())
}

fn target_restarted_workload_completion(
    context: WorkloadTaskContext,
    simulation_time_ms: u64,
) -> Result<WorkloadCompletion, SimError> {
    let now = unix_ms()?.to_string();
    Ok(WorkloadCompletion {
        event: ResultEvent::WorkloadFailed {
            event: WorkloadEvent {
                actor_id: context.actor_id,
                workload: context.workload,
                scheduled_time_ms: context.scheduled_time_ms,
                started_at_unix_ms: now.clone(),
                completed_at_unix_ms: now,
                latency_ms: 0,
                rpc_method: rpc_method(context.workload),
                chain_height: None,
                chain_block_hash: None,
                tx_hash: None,
                tx_nonce: None,
                receipt_present: None,
                receipt_status: None,
                receipt_status_detail: None,
                submit_to_receipt_latency_ms: None,
                view_result: None,
                view_return_data: None,
                view_abort_data: None,
                view_event_count: None,
                error_class: Some("target-restarted".into()),
                error: Some(format!(
                    "workload aborted at simulation time {simulation_time_ms} because target restarted"
                )),
            },
        },
        workload: context.workload,
        latency_ms: 0,
        error_class: Some("target-restarted".into()),
        skipped: false,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
    })
}

fn chaos_failed_workload_completion(
    context: WorkloadTaskContext,
    simulation_time_ms: u64,
) -> Result<WorkloadCompletion, SimError> {
    let now = unix_ms()?.to_string();
    Ok(WorkloadCompletion {
        event: ResultEvent::WorkloadFailed {
            event: WorkloadEvent {
                actor_id: context.actor_id,
                workload: context.workload,
                scheduled_time_ms: context.scheduled_time_ms,
                started_at_unix_ms: now.clone(),
                completed_at_unix_ms: now,
                latency_ms: 0,
                rpc_method: rpc_method(context.workload),
                chain_height: None,
                chain_block_hash: None,
                tx_hash: None,
                tx_nonce: None,
                receipt_present: None,
                receipt_status: None,
                receipt_status_detail: None,
                submit_to_receipt_latency_ms: None,
                view_result: None,
                view_return_data: None,
                view_abort_data: None,
                view_event_count: None,
                error_class: Some("chaos-failed".into()),
                error: Some(format!(
                    "workload aborted at simulation time {simulation_time_ms} because chaos failed"
                )),
            },
        },
        workload: context.workload,
        latency_ms: 0,
        error_class: Some("chaos-failed".into()),
        skipped: false,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
    })
}

fn target_restarted_receipt_completion(
    receipt: DrainedPendingReceipt,
    simulation_time_ms: u64,
) -> Result<WorkloadCompletion, SimError> {
    let now = unix_ms()?.to_string();
    let tx_hash = json_string(&receipt.tx_hash, "txHash")?;
    Ok(WorkloadCompletion {
        event: ResultEvent::WorkloadFailed {
            event: WorkloadEvent {
                actor_id: receipt.actor_id,
                workload: WorkloadKind::ReceiptPoll,
                scheduled_time_ms: simulation_time_ms,
                started_at_unix_ms: now.clone(),
                completed_at_unix_ms: now,
                latency_ms: 0,
                rpc_method: "groundhog_getTransactionReceipt",
                chain_height: None,
                chain_block_hash: None,
                tx_hash: Some(tx_hash),
                tx_nonce: None,
                receipt_present: Some(false),
                receipt_status: None,
                receipt_status_detail: None,
                submit_to_receipt_latency_ms: Some(duration_ms(
                    Instant::now().saturating_duration_since(receipt.submit_completed_at),
                )),
                view_result: None,
                view_return_data: None,
                view_abort_data: None,
                view_event_count: None,
                error_class: Some("target-restarted".into()),
                error: Some("pending receipt lost because target restarted".into()),
            },
        },
        workload: WorkloadKind::ReceiptPoll,
        latency_ms: 0,
        error_class: Some("target-restarted".into()),
        skipped: false,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
    })
}

struct ChaosTransitionInput {
    chaos_index: usize,
    chaos_kind: ChaosEventKind,
    state: ChaosEventState,
    simulation_time_ms: u64,
    started: Instant,
    started_at_unix_ms: String,
    completed: Option<Instant>,
    detail: Option<String>,
}

fn transition_start() -> Result<(Instant, String), SimError> {
    Ok((Instant::now(), unix_ms()?.to_string()))
}

async fn write_chaos_transition(
    event_writer: &mut EventWriter,
    chaos_events: &mut Vec<ChaosEventRecord>,
    input: ChaosTransitionInput,
) -> Result<(), SimError> {
    let completed_at_unix_ms = input
        .completed
        .map(|_| unix_ms())
        .transpose()?
        .map(|ms| ms.to_string());
    let latency_ms = input
        .completed
        .map(|completed| duration_ms(completed.saturating_duration_since(input.started)));
    let kind = chaos_kind_name(input.chaos_kind).to_string();
    // Keep the streaming JSONL event and aggregate result record coupled so
    // artifact consumers see the same transition sequence in both places.
    event_writer
        .write(&ResultEvent::ChaosTransition {
            event: ChaosTransitionEvent {
                chaos_index: input.chaos_index,
                chaos_kind: input.chaos_kind,
                state: input.state,
                simulation_time_ms: input.simulation_time_ms,
                started_at_unix_ms: input.started_at_unix_ms.clone(),
                completed_at_unix_ms,
                latency_ms,
                detail: input.detail.clone(),
            },
        })
        .await?;
    chaos_events.push(ChaosEventRecord {
        kind,
        state: input.state,
        simulation_time_ms: input.simulation_time_ms,
        chaos_index: Some(input.chaos_index),
        latency_ms,
        detail: input.detail,
    });
    Ok(())
}

fn chaos_kind_name(kind: ChaosEventKind) -> &'static str {
    match kind {
        ChaosEventKind::RestartTarget => "restart-target",
    }
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
                        receipt_present: None,
                        receipt_status: None,
                        receipt_status_detail: None,
                        submit_to_receipt_latency_ms: None,
                        view_result: None,
                        view_return_data: None,
                        view_abort_data: None,
                        view_event_count: None,
                        error_class: None,
                        error: None,
                    },
                },
                skipped: false,
                submitted_transaction: None,
                observed_receipt: None,
                receipt_poll: None,
                submit_to_receipt_latency_ms: None,
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
                    receipt_present: None,
                    receipt_status: None,
                    receipt_status_detail: None,
                    submit_to_receipt_latency_ms: None,
                    view_result: None,
                    view_return_data: None,
                    view_abort_data: None,
                    view_event_count: None,
                    error_class: Some("rpc-error".into()),
                    error: Some(error),
                },
            },
            skipped: false,
            submitted_transaction: None,
            observed_receipt: None,
            receipt_poll: None,
            submit_to_receipt_latency_ms: None,
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
    let completed_at = Instant::now();
    let latency_ms = duration_ms(completed_at.duration_since(started));
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
                    completed_at,
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
                        receipt_present: None,
                        receipt_status: None,
                        receipt_status_detail: None,
                        submit_to_receipt_latency_ms: None,
                        view_result: None,
                        view_return_data: None,
                        view_abort_data: None,
                        view_event_count: None,
                        error_class: None,
                        error: None,
                    },
                },
                skipped: false,
                observed_receipt: None,
                receipt_poll: None,
                submit_to_receipt_latency_ms: None,
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
                    receipt_present: None,
                    receipt_status: None,
                    receipt_status_detail: None,
                    submit_to_receipt_latency_ms: None,
                    view_result: None,
                    view_return_data: None,
                    view_abort_data: None,
                    view_event_count: None,
                    error_class: Some("rpc-error".into()),
                    error: Some(error),
                },
            },
            skipped: false,
            observed_receipt: None,
            receipt_poll: None,
            submit_to_receipt_latency_ms: None,
        }),
    }
}

async fn execute_receipt_poll<C>(
    client: C,
    tick: &ScheduledActorTick,
    poll: PreparedReceiptPoll,
) -> Result<WorkloadCompletion, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let Some(target) = poll.target else {
        return receipt_poll_no_submitted_tx(tick, started_at_unix_ms);
    };
    let tx_hash = target.tx_hash;

    let response = client.transaction_receipt(tx_hash).await;
    let completed = Instant::now();
    let times = ReceiptPollTimes {
        started_unix: started_at_unix_ms,
        completed_unix: unix_ms()?.to_string(),
        rpc_latency: duration_ms(completed.duration_since(started)),
        submit_to_receipt_latency: Some(duration_ms(
            completed.saturating_duration_since(target.submit_completed_at),
        )),
    };
    let tx_hash_json = json_string(&tx_hash, "txHash")?;

    match response {
        Ok(Some(receipt)) => Ok(receipt_poll_present(tick, tx_hash_json, &times, receipt)),
        Ok(None) => Ok(receipt_poll_absent(tick, tx_hash_json, &times)),
        Err(error) => Ok(receipt_poll_failed(tick, tx_hash_json, &times, error)),
    }
}

fn receipt_poll_no_submitted_tx(
    tick: &ScheduledActorTick,
    started_at_unix_ms: String,
) -> Result<WorkloadCompletion, SimError> {
    Ok(WorkloadCompletion {
        workload: WorkloadKind::ReceiptPoll,
        latency_ms: 0,
        error_class: Some("no-submitted-transaction".into()),
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        event: ResultEvent::WorkloadSkipped {
            event: receipt_poll_event(
                tick,
                &ReceiptPollTimes {
                    rpc_latency: 0,
                    submit_to_receipt_latency: None,
                    started_unix: started_at_unix_ms,
                    completed_unix: unix_ms()?.to_string(),
                },
                None,
            )
            .with_error(
                "no-submitted-transaction",
                "actor has not submitted a transaction yet",
            ),
        },
        skipped: true,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
    })
}

fn receipt_poll_present(
    tick: &ScheduledActorTick,
    tx_hash_json: String,
    times: &ReceiptPollTimes,
    receipt: TransactionReceipt,
) -> WorkloadCompletion {
    let chain_height = *receipt.height;
    let receipt_status = SimReceiptStatus::from_exec_status(&receipt.status);
    WorkloadCompletion {
        workload: WorkloadKind::ReceiptPoll,
        latency_ms: times.rpc_latency,
        error_class: None,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: Some(ObservedReceipt {
            actor_id: tick.actor_id,
            receipt,
        }),
        event: ResultEvent::WorkloadCompleted {
            event: receipt_poll_event(tick, times, Some(tx_hash_json)).with_receipt(
                Some(chain_height),
                true,
                receipt_status,
                times.submit_to_receipt_latency,
            ),
        },
        skipped: false,
        receipt_poll: Some(ReceiptPollCompletion { present: true }),
        submit_to_receipt_latency_ms: times.submit_to_receipt_latency,
    }
}

fn receipt_poll_absent(
    tick: &ScheduledActorTick,
    tx_hash_json: String,
    times: &ReceiptPollTimes,
) -> WorkloadCompletion {
    WorkloadCompletion {
        workload: WorkloadKind::ReceiptPoll,
        latency_ms: times.rpc_latency,
        error_class: None,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        event: ResultEvent::WorkloadCompleted {
            event: receipt_poll_event(tick, times, Some(tx_hash_json)).with_receipt(
                None,
                false,
                SimReceiptStatus::absent(),
                None,
            ),
        },
        skipped: false,
        receipt_poll: Some(ReceiptPollCompletion { present: false }),
        submit_to_receipt_latency_ms: None,
    }
}

fn receipt_poll_failed(
    tick: &ScheduledActorTick,
    tx_hash_json: String,
    times: &ReceiptPollTimes,
    error: String,
) -> WorkloadCompletion {
    WorkloadCompletion {
        workload: WorkloadKind::ReceiptPoll,
        latency_ms: times.rpc_latency,
        error_class: Some("rpc-error".into()),
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        event: ResultEvent::WorkloadFailed {
            event: receipt_poll_event(tick, times, Some(tx_hash_json))
                .with_error("rpc-error", error),
        },
        skipped: false,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
    }
}

async fn execute_view_call<C>(
    client: C,
    tick: &ScheduledActorTick,
    prepared: PreparedViewWorkload,
) -> Result<WorkloadCompletion, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let request = ViewRequest {
        contract: Hex0x(prepared.view.contract),
        entrypoint: HexBytes(prepared.view.entrypoint.clone()),
        calldata: HexBytes(prepared.view.calldata.clone()),
        caller: Some(Hex0x(prepared.caller)),
    };
    let response = client.view(request).await;
    let latency_ms = duration_ms(started.elapsed());
    let completed_at_unix_ms = unix_ms()?.to_string();

    match response {
        Ok(response) => Ok(view_response_completion(
            tick,
            &prepared.view,
            response,
            started_at_unix_ms,
            completed_at_unix_ms,
            latency_ms,
        )),
        Err(error) => Ok(WorkloadCompletion {
            workload: WorkloadKind::ViewCall,
            latency_ms,
            error_class: Some("view-rpc-error".into()),
            chain_tip: None,
            event: ResultEvent::WorkloadFailed {
                event: view_workload_event(
                    tick,
                    started_at_unix_ms,
                    completed_at_unix_ms,
                    latency_ms,
                )
                .with_error("view-rpc-error", error),
            },
            skipped: false,
            submitted_transaction: None,
            observed_receipt: None,
            receipt_poll: None,
            submit_to_receipt_latency_ms: None,
        }),
    }
}

fn view_response_completion(
    tick: &ScheduledActorTick,
    view: &PreparedViewCall,
    response: ViewResponse,
    started_at_unix_ms: String,
    completed_at_unix_ms: String,
    latency_ms: u64,
) -> WorkloadCompletion {
    let mut event = view_workload_event(tick, started_at_unix_ms, completed_at_unix_ms, latency_ms);
    event.chain_height = Some(*response.height);
    event.view_event_count = Some(response.events.len() as u64);

    match response.output {
        RootInvocationOutput::Return { return_data } => {
            let actual = format!("0x{}", hex::encode(&return_data.0));
            event.view_result = Some("return".into());
            event.view_return_data = Some(actual.clone());
            if let Some(expected) = &view.expect_return_data
                && expected.as_slice() != return_data.0.as_slice()
            {
                let expected = format!("0x{}", hex::encode(expected));
                let error = format!(
                    "view call `{}` on `{}` returned {actual}, expected {expected}",
                    view.name, view.contract_name
                );
                return view_failure_completion(event, latency_ms, "view-return-mismatch", error);
            }
            view_success_completion(event, latency_ms)
        }
        RootInvocationOutput::Abort { abort_data } => {
            event.view_result = Some("abort".into());
            event.view_abort_data = Some(format!("0x{}", hex::encode(abort_data.0)));
            view_failure_completion(
                event,
                latency_ms,
                "view-aborted",
                format!("view call `{}` aborted", view.name),
            )
        }
        RootInvocationOutput::Fault => {
            event.view_result = Some("fault".into());
            view_failure_completion(
                event,
                latency_ms,
                "view-faulted",
                format!("view call `{}` faulted", view.name),
            )
        }
        _ => {
            event.view_result = Some("fault".into());
            view_failure_completion(
                event,
                latency_ms,
                "view-faulted",
                format!(
                    "view call `{}` returned an unknown output variant",
                    view.name
                ),
            )
        }
    }
}

fn view_success_completion(event: WorkloadEvent, latency_ms: u64) -> WorkloadCompletion {
    WorkloadCompletion {
        workload: WorkloadKind::ViewCall,
        latency_ms,
        error_class: None,
        chain_tip: None,
        event: ResultEvent::WorkloadCompleted { event },
        skipped: false,
        submitted_transaction: None,
        observed_receipt: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
    }
}

fn view_failure_completion(
    event: WorkloadEvent,
    latency_ms: u64,
    class: impl Into<String>,
    error: impl Into<String>,
) -> WorkloadCompletion {
    let class = class.into();
    WorkloadCompletion {
        workload: WorkloadKind::ViewCall,
        latency_ms,
        error_class: Some(class.clone()),
        chain_tip: None,
        event: ResultEvent::WorkloadFailed {
            event: event.with_error(class, error),
        },
        skipped: false,
        submitted_transaction: None,
        observed_receipt: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
    }
}

fn view_workload_event(
    tick: &ScheduledActorTick,
    started_at_unix_ms: String,
    completed_at_unix_ms: String,
    latency_ms: u64,
) -> WorkloadEvent {
    WorkloadEvent {
        actor_id: tick.actor_id,
        workload: WorkloadKind::ViewCall,
        scheduled_time_ms: tick.simulation_time_ms,
        started_at_unix_ms,
        completed_at_unix_ms,
        latency_ms,
        rpc_method: "groundhog_view",
        chain_height: None,
        chain_block_hash: None,
        tx_hash: None,
        tx_nonce: None,
        receipt_present: None,
        receipt_status: None,
        receipt_status_detail: None,
        submit_to_receipt_latency_ms: None,
        view_result: None,
        view_return_data: None,
        view_abort_data: None,
        view_event_count: None,
        error_class: None,
        error: None,
    }
}

fn receipt_poll_event(
    tick: &ScheduledActorTick,
    times: &ReceiptPollTimes,
    tx_hash: Option<String>,
) -> WorkloadEvent {
    WorkloadEvent {
        actor_id: tick.actor_id,
        workload: WorkloadKind::ReceiptPoll,
        scheduled_time_ms: tick.simulation_time_ms,
        started_at_unix_ms: times.started_unix.clone(),
        completed_at_unix_ms: times.completed_unix.clone(),
        latency_ms: times.rpc_latency,
        rpc_method: "groundhog_getTransactionReceipt",
        chain_height: None,
        chain_block_hash: None,
        tx_hash,
        tx_nonce: None,
        receipt_present: None,
        receipt_status: None,
        receipt_status_detail: None,
        submit_to_receipt_latency_ms: None,
        view_result: None,
        view_return_data: None,
        view_abort_data: None,
        view_event_count: None,
        error_class: None,
        error: None,
    }
}

trait ReceiptPollEventExt {
    fn with_receipt(
        self,
        chain_height: Option<i64>,
        present: bool,
        status: SimReceiptStatus,
        submit_to_receipt_latency_ms: Option<u64>,
    ) -> Self;
    fn with_error(self, class: impl Into<String>, error: impl Into<String>) -> Self;
}

impl ReceiptPollEventExt for WorkloadEvent {
    fn with_receipt(
        mut self,
        chain_height: Option<i64>,
        present: bool,
        status: SimReceiptStatus,
        submit_to_receipt_latency_ms: Option<u64>,
    ) -> Self {
        self.chain_height = chain_height;
        self.receipt_present = Some(present);
        self.receipt_status = status.name;
        self.receipt_status_detail = status.detail;
        if present {
            self.submit_to_receipt_latency_ms = submit_to_receipt_latency_ms;
        }
        self
    }

    fn with_error(mut self, class: impl Into<String>, error: impl Into<String>) -> Self {
        self.error_class = Some(class.into());
        self.error = Some(error.into());
        self
    }
}

struct SimReceiptStatus {
    name: Option<String>,
    detail: Option<String>,
}

impl SimReceiptStatus {
    fn absent() -> Self {
        Self {
            name: None,
            detail: None,
        }
    }

    fn from_exec_status(status: &ExecStatus) -> Self {
        match status {
            ExecStatus::Ok => Self::named("ok"),
            ExecStatus::Reverted { message } => Self {
                name: Some("reverted".into()),
                detail: message.clone(),
            },
            ExecStatus::DeterministicError { kind } => Self {
                name: Some("deterministicError".into()),
                detail: Some(kind.clone()),
            },
            ExecStatus::NonDeterministicError => Self::named("nonDeterministicError"),
            ExecStatus::FailedGasSponsorship => Self::named("failedGasSponsorship"),
            _ => Self::named("other"),
        }
    }

    fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            detail: None,
        }
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
    observed_receipt: Option<ObservedReceipt>,
    receipt_poll: Option<ReceiptPollCompletion>,
    submit_to_receipt_latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct SubmittedTransaction {
    actor_id: u32,
    tx_hash: TxHash,
    scheduled_time_ms: u64,
    completed_at: Instant,
}

struct ObservedReceipt {
    actor_id: u32,
    receipt: TransactionReceipt,
}

#[derive(Debug, Clone, Copy)]
struct ReceiptPollCompletion {
    present: bool,
}

struct WorkloadPermits {
    _global: OwnedSemaphorePermit,
    _actor: OwnedSemaphorePermit,
}

enum PreparedWorkload {
    ChainTip,
    WalletSubmitSmoke(BuiltSubmitTransaction),
    ReceiptPoll(PreparedReceiptPoll),
    ViewCall(PreparedViewWorkload),
}

#[derive(Debug, Clone, Copy)]
struct PreparedReceiptPoll {
    target: Option<ReceiptPollTarget>,
}

#[derive(Debug, Clone)]
struct PreparedViewWorkload {
    view: PreparedViewCall,
    caller: groundhog_xdr::types::ContractAddress,
}

struct ReceiptPollTimes {
    started_unix: String,
    completed_unix: String,
    rpc_latency: u64,
    submit_to_receipt_latency: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct WorkloadTaskContext {
    actor_id: u32,
    workload: WorkloadKind,
    scheduled_time_ms: u64,
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
        BlockHeight,
        block::ChainTip,
        hex::Hex0x,
        receipt::{ExecStatus, TransactionReceipt},
        runtime_manifest::RuntimeManifest,
        view::{RootInvocationOutput, ViewResponse},
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
    use crate::target::{ChaosController, ChaosEventOutcome, TargetProviderFuture};

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

    #[derive(Clone)]
    struct ReceiptClient {
        receipt: Option<TransactionReceipt>,
        requested: Arc<Mutex<Vec<TxHash>>>,
    }

    #[derive(Clone)]
    struct EchoReceiptClient {
        requested: Arc<Mutex<Vec<TxHash>>>,
    }

    #[derive(Clone)]
    struct EventuallyReceiptClient {
        requested: Arc<Mutex<Vec<TxHash>>>,
        absent_before_present: Arc<Mutex<u32>>,
    }

    #[derive(Clone)]
    struct FailingReceiptClient {
        requested: Arc<Mutex<Vec<TxHash>>>,
    }

    #[derive(Clone)]
    struct SlowReceiptClient {
        requested: Arc<Mutex<Vec<TxHash>>>,
    }

    #[derive(Clone)]
    struct ViewClient {
        response: ViewResponse,
    }

    struct SuccessfulChaosController;

    struct FailingChaosController;

    impl ChaosController for SuccessfulChaosController {
        fn execute<'a>(
            &'a self,
            _event: &'a ChaosEventSpec,
        ) -> TargetProviderFuture<'a, Result<ChaosEventOutcome, SimError>> {
            Box::pin(async {
                Ok(ChaosEventOutcome {
                    state: ChaosEventState::Recovered,
                    detail: None,
                })
            })
        }
    }

    impl ChaosController for FailingChaosController {
        fn execute<'a>(
            &'a self,
            _event: &'a ChaosEventSpec,
        ) -> TargetProviderFuture<'a, Result<ChaosEventOutcome, SimError>> {
            Box::pin(async { Err(SimError::Executor("restart failed".into())) })
        }
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

    impl SimRpcClient for ViewClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { panic!("chain_tip should not be called") }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            async { panic!("send_raw_transaction should not be called") }
        }

        #[allow(clippy::manual_async_fn)]
        fn view(
            &self,
            _request: ViewRequest,
        ) -> impl Future<Output = Result<ViewResponse, String>> + Send {
            let response = self.response.clone();
            async move { Ok(response) }
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

    impl SimRpcClient for ReceiptClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { Err("chain tip should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            async { Err("submit should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let requested = self.requested.clone();
            let receipt = self.receipt.clone();
            async move {
                requested
                    .lock()
                    .expect("requested receipt list should lock")
                    .push(tx_hash);
                Ok(receipt)
            }
        }
    }

    impl SimRpcClient for EchoReceiptClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { Err("chain tip should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            async { Err("submit should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let requested = self.requested.clone();
            async move {
                requested
                    .lock()
                    .expect("requested receipt list should lock")
                    .push(tx_hash);
                Ok(Some(receipt(tx_hash)))
            }
        }
    }

    impl SimRpcClient for EventuallyReceiptClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { Err("chain tip should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            async { Err("submit should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let requested = self.requested.clone();
            let absent_before_present = self.absent_before_present.clone();
            async move {
                requested
                    .lock()
                    .expect("requested receipt list should lock")
                    .push(tx_hash);
                let mut remaining = absent_before_present
                    .lock()
                    .expect("absent count should lock");
                if *remaining == 0 {
                    Ok(Some(receipt(tx_hash)))
                } else {
                    *remaining -= 1;
                    Ok(None)
                }
            }
        }
    }

    impl SimRpcClient for FailingReceiptClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { Err("chain tip should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            async { Err("submit should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let requested = self.requested.clone();
            async move {
                requested
                    .lock()
                    .expect("requested receipt list should lock")
                    .push(tx_hash);
                Err("receipt rpc failed".into())
            }
        }
    }

    impl SimRpcClient for SlowReceiptClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { Err("chain tip should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            async { Err("submit should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let requested = self.requested.clone();
            async move {
                requested
                    .lock()
                    .expect("requested receipt list should lock")
                    .push(tx_hash);
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(None)
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
                view_calls: &[],
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
                view_calls: &[],
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
                view_calls: &[],
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
                view_calls: &[],
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

    #[tokio::test]
    async fn view_call_records_return_fields() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let completion = execute_view_call(
            ViewClient {
                response: ViewResponse {
                    output: RootInvocationOutput::Return {
                        return_data: HexBytes(vec![0x37]),
                    },
                    events: Vec::new(),
                    height: BlockHeight::from(44),
                },
            },
            &tick,
            prepared_view(Some(vec![0x37])),
        )
        .await
        .expect("view call should complete");

        assert_eq!(completion.error_class, None);
        match completion.event {
            ResultEvent::WorkloadCompleted { event } => {
                assert_eq!(event.workload, WorkloadKind::ViewCall);
                assert_eq!(event.rpc_method, "groundhog_view");
                assert_eq!(event.chain_height, Some(44));
                assert_eq!(event.view_result.as_deref(), Some("return"));
                assert_eq!(event.view_return_data.as_deref(), Some("0x37"));
                assert_eq!(event.view_event_count, Some(0));
            }
            other => panic!("expected completed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn view_call_return_mismatch_is_failed_workload() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let completion = execute_view_call(
            ViewClient {
                response: ViewResponse {
                    output: RootInvocationOutput::Return {
                        return_data: HexBytes(vec![0x37]),
                    },
                    events: Vec::new(),
                    height: BlockHeight::from(44),
                },
            },
            &tick,
            prepared_view(Some(vec![0x38])),
        )
        .await
        .expect("view call should complete");

        assert_eq!(
            completion.error_class.as_deref(),
            Some("view-return-mismatch")
        );
        match completion.event {
            ResultEvent::WorkloadFailed { event } => {
                assert_eq!(event.view_result.as_deref(), Some("return"));
                assert_eq!(event.view_return_data.as_deref(), Some("0x37"));
                assert_eq!(event.error_class.as_deref(), Some("view-return-mismatch"));
            }
            other => panic!("expected failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn receipt_poll_without_submitted_tx_is_skipped() {
        let manifest = receipt_manifest();
        let schedule = vec![ScheduledSimulationEvent::ActorTick(ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 0,
        })];
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let requested = Arc::new(Mutex::new(Vec::new()));

        run_schedule(
            ReceiptClient {
                receipt: None,
                requested: requested.clone(),
            },
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
        )
        .await
        .expect("receipt poll should run");

        assert!(
            requested
                .lock()
                .expect("requested list should lock")
                .is_empty()
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ReceiptPoll].skipped,
            1
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        assert!(events.contains("\"kind\":\"workload-skipped\""));
        assert!(events.contains("\"errorClass\":\"no-submitted-transaction\""));
    }

    #[tokio::test]
    async fn receipt_poll_records_null_receipt_as_completed() {
        let manifest = receipt_manifest();
        let schedule = vec![ScheduledSimulationEvent::ActorTick(ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 10,
        })];
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(
                tx_hash(0x5a),
                3,
                Instant::now()
                    .checked_sub(Duration::from_millis(7))
                    .expect("test instant should support subtraction"),
            )
            .expect("submitted tx should record");
        let requested = Arc::new(Mutex::new(Vec::new()));

        run_schedule(
            ReceiptClient {
                receipt: None,
                requested: requested.clone(),
            },
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
        )
        .await
        .expect("receipt poll should run");

        assert_eq!(
            requested
                .lock()
                .expect("requested list should lock")
                .as_slice(),
            &[tx_hash(0x5a)]
        );
        assert!(actors[0].last_observed_receipt.is_none());
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ReceiptPoll].completed,
            1
        );
        let finished = metrics.finish();
        assert_eq!(
            finished
                .workload_metrics
                .get(&WorkloadKind::ReceiptPoll)
                .and_then(|metrics| metrics.receipt_poll.as_ref())
                .expect("receipt-poll summary should be recorded")
                .absent,
            1
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        assert!(events.contains("\"workload\":\"receipt-poll\""));
        assert!(events.contains("\"receiptPresent\":false"));
        assert!(!events.contains("\"errorClass\""));
    }

    #[tokio::test]
    async fn receipt_poll_records_receipt_status_and_submit_latency() {
        let manifest = receipt_manifest();
        let schedule = vec![ScheduledSimulationEvent::ActorTick(ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 10,
        })];
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(
                tx_hash(0x5a),
                3,
                Instant::now()
                    .checked_sub(Duration::from_millis(7))
                    .expect("test instant should support subtraction"),
            )
            .expect("submitted tx should record");
        let requested = Arc::new(Mutex::new(Vec::new()));

        run_schedule(
            ReceiptClient {
                receipt: Some(receipt(tx_hash(0x5a))),
                requested: requested.clone(),
            },
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
        )
        .await
        .expect("receipt poll should run");

        assert_eq!(
            requested
                .lock()
                .expect("requested list should lock")
                .as_slice(),
            &[tx_hash(0x5a)]
        );
        let observed = actors[0]
            .last_observed_receipt
            .as_ref()
            .expect("receipt should be recorded on actor state");
        assert_eq!(observed.tx_hash, tx_hash(0x5a));
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ReceiptPoll].completed,
            1
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let event = find_event(&events, "workload-completed");
        assert_eq!(event["kind"], "workload-completed");
        assert_eq!(event["receiptPresent"], true);
        assert_eq!(event["receiptStatus"], "ok");
        assert!(
            event["submitToReceiptLatencyMs"]
                .as_u64()
                .expect("submit-to-receipt latency should be numeric")
                >= 7
        );
        assert_eq!(event["chainHeight"], 77);
    }

    #[tokio::test]
    async fn receipt_poll_records_receipt_status_detail() {
        let manifest = receipt_manifest();
        let schedule = vec![ScheduledSimulationEvent::ActorTick(ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 10,
        })];
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 3, Instant::now())
            .expect("submitted tx should record");
        let requested = Arc::new(Mutex::new(Vec::new()));

        run_schedule(
            ReceiptClient {
                receipt: Some(receipt_with_status(
                    tx_hash(0x5a),
                    ExecStatus::DeterministicError {
                        kind: "outOfGas".into(),
                    },
                )),
                requested,
            },
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
        )
        .await
        .expect("receipt poll should run");

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        assert!(events.contains("\"receiptStatus\":\"deterministicError\""));
        assert!(events.contains("\"receiptStatusDetail\":\"outOfGas\""));
    }

    #[tokio::test]
    async fn receipt_poll_records_rpc_failure() {
        let manifest = receipt_manifest();
        let schedule = vec![ScheduledSimulationEvent::ActorTick(ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 10,
        })];
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 3, Instant::now())
            .expect("submitted tx should record");
        let requested = Arc::new(Mutex::new(Vec::new()));

        run_schedule(
            FailingReceiptClient {
                requested: requested.clone(),
            },
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
        )
        .await
        .expect("receipt poll failure should be recorded as workload failure");

        assert_eq!(
            requested
                .lock()
                .expect("requested list should lock")
                .as_slice(),
            &[tx_hash(0x5a)]
        );
        assert!(actors[0].last_observed_receipt.is_none());
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ReceiptPoll].failed,
            1
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let event = find_event(&events, "workload-failed");
        assert_eq!(event["kind"], "workload-failed");
        assert_eq!(event["errorClass"], "rpc-error");
        assert_eq!(event["error"], "receipt rpc failed");
        assert_eq!(event["receiptPresent"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn shutdown_receipt_drain_records_tail_receipts() {
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 1, Instant::now())
            .expect("first submitted tx should record");
        actors[0]
            .record_submitted_tx(tx_hash(0x5b), 2, Instant::now())
            .expect("second submitted tx should record");
        let requested = Arc::new(Mutex::new(Vec::new()));

        let drained = drain_pending_receipt_polls(
            EchoReceiptClient {
                requested: requested.clone(),
            },
            &mut actors,
            &mut event_writer,
            &mut metrics,
            30,
            Duration::ZERO,
            Duration::ZERO,
        )
        .await
        .expect("pending receipt drain should run");

        assert_eq!(drained, 2);
        assert_eq!(
            requested
                .lock()
                .expect("requested list should lock")
                .as_slice(),
            &[tx_hash(0x5a), tx_hash(0x5b)]
        );
        assert_eq!(actors[0].pending_receipt_count(), 0);
        let finished = metrics.finish();
        assert_eq!(
            finished
                .workload_metrics
                .get(&WorkloadKind::ReceiptPoll)
                .and_then(|metrics| metrics.receipt_poll.as_ref())
                .expect("receipt-poll summary should be recorded")
                .present,
            2
        );
    }

    #[tokio::test]
    async fn shutdown_receipt_drain_retries_tail_receipts_until_present() {
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 1, Instant::now())
            .expect("submitted tx should record");
        let requested = Arc::new(Mutex::new(Vec::new()));

        let drained = drain_pending_receipt_polls(
            EventuallyReceiptClient {
                requested: requested.clone(),
                absent_before_present: Arc::new(Mutex::new(1)),
            },
            &mut actors,
            &mut event_writer,
            &mut metrics,
            30,
            Duration::from_secs(1),
            Duration::ZERO,
        )
        .await
        .expect("pending receipt drain should run");

        assert_eq!(drained, 2);
        assert_eq!(
            requested
                .lock()
                .expect("requested list should lock")
                .as_slice(),
            &[tx_hash(0x5a), tx_hash(0x5a)]
        );
        assert_eq!(actors[0].pending_receipt_count(), 0);
        let finished = metrics.finish();
        let receipt_poll = finished
            .workload_metrics
            .get(&WorkloadKind::ReceiptPoll)
            .and_then(|metrics| metrics.receipt_poll.as_ref())
            .expect("receipt-poll summary should be recorded");
        assert_eq!(receipt_poll.absent, 1);
        assert_eq!(receipt_poll.present, 1);
    }

    #[tokio::test]
    async fn run_schedule_records_successful_chaos_transitions() {
        let manifest = chaos_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let mut chaos_events = Vec::new();

        let report = run_schedule_with_chaos(
            DelayedClient,
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
            Some(&SuccessfulChaosController),
            &mut chaos_events,
        )
        .await
        .expect("schedule should run");

        assert!(!report.chaos_failed);
        let states: Vec<_> = chaos_events.iter().map(|event| event.state).collect();
        assert_eq!(
            states,
            vec![
                ChaosEventState::Scheduled,
                ChaosEventState::Started,
                ChaosEventState::Completed,
                ChaosEventState::Recovered,
            ]
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let chaos_event = find_event(&events, "chaos-transition");
        assert_eq!(chaos_event["chaosKind"], "restart-target");
        assert_eq!(chaos_event["state"], "scheduled");
        let stream_states: Vec<_> = find_events(&events, "chaos-transition")
            .iter()
            .map(|event| {
                event["state"]
                    .as_str()
                    .expect("chaos state should be a string")
                    .to_owned()
            })
            .collect();
        assert_eq!(
            stream_states,
            vec!["scheduled", "started", "completed", "recovered"]
        );
    }

    #[tokio::test]
    async fn restart_chaos_drains_pending_receipts_as_target_restarted() {
        let manifest = chaos_manifest();
        let schedule = vec![ScheduledSimulationEvent::Chaos(
            crate::scheduler::ScheduledChaos {
                chaos_index: 0,
                simulation_time_ms: 10,
                kind: ChaosEventKind::RestartTarget,
            },
        )];
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 3, Instant::now())
            .expect("submitted tx should record");
        let mut chaos_events = Vec::new();

        run_schedule_with_chaos(
            DelayedClient,
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
            Some(&SuccessfulChaosController),
            &mut chaos_events,
        )
        .await
        .expect("restart chaos should run");

        assert_eq!(actors[0].pending_receipt_count(), 0);
        let finished = metrics.finish();
        let receipt_metrics = finished
            .workload_metrics
            .get(&WorkloadKind::ReceiptPoll)
            .expect("receipt metrics should exist");
        assert_eq!(receipt_metrics.counts.failed, 1);
        assert_eq!(receipt_metrics.error_counts["target-restarted"], 1);

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "target-restarted");
        assert_eq!(failed["receiptPresent"], false);
    }

    #[tokio::test]
    async fn restart_chaos_does_not_double_count_in_flight_receipt_poll() {
        let manifest = receipt_chaos_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 0, Instant::now())
            .expect("submitted tx should record");
        let requested = Arc::new(Mutex::new(Vec::new()));
        let mut chaos_events = Vec::new();

        run_schedule_with_chaos(
            SlowReceiptClient {
                requested: requested.clone(),
            },
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
            Some(&SuccessfulChaosController),
            &mut chaos_events,
        )
        .await
        .expect("restart chaos should run");

        assert_eq!(
            requested
                .lock()
                .expect("requested receipt list should lock")
                .as_slice(),
            &[tx_hash(0x5a)]
        );
        assert_eq!(actors[0].pending_receipt_count(), 0);
        let counts = metrics.workload_counts();
        assert_eq!(counts[&WorkloadKind::ReceiptPoll].failed, 1);

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let failed_events = find_events(&events, "workload-failed");
        assert_eq!(failed_events.len(), 1);
        assert_eq!(failed_events[0]["errorClass"], "target-restarted");
        assert_eq!(
            failed_events[0]["txHash"],
            "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a"
        );
    }

    #[tokio::test]
    async fn restart_chaos_drains_completed_submit_before_marking_receipt_lost() {
        let manifest = wallet_chaos_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let mut chaos_events = Vec::new();
        let submitted = Arc::new(Mutex::new(Vec::new()));

        run_schedule_with_chaos(
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
                view_calls: &[],
            },
            Some(&SuccessfulChaosController),
            &mut chaos_events,
        )
        .await
        .expect("restart chaos should run");

        assert_eq!(
            submitted
                .lock()
                .expect("submitted transaction list should lock")
                .len(),
            1
        );
        assert_eq!(actors[0].pending_receipt_count(), 0);
        let counts = metrics.workload_counts();
        assert_eq!(counts[&WorkloadKind::WalletSubmitSmoke].completed, 1);
        assert_eq!(counts[&WorkloadKind::ReceiptPoll].failed, 1);

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        assert!(events.contains("\"workload\":\"wallet-submit-smoke\""));
        assert!(events.contains("\"errorClass\":\"target-restarted\""));
    }

    #[tokio::test]
    async fn failed_chaos_stops_schedule_and_reports_failure() {
        let manifest = chaos_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let mut chaos_events = Vec::new();

        let report = run_schedule_with_chaos(
            DelayedClient,
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
            },
            Some(&FailingChaosController),
            &mut chaos_events,
        )
        .await
        .expect("failed chaos should be recorded");

        assert!(report.chaos_failed);
        assert!(
            chaos_events
                .iter()
                .any(|event| event.state == ChaosEventState::Failed)
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

    fn receipt_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-receipt-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "20ms", "users": 1, "ratePerUserHz": 100.0}],
            "workloads": [{"name": "receipt-poll", "weight": 1}]
        }))
        .expect("manifest fixture should parse")
    }

    fn chaos_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-chaos-test",
            "seed": "seed",
            "target": {"provider": "local"},
            "loadShape": [{"duration": "2ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}],
            "chaos": [{"at": "10ms", "kind": "restart-target"}]
        }))
        .expect("manifest fixture should parse")
    }

    fn receipt_chaos_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-receipt-chaos-test",
            "seed": "seed",
            "target": {"provider": "local"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "receipt-poll", "weight": 1}],
            "chaos": [{"at": "1ms", "kind": "restart-target"}]
        }))
        .expect("manifest fixture should parse")
    }

    fn wallet_chaos_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-wallet-chaos-test",
            "seed": "seed",
            "target": {"provider": "local"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "wallet-submit-smoke", "weight": 1}],
            "chaos": [{"at": "1ms", "kind": "restart-target"}]
        }))
        .expect("manifest fixture should parse")
    }

    fn find_event(events: &str, kind: &str) -> serde_json::Value {
        events
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).expect("event should parse")
            })
            .find(|event| event["kind"] == kind)
            .unwrap_or_else(|| panic!("expected {kind} event"))
    }

    fn find_events(events: &str, kind: &str) -> Vec<serde_json::Value> {
        events
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).expect("event should parse")
            })
            .filter(|event| event["kind"] == kind)
            .collect()
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

    fn prepared_view(expect_return_data: Option<Vec<u8>>) -> PreparedViewWorkload {
        PreparedViewWorkload {
            view: PreparedViewCall {
                name: "fib-10".into(),
                contract_name: "fibonacci".into(),
                contract: ContractAddress::from([0x88; 32]),
                entrypoint: vec![0x11; 8],
                calldata: vec![0x0a],
                expect_return_data,
            },
            caller: ContractAddress::from([0x66; 32]),
        }
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

    fn tx_hash(byte: u8) -> TxHash {
        TxHash(Hex0x(SHA256Hash::from([byte; 32])))
    }

    fn receipt(tx_hash: TxHash) -> TransactionReceipt {
        receipt_with_status(tx_hash, ExecStatus::Ok)
    }

    fn receipt_with_status(tx_hash: TxHash, status: ExecStatus) -> TransactionReceipt {
        TransactionReceipt {
            tx_hash,
            height: BlockHeight::from(77),
            status,
            events: Vec::new(),
            return_data: None,
            abort_data: None,
        }
    }
}
