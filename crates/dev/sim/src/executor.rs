use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use groundhog_json_rpc_client::GroundhogApiClient;
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress,
    block::ChainTip,
    events::{
        BlockAnchor, ContractAddressList, EventFilter, EventFilterList, EventPage, EventQuery,
        EventTopicFilter, IterationOrder, PaginationCursor, TopicList,
    },
    hex::{Hex0x, HexBytes},
    receipt::{ExecStatus, TransactionReceipt},
    runtime_manifest::RuntimeManifest,
    state::{StorageItemInfo, StorageItemRead},
    transaction::TxHash,
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{Id as TaskId, JoinError, JoinSet};

use crate::actor::{
    ActorError, ActorState, DrainedPendingReceipt, PendingReceiptRecordOutcome,
    ReadAfterWriteState, ReceiptPollTarget,
};
use crate::contract::{
    PreparedContractCall, PreparedEventQuery, PreparedReadAfterWrite, PreparedStorageRead,
    PreparedViewCall,
};
use crate::error::SimError;
use crate::event::{
    ChaosTransitionEvent, EventWriter, ResourceSampleEvent, ResourceSampleStatus, ResultEvent,
    SimulatorOverloadEvent, WorkloadEvent,
};
use crate::manifest::{
    ChaosEventKind, ChaosEventSpec, EventQueryAnchorSpec, EventQueryOrderSpec, SimulationManifest,
    WorkloadKind,
};
use crate::metrics::{RunMetricsBuilder, SIMULATOR_BACKPRESSURE_ERROR_CLASS};
use crate::resource::{ResourceSample, ResourceSampleRequest, TargetResourceSampler};
use crate::result::{ChainTipObservation, ChaosEventRecord, ChaosEventState, ResourceSamplePoint};
use crate::scheduler::{
    PhaseStart, ScheduleCursor, ScheduledActorTick, ScheduledSimulationEvent,
    select_workload_for_tick,
};
use crate::target::ChaosController;
use crate::transaction::{
    BuiltSubmitTransaction, build_contract_call_transaction, build_wallet_submit_smoke_transaction,
    runtime_dev_wallet_action_entrypoint,
};
use crate::util::{duration_ms, json_string, unix_ms, workload_name};
use xdr_common::VecM;

