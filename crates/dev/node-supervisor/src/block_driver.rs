//! Local hogd-dev block production driver.
//!
//! Production deployments rely on an external coordinator to ask the scheduler
//! to nominate and commit blocks. `groundhog node` deliberately avoids running
//! chuckd for the localhost topology, so the supervisor owns that small piece
//! of coordination policy for hogd-dev.

use std::{fmt, sync::Arc};

use groundhog_scheduler::{
    BlockID, DbTip, SchedulerCommitOutcome, SchedulerGetTipResult, SchedulerNominatedBlock,
    SchedulerNominationResult, sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use tokio::{
    sync::{Notify, oneshot},
    task::JoinHandle,
};

/// Cheap wake handle held by submit-plane adapters.
#[derive(Clone)]
pub(crate) struct BlockDriverTrigger {
    notify: Arc<Notify>,
}

impl fmt::Debug for BlockDriverTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockDriverTrigger").finish_non_exhaustive()
    }
}

impl BlockDriverTrigger {
    fn new(notify: Arc<Notify>) -> Self {
        Self { notify }
    }

    /// Wake the block driver after the scheduler submit RPC succeeds.
    pub(crate) fn trigger_after_submit(&self) {
        self.notify.notify_one();
    }
}

/// Background automine task for the hogd-dev scheduler.
#[derive(Debug)]
pub(crate) struct BlockDriverTask {
    trigger: BlockDriverTrigger,
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl BlockDriverTask {
    /// Start a coalescing block driver for the supplied scheduler client.
    pub(crate) fn start(scheduler: SchedulerCoord_SchedulerCoord1_Client) -> Self {
        let notify = Arc::new(Notify::new());
        let trigger = BlockDriverTrigger::new(Arc::clone(&notify));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let join = tokio::spawn(run_driver(scheduler, notify, shutdown_rx));
        Self {
            trigger,
            shutdown: Some(shutdown_tx),
            join,
        }
    }

    /// Wake handle for scheduler submit adapters.
    pub(crate) fn trigger(&self) -> BlockDriverTrigger {
        self.trigger.clone()
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

async fn run_driver(
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

fn log_drive_result(result: Result<DriveOutcome, BlockDriverError>) {
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
) -> Result<DriveOutcome, BlockDriverError> {
    let tip =
        scheduler
            .schedcoord_get_tip()
            .await
            .map_err(|source| BlockDriverError::SchedulerRpc {
                op: "schedcoord_get_tip",
                source,
            })?;

    let db_tip = match tip {
        SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => db_tip,
        SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => return Ok(DriveOutcome::NoGenesis),
        other => {
            return Err(BlockDriverError::UnexpectedTipResult {
                result: format!("{other:?}"),
            });
        }
    };

    let nominated = scheduler
        .schedulercoord_nominate(&db_tip)
        .await
        .map_err(|source| BlockDriverError::SchedulerRpc {
            op: "schedulercoord_nominate",
            source,
        })?;

    let nominated_block = match nominated {
        SchedulerNominationResult::SchedulerNominationOK { nominated_block } => nominated_block,
        SchedulerNominationResult::SchedulerNominationError => {
            return Err(BlockDriverError::SchedulerRejected {
                op: "schedulercoord_nominate",
                result: "SchedulerNominationError".into(),
            });
        }
        other => {
            return Err(BlockDriverError::SchedulerRejected {
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
) -> Result<DriveOutcome, BlockDriverError> {
    let block_id = nominated_block.block_id;
    let batch_xdr_bytes = nominated_block.txs.xdr.0.len();
    let height = nominated_block.nomination.block_height;

    let outcome = scheduler
        .schedulercoord_commit_block(&block_id)
        .await
        .map_err(|source| BlockDriverError::SchedulerRpc {
            op: "schedulercoord_commit_block",
            source,
        })?;
    if outcome == SchedulerCommitOutcome::SchedulerCommitOK {
        return Ok(DriveOutcome::Committed {
            block_id,
            height: *height,
            batch_xdr_bytes,
        });
    }

    Err(BlockDriverError::SchedulerRejected {
        op: "schedulercoord_commit_block",
        result: format!(
            "{outcome}; previous height={}, nominated height={}",
            *db_tip.height, *height
        ),
    })
}

#[derive(Debug, thiserror::Error)]
enum BlockDriverError {
    #[error("scheduler RPC `{op}` failed: {source}")]
    SchedulerRpc {
        op: &'static str,
        #[source]
        source: rpc_driver::RpcError,
    },
    #[error("scheduler rejected `{op}`: {result}")]
    SchedulerRejected { op: &'static str, result: String },
    #[error("unexpected scheduler tip result: {result}")]
    UnexpectedTipResult { result: String },
}
