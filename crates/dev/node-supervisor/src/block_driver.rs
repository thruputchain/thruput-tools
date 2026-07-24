//! Local hogd-dev block production driver.
//!
//! Production deployments rely on an external coordinator to ask the scheduler
//! to nominate and commit blocks. `groundhog node` deliberately avoids running
//! chuckd for the localhost topology, so the supervisor owns that small piece
//! of coordination policy for hogd-dev.

use std::{fmt, sync::Arc, time::Duration};

use groundhog_scheduler::{
    BlockID, DbTip, SchedulerCommitOutcome, SchedulerGetTipResult, SchedulerNominatedBlock,
    SchedulerNominationResult, sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use tokio::{
    sync::{Notify, mpsc, oneshot},
    task::JoinHandle,
    time::MissedTickBehavior,
};

use crate::config::{BlockDriverConfig, BlockDriverMode};

const MANUAL_BLOCK_REQUEST_CAPACITY: usize = 1;

/// Metadata returned after the scheduler commits a manually-produced block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProducedBlock {
    /// Scheduler-local identifier used to commit the nominated block.
    pub block_id: BlockID,
    /// Height reported by the committed nomination.
    pub height: i64,
    /// Encoded transaction-batch size reported by the nomination.
    pub batch_xdr_bytes: usize,
}

/// Errors returned while manually producing a local block.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockProductionError {
    /// The block driver stopped before it could acknowledge the request.
    #[error("manual block producer has stopped")]
    DriverStopped,
    /// Shutdown interrupted a request after scheduler work may have begun.
    #[error("manual block production was interrupted; scheduler outcome may be unknown")]
    OutcomeUnknown,
    /// The scheduler did not complete a request before its deadline.
    #[error("manual block production did not complete within {timeout:?}; outcome may be unknown")]
    TimedOut {
        /// Configured request deadline.
        timeout: Duration,
    },
    /// The scheduler had no genesis block to build on.
    #[error("scheduler has no genesis block")]
    NoGenesis,
    /// A scheduler RPC failed.
    #[error("scheduler RPC `{op}` failed: {source}")]
    SchedulerRpc {
        /// Scheduler operation that failed.
        op: &'static str,
        /// Underlying x23 RPC error.
        #[source]
        source: rpc_driver::RpcError,
    },
    /// Nomination RPC failed after the scheduler may have retained a candidate.
    #[error("nomination outcome is unknown after RPC failure: {source}")]
    NominationOutcomeUnknown {
        /// Failed nomination RPC.
        #[source]
        source: rpc_driver::RpcError,
    },
    /// The scheduler rejected nomination or commit.
    #[error("scheduler rejected `{op}`: {result}")]
    SchedulerRejected {
        /// Scheduler operation that was rejected.
        op: &'static str,
        /// Scheduler result value.
        result: String,
    },
    /// The scheduler returned an unexpected tip result.
    #[error("unexpected scheduler tip result: {result}")]
    UnexpectedTipResult {
        /// Debug representation of the unexpected result.
        result: String,
    },
    /// Commit transport failed and the current tip did not confirm the nomination.
    #[error(
        "commit outcome for block {block_id} at height {height} is unknown after RPC failure: {source}; reconciliation: {reconciliation}"
    )]
    CommitOutcomeUnknown {
        /// Scheduler-local nominated block identifier.
        block_id: BlockID,
        /// Nominated block height.
        height: i64,
        /// Failed commit RPC.
        #[source]
        source: rpc_driver::RpcError,
        /// Result of the best-effort tip reconciliation.
        reconciliation: String,
    },
}

/// Cloneable in-process control for producing blocks in manual mode.
#[derive(Debug, Clone)]
pub struct ManualBlockProducer {
    requests: mpsc::Sender<ManualBlockRequest>,
}

impl ManualBlockProducer {
    /// Run one scheduler nomination/commit cycle and await its outcome.
    ///
    /// Dropping the returned future before the driver starts the request cancels
    /// it. Once scheduler work begins, dropping the future does not cancel the
    /// cycle. [`BlockProductionError::TimedOut`],
    /// [`BlockProductionError::OutcomeUnknown`],
    /// [`BlockProductionError::NominationOutcomeUnknown`], and
    /// [`BlockProductionError::CommitOutcomeUnknown`] mean callers must inspect
    /// chain state before deciding whether to request another block.
    /// The producer becomes terminal after an indeterminate result so queued
    /// requests cannot overlap scheduler work that may still be running.
    ///
    /// The current scheduler protocol reports an empty or otherwise
    /// non-nominatable mempool as [`BlockProductionError::SchedulerRejected`];
    /// it does not expose a distinct empty-mempool outcome.
    pub async fn produce_block(&self) -> Result<ProducedBlock, BlockProductionError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(ManualBlockRequest { reply })
            .await
            .map_err(|_| BlockProductionError::DriverStopped)?;
        response
            .await
            .unwrap_or(Err(BlockProductionError::DriverStopped))
    }
}

#[derive(Debug)]
struct ManualBlockRequest {
    reply: oneshot::Sender<Result<ProducedBlock, BlockProductionError>>,
}