const DEFAULT_GLOBAL_IN_FLIGHT_LIMIT: usize = 8_192;
// Nonce-bearing workloads still serialize per actor. Read-only and receipt
// lanes are independent so app-style reads do not wait behind block-bound
// writes from the same simulated wallet.
const DEFAULT_PER_ACTOR_WRITE_IN_FLIGHT_LIMIT: usize = 1;
const DEFAULT_PER_ACTOR_RECEIPT_IN_FLIGHT_LIMIT: usize = 4;
const DEFAULT_PER_ACTOR_READ_IN_FLIGHT_LIMIT: usize = 8;
pub const DEFAULT_RECEIPT_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const READ_AFTER_WRITE_POST_COMMIT_READ_RETRY_TIMEOUT: Duration = Duration::from_secs(1);
const READ_AFTER_WRITE_POST_COMMIT_READ_RETRY_INTERVAL: Duration = Duration::from_millis(100);

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
    fn storage_read(
        &self,
        _address: ContractAddress,
        _key: HexBytes,
    ) -> impl Future<Output = Result<StorageItemRead, String>> + Send {
        async { Err("storage read RPC is not implemented by this client".into()) }
    }
    fn events_query(
        &self,
        _query: EventQuery,
    ) -> impl Future<Output = Result<EventPage, String>> + Send {
        async { Err("events query RPC is not implemented by this client".into()) }
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

    #[allow(clippy::manual_async_fn)]
    fn storage_read(
        &self,
        address: ContractAddress,
        key: HexBytes,
    ) -> impl Future<Output = Result<StorageItemRead, String>> + Send {
        async move {
            GroundhogApiClient::get_storage_at(self, address, key)
                .await
                .map_err(|error| error.to_string())
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn events_query(
        &self,
        query: EventQuery,
    ) -> impl Future<Output = Result<EventPage, String>> + Send {
        async move {
            GroundhogApiClient::get_events(self, query)
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
    pub contract_calls: &'a [PreparedContractCall],
    pub storage_reads: &'a [PreparedStorageRead],
    pub event_queries: &'a [PreparedEventQuery],
    pub read_after_write: &'a [PreparedReadAfterWrite],
    pub resource_sampler: Option<&'a dyn TargetResourceSampler>,
}

/// Inputs for executing a generated schedule stream.
pub struct StreamingScheduleExecution<'a> {
    pub manifest: &'a SimulationManifest,
    pub runtime_manifest: &'a RuntimeManifest,
    pub phase_starts: &'a [PhaseStart],
    pub actor_states: &'a mut [ActorState],
    pub event_writer: &'a mut EventWriter,
    pub metrics: &'a mut RunMetricsBuilder,
    pub view_calls: &'a [PreparedViewCall],
    pub contract_calls: &'a [PreparedContractCall],
    pub storage_reads: &'a [PreparedStorageRead],
    pub event_queries: &'a [PreparedEventQuery],
    pub read_after_write: &'a [PreparedReadAfterWrite],
    pub resource_sampler: Option<&'a dyn TargetResourceSampler>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScheduleRunReport {
    pub chaos_failed: bool,
    /// Scheduling stopped on a recorded failure; accepted writes still need reconciliation.
    pub failure: Option<String>,
}

/// Execute a deterministic simulation schedule against a typed RPC client.
pub async fn run_schedule<C>(client: C, input: ScheduleExecution<'_>) -> Result<(), SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let mut chaos_events = Vec::new();
    run_schedule_with_chaos(client, input, None, &mut chaos_events)
        .await
        .and_then(|report| match report.failure {
            Some(reason) => Err(SimError::Executor(reason)),
            None => Ok(()),
        })
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
        contract_calls,
        storage_reads,
        event_queries,
        read_after_write,
        resource_sampler,
    } = input;
    run_schedule_events_with_chaos(
        client,
        manifest,
        runtime_manifest,
        schedule.iter().cloned(),
        phase_starts,
        actor_states,
        event_writer,
        metrics,
        view_calls,
        contract_calls,
        storage_reads,
        event_queries,
        read_after_write,
        resource_sampler,
        chaos_controller,
        chaos_events,
    )
    .await
}

pub async fn run_streaming_schedule_with_chaos<C>(
    client: C,
    input: StreamingScheduleExecution<'_>,
    chaos_controller: Option<&dyn ChaosController>,
    chaos_events: &mut Vec<ChaosEventRecord>,
) -> Result<ScheduleRunReport, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let StreamingScheduleExecution {
        manifest,
        runtime_manifest,
        phase_starts,
        actor_states,
        event_writer,
        metrics,
        view_calls,
        contract_calls,
        storage_reads,
        event_queries,
        read_after_write,
        resource_sampler,
    } = input;
    let cursor = ScheduleCursor::new(manifest)?;
    run_schedule_events_with_chaos(
        client,
        manifest,
        runtime_manifest,
        cursor,
        phase_starts,
        actor_states,
        event_writer,
        metrics,
        view_calls,
        contract_calls,
        storage_reads,
        event_queries,
        read_after_write,
        resource_sampler,
        chaos_controller,
        chaos_events,
    )
    .await
}

const NATIVE_TIP_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const NATIVE_TIP_RPC_TIMEOUT: Duration = Duration::from_secs(2);

/// Refresh in the scheduler rather than spawning a background task or adding
/// an RPC to every transaction. Any refresh failure prevents new native writes.
async fn refresh_native_chain_tip<C: SimRpcClient>(
    client: &C,
    cached: &mut Option<crate::actor::NativeChainTip>,
    actors: &mut [ActorState],
) -> Result<(), SimError> {
    if cached.is_some_and(|tip| tip.observed_at.elapsed() < NATIVE_TIP_REFRESH_INTERVAL) {
        return Ok(());
    }
    let response = tokio::time::timeout(NATIVE_TIP_RPC_TIMEOUT, client.chain_tip())
        .await
        .map_err(|_| SimError::Executor("native expiry chain-tip refresh timed out".into()))?
        .map_err(|error| {
            SimError::Executor(format!("native expiry chain-tip refresh failed: {error}"))
        })?;
    let tip = crate::actor::NativeChainTip {
        height: *response.height,
        observed_at: Instant::now(),
    };
    tip.expiry()?;
    if cached.is_some_and(|previous| tip.height < previous.height) {
        return Err(SimError::Executor(
            "native expiry chain-tip height regressed; target may have reset".into(),
        ));
    }
    *cached = Some(tip);
    for actor in actors {
        if actor.wallet_kind == crate::actor::WalletKind::NativePubkey {
            actor.native_chain_tip = Some(tip);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_schedule_events_with_chaos<C, I>(
    client: C,
    manifest: &SimulationManifest,
    runtime_manifest: &RuntimeManifest,
    events: I,
    phase_starts: &[PhaseStart],
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
    view_calls: &[PreparedViewCall],
    contract_calls: &[PreparedContractCall],
    storage_reads: &[PreparedStorageRead],
    event_queries: &[PreparedEventQuery],
    read_after_write: &[PreparedReadAfterWrite],
    resource_sampler: Option<&dyn TargetResourceSampler>,
    chaos_controller: Option<&dyn ChaosController>,
    chaos_events: &mut Vec<ChaosEventRecord>,
) -> Result<ScheduleRunReport, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
    I: IntoIterator<Item = ScheduledSimulationEvent>,
{
    let wallet_entrypoint = if manifest.accounts.is_some() {
        crate::native::EXECUTE
    } else {
        runtime_dev_wallet_action_entrypoint(runtime_manifest)?
    };
    let run_start = Instant::now();
    let mut next_phase = phase_starts.iter().peekable();
    let mut tasks = JoinSet::new();
    let mut task_contexts = HashMap::new();
    let mut resource_tasks = JoinSet::new();
    let mut resource_task_contexts = HashMap::new();
    let global_in_flight = Arc::new(Semaphore::new(DEFAULT_GLOBAL_IN_FLIGHT_LIMIT));
    let actor_in_flight = actor_in_flight_lanes(phase_starts);
    let mut in_flight = 0_u64;
    let mut chaos_failed = false;
    let mut failure = None;
    let mut native_tip = None;

    for event in events {
        Box::pin(drain_completed_workloads_and_update_in_flight(
            &mut tasks,
            &mut task_contexts,
            actor_states,
            event_writer,
            metrics,
            &mut in_flight,
        ))
        .await?;
        drain_completed_resource_samples(
            &mut resource_tasks,
            &mut resource_task_contexts,
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
        match &event {
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
                if actor_states[actor_index].wallet_kind == crate::actor::WalletKind::NativePubkey
                    && matches!(
                        workload,
                        WorkloadKind::WalletSubmitSmoke
                            | WorkloadKind::ContractCall
                            | WorkloadKind::ReadAfterWrite
                    )
                    && let Err(error) =
                        refresh_native_chain_tip(&client, &mut native_tip, actor_states).await
                {
                    let reason = error.to_string();
                    let completion = stopped_workload_completion(
                        WorkloadTaskContext {
                            actor_id: tick.actor_id,
                            workload,
                            scheduled_time_ms: tick.simulation_time_ms,
                        },
                        "native-tip-refresh",
                        reason.clone(),
                        "groundhog_chainTip",
                    )?;
                    record_completion(completion, actor_states, event_writer, metrics).await?;
                    failure = Some(reason);
                    break;
                }
                let permits = match acquire_workload_permits(
                    tick,
                    workload,
                    global_in_flight.clone(),
                    actor_in_flight
                        .get(actor_index)
                        .ok_or(SimError::Executor("actor in-flight lanes missing".into()))?,
                )? {
                    Ok(permits) => permits,
                    Err(skipped) => {
                        record_completion(skipped, actor_states, event_writer, metrics).await?;
                        continue;
                    }
                };
                Box::pin(drain_completed_workloads_and_update_in_flight(
                    &mut tasks,
                    &mut task_contexts,
                    actor_states,
                    event_writer,
                    metrics,
                    &mut in_flight,
                ))
                .await?;
                let prepared = prepare_workload(
                    manifest,
                    tick,
                    actor_states
                        .get_mut(actor_index)
                        .ok_or(SimError::Executor("actor state missing".into()))?,
                    workload,
                    wallet_entrypoint,
                    view_calls,
                    contract_calls,
                    storage_reads,
                    event_queries,
                    read_after_write,
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
            ScheduledSimulationEvent::ResourceSample(sample) => {
                sleep_until_sim_time_draining(
                    run_start,
                    sample.simulation_time_ms,
                    &mut tasks,
                    &mut task_contexts,
                    event_writer,
                    metrics,
                    actor_states,
                    &mut in_flight,
                )
                .await?;
                dispatch_resource_sample(
                    sample.simulation_time_ms,
                    resource_sampler,
                    &mut resource_tasks,
                    &mut resource_task_contexts,
                )
                .await?;
            }
        }
    }
    if !chaos_failed && failure.is_none() {
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
        drain_completed_resource_samples(
            &mut resource_tasks,
            &mut resource_task_contexts,
            event_writer,
            metrics,
        )
        .await?;
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
    drain_all_resource_samples(
        &mut resource_tasks,
        &mut resource_task_contexts,
        event_writer,
        metrics,
    )
    .await?;
    Ok(ScheduleRunReport {
        chaos_failed,
        failure,
    })
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
            loop {
                if Instant::now() >= deadline {
                    return Ok(drained);
                }
                let Some(target) = actor_states[actor_index].reserve_receipt_poll_target() else {
                    break;
                };
                let tx_hash = target.tx_hash;
                let tick = ScheduledActorTick {
                    phase_index: 0,
                    actor_id: actor_states[actor_index].actor_id,
                    actor_sequence: u64::from(actor_states[actor_index].next_nonce_sequence),
                    simulation_time_ms,
                };
                let poll = execute_receipt_poll(
                    client.clone(),
                    &tick,
                    PreparedReceiptPoll {
                        target: Some(target),
                    },
                );
                let completion = if let Ok(completion) =
                    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), poll).await
                {
                    completion?
                } else {
                    actor_states[actor_index].release_receipt_poll_target(tx_hash);
                    return Ok(drained);
                };
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
            .all(|actor| actor.pending_receipt_count() == 0)
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

fn actor_in_flight_lanes(phase_starts: &[PhaseStart]) -> Vec<ActorInFlightLanes> {
    let max_users = phase_starts
        .iter()
        .map(|phase| phase.users)
        .max()
        .unwrap_or_default();
    (0..max_users)
        .map(|_| ActorInFlightLanes {
            write: Arc::new(Semaphore::new(DEFAULT_PER_ACTOR_WRITE_IN_FLIGHT_LIMIT)),
            receipt: Arc::new(Semaphore::new(DEFAULT_PER_ACTOR_RECEIPT_IN_FLIGHT_LIMIT)),
            read: Arc::new(Semaphore::new(DEFAULT_PER_ACTOR_READ_IN_FLIGHT_LIMIT)),
        })
        .collect()
}

#[derive(Debug, Clone)]
struct ActorInFlightLanes {
    write: Arc<Semaphore>,
    receipt: Arc<Semaphore>,
    read: Arc<Semaphore>,
}

impl ActorInFlightLanes {
    fn semaphore(&self, lane: ActorInFlightLane) -> Arc<Semaphore> {
        match lane {
            ActorInFlightLane::Write => self.write.clone(),
            ActorInFlightLane::Receipt => self.receipt.clone(),
            ActorInFlightLane::Read => self.read.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorInFlightLane {
    Write,
    Receipt,
    Read,
}

impl ActorInFlightLane {
    const fn scope(self) -> &'static str {
        match self {
            Self::Write => "actor-write",
            Self::Receipt => "actor-receipt",
            Self::Read => "actor-read",
        }
    }
}

const fn actor_in_flight_lane(workload: WorkloadKind) -> ActorInFlightLane {
    match workload {
        WorkloadKind::WalletSubmitSmoke
        | WorkloadKind::ContractCall
        | WorkloadKind::ReadAfterWrite => ActorInFlightLane::Write,
        WorkloadKind::ReceiptPoll | WorkloadKind::StatusPoll => ActorInFlightLane::Receipt,
        WorkloadKind::ChainTip
        | WorkloadKind::EventsQuery
        | WorkloadKind::StorageRead
        | WorkloadKind::ViewCall => ActorInFlightLane::Read,
    }
}

fn acquire_workload_permits(
    tick: &ScheduledActorTick,
    workload: WorkloadKind,
    global_in_flight: Arc<Semaphore>,
    actor_in_flight: &ActorInFlightLanes,
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
    let lane = actor_in_flight_lane(workload);
    let actor_permit = match actor_in_flight.semaphore(lane).try_acquire_owned() {
        Ok(permit) => permit,
        Err(error) => {
            drop(global_permit);
            return Ok(Err(backpressure_completion(
                tick,
                workload,
                lane.scope(),
                &error.to_string(),
            )?));
        }
    };
    Ok(Ok(WorkloadPermits {
        _global: global_permit,
        _actor: actor_permit,
    }))
}
#[allow(clippy::too_many_arguments)]
fn prepare_workload(
    manifest: &SimulationManifest,
    tick: &ScheduledActorTick,
    actor: &mut ActorState,
    workload: WorkloadKind,
    wallet_entrypoint: groundhog_xdr::contract::ContractEntrypointID,
    view_calls: &[PreparedViewCall],
    contract_calls: &[PreparedContractCall],
    storage_reads: &[PreparedStorageRead],
    event_queries: &[PreparedEventQuery],
    read_after_write: &[PreparedReadAfterWrite],
) -> Result<PreparedWorkload, SimError> {
    match workload {
        WorkloadKind::ChainTip => Ok(PreparedWorkload::ChainTip),
        WorkloadKind::WalletSubmitSmoke => Ok(PreparedWorkload::WalletSubmitSmoke(
            build_wallet_submit_smoke_transaction(actor, wallet_entrypoint)?,
        )),
        WorkloadKind::ReceiptPoll => Ok(PreparedWorkload::ReceiptPoll(PreparedReceiptPoll {
            target: actor.reserve_receipt_poll_target(),
        })),
        WorkloadKind::ViewCall => {
            let view = select_weighted_target(
                manifest,
                workload,
                tick,
                view_calls,
                "view-call workload requires at least one prepared view",
            )?;
            Ok(PreparedWorkload::ViewCall(PreparedViewWorkload {
                view: view.clone(),
                caller: actor.wallet_address,
            }))
        }
        WorkloadKind::ContractCall => {
            let call = select_weighted_target(
                manifest,
                workload,
                tick,
                contract_calls,
                "contract-call workload requires at least one prepared contract call",
            )?;
            prepare_contract_call_workload(actor, wallet_entrypoint, call)
                .map(PreparedWorkload::ContractCall)
        }
        WorkloadKind::StorageRead => {
            let read = select_weighted_target(
                manifest,
                workload,
                tick,
                storage_reads,
                "storage-read workload requires at least one prepared storage read",
            )?;
            Ok(PreparedWorkload::StorageRead(PreparedStorageReadWorkload {
                read: read.clone(),
            }))
        }
        WorkloadKind::EventsQuery => {
            let query = select_weighted_target(
                manifest,
                workload,
                tick,
                event_queries,
                "events-query workload requires at least one prepared event query",
            )?;
            Ok(PreparedWorkload::EventsQuery(PreparedEventQueryWorkload {
                query: query.clone(),
            }))
        }
        WorkloadKind::ReadAfterWrite => {
            let entry = select_weighted_target(
                manifest,
                workload,
                tick,
                read_after_write,
                "read-after-write workload requires at least one prepared entry",
            )?;
            prepare_read_after_write_workload(actor, wallet_entrypoint, entry)
                .map(PreparedWorkload::ReadAfterWrite)
        }
        WorkloadKind::StatusPoll => Err(SimError::UnsupportedRuntimeWorkloads(workload_name(
            workload,
        ))),
    }
}

fn prepare_contract_call_workload(
    actor: &mut ActorState,
    wallet_entrypoint: groundhog_xdr::contract::ContractEntrypointID,
    call: &PreparedContractCall,
) -> Result<PreparedContractCallWorkload, SimError> {
    let entrypoint: [u8; 8] = call.entrypoint.as_slice().try_into().map_err(|_| {
        SimError::Executor("contract-call prepared entrypoint must be 8 bytes".into())
    })?;
    Ok(PreparedContractCallWorkload {
        call: call.clone(),
        tx: build_contract_call_transaction(
            actor,
            wallet_entrypoint,
            call.contract,
            groundhog_xdr::contract::ContractEntrypointID::from(entrypoint),
            &call.calldata,
        )?,
    })
}

fn prepare_read_after_write_workload(
    actor: &mut ActorState,
    wallet_entrypoint: groundhog_xdr::contract::ContractEntrypointID,
    entry: &PreparedReadAfterWrite,
) -> Result<PreparedReadAfterWriteWorkload, SimError> {
    let write_entrypoint: [u8; 8] = entry.write_entrypoint.as_slice().try_into().map_err(|_| {
        SimError::Executor("read-after-write prepared write entrypoint must be 8 bytes".into())
    })?;
    Ok(PreparedReadAfterWriteWorkload {
        entry: entry.clone(),
        tx: build_contract_call_transaction(
            actor,
            wallet_entrypoint,
            entry.contract,
            groundhog_xdr::contract::ContractEntrypointID::from(write_entrypoint),
            &entry.write_calldata,
        )?,
        caller: actor.wallet_address,
        state: actor.read_after_write_state(),
    })
}

trait WeightedPreparedTarget {
    fn weight(&self) -> u32;
}

impl WeightedPreparedTarget for PreparedViewCall {
    fn weight(&self) -> u32 {
        self.weight
    }
}

impl WeightedPreparedTarget for PreparedContractCall {
    fn weight(&self) -> u32 {
        self.weight
    }
}

impl WeightedPreparedTarget for PreparedStorageRead {
    fn weight(&self) -> u32 {
        self.weight
    }
}

impl WeightedPreparedTarget for PreparedEventQuery {
    fn weight(&self) -> u32 {
        self.weight
    }
}

impl WeightedPreparedTarget for PreparedReadAfterWrite {
    fn weight(&self) -> u32 {
        self.weight
    }
}

fn select_weighted_target<'a, T: WeightedPreparedTarget>(
    manifest: &SimulationManifest,
    workload: WorkloadKind,
    tick: &ScheduledActorTick,
    targets: &'a [T],
    missing_message: &'static str,
) -> Result<&'a T, SimError> {
    if targets.is_empty() {
        return Err(SimError::Executor(missing_message.into()));
    }
    let total_weight = targets.iter().try_fold(0_u64, |total, target| {
        total
            .checked_add(u64::from(target.weight()))
            .ok_or(SimError::Executor(
                "target selection weight overflow".into(),
            ))
    })?;
    if total_weight == 0 {
        return Err(SimError::Executor(
            "target selection requires at least one nonzero weight".into(),
        ));
    }
    let mut draw = weighted_target_draw(manifest, workload, tick, total_weight);
    for target in targets {
        let weight = u64::from(target.weight());
        if draw < weight {
            return Ok(target);
        }
        draw -= weight;
    }
    Err(SimError::Executor(
        "bug: weighted target draw escaped target list".into(),
    ))
}

fn weighted_target_draw(
    manifest: &SimulationManifest,
    workload: WorkloadKind,
    tick: &ScheduledActorTick,
    total_weight: u64,
) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"hogsim weighted target selection v1");
    hasher.update(manifest.seed.as_bytes());
    hasher.update([0]);
    hasher.update(manifest.name.as_bytes());
    hasher.update([0]);
    hasher.update(workload_name(workload).as_bytes());
    hasher.update([0]);
    let phase_index = u64::try_from(tick.phase_index).expect("bug: phase index should fit u64");
    hasher.update(phase_index.to_le_bytes());
    hasher.update(tick.actor_id.to_le_bytes());
    hasher.update(tick.actor_sequence.to_le_bytes());
    hasher.update(tick.simulation_time_ms.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes) % total_weight
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
            PreparedWorkload::ContractCall(call) => {
                execute_contract_call(client, &tick, call).await
            }
            PreparedWorkload::StorageRead(read) => execute_storage_read(client, &tick, read).await,
            PreparedWorkload::EventsQuery(query) => {
                execute_events_query(client, &tick, query).await
            }
            PreparedWorkload::ReadAfterWrite(workload) => {
                execute_read_after_write(client, &tick, workload).await
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

async fn drain_completed_workloads_and_update_in_flight(
    tasks: &mut JoinSet<Result<WorkloadCompletion, SimError>>,
    task_contexts: &mut HashMap<TaskId, WorkloadTaskContext>,
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
    in_flight: &mut u64,
) -> Result<(), SimError> {
    let drained =
        drain_completed_workloads(tasks, task_contexts, actor_states, event_writer, metrics)
            .await?;
    *in_flight = in_flight.checked_sub(drained).ok_or(SimError::Executor(
        "in-flight workload count underflowed".into(),
    ))?;
    Ok(())
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

fn native_failed_receipt_event(event: ResultEvent, status: &ExecStatus) -> ResultEvent {
    match event {
        ResultEvent::WorkloadCompleted { mut event } => {
            event.error_class = Some("native-receipt-status".into());
            event.error = Some(format!("native transaction committed with {status:?}"));
            ResultEvent::WorkloadFailed { event }
        }
        other => other,
    }
}

async fn record_completion(
    mut completion: WorkloadCompletion,
    actor_states: &mut [ActorState],
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<(), SimError> {
    let mut overload_event = None;
    if let Some(submitted) = completion.submitted_transaction {
        let actor = actor_states
            .get_mut(submitted.actor_id as usize)
            .ok_or(SimError::Executor("actor state missing".into()))?;
        let pending_outcome = actor.record_submitted_tx(
            submitted.tx_hash,
            submitted.scheduled_time_ms,
            submitted.completed_at,
        )?;
        if let PendingReceiptRecordOutcome::Dropped {
            pending_receipts,
            pending_receipt_limit,
        } = pending_outcome
        {
            metrics.record_simulator_overload();
            overload_event = Some(ResultEvent::SimulatorOverload {
                event: SimulatorOverloadEvent {
                    actor_id: submitted.actor_id,
                    scheduled_time_ms: submitted.scheduled_time_ms,
                    tx_hash: json_string(&submitted.tx_hash, "txHash")?,
                    reason: "pending receipt tracking limit reached".into(),
                    pending_receipts,
                    pending_receipt_limit,
                },
            });
        }
    }
    if let Some(observed) = completion.observed_receipt {
        let actor = actor_states
            .get_mut(observed.actor_id as usize)
            .ok_or(SimError::Executor("actor state missing".into()))?;
        if actor.wallet_kind == crate::actor::WalletKind::NativePubkey
            && completion.workload == WorkloadKind::ReceiptPoll
            && !matches!(observed.receipt.status, ExecStatus::Ok)
        {
            completion.error_class = Some("native-receipt-status".into());
            completion.event =
                native_failed_receipt_event(completion.event, &observed.receipt.status);
        }

        actor.record_receipt(observed.receipt);
    }
    if let Some(release) = completion.release_receipt_poll {
        let actor = actor_states
            .get_mut(release.actor_id as usize)
            .ok_or(SimError::Executor("actor state missing".into()))?;
        actor.release_receipt_poll_target(release.tx_hash);
    }
    if let Some(update) = completion.read_after_write_transition {
        let actor = actor_states
            .get_mut(update.actor_id as usize)
            .ok_or(SimError::Executor("actor state missing".into()))?;
        match update.transition {
            ReadAfterWriteTransition::SetKnown(value) => {
                actor.record_read_after_write_committed(value);
            }
            ReadAfterWriteTransition::Invalidate => actor.invalidate_read_after_write(),
        }
    }
    if let Some(receipt_poll) = completion.receipt_poll {
        metrics.record_receipt_poll(receipt_poll.present);
    }
    if let Some(latency_ms) = completion.submit_to_receipt_latency_ms {
        metrics.record_submit_to_receipt_latency(completion.workload, latency_ms);
    }
    if let Some(error_class) = completion.error_class.as_deref() {
        if let Some(event) = workload_event(&completion.event) {
            metrics.record_error_diagnostic(
                completion.workload,
                event.rpc_method,
                event.scheduled_time_ms,
                error_class,
                event.error.as_deref(),
            );
        }
        if completion.skipped {
            metrics.record_skipped(completion.workload, error_class);
        } else {
            metrics.record_failure(completion.workload, error_class, completion.latency_ms);
        }
    } else {
        metrics.record_success(completion.workload, completion.latency_ms);
    }
    if let Some(observation) = completion.chain_tip {
        metrics.record_chain_tip(&observation);
    }
    event_writer.write(&completion.event).await?;
    if let Some(event) = overload_event {
        event_writer.write(&event).await?;
    }
    Ok(())
}

fn workload_event(event: &ResultEvent) -> Option<&WorkloadEvent> {
    match event {
        ResultEvent::WorkloadCompleted { event }
        | ResultEvent::WorkloadFailed { event }
        | ResultEvent::WorkloadSkipped { event } => Some(event),
        _ => None,
    }
}

async fn dispatch_resource_sample(
    scheduled_time_ms: u64,
    resource_sampler: Option<&dyn TargetResourceSampler>,
    tasks: &mut JoinSet<ResourceSample>,
    task_contexts: &mut HashMap<TaskId, ResourceSampleTaskContext>,
) -> Result<(), SimError> {
    let request = match resource_sampler {
        Some(sampler) => sampler.sample(scheduled_time_ms).await,
        None => ResourceSampleRequest::Ready(ResourceSample::unavailable(
            scheduled_time_ms,
            "target provider does not expose process resource sampling",
        )),
    };
    let abort = tasks.spawn(async move { request.run().await });
    task_contexts.insert(abort.id(), ResourceSampleTaskContext { scheduled_time_ms });
    Ok(())
}

async fn drain_completed_resource_samples(
    tasks: &mut JoinSet<ResourceSample>,
    task_contexts: &mut HashMap<TaskId, ResourceSampleTaskContext>,
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<u64, SimError> {
    let mut drained = 0_u64;
    while let Some(joined) = tasks.try_join_next_with_id() {
        let sample = join_resource_sample(joined, task_contexts)?;
        record_resource_sample(sample, event_writer, metrics).await?;
        drained += 1;
    }
    Ok(drained)
}

async fn drain_all_resource_samples(
    tasks: &mut JoinSet<ResourceSample>,
    task_contexts: &mut HashMap<TaskId, ResourceSampleTaskContext>,
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<(), SimError> {
    while let Some(joined) = tasks.join_next_with_id().await {
        let sample = join_resource_sample(joined, task_contexts)?;
        record_resource_sample(sample, event_writer, metrics).await?;
    }
    Ok(())
}

fn join_resource_sample(
    joined: Result<(TaskId, ResourceSample), JoinError>,
    task_contexts: &mut HashMap<TaskId, ResourceSampleTaskContext>,
) -> Result<ResourceSample, SimError> {
    match joined {
        Ok((task_id, sample)) => {
            task_contexts.remove(&task_id);
            Ok(sample)
        }
        Err(error) => {
            let context = task_contexts.remove(&error.id()).ok_or_else(|| {
                SimError::Executor(format!("missing resource sample task context for {error}"))
            })?;
            Ok(ResourceSample::unavailable(
                context.scheduled_time_ms,
                format!("target resource sampling task failed: {error}"),
            ))
        }
    }
}

async fn record_resource_sample(
    sample: ResourceSample,
    event_writer: &mut EventWriter,
    metrics: &mut RunMetricsBuilder,
) -> Result<(), SimError> {
    if let ResourceSample::Observed(point) = &sample {
        metrics.record_resource_sample(point);
    } else if let ResourceSample::Unavailable {
        scheduled_time_ms,
        reason,
        ..
    } = &sample
    {
        metrics.record_resource_sample_failure(*scheduled_time_ms, reason);
    }
    event_writer.write(&resource_sample_event(&sample)).await?;
    Ok(())
}

fn resource_sample_event(sample: &ResourceSample) -> ResultEvent {
    match sample {
        ResourceSample::Observed(point) => ResultEvent::ResourceSample {
            event: observed_resource_sample_event(point),
        },
        ResourceSample::Unavailable {
            scheduled_time_ms,
            observed_at_unix_ms,
            reason,
        } => ResultEvent::ResourceSample {
            event: ResourceSampleEvent {
                scheduled_time_ms: *scheduled_time_ms,
                observed_at_unix_ms: observed_at_unix_ms.clone(),
                status: ResourceSampleStatus::Unavailable,
                reason: Some(reason.clone()),
                scope: None,
                root_pid: None,
                pgid: None,
                process_count: None,
                rss_bytes: None,
                cpu_user_ms: None,
                cpu_system_ms: None,
                threads: None,
                open_fds: None,
            },
        },
    }
}

fn observed_resource_sample_event(point: &ResourceSamplePoint) -> ResourceSampleEvent {
    ResourceSampleEvent {
        scheduled_time_ms: point.simulation_time_ms,
        observed_at_unix_ms: point.observed_at_unix_ms.clone(),
        status: ResourceSampleStatus::Observed,
        reason: None,
        scope: Some(point.scope.clone()),
        root_pid: Some(point.root_pid),
        pgid: point.pgid,
        process_count: Some(point.process_count),
        rss_bytes: point.rss_bytes,
        cpu_user_ms: point.cpu_user_ms,
        cpu_system_ms: point.cpu_system_ms,
        threads: point.threads,
        open_fds: point.open_fds,
    }
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
                contract_call_name: None,
                contract_call_contract: None,
                storage_read_name: None,
                storage_contract: None,
                storage_key: None,
                storage_item_present: None,
                storage_item_kind: None,
                event_query_name: None,
                event_contract: None,
                event_count: None,
                event_cursor: None,
                event_latest_block: None,
                event_oldest_block: None,
                error_class: Some(SIMULATOR_BACKPRESSURE_ERROR_CLASS.into()),
                error: Some(format!("{scope} in-flight limit reached: {error}")),
                read_after_write_name: None,
                read_after_write_contract: None,
                read_after_write_expected: None,
                read_after_write_observed: None,
                read_after_write_matched: None,
                read_after_write_rebased: None,
            },
        },
        workload,
        latency_ms: 0,
        error_class: Some(SIMULATOR_BACKPRESSURE_ERROR_CLASS.into()),
        skipped: true,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        release_receipt_poll: None,
        read_after_write_transition: None,
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
        if matches!(
            context.workload,
            WorkloadKind::WalletSubmitSmoke
                | WorkloadKind::ContractCall
                | WorkloadKind::ReadAfterWrite
        ) {
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
                contract_call_name: None,
                contract_call_contract: None,
                storage_read_name: None,
                storage_contract: None,
                storage_key: None,
                storage_item_present: None,
                storage_item_kind: None,
                event_query_name: None,
                event_contract: None,
                event_count: None,
                event_cursor: None,
                event_latest_block: None,
                event_oldest_block: None,
                error_class: Some("target-restarted".into()),
                error: Some(format!(
                    "workload aborted at simulation time {simulation_time_ms} because target restarted"
                )),
                read_after_write_name: None,
                read_after_write_contract: None,
                read_after_write_expected: None,
                read_after_write_observed: None,
                read_after_write_matched: None,
                read_after_write_rebased: None,
            },
        },
        workload: context.workload,
        latency_ms: 0,
        error_class: Some("target-restarted".into()),
        skipped: false,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: if context.workload == WorkloadKind::ReadAfterWrite {
            Some(ReadAfterWriteUpdate {
                actor_id: context.actor_id,
                transition: ReadAfterWriteTransition::Invalidate,
            })
        } else {
            None
        },
    })
}

fn chaos_failed_workload_completion(
    context: WorkloadTaskContext,
    simulation_time_ms: u64,
) -> Result<WorkloadCompletion, SimError> {
    let method = rpc_method(context.workload);
    stopped_workload_completion(
        context,
        "chaos-failed",
        format!("workload aborted at simulation time {simulation_time_ms} because chaos failed"),
        method,
    )
}

fn stopped_workload_completion(
    context: WorkloadTaskContext,
    error_class: &str,
    error: String,
    rpc_method: &'static str,
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
                rpc_method,
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
                contract_call_name: None,
                contract_call_contract: None,
                storage_read_name: None,
                storage_contract: None,
                storage_key: None,
                storage_item_present: None,
                storage_item_kind: None,
                event_query_name: None,
                event_contract: None,
                event_count: None,
                event_cursor: None,
                event_latest_block: None,
                event_oldest_block: None,
                error_class: Some(error_class.into()),
                error: Some(error),
                read_after_write_name: None,
                read_after_write_contract: None,
                read_after_write_expected: None,
                read_after_write_observed: None,
                read_after_write_matched: None,
                read_after_write_rebased: None,
            },
        },
        workload: context.workload,
        latency_ms: 0,
        error_class: Some(error_class.into()),
        skipped: false,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
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
                contract_call_name: None,
                contract_call_contract: None,
                storage_read_name: None,
                storage_contract: None,
                storage_key: None,
                storage_item_present: None,
                storage_item_kind: None,
                event_query_name: None,
                event_contract: None,
                event_count: None,
                event_cursor: None,
                event_latest_block: None,
                event_oldest_block: None,
                error_class: Some("target-restarted".into()),
                error: Some("pending receipt lost because target restarted".into()),
                read_after_write_name: None,
                read_after_write_contract: None,
                read_after_write_expected: None,
                read_after_write_observed: None,
                read_after_write_matched: None,
                read_after_write_rebased: None,
            },
        },
        workload: WorkloadKind::ReceiptPoll,
        latency_ms: 0,
        error_class: Some("target-restarted".into()),
        skipped: false,
        chain_tip: None,
        submitted_transaction: None,
        observed_receipt: None,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
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

#[allow(clippy::too_many_lines)]
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
                        contract_call_name: None,
                        contract_call_contract: None,
                        storage_read_name: None,
                        storage_contract: None,
                        storage_key: None,
                        storage_item_present: None,
                        storage_item_kind: None,
                        event_query_name: None,
                        event_contract: None,
                        event_count: None,
                        event_cursor: None,
                        event_latest_block: None,
                        event_oldest_block: None,
                        error_class: None,
                        error: None,
                        read_after_write_name: None,
                        read_after_write_contract: None,
                        read_after_write_expected: None,
                        read_after_write_observed: None,
                        read_after_write_matched: None,
                        read_after_write_rebased: None,
                    },
                },
                skipped: false,
                submitted_transaction: None,
                observed_receipt: None,
                release_receipt_poll: None,
                receipt_poll: None,
                submit_to_receipt_latency_ms: None,
                read_after_write_transition: None,
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
                    contract_call_name: None,
                    contract_call_contract: None,
                    storage_read_name: None,
                    storage_contract: None,
                    storage_key: None,
                    storage_item_present: None,
                    storage_item_kind: None,
                    event_query_name: None,
                    event_contract: None,
                    event_count: None,
                    event_cursor: None,
                    event_latest_block: None,
                    event_oldest_block: None,
                    error_class: Some("rpc-error".into()),
                    error: Some(error),
                    read_after_write_name: None,
                    read_after_write_contract: None,
                    read_after_write_expected: None,
                    read_after_write_observed: None,
                    read_after_write_matched: None,
                    read_after_write_rebased: None,
                },
            },
            skipped: false,
            submitted_transaction: None,
            observed_receipt: None,
            release_receipt_poll: None,
            receipt_poll: None,
            submit_to_receipt_latency_ms: None,
            read_after_write_transition: None,
        }),
    }
}

