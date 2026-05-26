use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use groundhog_rpc_api_types::{
    block::ChainTip,
    receipt::{ExecStatus, TransactionReceipt},
    runtime_manifest::RuntimeManifest,
    transaction::TxHash,
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
    /// Return the committed receipt for a transaction hash, or `None` when it is not indexed yet.
    fn transaction_receipt(
        &self,
        _tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, String>> + Send {
        async { Err("transaction receipt RPC is not implemented by this client".into()) }
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
        WorkloadKind::ReceiptPoll => Ok(PreparedWorkload::ReceiptPoll(PreparedReceiptPoll {
            tx_hash: actor.last_submitted_tx_hash,
            submit_completed_at: actor.last_submitted_completed_at,
        })),
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
            PreparedWorkload::ReceiptPoll(poll) => execute_receipt_poll(client, &tick, poll).await,
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
                        receipt_present: None,
                        receipt_status: None,
                        receipt_status_detail: None,
                        submit_to_receipt_latency_ms: None,
                        error_class: None,
                        error: None,
                    },
                },
                skipped: false,
                submitted_transaction: None,
                observed_receipt: None,
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
                    error_class: Some("rpc-error".into()),
                    error: Some(error),
                },
            },
            skipped: false,
            submitted_transaction: None,
            observed_receipt: None,
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
                        error_class: None,
                        error: None,
                    },
                },
                skipped: false,
                observed_receipt: None,
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
                    error_class: Some("rpc-error".into()),
                    error: Some(error),
                },
            },
            skipped: false,
            observed_receipt: None,
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
    let Some(tx_hash) = poll.tx_hash else {
        return receipt_poll_no_submitted_tx(tick, started_at_unix_ms);
    };

    let response = client.transaction_receipt(tx_hash).await;
    let completed = Instant::now();
    let times = ReceiptPollTimes {
        started_unix: started_at_unix_ms,
        completed_unix: unix_ms()?.to_string(),
        rpc_latency: duration_ms(completed.duration_since(started)),
        submit_to_receipt_latency: poll
            .submit_completed_at
            .map(|submitted| duration_ms(completed.saturating_duration_since(submitted))),
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

struct WorkloadPermits {
    _global: OwnedSemaphorePermit,
    _actor: OwnedSemaphorePermit,
}

enum PreparedWorkload {
    ChainTip,
    WalletSubmitSmoke(BuiltSubmitTransaction),
    ReceiptPoll(PreparedReceiptPoll),
}

#[derive(Debug, Clone, Copy)]
struct PreparedReceiptPoll {
    tx_hash: Option<TxHash>,
    submit_completed_at: Option<Instant>,
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

    #[derive(Clone)]
    struct ReceiptClient {
        receipt: Option<TransactionReceipt>,
        requested: Arc<Mutex<Vec<TxHash>>>,
    }

    #[derive(Clone)]
    struct FailingReceiptClient {
        requested: Arc<Mutex<Vec<TxHash>>>,
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

    fn find_event(events: &str, kind: &str) -> serde_json::Value {
        events
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).expect("event should parse")
            })
            .find(|event| event["kind"] == kind)
            .unwrap_or_else(|| panic!("expected {kind} event"))
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