/// Cheap wake handle held by submit-plane adapters.
#[derive(Clone)]
pub(crate) struct BlockDriverTrigger {
    notify: Option<Arc<Notify>>,
}

impl fmt::Debug for BlockDriverTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockDriverTrigger")
            .field("enabled", &self.notify.is_some())
            .finish_non_exhaustive()
    }
}

impl BlockDriverTrigger {
    fn new(notify: Arc<Notify>) -> Self {
        Self {
            notify: Some(notify),
        }
    }

    fn noop() -> Self {
        Self { notify: None }
    }

    /// Wake the block driver after the scheduler submit RPC succeeds.
    pub(crate) fn trigger_after_submit(&self) {
        if let Some(notify) = &self.notify {
            notify.notify_one();
        }
    }
}

/// Background automine task for the hogd-dev scheduler.
#[derive(Debug)]
pub(crate) struct BlockDriverTask {
    trigger: BlockDriverTrigger,
    manual_producer: Option<ManualBlockProducer>,
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl BlockDriverTask {
    /// Start a coalescing block driver for the supplied scheduler client.
    pub(crate) fn start(
        scheduler: SchedulerCoord_SchedulerCoord1_Client,
        config: BlockDriverConfig,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (trigger, manual_producer, join) = match config.mode {
            BlockDriverMode::Instant => {
                let notify = Arc::new(Notify::new());
                let trigger = BlockDriverTrigger::new(Arc::clone(&notify));
                let join = tokio::spawn(run_instant_driver(scheduler, notify, shutdown_rx));
                (trigger, None, join)
            }
            BlockDriverMode::Interval => {
                let join = tokio::spawn(run_interval_driver(
                    scheduler,
                    config.effective_interval(),
                    shutdown_rx,
                ));
                (BlockDriverTrigger::noop(), None, join)
            }
            BlockDriverMode::Manual => {
                let (requests, receiver) = mpsc::channel(MANUAL_BLOCK_REQUEST_CAPACITY);
                let manual_producer = ManualBlockProducer { requests };
                let join = tokio::spawn(run_manual_driver(
                    scheduler,
                    receiver,
                    config.effective_manual_request_timeout(),
                    shutdown_rx,
                ));
                (BlockDriverTrigger::noop(), Some(manual_producer), join)
            }
        };
        Self {
            trigger,
            manual_producer,
            shutdown: Some(shutdown_tx),
            join,
        }
    }

    /// Wake handle for scheduler submit adapters.
    pub(crate) fn trigger(&self) -> BlockDriverTrigger {
        self.trigger.clone()
    }

    /// Manual producer when the task was started in manual mode.
    pub(crate) fn manual_producer(&self) -> Option<ManualBlockProducer> {
        self.manual_producer.clone()
    }

    /// Stop the driver task and wait for it to exit.
    pub(crate) async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Err(error) = self.join.await {
            tracing::warn!(%error, "hogd-dev block driver task failed to join");
        }
    }
}

async fn run_manual_driver(
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
    mut requests: mpsc::Receiver<ManualBlockRequest>,
    request_timeout: Duration,
    mut shutdown: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                tracing::debug!("hogd-dev manual block driver stopping");
                return;
            }
            request = requests.recv() => {
                let Some(request) = request else {
                    return;
                };
                if request.reply.is_closed() {
                    continue;
                }
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        let _ = request.reply.send(Err(BlockProductionError::OutcomeUnknown));
                        tracing::debug!("hogd-dev manual block driver stopping");
                        return;
                    }
                    result = tokio::time::timeout(request_timeout, drive_one_block(&scheduler)) => {
                        let result = match result {
                            Ok(result) => manual_result(result),
                            Err(_) => Err(BlockProductionError::TimedOut {
                                timeout: request_timeout,
                            }),
                        };
                        if let Err(error) = &result {
                            tracing::warn!(%error, "manual hogd-dev block production failed");
                        }
                        let terminal = matches!(
                            &result,
                            Err(
                                BlockProductionError::TimedOut { .. }
                                    | BlockProductionError::NominationOutcomeUnknown { .. }
                                    | BlockProductionError::CommitOutcomeUnknown { .. }
                            )
                        );
                        let _ = request.reply.send(result);
                        if terminal {
                            tracing::warn!(
                                "manual block producer stopping after indeterminate outcome"
                            );
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn manual_result(
    result: Result<DriveOutcome, BlockProductionError>,
) -> Result<ProducedBlock, BlockProductionError> {
    match result {
        Ok(DriveOutcome::Committed {
            block_id,
            height,
            batch_xdr_bytes,
        }) => Ok(ProducedBlock {
            block_id,
            height,
            batch_xdr_bytes,
        }),
        Ok(DriveOutcome::NoGenesis) => Err(BlockProductionError::NoGenesis),
        Err(error) => Err(error),
    }
}

async fn run_instant_driver(
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
    notify: Arc<Notify>,
    mut shutdown: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            () = notify.notified() => {
                tokio::select! {
                    result = drive_one_block(&scheduler) => {
                        log_drive_result(result);
                    }
                    _ = &mut shutdown => {
                        tracing::debug!("hogd-dev block driver stopping");
                        return;
                    }
                }
            }
            _ = &mut shutdown => {
                tracing::debug!("hogd-dev block driver stopping");
                return;
            }
        }
    }
}