#[allow(clippy::too_many_lines)]
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
                        contract_call_name: None,
                        contract_call_contract: None,
                        storage_read_name: None,
                        storage_contract: None,
                        storage_key: None,
                        storage_item_present: None,
                        storage_item_kind: None,
                        event_query_name: None,
                        event_contract: None,
                        event_count: None,
                        event_cursor: None,
                        event_latest_block: None,
                        event_oldest_block: None,
                        error_class: None,
                        error: None,
                        read_after_write_name: None,
                        read_after_write_contract: None,
                        read_after_write_expected: None,
                        read_after_write_observed: None,
                        read_after_write_matched: None,
                        read_after_write_rebased: None,
                    },
                },
                skipped: false,
                observed_receipt: None,
                release_receipt_poll: None,
                receipt_poll: None,
                submit_to_receipt_latency_ms: None,
                read_after_write_transition: None,
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
                    contract_call_name: None,
                    contract_call_contract: None,
                    storage_read_name: None,
                    storage_contract: None,
                    storage_key: None,
                    storage_item_present: None,
                    storage_item_kind: None,
                    event_query_name: None,
                    event_contract: None,
                    event_count: None,
                    event_cursor: None,
                    event_latest_block: None,
                    event_oldest_block: None,
                    error_class: Some("rpc-error".into()),
                    error: Some(error),
                    read_after_write_name: None,
                    read_after_write_contract: None,
                    read_after_write_expected: None,
                    read_after_write_observed: None,
                    read_after_write_matched: None,
                    read_after_write_rebased: None,
                },
            },
            skipped: false,
            observed_receipt: None,
            release_receipt_poll: None,
            receipt_poll: None,
            submit_to_receipt_latency_ms: None,
            read_after_write_transition: None,
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
        Ok(None) => Ok(receipt_poll_absent(tick, tx_hash, tx_hash_json, &times)),
        Err(error) => Ok(receipt_poll_failed(
            tick,
            tx_hash,
            tx_hash_json,
            &times,
            error,
        )),
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
        release_receipt_poll: None,
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
        read_after_write_transition: None,
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
        release_receipt_poll: None,
        read_after_write_transition: None,
    }
}

fn receipt_poll_absent(
    tick: &ScheduledActorTick,
    tx_hash: TxHash,
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
        release_receipt_poll: Some(ReceiptPollLeaseRelease {
            actor_id: tick.actor_id,
            tx_hash,
        }),
        read_after_write_transition: None,
    }
}

fn receipt_poll_failed(
    tick: &ScheduledActorTick,
    tx_hash: TxHash,
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
        release_receipt_poll: Some(ReceiptPollLeaseRelease {
            actor_id: tick.actor_id,
            tx_hash,
        }),
        read_after_write_transition: None,
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
            release_receipt_poll: None,
            receipt_poll: None,
            submit_to_receipt_latency_ms: None,
            read_after_write_transition: None,
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
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
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
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
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
        contract_call_name: None,
        contract_call_contract: None,
        storage_read_name: None,
        storage_contract: None,
        storage_key: None,
        storage_item_present: None,
        storage_item_kind: None,
        event_query_name: None,
        event_contract: None,
        event_count: None,
        event_cursor: None,
        event_latest_block: None,
        event_oldest_block: None,
        error_class: None,
        error: None,
        read_after_write_name: None,
        read_after_write_contract: None,
        read_after_write_expected: None,
        read_after_write_observed: None,
        read_after_write_matched: None,
        read_after_write_rebased: None,
    }
}

#[allow(clippy::too_many_lines)]
async fn execute_contract_call<C>(
    client: C,
    tick: &ScheduledActorTick,
    prepared: PreparedContractCallWorkload,
) -> Result<WorkloadCompletion, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let submit_response = client
        .send_raw_transaction(prepared.tx.tx_hex.clone())
        .await;
    let submitted_at = Instant::now();
    let tx_nonce = Some(prepared.tx.nonce.to_string());

    let tx_hash = match submit_response {
        Ok(tx_hash) => tx_hash,
        Err(error) => {
            let latency_ms = duration_ms(submitted_at.duration_since(started));
            return Ok(contract_call_failure_completion(
                contract_call_workload_event(
                    tick,
                    &prepared.call,
                    started_at_unix_ms,
                    unix_ms()?.to_string(),
                    latency_ms,
                    None,
                    tx_nonce,
                )
                .with_error("contract-call-submit-rpc-error", error),
                latency_ms,
                "contract-call-submit-rpc-error",
                None,
                None,
            ));
        }
    };

    let tx_hash_json = json_string(&tx_hash, "txHash")?;
    let receipt = poll_workload_receipt(client, tx_hash, "contract-call").await;
    let completed_at = Instant::now();
    let latency_ms = duration_ms(completed_at.duration_since(started));
    let submit_to_receipt_latency_ms = Some(duration_ms(
        completed_at.saturating_duration_since(submitted_at),
    ));
    let event = contract_call_workload_event(
        tick,
        &prepared.call,
        started_at_unix_ms,
        unix_ms()?.to_string(),
        latency_ms,
        Some(tx_hash_json),
        tx_nonce,
    );

    match receipt {
        Ok(receipt) => {
            let status = SimReceiptStatus::from_exec_status(&receipt.status);
            let event = event.with_receipt(
                Some(*receipt.height),
                true,
                status,
                submit_to_receipt_latency_ms,
            );
            let observed_receipt = Some(ObservedReceipt {
                actor_id: tick.actor_id,
                receipt,
            });
            let submitted_transaction = Some(SubmittedTransaction {
                actor_id: tick.actor_id,
                tx_hash,
                scheduled_time_ms: tick.simulation_time_ms,
                completed_at: submitted_at,
            });
            if event.receipt_status.as_deref() == Some("ok") {
                Ok(WorkloadCompletion {
                    workload: WorkloadKind::ContractCall,
                    latency_ms,
                    error_class: None,
                    chain_tip: None,
                    event: ResultEvent::WorkloadCompleted { event },
                    skipped: false,
                    submitted_transaction,
                    observed_receipt,
                    release_receipt_poll: None,
                    receipt_poll: None,
                    submit_to_receipt_latency_ms,
                    read_after_write_transition: None,
                })
            } else {
                Ok(contract_call_failure_completion(
                    event.with_error(
                        "contract-call-receipt-status",
                        format!(
                            "contract call `{}` receipt status was not ok",
                            prepared.call.name
                        ),
                    ),
                    latency_ms,
                    "contract-call-receipt-status",
                    submitted_transaction,
                    observed_receipt,
                ))
            }
        }
        Err(error) => Ok(contract_call_failure_completion(
            event.with_error("contract-call-receipt-poll-error", error.to_string()),
            latency_ms,
            "contract-call-receipt-poll-error",
            Some(SubmittedTransaction {
                actor_id: tick.actor_id,
                tx_hash,
                scheduled_time_ms: tick.simulation_time_ms,
                completed_at: submitted_at,
            }),
            None,
        )),
    }
}

/// A failed read-back of the counter, with the artifact error class.
struct ReadAfterWriteReadFailure {
    class: &'static str,
    message: String,
}

/// Read the actor's counter via `groundhog_view`, decoding the CXS `u64` return.
async fn read_after_write_counter<C>(
    client: &C,
    prepared: &PreparedReadAfterWriteWorkload,
) -> Result<u64, ReadAfterWriteReadFailure>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let entry = &prepared.entry;
    let request = ViewRequest {
        contract: Hex0x(entry.contract),
        entrypoint: HexBytes(entry.read_entrypoint.clone()),
        calldata: HexBytes(entry.read_calldata.clone()),
        caller: Some(Hex0x(prepared.caller)),
    };
    let response = client
        .view(request)
        .await
        .map_err(|error| ReadAfterWriteReadFailure {
            class: "view-rpc-error",
            message: error,
        })?;
    match response.output {
        RootInvocationOutput::Return { return_data } => xdrs::cxs::decode::<u64>(&return_data.0)
            .map_err(|error| ReadAfterWriteReadFailure {
                class: "read-decode-error",
                message: format!(
                    "read-after-write read `{}` return is not a u64: {error}",
                    entry.name
                ),
            }),
        RootInvocationOutput::Abort { .. } => Err(ReadAfterWriteReadFailure {
            class: "view-aborted",
            message: format!("read-after-write read `{}` aborted", entry.name),
        }),
        RootInvocationOutput::Fault => Err(ReadAfterWriteReadFailure {
            class: "view-fault",
            message: format!("read-after-write read `{}` faulted", entry.name),
        }),
        _ => Err(ReadAfterWriteReadFailure {
            class: "view-unsupported-output",
            message: format!(
                "read-after-write read `{}` returned an unsupported view output",
                entry.name
            ),
        }),
    }
}

/// Read the committed counter after an OK receipt. Some local targets can make
/// the receipt visible just before the view plane observes the same write.
async fn read_after_write_counter_after_commit<C>(
    client: &C,
    prepared: &PreparedReadAfterWriteWorkload,
    expected: u64,
) -> Result<u64, ReadAfterWriteReadFailure>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let deadline = Instant::now()
        .checked_add(READ_AFTER_WRITE_POST_COMMIT_READ_RETRY_TIMEOUT)
        .unwrap_or_else(Instant::now);
    loop {
        let observed = read_after_write_counter(client, prepared).await?;
        if observed >= expected {
            return Ok(observed);
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(observed);
        }
        tokio::time::sleep(READ_AFTER_WRITE_POST_COMMIT_READ_RETRY_INTERVAL.min(remaining)).await;
    }
}

#[allow(clippy::too_many_arguments)]
fn read_after_write_base_event(
    tick: &ScheduledActorTick,
    prepared: &PreparedReadAfterWriteWorkload,
    started_at_unix_ms: String,
    completed_at_unix_ms: String,
    latency_ms: u64,
    rpc_method: &'static str,
    tx_nonce: Option<String>,
    expected: Option<u64>,
    rebased: bool,
) -> WorkloadEvent {
    WorkloadEvent {
        actor_id: tick.actor_id,
        workload: WorkloadKind::ReadAfterWrite,
        scheduled_time_ms: tick.simulation_time_ms,
        started_at_unix_ms,
        completed_at_unix_ms,
        latency_ms,
        rpc_method,
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
        contract_call_name: None,
        contract_call_contract: None,
        storage_read_name: None,
        storage_contract: None,
        storage_key: None,
        storage_item_present: None,
        storage_item_kind: None,
        event_query_name: None,
        event_contract: None,
        event_count: None,
        event_cursor: None,
        event_latest_block: None,
        event_oldest_block: None,
        error_class: None,
        error: None,
        read_after_write_name: Some(prepared.entry.name.clone()),
        read_after_write_contract: Some(prepared.entry.contract_name.clone()),
        read_after_write_expected: expected.map(|value| value.to_string()),
        read_after_write_observed: None,
        read_after_write_matched: None,
        read_after_write_rebased: Some(rebased),
    }
}

#[allow(clippy::too_many_arguments)]
fn read_after_write_completion(
    event: WorkloadEvent,
    completed: bool,
    latency_ms: u64,
    error_class: Option<String>,
    submitted_transaction: Option<SubmittedTransaction>,
    observed_receipt: Option<ObservedReceipt>,
    submit_to_receipt_latency_ms: Option<u64>,
    transition: Option<ReadAfterWriteUpdate>,
) -> WorkloadCompletion {
    let event = if completed {
        ResultEvent::WorkloadCompleted { event }
    } else {
        ResultEvent::WorkloadFailed { event }
    };
    WorkloadCompletion {
        event,
        workload: WorkloadKind::ReadAfterWrite,
        latency_ms,
        error_class,
        skipped: false,
        chain_tip: None,
        submitted_transaction,
        observed_receipt,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms,
        read_after_write_transition: transition,
    }
}

fn read_after_write_overflow_completion(
    tick: &ScheduledActorTick,
    prepared: &PreparedReadAfterWriteWorkload,
    started_at_unix_ms: &str,
    started: &Instant,
    rpc_method: &'static str,
    tx_nonce: Option<String>,
    rebased: bool,
) -> Result<WorkloadCompletion, SimError> {
    let latency_ms = duration_ms(started.elapsed());
    let event = read_after_write_base_event(
        tick,
        prepared,
        started_at_unix_ms.to_owned(),
        unix_ms()?.to_string(),
        latency_ms,
        rpc_method,
        tx_nonce,
        None,
        rebased,
    )
    .with_error(
        "read-after-write-overflow",
        "read-after-write counter overflowed u64",
    );
    Ok(read_after_write_completion(
        event,
        false,
        latency_ms,
        Some("read-after-write-overflow".into()),
        None,
        None,
        None,
        None,
    ))
}

/// Execute one read-after-write round-trip: reconcile, write, await commit, read
/// back, and assert. Advances actor state only at known-committed.
#[allow(clippy::too_many_lines)]
async fn execute_read_after_write<C>(
    client: C,
    tick: &ScheduledActorTick,
    prepared: PreparedReadAfterWriteWorkload,
) -> Result<WorkloadCompletion, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let actor_id = tick.actor_id;
    let tx_nonce = Some(prepared.tx.nonce.to_string());

    // 1. Reconcile (NeedsReconcile only): pre-read to establish the baseline.
    let (expected, rebased) = match prepared.state {
        ReadAfterWriteState::Known(_) => {
            match prepared.state.expected_after_increment(actor_id, None) {
                Ok(expected) => (expected, false),
                Err(ActorError::ReadAfterWriteOverflow { .. }) => {
                    return read_after_write_overflow_completion(
                        tick,
                        &prepared,
                        &started_at_unix_ms,
                        &started,
                        "groundhog_sendRawTransaction",
                        tx_nonce,
                        false,
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        ReadAfterWriteState::NeedsReconcile => {
            match read_after_write_counter(&client, &prepared).await {
                Ok(baseline) => match prepared
                    .state
                    .expected_after_increment(actor_id, Some(baseline))
                {
                    Ok(expected) => (expected, true),
                    Err(ActorError::ReadAfterWriteOverflow { .. }) => {
                        return read_after_write_overflow_completion(
                            tick,
                            &prepared,
                            &started_at_unix_ms,
                            &started,
                            "groundhog_view",
                            tx_nonce,
                            true,
                        );
                    }
                    Err(error) => return Err(error.into()),
                },
                Err(failure) => {
                    // Reconcile pre-read failed: stay NeedsReconcile, write nothing.
                    let latency_ms = duration_ms(started.elapsed());
                    let event = read_after_write_base_event(
                        tick,
                        &prepared,
                        started_at_unix_ms.clone(),
                        unix_ms()?.to_string(),
                        latency_ms,
                        "groundhog_view",
                        tx_nonce.clone(),
                        None,
                        false,
                    )
                    .with_error(failure.class, failure.message);
                    return Ok(read_after_write_completion(
                        event,
                        false,
                        latency_ms,
                        Some(failure.class.into()),
                        None,
                        None,
                        None,
                        None,
                    ));
                }
            }
        }
    };

    // 2-3. Submit the write.
    let submitted_at = Instant::now();
    let submit = client
        .send_raw_transaction(prepared.tx.tx_hex.clone())
        .await;
    let tx_hash = match submit {
        Ok(tx_hash) => tx_hash,
        Err(error) => {
            // Submit RPC failure: no commit status, state unchanged, nonce not advanced.
            let latency_ms = duration_ms(started.elapsed());
            let event = read_after_write_base_event(
                tick,
                &prepared,
                started_at_unix_ms.clone(),
                unix_ms()?.to_string(),
                latency_ms,
                "groundhog_sendRawTransaction",
                tx_nonce.clone(),
                Some(expected),
                rebased,
            )
            .with_error("rpc-error", error);
            return Ok(read_after_write_completion(
                event,
                false,
                latency_ms,
                Some("rpc-error".into()),
                None,
                None,
                None,
                None,
            ));
        }
    };

    // 4. Await receipt.
    let receipt = poll_workload_receipt(client.clone(), tx_hash, "read-after-write").await;
    let completed_at = Instant::now();
    let submit_to_receipt_latency_ms = Some(duration_ms(
        completed_at.saturating_duration_since(submitted_at),
    ));
    let tx_hash_json = json_string(&tx_hash, "txHash")?;
    let submitted_transaction = Some(SubmittedTransaction {
        actor_id,
        tx_hash,
        scheduled_time_ms: tick.simulation_time_ms,
        completed_at: submitted_at,
    });

    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(error) => {
            // Receipt unknown: nonce advanced; commit status unknown -> Invalidate.
            let latency_ms = duration_ms(started.elapsed());
            let mut event = read_after_write_base_event(
                tick,
                &prepared,
                started_at_unix_ms.clone(),
                unix_ms()?.to_string(),
                latency_ms,
                "groundhog_getTransactionReceipt",
                tx_nonce.clone(),
                Some(expected),
                rebased,
            );
            event.tx_hash = Some(tx_hash_json.clone());
            let event = event.with_error("rpc-error", error.to_string());
            return Ok(read_after_write_completion(
                event,
                false,
                latency_ms,
                Some("rpc-error".into()),
                submitted_transaction,
                None,
                None,
                Some(ReadAfterWriteUpdate {
                    actor_id,
                    transition: ReadAfterWriteTransition::Invalidate,
                }),
            ));
        }
    };

    let status = SimReceiptStatus::from_exec_status(&receipt.status);
    let receipt_height = *receipt.height;
    let committed = matches!(receipt.status, ExecStatus::Ok);
    let observed_receipt = Some(ObservedReceipt { actor_id, receipt });

    if !committed {
        // Non-ok receipt: the write did not take effect; do not advance state.
        let latency_ms = duration_ms(started.elapsed());
        let mut event = read_after_write_base_event(
            tick,
            &prepared,
            started_at_unix_ms.clone(),
            unix_ms()?.to_string(),
            latency_ms,
            "groundhog_getTransactionReceipt",
            tx_nonce.clone(),
            Some(expected),
            rebased,
        );
        event.tx_hash = Some(tx_hash_json.clone());
        let event = event
            .with_receipt(
                Some(receipt_height),
                true,
                status,
                submit_to_receipt_latency_ms,
            )
            .with_error(
                "contract-call-receipt-status",
                format!(
                    "read-after-write write `{}` receipt status was not ok",
                    prepared.entry.name
                ),
            );
        return Ok(read_after_write_completion(
            event,
            false,
            latency_ms,
            Some("contract-call-receipt-status".into()),
            submitted_transaction,
            observed_receipt,
            submit_to_receipt_latency_ms,
            None,
        ));
    }

    // 5. Known committed: rebase to Known(expected), then read back and assert.
    let set_known = Some(ReadAfterWriteUpdate {
        actor_id,
        transition: ReadAfterWriteTransition::SetKnown(expected),
    });
    let read_back = read_after_write_counter_after_commit(&client, &prepared, expected).await;
    let latency_ms = duration_ms(started.elapsed());
    let mut event = read_after_write_base_event(
        tick,
        &prepared,
        started_at_unix_ms,
        unix_ms()?.to_string(),
        latency_ms,
        "groundhog_view",
        tx_nonce,
        Some(expected),
        rebased,
    );
    event.tx_hash = Some(tx_hash_json);
    let event = event.with_receipt(
        Some(receipt_height),
        true,
        status,
        submit_to_receipt_latency_ms,
    );

    match read_back {
        Ok(observed) => {
            let mut event = event;
            event.read_after_write_observed = Some(observed.to_string());
            if observed == expected {
                event.read_after_write_matched = Some(true);
                Ok(read_after_write_completion(
                    event,
                    true,
                    latency_ms,
                    None,
                    submitted_transaction,
                    observed_receipt,
                    submit_to_receipt_latency_ms,
                    set_known,
                ))
            } else {
                event.read_after_write_matched = Some(false);
                let event = event.with_error(
                    "read-after-write-mismatch",
                    format!(
                        "read-after-write `{}` read back {observed}, expected {expected}",
                        prepared.entry.name
                    ),
                );
                Ok(read_after_write_completion(
                    event,
                    false,
                    latency_ms,
                    Some("read-after-write-mismatch".into()),
                    submitted_transaction,
                    observed_receipt,
                    submit_to_receipt_latency_ms,
                    Some(ReadAfterWriteUpdate {
                        actor_id,
                        transition: ReadAfterWriteTransition::Invalidate,
                    }),
                ))
            }
        }
        Err(failure) => {
            // Post-commit read failure: keep Known(expected); only observation failed.
            let event = event.with_error(failure.class, failure.message);
            Ok(read_after_write_completion(
                event,
                false,
                latency_ms,
                Some(failure.class.into()),
                submitted_transaction,
                observed_receipt,
                submit_to_receipt_latency_ms,
                set_known,
            ))
        }
    }
}

async fn poll_workload_receipt<C>(
    client: C,
    tx_hash: TxHash,
    workload: &str,
) -> Result<TransactionReceipt, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let deadline = Instant::now()
        .checked_add(DEFAULT_RECEIPT_DRAIN_TIMEOUT)
        .unwrap_or_else(Instant::now);
    loop {
        if let Some(receipt) = client.transaction_receipt(tx_hash).await.map_err(|error| {
            SimError::Executor(format!("{workload} receipt poll failed: {error}"))
        })? {
            return Ok(receipt);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(SimError::Executor(format!(
                "{workload} receipt poll timed out"
            )));
        }
        tokio::time::sleep(DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL.min(remaining)).await;
    }
}

fn contract_call_failure_completion(
    event: WorkloadEvent,
    latency_ms: u64,
    class: impl Into<String>,
    submitted_transaction: Option<SubmittedTransaction>,
    observed_receipt: Option<ObservedReceipt>,
) -> WorkloadCompletion {
    WorkloadCompletion {
        workload: WorkloadKind::ContractCall,
        latency_ms,
        error_class: Some(class.into()),
        chain_tip: None,
        event: ResultEvent::WorkloadFailed { event },
        skipped: false,
        submitted_transaction,
        observed_receipt,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
    }
}

fn contract_call_workload_event(
    tick: &ScheduledActorTick,
    call: &PreparedContractCall,
    started_at_unix_ms: String,
    completed_at_unix_ms: String,
    latency_ms: u64,
    tx_hash: Option<String>,
    tx_nonce: Option<String>,
) -> WorkloadEvent {
    WorkloadEvent {
        actor_id: tick.actor_id,
        workload: WorkloadKind::ContractCall,
        scheduled_time_ms: tick.simulation_time_ms,
        started_at_unix_ms,
        completed_at_unix_ms,
        latency_ms,
        rpc_method: "groundhog_sendRawTransaction",
        chain_height: None,
        chain_block_hash: None,
        tx_hash,
        tx_nonce,
        receipt_present: None,
        receipt_status: None,
        receipt_status_detail: None,
        submit_to_receipt_latency_ms: None,
        view_result: None,
        view_return_data: None,
        view_abort_data: None,
        view_event_count: None,
        contract_call_name: Some(call.name.clone()),
        contract_call_contract: Some(format!("0x{}", hex::encode(call.contract.as_ref()))),
        storage_read_name: None,
        storage_contract: None,
        storage_key: None,
        storage_item_present: None,
        storage_item_kind: None,
        event_query_name: None,
        event_contract: None,
        event_count: None,
        event_cursor: None,
        event_latest_block: None,
        event_oldest_block: None,
        error_class: None,
        error: None,
        read_after_write_name: None,
        read_after_write_contract: None,
        read_after_write_expected: None,
        read_after_write_observed: None,
        read_after_write_matched: None,
        read_after_write_rebased: None,
    }
}

async fn execute_storage_read<C>(
    client: C,
    tick: &ScheduledActorTick,
    prepared: PreparedStorageReadWorkload,
) -> Result<WorkloadCompletion, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let response = client
        .storage_read(
            Hex0x(prepared.read.contract),
            HexBytes(prepared.read.key.clone()),
        )
        .await;
    let latency_ms = duration_ms(started.elapsed());
    let completed_at_unix_ms = unix_ms()?.to_string();

    match response {
        Ok(response) => Ok(storage_read_response_completion(
            tick,
            &prepared.read,
            &response,
            started_at_unix_ms,
            completed_at_unix_ms,
            latency_ms,
        )),
        Err(error) => Ok(storage_read_failure_completion(
            storage_read_workload_event(
                tick,
                &prepared.read,
                started_at_unix_ms,
                completed_at_unix_ms,
                latency_ms,
            ),
            latency_ms,
            "storage-read-rpc-error",
            error,
        )),
    }
}

fn storage_read_response_completion(
    tick: &ScheduledActorTick,
    read: &PreparedStorageRead,
    response: &StorageItemRead,
    started_at_unix_ms: String,
    completed_at_unix_ms: String,
    latency_ms: u64,
) -> WorkloadCompletion {
    let mut event = storage_read_workload_event(
        tick,
        read,
        started_at_unix_ms,
        completed_at_unix_ms,
        latency_ms,
    );
    let present = response.item.is_some();
    event.chain_height = Some(*response.block_height);
    event.storage_item_present = Some(present);
    event.storage_item_kind = response
        .item
        .as_ref()
        .map(storage_item_kind)
        .map(str::to_owned);

    if let Some(expected) = read.expect_present
        && expected != present
    {
        let error = format!(
            "storage read `{}` on `{}` present={present}, expected {expected}",
            read.name, read.contract_name
        );
        return storage_read_failure_completion(
            event,
            latency_ms,
            "storage-presence-mismatch",
            error,
        );
    }
    storage_read_success_completion(event, latency_ms)
}

fn storage_read_success_completion(event: WorkloadEvent, latency_ms: u64) -> WorkloadCompletion {
    WorkloadCompletion {
        workload: WorkloadKind::StorageRead,
        latency_ms,
        error_class: None,
        chain_tip: None,
        event: ResultEvent::WorkloadCompleted { event },
        skipped: false,
        submitted_transaction: None,
        observed_receipt: None,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
    }
}

fn storage_read_failure_completion(
    event: WorkloadEvent,
    latency_ms: u64,
    class: impl Into<String>,
    error: impl Into<String>,
) -> WorkloadCompletion {
    let class = class.into();
    WorkloadCompletion {
        workload: WorkloadKind::StorageRead,
        latency_ms,
        error_class: Some(class.clone()),
        chain_tip: None,
        event: ResultEvent::WorkloadFailed {
            event: event.with_error(class, error),
        },
        skipped: false,
        submitted_transaction: None,
        observed_receipt: None,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
    }
}

fn storage_read_workload_event(
    tick: &ScheduledActorTick,
    read: &PreparedStorageRead,
    started_at_unix_ms: String,
    completed_at_unix_ms: String,
    latency_ms: u64,
) -> WorkloadEvent {
    WorkloadEvent {
        actor_id: tick.actor_id,
        workload: WorkloadKind::StorageRead,
        scheduled_time_ms: tick.simulation_time_ms,
        started_at_unix_ms,
        completed_at_unix_ms,
        latency_ms,
        rpc_method: "groundhog_getStorageAt",
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
        contract_call_name: None,
        contract_call_contract: None,
        storage_read_name: Some(read.name.clone()),
        storage_contract: Some(format!("0x{}", hex::encode(read.contract.as_ref()))),
        storage_key: Some(format!("0x{}", hex::encode(&read.key))),
        storage_item_present: None,
        storage_item_kind: None,
        event_query_name: None,
        event_contract: None,
        event_count: None,
        event_cursor: None,
        event_latest_block: None,
        event_oldest_block: None,
        error_class: None,
        error: None,
        read_after_write_name: None,
        read_after_write_contract: None,
        read_after_write_expected: None,
        read_after_write_observed: None,
        read_after_write_matched: None,
        read_after_write_rebased: None,
    }
}