async fn run_interval_driver(
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
    interval: std::time::Duration,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // `interval` ticks immediately on creation. Consume that tick so the
    // local node's first block opportunity reflects the configured cadence.
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                tokio::select! {
                    result = drive_one_block(&scheduler) => {
                        log_drive_result(result);
                    }
                    _ = &mut shutdown => {
                        tracing::debug!("hogd-dev interval block driver stopping");
                        return;
                    }
                }
            }
            _ = &mut shutdown => {
                tracing::debug!("hogd-dev interval block driver stopping");
                return;
            }
        }
    }
}

fn log_drive_result(result: Result<DriveOutcome, BlockProductionError>) {
    match result {
        Ok(DriveOutcome::Committed {
            block_id,
            height,
            batch_xdr_bytes,
        }) => {
            tracing::info!(
                block_id,
                height,
                batch_xdr_bytes,
                "hogd-dev block driver committed block"
            );
        }
        Ok(DriveOutcome::NoGenesis) => {
            tracing::warn!("hogd-dev block driver woke before genesis was loaded");
        }
        Err(error) => {
            tracing::warn!(%error, "hogd-dev block driver failed to commit block");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriveOutcome {
    Committed {
        block_id: BlockID,
        height: i64,
        batch_xdr_bytes: usize,
    },
    NoGenesis,
}

async fn drive_one_block(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
) -> Result<DriveOutcome, BlockProductionError> {
    let tip = scheduler.schedcoord_get_tip().await.map_err(|source| {
        BlockProductionError::SchedulerRpc {
            op: "schedcoord_get_tip",
            source,
        }
    })?;

    let db_tip = match tip {
        SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => db_tip,
        SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => return Ok(DriveOutcome::NoGenesis),
        other => {
            return Err(BlockProductionError::UnexpectedTipResult {
                result: format!("{other:?}"),
            });
        }
    };

    let nominated = scheduler
        .schedulercoord_nominate(&db_tip)
        .await
        .map_err(|source| BlockProductionError::NominationOutcomeUnknown { source })?;

    let nominated_block = match nominated {
        SchedulerNominationResult::SchedulerNominationOK { nominated_block } => nominated_block,
        SchedulerNominationResult::SchedulerNominationError => {
            return Err(BlockProductionError::SchedulerRejected {
                op: "schedulercoord_nominate",
                result: "SchedulerNominationError".into(),
            });
        }
        other => {
            return Err(BlockProductionError::SchedulerRejected {
                op: "schedulercoord_nominate",
                result: format!("{other:?}"),
            });
        }
    };

    commit_nominated_block(scheduler, db_tip, nominated_block).await
}

async fn commit_nominated_block(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
    db_tip: DbTip,
    nominated_block: SchedulerNominatedBlock,
) -> Result<DriveOutcome, BlockProductionError> {
    let block_id = nominated_block.block_id;
    let batch_xdr_bytes = nominated_block.txs.xdr.0.len();
    let height = nominated_block.nomination.block_height;
    let block_hash = nominated_block.nomination.block_hash;

    let outcome = match scheduler.schedulercoord_commit_block(&block_id).await {
        Ok(outcome) => outcome,
        Err(source) => {
            return reconcile_commit(
                scheduler,
                block_id,
                *height,
                block_hash,
                batch_xdr_bytes,
                source,
            )
            .await;
        }
    };
    if outcome == SchedulerCommitOutcome::SchedulerCommitOK {
        return Ok(DriveOutcome::Committed {
            block_id,
            height: *height,
            batch_xdr_bytes,
        });
    }

    Err(BlockProductionError::SchedulerRejected {
        op: "schedulercoord_commit_block",
        result: format!(
            "{outcome}; previous height={}, nominated height={}",
            *db_tip.height, *height
        ),
    })
}

async fn reconcile_commit(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
    block_id: BlockID,
    height: i64,
    block_hash: groundhog_xdr::types::BlockHash,
    batch_xdr_bytes: usize,
    source: rpc_driver::RpcError,
) -> Result<DriveOutcome, BlockProductionError> {
    let reconciliation = match scheduler.schedcoord_get_tip().await {
        Ok(SchedulerGetTipResult::SchedulerGetTipOK { db_tip })
            if *db_tip.height == height && db_tip.block_hash == block_hash =>
        {
            tracing::warn!(
                block_id,
                height,
                error = %source,
                "commit RPC failed but scheduler tip confirms the nominated block"
            );
            return Ok(DriveOutcome::Committed {
                block_id,
                height,
                batch_xdr_bytes,
            });
        }
        Ok(result) => format!("scheduler tip did not confirm the nomination: {result:?}"),
        Err(error) => format!("tip lookup failed: {error}"),
    };
    Err(BlockProductionError::CommitOutcomeUnknown {
        block_id,
        height,
        source,
        reconciliation,
    })
}