fn storage_item_kind(item: &StorageItemInfo) -> &'static str {
    match item {
        StorageItemInfo::NonnegativeInt { .. } => "nonnegative-int",
        StorageItemInfo::Bytestring { .. } => "bytestring",
        StorageItemInfo::AuthData => "auth-data",
        StorageItemInfo::RetrocausalQueue => "retrocausal-queue",
        StorageItemInfo::ReplayCacheEntry => "replay-cache-entry",
        StorageItemInfo::DeployedContract { .. } => "deployed-contract",
        StorageItemInfo::Unknown { .. } => "unknown",
    }
}

async fn execute_events_query<C>(
    client: C,
    tick: &ScheduledActorTick,
    prepared: PreparedEventQueryWorkload,
) -> Result<WorkloadCompletion, SimError>
where
    C: SimRpcClient + Clone + Send + Sync + 'static,
{
    let started_at_unix_ms = unix_ms()?.to_string();
    let started = Instant::now();
    let request = event_query_request(&prepared.query);
    let response = client.events_query(request).await;
    let latency_ms = duration_ms(started.elapsed());
    let completed_at_unix_ms = unix_ms()?.to_string();

    match response {
        Ok(response) => Ok(events_query_response_completion(
            tick,
            &prepared.query,
            &response,
            started_at_unix_ms,
            completed_at_unix_ms,
            latency_ms,
        )),
        Err(error) => Ok(events_query_failure_completion(
            events_query_workload_event(
                tick,
                &prepared.query,
                started_at_unix_ms,
                completed_at_unix_ms,
                latency_ms,
            ),
            latency_ms,
            "events-query-rpc-error",
            error,
        )),
    }
}

fn event_query_request(query: &PreparedEventQuery) -> EventQuery {
    EventQuery {
        filters: EventFilterList(VecM(vec![EventFilter {
            contract_addresses: ContractAddressList(VecM(vec![Hex0x(query.contract)])),
            topics: EventTopicFilter {
                topic0: TopicList(VecM(Vec::new())),
                topic1: TopicList(VecM(Vec::new())),
                topic2: TopicList(VecM(Vec::new())),
            },
        }])),
        order: match query.order {
            EventQueryOrderSpec::Ascending => IterationOrder::Ascending,
            EventQueryOrderSpec::Descending => IterationOrder::Descending,
        },
        anchor: match query.anchor {
            EventQueryAnchorSpec::AtBeginning => BlockAnchor::AtBeginning,
            EventQueryAnchorSpec::AtEnd => BlockAnchor::AtEnd,
            EventQueryAnchorSpec::AtHeight { height } => BlockAnchor::AtHeight {
                height: BlockHeight::from(height),
            },
        },
        cursor: PaginationCursor(query.cursor.clone()),
        limit: query.limit,
    }
}

fn events_query_response_completion(
    tick: &ScheduledActorTick,
    query: &PreparedEventQuery,
    response: &EventPage,
    started_at_unix_ms: String,
    completed_at_unix_ms: String,
    latency_ms: u64,
) -> WorkloadCompletion {
    let mut event = events_query_workload_event(
        tick,
        query,
        started_at_unix_ms,
        completed_at_unix_ms,
        latency_ms,
    );
    let event_count = response.events.len() as u64;
    event.event_count = Some(event_count);
    event.event_cursor = Some(format!("0x{}", hex::encode(&response.cursor.0)));
    event.event_latest_block = Some(*response.latest_block);
    event.event_oldest_block = Some(*response.oldest_block);

    if let Some(expected) = query.expect_min_events
        && event_count < expected
    {
        let error = format!(
            "event query `{}` on `{}` returned {event_count} events, expected at least {expected}",
            query.name, query.contract_name
        );
        return events_query_failure_completion(
            event,
            latency_ms,
            "event-count-below-minimum",
            error,
        );
    }
    events_query_success_completion(event, latency_ms)
}

fn events_query_success_completion(event: WorkloadEvent, latency_ms: u64) -> WorkloadCompletion {
    WorkloadCompletion {
        workload: WorkloadKind::EventsQuery,
        latency_ms,
        error_class: None,
        chain_tip: None,
        event: ResultEvent::WorkloadCompleted { event },
        skipped: false,
        submitted_transaction: None,
        observed_receipt: None,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
    }
}

fn events_query_failure_completion(
    event: WorkloadEvent,
    latency_ms: u64,
    class: impl Into<String>,
    error: impl Into<String>,
) -> WorkloadCompletion {
    let class = class.into();
    WorkloadCompletion {
        workload: WorkloadKind::EventsQuery,
        latency_ms,
        error_class: Some(class.clone()),
        chain_tip: None,
        event: ResultEvent::WorkloadFailed {
            event: event.with_error(class, error),
        },
        skipped: false,
        submitted_transaction: None,
        observed_receipt: None,
        release_receipt_poll: None,
        receipt_poll: None,
        submit_to_receipt_latency_ms: None,
        read_after_write_transition: None,
    }
}

fn events_query_workload_event(
    tick: &ScheduledActorTick,
    query: &PreparedEventQuery,
    started_at_unix_ms: String,
    completed_at_unix_ms: String,
    latency_ms: u64,
) -> WorkloadEvent {
    WorkloadEvent {
        actor_id: tick.actor_id,
        workload: WorkloadKind::EventsQuery,
        scheduled_time_ms: tick.simulation_time_ms,
        started_at_unix_ms,
        completed_at_unix_ms,
        latency_ms,
        rpc_method: "groundhog_getEvents",
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
        contract_call_name: None,
        contract_call_contract: None,
        storage_read_name: None,
        storage_contract: None,
        storage_key: None,
        storage_item_present: None,
        storage_item_kind: None,
        event_query_name: Some(query.name.clone()),
        event_contract: Some(format!("0x{}", hex::encode(query.contract.as_ref()))),
        event_count: None,
        event_cursor: None,
        event_latest_block: None,
        event_oldest_block: None,
        error_class: None,
        error: None,
        read_after_write_name: None,
        read_after_write_contract: None,
        read_after_write_expected: None,
        read_after_write_observed: None,
        read_after_write_matched: None,
        read_after_write_rebased: None,
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
        contract_call_name: None,
        contract_call_contract: None,
        storage_read_name: None,
        storage_contract: None,
        storage_key: None,
        storage_item_present: None,
        storage_item_kind: None,
        event_query_name: None,
        event_contract: None,
        event_count: None,
        event_cursor: None,
        event_latest_block: None,
        event_oldest_block: None,
        error_class: None,
        error: None,
        read_after_write_name: None,
        read_after_write_contract: None,
        read_after_write_expected: None,
        read_after_write_observed: None,
        read_after_write_matched: None,
        read_after_write_rebased: None,
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
    release_receipt_poll: Option<ReceiptPollLeaseRelease>,
    receipt_poll: Option<ReceiptPollCompletion>,
    submit_to_receipt_latency_ms: Option<u64>,
    read_after_write_transition: Option<ReadAfterWriteUpdate>,
}

/// A read-after-write actor-state transition produced by an executed round-trip
/// and applied to `ActorState` in `record_completion`.
#[derive(Debug, Clone, Copy)]
struct ReadAfterWriteUpdate {
    actor_id: u32,
    transition: ReadAfterWriteTransition,
}

#[derive(Debug, Clone, Copy)]
enum ReadAfterWriteTransition {
    /// The write is known committed; rebase to this counter value.
    SetKnown(u64),
    /// Commit status is unknown or the read-back mismatched; force a reconcile.
    Invalidate,
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
struct ReceiptPollLeaseRelease {
    actor_id: u32,
    tx_hash: TxHash,
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
    ContractCall(PreparedContractCallWorkload),
    StorageRead(PreparedStorageReadWorkload),
    EventsQuery(PreparedEventQueryWorkload),
    ReadAfterWrite(PreparedReadAfterWriteWorkload),
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

#[derive(Debug, Clone)]
struct PreparedContractCallWorkload {
    call: PreparedContractCall,
    tx: BuiltSubmitTransaction,
}

#[derive(Debug, Clone)]
struct PreparedStorageReadWorkload {
    read: PreparedStorageRead,
}

#[derive(Debug, Clone)]
struct PreparedEventQueryWorkload {
    query: PreparedEventQuery,
}

#[derive(Debug, Clone)]
struct PreparedReadAfterWriteWorkload {
    entry: PreparedReadAfterWrite,
    /// The signed dev-wallet `doAction` write transaction (e.g. `bump()`).
    tx: BuiltSubmitTransaction,
    /// Caller identity for the `get()` read-back; matches the write's effective
    /// `msg.sender` (the actor wallet).
    caller: groundhog_xdr::types::ContractAddress,
    /// Actor read-after-write belief snapshot at prepare time.
    state: ReadAfterWriteState,
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

#[derive(Debug, Clone, Copy)]
struct ResourceSampleTaskContext {
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
        // read-after-write reports its write RPC for skipped/aborted ticks; the
        // executed round-trip sets its own terminal rpc_method in Slice L3.
        WorkloadKind::WalletSubmitSmoke
        | WorkloadKind::ContractCall
        | WorkloadKind::ReadAfterWrite => "groundhog_sendRawTransaction",
        WorkloadKind::ReceiptPoll => "groundhog_getTransactionReceipt",
        WorkloadKind::StatusPoll => "groundhog_getTransactionStatus",
        WorkloadKind::EventsQuery => "groundhog_getEvents",
        WorkloadKind::StorageRead => "groundhog_getStorageAt",
        WorkloadKind::ViewCall => "groundhog_view",
    }
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::{
        BlockHeight,
        block::ChainTip,
        hex::Hex0x,
        receipt::{ExecStatus, TransactionReceipt},
        runtime_manifest::RuntimeManifest,
        state::StorageItemRead,
        view::{RootInvocationOutput, ViewResponse},
    };
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::{ContractAddress, SHA256Hash};
    use serde_json::json;
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };
    use tempfile::NamedTempFile;

    use super::*;
    use crate::event::EventWriter;
    use crate::manifest::SimulationManifest;
    use crate::metrics::RunMetricsBuilder;
    use crate::nonce::NonceNamespace;
    use crate::result::ResourceSampleScope;
    use crate::runtime::SimAccount;
    use crate::scheduler::{generate_schedule, phase_starts};
    use crate::target::{ChaosController, ChaosEventOutcome, TargetProviderFuture};

    type StorageRequests = Arc<Mutex<Vec<(ContractAddress, Vec<u8>)>>>;

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
    struct ContractCallClient {
        submitted: Arc<Mutex<Vec<String>>>,
        receipts: Arc<Mutex<Vec<TxHash>>>,
        status: ExecStatus,
    }

    #[derive(Clone)]
    struct FailingContractCallReceiptClient {
        submitted: Arc<Mutex<Vec<String>>>,
        receipts: Arc<Mutex<Vec<TxHash>>>,
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

    #[derive(Clone)]
    struct StorageReadClient {
        response: StorageItemRead,
        requested: StorageRequests,
    }

    #[derive(Clone)]
    struct EventsQueryClient {
        response: EventPage,
        requested: Arc<Mutex<Vec<EventQuery>>>,
    }

    #[derive(Clone)]
    struct FailingEventsQueryClient {
        requested: Arc<Mutex<Vec<EventQuery>>>,
    }

    struct SuccessfulChaosController;

    struct FailingChaosController;

    #[derive(Clone)]
    struct NativeExpiryClient {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        response: Arc<std::sync::Mutex<Result<i64, String>>>,
    }

    impl SimRpcClient for NativeExpiryClient {
        async fn send_raw_transaction(&self, _: String) -> Result<TxHash, String> {
            panic!("expiry refresh must not submit transactions")
        }
        async fn chain_tip(&self) -> Result<ChainTip, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let height = self.response.lock().expect("tip response").clone()?;
            Ok(ChainTip {
                height: height.into(),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0; 32])),
            })
        }
    }

    #[tokio::test]
    async fn native_schedule_stops_before_submit_when_expiry_tip_is_unavailable() {
        let client = NativeExpiryClient {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            response: Arc::new(std::sync::Mutex::new(Err("unavailable".into()))),
        };
        let manifest = wallet_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule");
        let starts = phase_starts(&manifest).expect("phases");
        let file = NamedTempFile::new().expect("event file");
        let mut writer = EventWriter::create(file.path()).await.expect("writer");
        let mut actors = actor_states();
        actors[0].use_native_wallet();
        let mut metrics = RunMetricsBuilder::default();
        let report = run_schedule_with_chaos(
            client.clone(),
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut writer,
                metrics: &mut metrics,
                view_calls: &[],
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
            },
            None,
            &mut Vec::new(),
        )
        .await
        .expect("failed schedule report");
        assert!(
            report
                .failure
                .expect("failure")
                .contains("native expiry chain-tip refresh failed")
        );
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        assert_eq!(actors[0].next_nonce_sequence, 0);
        assert!(actors[0].last_submitted_tx_hash.is_none());
    }

    #[tokio::test]
    async fn native_expiry_cache_reuses_then_refreshes_tip_and_rejects_failures() {
        let client = NativeExpiryClient {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            response: Arc::new(std::sync::Mutex::new(Ok(10_000_100))),
        };
        let mut actors = actor_states();
        actors[0].use_native_wallet();
        let mut cache = None;
        refresh_native_chain_tip(&client, &mut cache, &mut actors)
            .await
            .expect("initial tip");
        assert_eq!(
            actors[0]
                .native_chain_tip
                .expect("actor tip")
                .expiry()
                .expect("expiry"),
            10_001_100
        );
        refresh_native_chain_tip(&client, &mut cache, &mut actors)
            .await
            .expect("reuse tip");
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        cache.as_mut().expect("cache").observed_at = Instant::now()
            .checked_sub(NATIVE_TIP_REFRESH_INTERVAL)
            .expect("test clock age");
        *client.response.lock().expect("response") = Ok(10_000_200);
        refresh_native_chain_tip(&client, &mut cache, &mut actors)
            .await
            .expect("refresh tip");
        assert_eq!(client.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            actors[0]
                .native_chain_tip
                .expect("actor tip")
                .expiry()
                .expect("expiry"),
            10_001_200
        );
        for response in [
            Err("unavailable".into()),
            Ok(10_000_199),
            Ok(i64::MAX),
            Ok(-1),
        ] {
            cache.as_mut().expect("cache").observed_at = Instant::now()
                .checked_sub(NATIVE_TIP_REFRESH_INTERVAL)
                .expect("test clock age");
            *client.response.lock().expect("response") = response;
            assert!(
                refresh_native_chain_tip(&client, &mut cache, &mut actors)
                    .await
                    .is_err()
            );
            assert_eq!(
                actors[0]
                    .native_chain_tip
                    .expect("last valid observation")
                    .height,
                10_000_200
            );
        }
        *client.response.lock().expect("response") = Err("initial failure".into());
        assert!(
            refresh_native_chain_tip(&client, &mut None, &mut actors)
                .await
                .is_err()
        );
    }

    struct StubResourceSampler {
        calls: Arc<AtomicU64>,
    }

    impl TargetResourceSampler for StubResourceSampler {
        fn sample(&self, scheduled_time_ms: u64) -> crate::resource::ResourceSamplerFuture<'_> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                ResourceSampleRequest::Ready(ResourceSample::Observed(ResourceSamplePoint {
                    simulation_time_ms: scheduled_time_ms,
                    observed_at_unix_ms: "1000".into(),
                    scope: ResourceSampleScope::ProcessGroup,
                    root_pid: 4242,
                    pgid: Some(4242),
                    process_count: 1,
                    rss_bytes: Some(12_345),
                    cpu_user_ms: Some(7),
                    cpu_system_ms: Some(11),
                    threads: Some(3),
                    open_fds: Some(5),
                }))
            })
        }
    }

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

    impl SimRpcClient for StorageReadClient {
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
        fn storage_read(
            &self,
            address: Hex0x<ContractAddress>,
            key: HexBytes,
        ) -> impl Future<Output = Result<StorageItemRead, String>> + Send {
            let response = self.response.clone();
            let requested = self.requested.clone();
            async move {
                requested
                    .lock()
                    .expect("storage request list should lock")
                    .push((address.0, key.0));
                Ok(response)
            }
        }
    }

    impl SimRpcClient for EventsQueryClient {
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
        fn events_query(
            &self,
            query: EventQuery,
        ) -> impl Future<Output = Result<EventPage, String>> + Send {
            let response = self.response.clone();
            let requested = self.requested.clone();
            async move {
                requested
                    .lock()
                    .expect("event query request list should lock")
                    .push(query);
                Ok(response)
            }
        }
    }

    impl SimRpcClient for FailingEventsQueryClient {
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
        fn events_query(
            &self,
            query: EventQuery,
        ) -> impl Future<Output = Result<EventPage, String>> + Send {
            let requested = self.requested.clone();
            async move {
                requested
                    .lock()
                    .expect("event query request list should lock")
                    .push(query);
                Err("simulated events query failure".into())
            }
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

    impl SimRpcClient for ContractCallClient {
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
                Ok(tx_hash(0x6c))
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let receipts = self.receipts.clone();
            let status = self.status.clone();
            async move {
                receipts
                    .lock()
                    .expect("receipt request list should lock")
                    .push(tx_hash);
                Ok(Some(receipt_with_status(tx_hash, status)))
            }
        }
    }

    impl SimRpcClient for FailingContractCallReceiptClient {
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
                Ok(tx_hash(0x6d))
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let receipts = self.receipts.clone();
            async move {
                receipts
                    .lock()
                    .expect("receipt request list should lock")
                    .push(tx_hash);
                Err("contract-call receipt failed".into())
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
    async fn run_schedule_records_resource_sample_without_workload_accounting() {
        let manifest = manifest();
        let schedule = vec![ScheduledSimulationEvent::ResourceSample(
            crate::scheduler::ScheduledResourceSample {
                resource_sample_index: 0,
                simulation_time_ms: 0,
            },
        )];
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let calls = Arc::new(AtomicU64::new(0));
        let sampler = StubResourceSampler {
            calls: calls.clone(),
        };

        run_schedule(
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: Some(&sampler),
            },
        )
        .await
        .expect("resource sample schedule should run");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(metrics.completed_count(), 0);
        assert_eq!(metrics.failed_count(), 0);
        assert_eq!(metrics.skipped_count(), 0);
        assert_eq!(metrics.simulator_overload_count(), 0);

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let event = find_event(&events, "resource-sample");
        assert_eq!(event["status"], "observed");
        assert_eq!(event["rootPid"], 4242);
        assert_eq!(event["rssBytes"], 12_345);

        let timeline = metrics
            .finish()
            .resource_timeline
            .expect("resource sample should produce a timeline");
        assert_eq!(timeline.sample_count, 1);
        assert_eq!(timeline.first.root_pid, 4242);
        assert!(timeline.segments[0].rss_growth_bytes_per_minute.is_none());
    }

    #[test]
    fn weighted_target_selection_is_deterministic_and_tracks_configured_skew() {
        let manifest = manifest();
        let targets = vec![
            PreparedContractCall {
                name: "hot".into(),
                contract_name: "events".into(),
                weight: 9,
                contract: ContractAddress::from([0x88; 32]),
                entrypoint: vec![0x11; 8],
                calldata: Vec::new(),
            },
            PreparedContractCall {
                name: "cold".into(),
                contract_name: "events".into(),
                weight: 1,
                contract: ContractAddress::from([0x89; 32]),
                entrypoint: vec![0x22; 8],
                calldata: Vec::new(),
            },
        ];
        let mut hot = 0_u64;
        let mut cold = 0_u64;

        for actor_sequence in 0..10_000 {
            let tick = ScheduledActorTick {
                phase_index: 7,
                actor_id: 0,
                actor_sequence,
                simulation_time_ms: actor_sequence,
            };
            let first = select_weighted_target(
                &manifest,
                WorkloadKind::ContractCall,
                &tick,
                &targets,
                "missing target",
            )
            .expect("target should select");
            let second = select_weighted_target(
                &manifest,
                WorkloadKind::ContractCall,
                &tick,
                &targets,
                "missing target",
            )
            .expect("target should select again");
            assert_eq!(first.name, second.name);
            match first.name.as_str() {
                "hot" => hot += 1,
                "cold" => cold += 1,
                other => panic!("unexpected target {other}"),
            }
        }

        assert_eq!(hot + cold, 10_000);
        assert!((8_700..=9_300).contains(&hot), "hot count was {hot}");
        assert!((700..=1_300).contains(&cold), "cold count was {cold}");
    }

    #[test]
    fn weighted_target_selection_preserves_single_target_default() {
        let manifest = manifest();
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 0,
        };
        let targets = vec![PreparedStorageRead {
            name: "only".into(),
            contract_name: "fibonacci".into(),
            weight: 1,
            contract: ContractAddress::from([0x88; 32]),
            key: vec![0; 32],
            expect_present: None,
        }];

        let selected = select_weighted_target(
            &manifest,
            WorkloadKind::StorageRead,
            &tick,
            &targets,
            "missing target",
        )
        .expect("single target should select");

        assert_eq!(selected.name, "only");
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                stagger_actors: false,
            },
            PhaseStart {
                phase_index: 1,
                simulation_time_ms: 20,
                duration_ms: 1,
                users: 1,
                rate_per_user_millihz: 1_000,
                stagger_actors: false,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
    async fn record_completion_emits_simulator_overload_when_receipt_tracking_is_full() {
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0].set_pending_receipt_limit_for_test(1);
        actors[0]
            .record_submitted_tx(tx_hash(0x10), 10, Instant::now())
            .expect("seed pending receipt should record");

        record_completion(
            WorkloadCompletion {
                event: ResultEvent::WorkloadCompleted {
                    event: WorkloadEvent {
                        actor_id: 0,
                        workload: WorkloadKind::WalletSubmitSmoke,
                        scheduled_time_ms: 20,
                        started_at_unix_ms: "100".into(),
                        completed_at_unix_ms: "101".into(),
                        latency_ms: 1,
                        rpc_method: "groundhog_sendRawTransaction",
                        chain_height: None,
                        chain_block_hash: None,
                        tx_hash: Some(prefixed_hex(0x20)),
                        tx_nonce: None,
                        receipt_present: None,
                        receipt_status: None,
                        receipt_status_detail: None,
                        submit_to_receipt_latency_ms: None,
                        view_result: None,
                        view_return_data: None,
                        view_abort_data: None,
                        view_event_count: None,
                        contract_call_name: None,
                        contract_call_contract: None,
                        storage_read_name: None,
                        storage_contract: None,
                        storage_key: None,
                        storage_item_present: None,
                        storage_item_kind: None,
                        event_query_name: None,
                        event_contract: None,
                        event_count: None,
                        event_cursor: None,
                        event_latest_block: None,
                        event_oldest_block: None,
                        error_class: None,
                        error: None,
                        read_after_write_name: None,
                        read_after_write_contract: None,
                        read_after_write_expected: None,
                        read_after_write_observed: None,
                        read_after_write_matched: None,
                        read_after_write_rebased: None,
                    },
                },
                workload: WorkloadKind::WalletSubmitSmoke,
                latency_ms: 1,
                error_class: None,
                skipped: false,
                chain_tip: None,
                submitted_transaction: Some(SubmittedTransaction {
                    actor_id: 0,
                    tx_hash: tx_hash(0x20),
                    scheduled_time_ms: 20,
                    completed_at: Instant::now(),
                }),
                observed_receipt: None,
                receipt_poll: None,
                submit_to_receipt_latency_ms: None,
                read_after_write_transition: None,
                release_receipt_poll: None,
            },
            &mut actors,
            &mut event_writer,
            &mut metrics,
        )
        .await
        .expect("completion should record");

        assert_eq!(actors[0].pending_receipt_count(), 1);
        assert_eq!(metrics.simulator_overload_count(), 1);
        let events: Vec<serde_json::Value> = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read")
            .lines()
            .map(|line| serde_json::from_str(line).expect("event should parse"))
            .collect();
        assert_eq!(events[0]["kind"], "workload-completed");
        assert_eq!(events[1]["kind"], "simulator-overload");
        assert_eq!(events[1]["txHash"], prefixed_hex(0x20));
        assert_eq!(events[1]["pendingReceipts"], 1);
        assert_eq!(events[1]["pendingReceiptLimit"], 1);
    }

    #[tokio::test]
    async fn contract_call_submits_polls_receipt_and_updates_actor_state() {
        let manifest = contract_call_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let call = prepared_contract_call().call;

        run_schedule(
            ContractCallClient {
                submitted: submitted.clone(),
                receipts: receipts.clone(),
                status: ExecStatus::Ok,
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
                contract_calls: &[call],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
            },
        )
        .await
        .expect("contract-call schedule should run");

        assert_eq!(actors[0].next_nonce_sequence, 1);
        assert_eq!(actors[0].last_submitted_tx_hash, Some(tx_hash(0x6c)));
        assert_eq!(
            actors[0]
                .last_observed_receipt
                .as_ref()
                .map(|receipt| receipt.tx_hash),
            Some(tx_hash(0x6c))
        );
        assert_eq!(
            submitted
                .lock()
                .expect("submitted transaction list should lock")
                .len(),
            1
        );
        assert_eq!(
            receipts
                .lock()
                .expect("receipt list should lock")
                .as_slice(),
            &[tx_hash(0x6c)]
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ContractCall].completed,
            1
        );

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let completed = find_event(&events, "workload-completed");
        assert_eq!(completed["workload"], "contract-call");
        assert_eq!(completed["contractCallName"], "events-store-value");
        assert_eq!(completed["contractCallContract"], prefixed_hex(0x88));
        assert_eq!(completed["receiptPresent"], true);
        assert_eq!(completed["receiptStatus"], "ok");
    }

    #[tokio::test]
    async fn contract_call_non_ok_receipt_is_failed_workload() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let completion = execute_contract_call(
            ContractCallClient {
                submitted,
                receipts,
                status: ExecStatus::DeterministicError {
                    kind: "deterministic-error".into(),
                },
            },
            &tick,
            prepared_contract_call(),
        )
        .await
        .expect("contract call should complete");

        assert_eq!(
            completion.error_class.as_deref(),
            Some("contract-call-receipt-status")
        );
        assert!(completion.submitted_transaction.is_some());
        assert!(completion.observed_receipt.is_some());
        match completion.event {
            ResultEvent::WorkloadFailed { event } => {
                assert_eq!(event.workload, WorkloadKind::ContractCall);
                assert_eq!(
                    event.contract_call_name.as_deref(),
                    Some("events-store-value")
                );
                assert_eq!(event.receipt_status.as_deref(), Some("deterministicError"));
                assert_eq!(
                    event.error_class.as_deref(),
                    Some("contract-call-receipt-status")
                );
            }
            other => panic!("expected failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn contract_call_submit_rpc_error_does_not_advance_actor_nonce() {
        let manifest = contract_call_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let call = prepared_contract_call().call;

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
                contract_calls: &[call],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
            },
        )
        .await
        .expect("contract-call submit failure should be recorded");

        assert_eq!(actors[0].next_nonce_sequence, 0);
        assert_eq!(actors[0].last_submitted_tx_hash, None);
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ContractCall].failed,
            1
        );
        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["workload"], "contract-call");
        assert_eq!(failed["errorClass"], "contract-call-submit-rpc-error");
        assert!(failed["txHash"].is_null());
    }

    #[tokio::test]
    async fn contract_call_receipt_rpc_error_advances_nonce_and_records_pending_tx() {
        let manifest = contract_call_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let call = prepared_contract_call().call;

        run_schedule(
            FailingContractCallReceiptClient {
                submitted: submitted.clone(),
                receipts: receipts.clone(),
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
                contract_calls: &[call],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
            },
        )
        .await
        .expect("contract-call receipt failure should be recorded");

        assert_eq!(actors[0].next_nonce_sequence, 1);
        assert_eq!(actors[0].last_submitted_tx_hash, Some(tx_hash(0x6d)));
        assert_eq!(actors[0].pending_receipt_count(), 1);
        assert_eq!(
            receipts
                .lock()
                .expect("receipt list should lock")
                .as_slice(),
            &[tx_hash(0x6d)]
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ContractCall].failed,
            1
        );
        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["workload"], "contract-call");
        assert_eq!(failed["errorClass"], "contract-call-receipt-poll-error");
        assert_eq!(failed["txHash"], prefixed_hex(0x6d));
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
    async fn storage_read_records_absent_item_fields() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let requested = Arc::new(Mutex::new(Vec::new()));
        let completion = execute_storage_read(
            StorageReadClient {
                response: StorageItemRead {
                    block_height: BlockHeight::from(45),
                    item: None,
                },
                requested: requested.clone(),
            },
            &tick,
            prepared_storage_read(Some(false)),
        )
        .await
        .expect("storage read should complete");

        assert_eq!(completion.error_class, None);
        assert_eq!(
            requested
                .lock()
                .expect("storage request list should lock")
                .as_slice(),
            &[(ContractAddress::from([0x88; 32]), vec![0; 32])]
        );
        match completion.event {
            ResultEvent::WorkloadCompleted { event } => {
                assert_eq!(event.workload, WorkloadKind::StorageRead);
                assert_eq!(event.rpc_method, "groundhog_getStorageAt");
                assert_eq!(event.chain_height, Some(45));
                assert_eq!(event.storage_read_name.as_deref(), Some("fib-zero-key"));
                assert_eq!(
                    event.storage_contract.as_deref(),
                    Some(prefixed_hex(0x88).as_str())
                );
                assert_eq!(
                    event.storage_key.as_deref(),
                    Some(prefixed_hex(0x00).as_str())
                );
                assert_eq!(event.storage_item_present, Some(false));
                assert_eq!(event.storage_item_kind, None);
            }
            other => panic!("expected completed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn storage_read_presence_mismatch_is_failed_workload() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let completion = execute_storage_read(
            StorageReadClient {
                response: StorageItemRead {
                    block_height: BlockHeight::from(45),
                    item: None,
                },
                requested: Arc::new(Mutex::new(Vec::new())),
            },
            &tick,
            prepared_storage_read(Some(true)),
        )
        .await
        .expect("storage read should complete");

        assert_eq!(
            completion.error_class.as_deref(),
            Some("storage-presence-mismatch")
        );
        match completion.event {
            ResultEvent::WorkloadFailed { event } => {
                assert_eq!(event.storage_item_present, Some(false));
                assert_eq!(
                    event.error_class.as_deref(),
                    Some("storage-presence-mismatch")
                );
            }
            other => panic!("expected failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_schedule_executes_storage_read_and_records_metrics() {
        let manifest = storage_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let requested = Arc::new(Mutex::new(Vec::new()));
        let storage_read = prepared_storage_read(Some(false)).read;

        run_schedule(
            StorageReadClient {
                response: StorageItemRead {
                    block_height: BlockHeight::from(45),
                    item: None,
                },
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
                contract_calls: &[],
                storage_reads: &[storage_read],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
            },
        )
        .await
        .expect("storage-read schedule should run");

        assert_eq!(
            requested
                .lock()
                .expect("storage request list should lock")
                .len(),
            1
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::StorageRead].completed,
            1
        );
        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let completed = find_event(&events, "workload-completed");
        assert_eq!(completed["workload"], "storage-read");
        assert_eq!(completed["storageReadName"], "fib-zero-key");
        assert_eq!(completed["storageItemPresent"], false);
    }

    #[tokio::test]
    async fn events_query_records_page_fields() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let requested = Arc::new(Mutex::new(Vec::new()));
        let completion = execute_events_query(
            EventsQueryClient {
                response: event_page(Vec::new(), vec![0xab], 45, 1),
                requested: requested.clone(),
            },
            &tick,
            prepared_event_query(Some(0)),
        )
        .await
        .expect("events query should complete");

        assert!(completion.error_class.is_none());
        let requests = requested.lock().expect("event request list should lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].limit, 10);
        assert!(matches!(requests[0].order, IterationOrder::Descending));
        assert!(matches!(requests[0].anchor, BlockAnchor::AtEnd));
        assert_eq!(requests[0].cursor.0, Vec::<u8>::new());
        let filter = requests[0]
            .filters
            .iter()
            .next()
            .expect("request should contain one filter");
        assert_eq!(requests[0].filters.iter().len(), 1);
        assert_eq!(
            filter
                .contract_addresses
                .iter()
                .next()
                .expect("filter should contain one contract address")
                .0,
            ContractAddress::from([0x88; 32])
        );
        drop(requests);

        match completion.event {
            ResultEvent::WorkloadCompleted { event } => {
                assert_eq!(event.workload, WorkloadKind::EventsQuery);
                assert_eq!(event.rpc_method, "groundhog_getEvents");
                assert_eq!(event.event_query_name.as_deref(), Some("fib-events"));
                assert_eq!(
                    event.event_contract.as_deref(),
                    Some(prefixed_hex(0x88).as_str())
                );
                assert_eq!(event.event_count, Some(0));
                assert_eq!(event.event_cursor.as_deref(), Some("0xab"));
                assert_eq!(event.event_latest_block, Some(45));
                assert_eq!(event.event_oldest_block, Some(1));
            }
            other => panic!("expected completed event, got {other:?}"),
        }
    }

    #[test]
    fn event_query_request_preserves_cursor_and_anchor_variants() {
        let mut prepared = prepared_event_query(Some(0)).query;
        prepared.cursor = vec![0x12, 0x34];
        prepared.order = EventQueryOrderSpec::Ascending;
        prepared.anchor = EventQueryAnchorSpec::AtBeginning;

        let request = event_query_request(&prepared);

        assert_eq!(request.cursor.0, vec![0x12, 0x34]);
        assert!(matches!(request.order, IterationOrder::Ascending));
        assert!(matches!(request.anchor, BlockAnchor::AtBeginning));

        prepared.anchor = EventQueryAnchorSpec::AtHeight { height: 77 };
        let request = event_query_request(&prepared);

        assert!(matches!(
            request.anchor,
            BlockAnchor::AtHeight { height } if *height == 77
        ));
    }

    #[tokio::test]
    async fn events_query_records_non_empty_response_cursor() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let requested = Arc::new(Mutex::new(Vec::new()));
        let mut prepared = prepared_event_query(Some(0));
        prepared.query.cursor = vec![0x12, 0x34];
        let completion = execute_events_query(
            EventsQueryClient {
                response: event_page(Vec::new(), vec![0xab, 0xcd], 45, 1),
                requested: requested.clone(),
            },
            &tick,
            prepared,
        )
        .await
        .expect("events query should complete");

        let requests = requested.lock().expect("event request list should lock");
        assert_eq!(requests[0].cursor.0, vec![0x12, 0x34]);
        drop(requests);

        match completion.event {
            ResultEvent::WorkloadCompleted { event } => {
                assert_eq!(event.event_cursor.as_deref(), Some("0xabcd"));
            }
            other => panic!("expected completed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_query_minimum_mismatch_is_failed_workload() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let completion = execute_events_query(
            EventsQueryClient {
                response: event_page(Vec::new(), Vec::new(), 45, 1),
                requested: Arc::new(Mutex::new(Vec::new())),
            },
            &tick,
            prepared_event_query(Some(1)),
        )
        .await
        .expect("events query should complete");

        assert_eq!(
            completion.error_class.as_deref(),
            Some("event-count-below-minimum")
        );
        match completion.event {
            ResultEvent::WorkloadFailed { event } => {
                assert_eq!(event.event_count, Some(0));
                assert_eq!(
                    event.error_class.as_deref(),
                    Some("event-count-below-minimum")
                );
            }
            other => panic!("expected failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_query_rpc_error_is_failed_workload() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 25,
        };
        let requested = Arc::new(Mutex::new(Vec::new()));
        let completion = execute_events_query(
            FailingEventsQueryClient {
                requested: requested.clone(),
            },
            &tick,
            prepared_event_query(Some(0)),
        )
        .await
        .expect("events query should complete as failed workload");

        assert_eq!(
            requested
                .lock()
                .expect("event request list should lock")
                .len(),
            1
        );
        assert_eq!(
            completion.error_class.as_deref(),
            Some("events-query-rpc-error")
        );
        match completion.event {
            ResultEvent::WorkloadFailed { event } => {
                assert_eq!(event.workload, WorkloadKind::EventsQuery);
                assert_eq!(event.event_query_name.as_deref(), Some("fib-events"));
                assert_eq!(event.error_class.as_deref(), Some("events-query-rpc-error"));
                assert!(
                    event
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains("simulated events query failure"))
                );
            }
            other => panic!("expected failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_schedule_executes_events_query_and_records_metrics() {
        let manifest = events_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        let requested = Arc::new(Mutex::new(Vec::new()));
        let event_query = prepared_event_query(Some(0)).query;

        run_schedule(
            EventsQueryClient {
                response: event_page(Vec::new(), Vec::new(), 45, 1),
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[event_query],
                read_after_write: &[],
                resource_sampler: None,
            },
        )
        .await
        .expect("events-query schedule should run");

        assert_eq!(
            requested
                .lock()
                .expect("event query request list should lock")
                .len(),
            1
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::EventsQuery].completed,
            1
        );
        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let completed = find_event(&events, "workload-completed");
        assert_eq!(completed["workload"], "events-query");
        assert_eq!(completed["eventQueryName"], "fib-events");
        assert_eq!(completed["eventCount"], 0);
        assert_eq!(completed["eventCursor"], "0x");
        assert_eq!(completed["eventLatestBlock"], 45);
        assert_eq!(completed["eventOldestBlock"], 1);
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
    async fn native_failed_receipt_is_reconciled_and_fails_the_workload() {
        let file = NamedTempFile::new().expect("event file");
        let mut writer = EventWriter::create(file.path())
            .await
            .expect("event writer");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0].use_native_wallet();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 0, Instant::now())
            .expect("submitted tx");
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 1,
        };
        let times = ReceiptPollTimes {
            started_unix: "1".into(),
            completed_unix: "2".into(),
            rpc_latency: 1,
            submit_to_receipt_latency: Some(1),
        };
        let completion = receipt_poll_present(
            &tick,
            json_string(&tx_hash(0x5a), "txHash").expect("hash"),
            &times,
            receipt_with_status(tx_hash(0x5a), ExecStatus::Reverted { message: None }),
        );
        record_completion(completion, &mut actors, &mut writer, &mut metrics)
            .await
            .expect("reconcile");
        assert_eq!(actors[0].pending_receipt_count(), 0);
        assert_eq!(metrics.failed_count(), 1);
        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events");
        assert!(events.contains("native-receipt-status"));
        assert!(events.contains("workload-failed"));
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
    async fn shutdown_receipt_drain_deadline_cancels_poll_and_preserves_pending() {
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path()).await.unwrap();
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 1, Instant::now())
            .unwrap();
        actors[0]
            .record_submitted_tx(tx_hash(0x5b), 2, Instant::now())
            .unwrap();
        let requested = Arc::new(Mutex::new(Vec::new()));
        let drained = tokio::time::timeout(
            Duration::from_millis(500),
            drain_pending_receipt_polls(
                SlowReceiptClient {
                    requested: requested.clone(),
                },
                &mut actors,
                &mut event_writer,
                &mut metrics,
                30,
                Duration::from_millis(20),
                Duration::ZERO,
            ),
        )
        .await
        .expect("drain must not wait for the one-second RPC")
        .unwrap();
        assert_eq!(drained, 0);
        assert_eq!(*requested.lock().unwrap(), vec![tx_hash(0x5a)]);
        assert_eq!(actors[0].pending_receipt_count(), 2);
        // Cancellation must release the reservation, leaving this hash available
        // for later reconciliation rather than silently losing it.
        assert_eq!(
            actors[0].reserve_receipt_poll_target().unwrap().tx_hash,
            tx_hash(0x5a)
        );
    }

    #[tokio::test]
    async fn shutdown_receipt_drain_zero_budget_issues_no_requests() {
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path()).await.unwrap();
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0]
            .record_submitted_tx(tx_hash(0x5a), 1, Instant::now())
            .unwrap();
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
        .unwrap();
        assert_eq!(drained, 0);
        assert!(requested.lock().unwrap().is_empty());
        assert_eq!(actors[0].pending_receipt_count(), 1);
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
    async fn restart_abort_advances_uncertain_contract_call_nonce() {
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();

        record_aborted_for_restart(
            vec![WorkloadTaskContext {
                actor_id: 0,
                workload: WorkloadKind::ContractCall,
                scheduled_time_ms: 10,
            }],
            &mut actors,
            &mut event_writer,
            &mut metrics,
            20,
        )
        .await
        .expect("restart abort should record");

        assert_eq!(actors[0].next_nonce_sequence, 1);
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ContractCall].failed,
            1
        );
        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["workload"], "contract-call");
        assert_eq!(failed["errorClass"], "target-restarted");
    }

    #[tokio::test]
    async fn restart_abort_invalidates_read_after_write_and_advances_nonce() {
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        actors[0].record_read_after_write_committed(5);

        record_aborted_for_restart(
            vec![WorkloadTaskContext {
                actor_id: 0,
                workload: WorkloadKind::ReadAfterWrite,
                scheduled_time_ms: 10,
            }],
            &mut actors,
            &mut event_writer,
            &mut metrics,
            20,
        )
        .await
        .expect("restart abort should record");

        assert_eq!(actors[0].next_nonce_sequence, 1);
        assert!(actors[0].read_after_write_needs_reconcile());
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ReadAfterWrite].failed,
            1
        );
        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["workload"], "read-after-write");
        assert_eq!(failed["errorClass"], "target-restarted");
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[],
                resource_sampler: None,
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

    fn storage_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-storage-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "storage-read", "weight": 1}],
            "contractArtifacts": [{"name": "fibonacci", "artifact": {"kind": "rv32", "path": "fibonacci.rv32a"}}],
            "storageReads": [{
                "name": "fib-zero-key",
                "contract": "fibonacci",
                "key": "0x0000000000000000000000000000000000000000000000000000000000000000",
                "expectPresent": false
            }]
        }))
        .expect("manifest fixture should parse")
    }

    fn contract_call_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-contract-call-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "contract-call", "weight": 1}],
            "contractArtifacts": [{"name": "events", "artifact": {"kind": "rv32", "path": "events.rv32a"}}],
            "contractCalls": [{
                "name": "events-store-value",
                "contract": "events",
                "entrypoint": "0x0c012d3b21f5676d",
                "calldata": "0x2a64"
            }]
        }))
        .expect("manifest fixture should parse")
    }

    fn events_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-events-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "events-query", "weight": 1}],
            "contractArtifacts": [{"name": "fibonacci", "artifact": {"kind": "rv32", "path": "fibonacci.rv32a"}}],
            "eventQueries": [{
                "name": "fib-events",
                "contract": "fibonacci",
                "order": "descending",
                "anchor": {"kind": "atEnd"},
                "cursor": "0x",
                "limit": 10,
                "expectMinEvents": 0
            }]
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
                network_hash: groundhog_xdr::types::NetworkHash::new(
                    groundhog_xdr::types::SHA256Hash::new([0x5a; 32]),
                ),
            },
            NonceNamespace::new(1).expect("namespace should fit"),
        )]
    }

    fn prepared_view(expect_return_data: Option<Vec<u8>>) -> PreparedViewWorkload {
        PreparedViewWorkload {
            view: PreparedViewCall {
                name: "fib-10".into(),
                contract_name: "fibonacci".into(),
                weight: 1,
                contract: ContractAddress::from([0x88; 32]),
                entrypoint: vec![0x11; 8],
                calldata: vec![0x0a],
                expect_return_data,
            },
            caller: ContractAddress::from([0x66; 32]),
        }
    }

    fn prepared_storage_read(expect_present: Option<bool>) -> PreparedStorageReadWorkload {
        PreparedStorageReadWorkload {
            read: PreparedStorageRead {
                name: "fib-zero-key".into(),
                contract_name: "fibonacci".into(),
                weight: 1,
                contract: ContractAddress::from([0x88; 32]),
                key: vec![0; 32],
                expect_present,
            },
        }
    }

    fn prepared_contract_call() -> PreparedContractCallWorkload {
        PreparedContractCallWorkload {
            call: PreparedContractCall {
                name: "events-store-value".into(),
                contract_name: "events".into(),
                weight: 1,
                contract: ContractAddress::from([0x88; 32]),
                entrypoint: vec![0x0c, 0x01, 0x2d, 0x3b, 0x21, 0xf5, 0x67, 0x6d],
                calldata: vec![0x2a, 0x64],
            },
            tx: BuiltSubmitTransaction {
                tx_hex: "0xsigned".into(),
                nonce: 4_294_967_296,
            },
        }
    }

    fn prepared_event_query(expect_min_events: Option<u64>) -> PreparedEventQueryWorkload {
        PreparedEventQueryWorkload {
            query: PreparedEventQuery {
                name: "fib-events".into(),
                contract_name: "fibonacci".into(),
                weight: 1,
                contract: ContractAddress::from([0x88; 32]),
                order: EventQueryOrderSpec::Descending,
                anchor: EventQueryAnchorSpec::AtEnd,
                cursor: Vec::new(),
                limit: 10,
                expect_min_events,
            },
        }
    }

    fn event_page(
        events: Vec<groundhog_json_rpc_types::events::IdentifiedEvent>,
        cursor: Vec<u8>,
        latest_block: i64,
        oldest_block: i64,
    ) -> EventPage {
        EventPage {
            events,
            cursor: PaginationCursor(cursor),
            latest_block: BlockHeight::from(latest_block),
            oldest_block: BlockHeight::from(oldest_block),
        }
    }

    fn runtime_manifest() -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 2,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test00000000000",
            "producer": {
                "binary": "hog-devnode",
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
                    "artifactHint": "./dev_wallet.rv32a",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "createWalletEntrypoint": "0x6372656174655761",
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
            gas_used: 1_500,
        }
    }

    #[derive(Clone)]
    struct ReadAfterWriteClient {
        reads: ReadAfterWriteReadQueue,
        submit: Result<TxHash, String>,
        receipt: Result<Option<TransactionReceipt>, String>,
        view_calls: Arc<Mutex<u32>>,
    }

    type ReadAfterWriteReadQueue =
        Arc<Mutex<std::collections::VecDeque<Result<ViewResponse, String>>>>;

    impl SimRpcClient for ReadAfterWriteClient {
        #[allow(clippy::manual_async_fn)]
        fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, String>> + Send {
            async { Err("chain tip should not be called".into()) }
        }

        #[allow(clippy::manual_async_fn)]
        fn send_raw_transaction(
            &self,
            _tx_hex: String,
        ) -> impl Future<Output = Result<TxHash, String>> + Send {
            let submit = self.submit.clone();
            async move { submit }
        }

        #[allow(clippy::manual_async_fn)]
        fn transaction_receipt(
            &self,
            _tx_hash: TxHash,
        ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
            let receipt = self.receipt.clone();
            async move { receipt }
        }

        #[allow(clippy::manual_async_fn)]
        fn view(
            &self,
            _request: ViewRequest,
        ) -> impl Future<Output = Result<ViewResponse, String>> + Send {
            let reads = self.reads.clone();
            let view_calls = self.view_calls.clone();
            async move {
                *view_calls.lock().expect("view counter should lock") += 1;
                reads
                    .lock()
                    .expect("read queue should lock")
                    .pop_front()
                    .expect("unexpected read-after-write view call")
            }
        }
    }

    fn read_after_write_view_response(value: u64) -> ViewResponse {
        read_after_write_view_response_with_output(RootInvocationOutput::Return {
            return_data: HexBytes(xdrs::cxs::encode(&value).expect("u64 should encode")),
        })
    }

    fn read_after_write_view_response_with_output(output: RootInvocationOutput) -> ViewResponse {
        ViewResponse {
            output,
            events: Vec::new(),
            height: BlockHeight::from(77),
        }
    }

    fn read_after_write_reads(values: Vec<Result<u64, String>>) -> ReadAfterWriteReadQueue {
        read_after_write_response_reads(
            values
                .into_iter()
                .map(|read| read.map(read_after_write_view_response))
                .collect(),
        )
    }

    fn read_after_write_response_reads(
        values: Vec<Result<ViewResponse, String>>,
    ) -> ReadAfterWriteReadQueue {
        Arc::new(Mutex::new(values.into_iter().collect()))
    }

    fn read_after_write_manifest() -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "executor-read-after-write-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1ms", "users": 1, "ratePerUserHz": 1000.0}],
            "workloads": [{"name": "read-after-write", "weight": 1}],
            "contractArtifacts": [{"name": "caller-counter", "artifact": {"kind": "rv32", "path": "caller-counter.rv32a"}}],
            "readAfterWrite": [{
                "name": "counter-roundtrip",
                "contract": "caller-counter",
                "write": {"entrypoint": "0x4f4f4f4f4f4f4f4f"},
                "read": {"entrypoint": "0xdfdfdfdfdfdfdfdf"}
            }]
        }))
        .expect("manifest fixture should parse")
    }

    fn prepared_read_after_write() -> PreparedReadAfterWrite {
        PreparedReadAfterWrite {
            name: "counter-roundtrip".into(),
            contract_name: "caller-counter".into(),
            weight: 1,
            contract: ContractAddress::from([0x88; 32]),
            write_entrypoint: vec![0x4f; 8],
            write_calldata: Vec::new(),
            read_entrypoint: vec![0xdf; 8],
            read_calldata: Vec::new(),
        }
    }

    async fn run_read_after_write(
        client: ReadAfterWriteClient,
        setup: impl FnOnce(&mut ActorState),
    ) -> (Vec<ActorState>, RunMetricsBuilder, String) {
        let manifest = read_after_write_manifest();
        let schedule = generate_schedule(&manifest).expect("schedule should generate");
        let starts = phase_starts(&manifest).expect("phase starts should generate");
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut metrics = RunMetricsBuilder::default();
        let mut actors = actor_states();
        setup(&mut actors[0]);
        let entry = prepared_read_after_write();

        run_schedule(
            client,
            ScheduleExecution {
                manifest: &manifest,
                runtime_manifest: &runtime_manifest(),
                schedule: &schedule,
                phase_starts: &starts,
                actor_states: &mut actors,
                event_writer: &mut event_writer,
                metrics: &mut metrics,
                view_calls: &[],
                contract_calls: &[],
                storage_reads: &[],
                event_queries: &[],
                read_after_write: &[entry],
                resource_sampler: None,
            },
        )
        .await
        .expect("read-after-write schedule should run");

        let events = tokio::fs::read_to_string(file.path())
            .await
            .expect("events should read");
        (actors, metrics, events)
    }

    fn completed_read_after_write_update(value: u64) -> WorkloadCompletion {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 0,
        };
        let prepared = PreparedReadAfterWriteWorkload {
            entry: prepared_read_after_write(),
            tx: BuiltSubmitTransaction {
                tx_hex: "0xsigned".into(),
                nonce: 4_294_967_296,
            },
            caller: ContractAddress::from([0x99; 32]),
            state: ReadAfterWriteState::NeedsReconcile,
        };
        let mut event = read_after_write_base_event(
            &tick,
            &prepared,
            "0".into(),
            "0".into(),
            0,
            "groundhog_view",
            Some("4294967296".into()),
            Some(value),
            true,
        );
        event.tx_hash = Some("0x5a".into());
        WorkloadCompletion {
            workload: WorkloadKind::ReadAfterWrite,
            latency_ms: 0,
            error_class: None,
            chain_tip: None,
            event: ResultEvent::WorkloadCompleted { event },
            skipped: false,
            submitted_transaction: Some(SubmittedTransaction {
                actor_id: 0,
                tx_hash: tx_hash(0x5a),
                scheduled_time_ms: 0,
                completed_at: Instant::now(),
            }),
            observed_receipt: None,
            receipt_poll: None,
            submit_to_receipt_latency_ms: None,
            read_after_write_transition: Some(ReadAfterWriteUpdate {
                actor_id: 0,
                transition: ReadAfterWriteTransition::SetKnown(value),
            }),
            release_receipt_poll: None,
        }
    }

    #[tokio::test]
    async fn drain_after_permit_reacquire_updates_actor_before_prepare() {
        let mut tasks = JoinSet::new();
        let mut task_contexts = HashMap::new();
        let global = Arc::new(Semaphore::new(1));
        let actor_lanes = ActorInFlightLanes {
            write: Arc::new(Semaphore::new(1)),
            receipt: Arc::new(Semaphore::new(DEFAULT_PER_ACTOR_RECEIPT_IN_FLIGHT_LIMIT)),
            read: Arc::new(Semaphore::new(DEFAULT_PER_ACTOR_READ_IN_FLIGHT_LIMIT)),
        };
        let first_permits = WorkloadPermits {
            _global: global
                .clone()
                .try_acquire_owned()
                .expect("global permit should acquire"),
            _actor: actor_lanes
                .write
                .clone()
                .try_acquire_owned()
                .expect("actor permit should acquire"),
        };
        let abort = tasks.spawn(async move {
            let _permits = first_permits;
            Ok(completed_read_after_write_update(1))
        });
        task_contexts.insert(
            abort.id(),
            WorkloadTaskContext {
                actor_id: 0,
                workload: WorkloadKind::ReadAfterWrite,
                scheduled_time_ms: 0,
            },
        );

        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 1,
        };
        let _second_permits = loop {
            match acquire_workload_permits(
                &tick,
                WorkloadKind::ReadAfterWrite,
                global.clone(),
                &actor_lanes,
            )
            .expect("permit acquisition should not error")
            {
                Ok(permits) => break permits,
                Err(_) => tokio::task::yield_now().await,
            }
        };

        let mut actors = actor_states();
        let mut metrics = RunMetricsBuilder::default();
        let file = NamedTempFile::new().expect("temp file should be created");
        let mut event_writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        let mut in_flight = 1;
        assert!(actors[0].read_after_write_needs_reconcile());

        drain_completed_workloads_and_update_in_flight(
            &mut tasks,
            &mut task_contexts,
            &mut actors,
            &mut event_writer,
            &mut metrics,
            &mut in_flight,
        )
        .await
        .expect("completed task should drain");

        assert_eq!(in_flight, 0);
        assert_eq!(
            actors[0].read_after_write_state(),
            ReadAfterWriteState::Known(1)
        );
        assert_eq!(actors[0].next_nonce_sequence, 1);
        let expected_nonce = actors[0].current_nonce().expect("nonce should build");
        let runtime = runtime_manifest();
        let manifest = read_after_write_manifest();
        let prepared = prepare_workload(
            &manifest,
            &tick,
            &mut actors[0],
            WorkloadKind::ReadAfterWrite,
            runtime_dev_wallet_action_entrypoint(&runtime).expect("wallet entrypoint should parse"),
            &[],
            &[],
            &[],
            &[],
            &[prepared_read_after_write()],
        )
        .expect("read-after-write should prepare");
        match prepared {
            PreparedWorkload::ReadAfterWrite(workload) => {
                assert_eq!(workload.state, ReadAfterWriteState::Known(1));
                assert_eq!(workload.tx.nonce, expected_nonce);
            }
            _ => panic!("expected read-after-write workload"),
        }
    }

    #[test]
    fn actor_in_flight_lanes_isolate_independent_workloads() {
        let tick = ScheduledActorTick {
            phase_index: 0,
            actor_id: 0,
            actor_sequence: 0,
            simulation_time_ms: 0,
        };
        let global = Arc::new(Semaphore::new(8));
        let actor_lanes = ActorInFlightLanes {
            write: Arc::new(Semaphore::new(1)),
            receipt: Arc::new(Semaphore::new(1)),
            read: Arc::new(Semaphore::new(1)),
        };

        let Ok(_write) = acquire_workload_permits(
            &tick,
            WorkloadKind::ContractCall,
            global.clone(),
            &actor_lanes,
        )
        .expect("permit acquisition should not error") else {
            panic!("first write should acquire");
        };
        let Ok(_read) = acquire_workload_permits(
            &tick,
            WorkloadKind::EventsQuery,
            global.clone(),
            &actor_lanes,
        )
        .expect("permit acquisition should not error") else {
            panic!("read lane should acquire while write lane is busy");
        };
        let Ok(_receipt) = acquire_workload_permits(
            &tick,
            WorkloadKind::ReceiptPoll,
            global.clone(),
            &actor_lanes,
        )
        .expect("permit acquisition should not error") else {
            panic!("receipt lane should acquire while write lane is busy");
        };

        let Err(skipped) =
            acquire_workload_permits(&tick, WorkloadKind::WalletSubmitSmoke, global, &actor_lanes)
                .expect("permit acquisition should not error")
        else {
            panic!("second write should hit the write lane limit");
        };
        let event = workload_event(&skipped.event).expect("skipped event should be workload event");
        assert_eq!(event.error_class.as_deref(), Some("simulator-backpressure"));
        assert!(
            event
                .error
                .as_deref()
                .is_some_and(|error| error.contains("actor-write in-flight limit reached"))
        );
    }

    #[tokio::test]
    async fn read_after_write_round_trip_matches_and_advances_actor() {
        let view_calls = Arc::new(Mutex::new(0));
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Ok(0), Ok(1)]),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt(tx_hash(0x6c)))),
            view_calls: view_calls.clone(),
        };

        let (actors, metrics, events) = run_read_after_write(client, |_| {}).await;

        // Reconcile pre-read (0) -> write -> read-back (1) == expected (1).
        assert_eq!(*view_calls.lock().expect("view counter"), 2);
        assert_eq!(
            actors[0].read_after_write_state(),
            ReadAfterWriteState::Known(1)
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ReadAfterWrite].completed,
            1
        );
        let completed = find_event(&events, "workload-completed");
        assert_eq!(completed["workload"], "read-after-write");
        assert_eq!(completed["readAfterWriteName"], "counter-roundtrip");
        assert_eq!(completed["readAfterWriteContract"], "caller-counter");
        assert_eq!(completed["readAfterWriteExpected"], "1");
        assert_eq!(completed["readAfterWriteObserved"], "1");
        assert_eq!(completed["readAfterWriteMatched"], true);
        assert_eq!(completed["readAfterWriteRebased"], true);
        assert_eq!(completed["rpcMethod"], "groundhog_view");
        assert_eq!(completed["receiptStatus"], "ok");
    }

    #[tokio::test]
    async fn read_after_write_retries_stale_post_commit_read() {
        let view_calls = Arc::new(Mutex::new(0));
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Ok(5), Ok(6)]),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt(tx_hash(0x6c)))),
            view_calls: view_calls.clone(),
        };

        let (actors, metrics, events) = run_read_after_write(client, |actor| {
            actor.record_read_after_write_committed(5);
        })
        .await;

        assert_eq!(*view_calls.lock().expect("view counter"), 2);
        assert_eq!(
            actors[0].read_after_write_state(),
            ReadAfterWriteState::Known(6)
        );
        assert_eq!(
            metrics.workload_counts()[&WorkloadKind::ReadAfterWrite].completed,
            1
        );
        let completed = find_event(&events, "workload-completed");
        assert_eq!(completed["readAfterWriteExpected"], "6");
        assert_eq!(completed["readAfterWriteObserved"], "6");
        assert_eq!(completed["readAfterWriteMatched"], true);
    }

    #[tokio::test]
    async fn read_after_write_known_state_skips_pre_read() {
        let view_calls = Arc::new(Mutex::new(0));
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Ok(6)]),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt(tx_hash(0x6c)))),
            view_calls: view_calls.clone(),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |actor| {
            actor.record_read_after_write_committed(5);
        })
        .await;

        // Known state needs no reconcile pre-read: one view call (the read-back).
        assert_eq!(*view_calls.lock().expect("view counter"), 1);
        assert_eq!(
            actors[0].read_after_write_state(),
            ReadAfterWriteState::Known(6)
        );
        let completed = find_event(&events, "workload-completed");
        assert_eq!(completed["readAfterWriteExpected"], "6");
        assert_eq!(completed["readAfterWriteMatched"], true);
        assert_eq!(completed["readAfterWriteRebased"], false);
    }

    #[tokio::test]
    async fn read_after_write_reconcile_read_failure_is_not_rebased() {
        let view_calls = Arc::new(Mutex::new(0));
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Err("read boom".into())]),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt(tx_hash(0x6c)))),
            view_calls: view_calls.clone(),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |_| {}).await;

        assert_eq!(*view_calls.lock().expect("view counter"), 1);
        assert!(actors[0].read_after_write_needs_reconcile());
        assert_eq!(actors[0].next_nonce_sequence, 0);
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "view-rpc-error");
        assert_eq!(failed["rpcMethod"], "groundhog_view");
        assert_eq!(failed["readAfterWriteRebased"], false);
        assert!(failed.get("readAfterWriteExpected").is_none());
    }

    #[tokio::test]
    async fn read_after_write_mismatch_invalidates_actor() {
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Ok(9)]),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt(tx_hash(0x6c)))),
            view_calls: Arc::new(Mutex::new(0)),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |actor| {
            actor.record_read_after_write_committed(5);
        })
        .await;

        assert!(actors[0].read_after_write_needs_reconcile());
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "read-after-write-mismatch");
        assert_eq!(failed["rpcMethod"], "groundhog_view");
        assert_eq!(failed["readAfterWriteExpected"], "6");
        assert_eq!(failed["readAfterWriteObserved"], "9");
        assert_eq!(failed["readAfterWriteMatched"], false);
    }

    #[tokio::test]
    async fn read_after_write_keeps_known_on_post_commit_read_failure() {
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Err("read boom".into())]),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt(tx_hash(0x6c)))),
            view_calls: Arc::new(Mutex::new(0)),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |actor| {
            actor.record_read_after_write_committed(5);
        })
        .await;

        // Write committed; only the read-back RPC failed, so the actor model
        // must still advance to the provable committed value.
        assert_eq!(
            actors[0].read_after_write_state(),
            ReadAfterWriteState::Known(6)
        );
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "view-rpc-error");
        assert_eq!(failed["rpcMethod"], "groundhog_view");
        assert_eq!(failed["readAfterWriteExpected"], "6");
        assert!(failed.get("readAfterWriteObserved").is_none());
        assert!(failed.get("readAfterWriteMatched").is_none());
    }

    #[tokio::test]
    async fn read_after_write_keeps_known_on_post_commit_view_output_failures() {
        let cases = vec![
            (
                "read-decode-error",
                read_after_write_view_response_with_output(RootInvocationOutput::Return {
                    return_data: HexBytes(vec![0xff]),
                }),
            ),
            (
                "view-aborted",
                read_after_write_view_response_with_output(RootInvocationOutput::Abort {
                    abort_data: HexBytes(vec![0x01]),
                }),
            ),
            (
                "view-fault",
                read_after_write_view_response_with_output(RootInvocationOutput::Fault),
            ),
        ];

        for (error_class, response) in cases {
            let client = ReadAfterWriteClient {
                reads: read_after_write_response_reads(vec![Ok(response)]),
                submit: Ok(tx_hash(0x6c)),
                receipt: Ok(Some(receipt(tx_hash(0x6c)))),
                view_calls: Arc::new(Mutex::new(0)),
            };

            let (actors, _metrics, events) = run_read_after_write(client, |actor| {
                actor.record_read_after_write_committed(5);
            })
            .await;

            assert_eq!(
                actors[0].read_after_write_state(),
                ReadAfterWriteState::Known(6)
            );
            let failed = find_event(&events, "workload-failed");
            assert_eq!(failed["errorClass"], error_class);
            assert_eq!(failed["rpcMethod"], "groundhog_view");
            assert_eq!(failed["readAfterWriteExpected"], "6");
            assert!(failed.get("readAfterWriteObserved").is_none());
            assert!(failed.get("readAfterWriteMatched").is_none());
        }
    }

    #[tokio::test]
    async fn read_after_write_known_overflow_fails_before_submit() {
        let view_calls = Arc::new(Mutex::new(0));
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(Vec::new()),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt(tx_hash(0x6c)))),
            view_calls: view_calls.clone(),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |actor| {
            actor.record_read_after_write_committed(u64::MAX);
        })
        .await;

        assert_eq!(*view_calls.lock().expect("view counter"), 0);
        assert_eq!(
            actors[0].read_after_write_state(),
            ReadAfterWriteState::Known(u64::MAX)
        );
        assert_eq!(actors[0].next_nonce_sequence, 0);
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "read-after-write-overflow");
        assert_eq!(failed["rpcMethod"], "groundhog_sendRawTransaction");
        assert!(failed.get("readAfterWriteExpected").is_none());
    }

    #[tokio::test]
    async fn read_after_write_reconcile_overflow_fails_before_submit() {
        let view_calls = Arc::new(Mutex::new(0));
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Ok(u64::MAX)]),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt(tx_hash(0x6c)))),
            view_calls: view_calls.clone(),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |_| {}).await;

        assert_eq!(*view_calls.lock().expect("view counter"), 1);
        assert!(actors[0].read_after_write_needs_reconcile());
        assert_eq!(actors[0].next_nonce_sequence, 0);
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "read-after-write-overflow");
        assert_eq!(failed["rpcMethod"], "groundhog_view");
        assert_eq!(failed["readAfterWriteRebased"], true);
        assert!(failed.get("readAfterWriteExpected").is_none());
    }

    #[tokio::test]
    async fn read_after_write_submit_failure_leaves_state_unchanged() {
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Ok(3)]),
            submit: Err("submit boom".into()),
            receipt: Ok(None),
            view_calls: Arc::new(Mutex::new(0)),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |_| {}).await;

        // Submit never landed: nonce not advanced, state stays NeedsReconcile.
        assert!(actors[0].read_after_write_needs_reconcile());
        assert_eq!(actors[0].next_nonce_sequence, 0);
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "rpc-error");
        assert_eq!(failed["rpcMethod"], "groundhog_sendRawTransaction");
    }

    #[tokio::test]
    async fn read_after_write_receipt_rpc_failure_invalidates_actor() {
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(vec![Ok(3)]),
            submit: Ok(tx_hash(0x6c)),
            receipt: Err("receipt boom".into()),
            view_calls: Arc::new(Mutex::new(0)),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |_| {}).await;

        assert!(actors[0].read_after_write_needs_reconcile());
        assert_eq!(actors[0].next_nonce_sequence, 1);
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "rpc-error");
        assert_eq!(failed["rpcMethod"], "groundhog_getTransactionReceipt");
        assert_eq!(failed["readAfterWriteExpected"], "4");
    }

    #[tokio::test]
    async fn read_after_write_non_ok_receipt_does_not_advance() {
        let client = ReadAfterWriteClient {
            reads: read_after_write_reads(Vec::new()),
            submit: Ok(tx_hash(0x6c)),
            receipt: Ok(Some(receipt_with_status(
                tx_hash(0x6c),
                ExecStatus::Reverted { message: None },
            ))),
            view_calls: Arc::new(Mutex::new(0)),
        };

        let (actors, _metrics, events) = run_read_after_write(client, |actor| {
            actor.record_read_after_write_committed(5);
        })
        .await;

        // Non-ok write did not take effect: belief unchanged at Known(5).
        assert_eq!(
            actors[0].read_after_write_state(),
            ReadAfterWriteState::Known(5)
        );
        let failed = find_event(&events, "workload-failed");
        assert_eq!(failed["errorClass"], "contract-call-receipt-status");
        assert_eq!(failed["rpcMethod"], "groundhog_getTransactionReceipt");
    }
}
