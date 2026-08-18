//! Local hogd-dev block production driver.
//!
//! Production deployments rely on an external coordinator to ask the scheduler
//! to nominate and commit blocks. `groundhog node` deliberately avoids running
//! chuckd for the localhost topology, so the supervisor owns that small piece
//! of coordination policy for hogd-dev.

use std::{fmt, sync::Arc, time::Duration};

use groundhog_scheduler::{
    BlockID, ConsensusValue, ProposalTxsHash, SchedulerCommitOutcome, SchedulerGetTipResult,
    SchedulerProposalResult, SchedulerProposedBlock,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use groundhog_xdr::{
    header::{ProposalContext, ProposalHeader},
    types::{ConsensusValueHash, ExtensionPoint, Protocol, SHA256Hash, TimePoint},
    validator::ValidatorID,
};
use sha2::{Digest, Sha256};
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
    /// Scheduler-local identifier used to commit the proposed block.
    pub block_id: BlockID,
    /// Height reported by the committed proposal.
    pub height: i64,
    /// Encoded transaction-batch size reported by the proposal.
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
    /// Proposal RPC failed after the scheduler may have retained a candidate.
    #[error("proposal outcome is unknown after RPC failure: {source}")]
    ProposalOutcomeUnknown {
        /// Failed proposal RPC.
        #[source]
        source: rpc_driver::RpcError,
    },
    /// The scheduler rejected proposal or commit.
    #[error("scheduler rejected `{op}`: {result}")]
    SchedulerRejected {
        /// Scheduler operation that was rejected.
        op: &'static str,
        /// Scheduler result value.
        result: String,
    },
    /// The scheduler rejected commit and may still retain the proposed candidate.
    #[error("scheduler rejected commit for block {block_id} at height {height}: {result}")]
    CommitRejected {
        /// Scheduler-local proposed block identifier.
        block_id: BlockID,
        /// Proposed block height.
        height: i64,
        /// Scheduler commit result value.
        result: String,
    },
    /// The scheduler returned an unexpected tip result.
    #[error("unexpected scheduler tip result: {result}")]
    UnexpectedTipResult {
        /// Debug representation of the unexpected result.
        result: String,
    },
    /// Commit transport failed and the current tip did not confirm the proposal.
    #[error(
        "commit outcome for block {block_id} at height {height} is unknown after RPC failure: {source}; reconciliation: {reconciliation}"
    )]
    CommitOutcomeUnknown {
        /// Scheduler-local proposed block identifier.
        block_id: BlockID,
        /// Nominated block height.
        height: i64,
        /// Failed commit RPC.
        #[source]
        source: rpc_driver::RpcError,
        /// Result of the best-effort tip reconciliation.
        reconciliation: String,
    },
    /// The scheduler returned internally inconsistent proposal batch metadata.
    #[error("scheduler returned malformed proposal batch `{batch}`: {detail}")]
    MalformedSchedulerProposal {
        /// Proposal batch whose metadata was inconsistent.
        batch: &'static str,
        /// Validation or range failure details.
        detail: String,
    },
    /// A scheduler protocol value could not be encoded for consensus hashing.
    #[error("failed to XDR-encode {value} for consensus hashing: {source}")]
    ConsensusHashEncoding {
        /// Protocol value being hashed.
        value: &'static str,
        /// XDR encoding failure.
        #[source]
        source: xdrs::WriteError,
    },
}

/// Cloneable in-process control for producing blocks in manual mode.
#[derive(Debug, Clone)]
pub struct ManualBlockProducer {
    requests: mpsc::Sender<ManualBlockRequest>,
}

impl ManualBlockProducer {
    /// Run one scheduler proposal/commit cycle and await its outcome.
    ///
    /// Dropping the returned future before the driver starts the request cancels
    /// it. Once scheduler work begins, dropping the future does not cancel the
    /// cycle. [`BlockProductionError::TimedOut`],
    /// [`BlockProductionError::OutcomeUnknown`],
    /// [`BlockProductionError::ProposalOutcomeUnknown`], and
    /// [`BlockProductionError::CommitOutcomeUnknown`] mean callers must inspect
    /// chain state before deciding whether to request another block. Malformed
    /// proposal, commit rejection, and consensus-hash errors are also terminal
    /// because the scheduler may have retained the proposed candidate.
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
                                     | BlockProductionError::ProposalOutcomeUnknown { .. }
                                     | BlockProductionError::CommitOutcomeUnknown { .. }
                                     | BlockProductionError::CommitRejected { .. }
                                     | BlockProductionError::MalformedSchedulerProposal { .. }
                                     | BlockProductionError::ConsensusHashEncoding { .. }
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

    let proposal_context = ProposalContext {
        protocol: Protocol::new(0),
        proposer: ValidatorID::default(),
        block_timepoint: TimePoint::new(0),
    };
    let proposed = scheduler
        .schedulercoord_propose_block(&db_tip.id, &proposal_context)
        .await
        .map_err(|source| BlockProductionError::ProposalOutcomeUnknown { source })?;

    let proposed_block = match proposed {
        SchedulerProposalResult::SchedulerProposalOK { proposed_block } => proposed_block,
        SchedulerProposalResult::SchedulerProposalError => {
            return Err(BlockProductionError::SchedulerRejected {
                op: "schedulercoord_propose_block",
                result: "SchedulerProposalError".into(),
            });
        }
        other => {
            return Err(BlockProductionError::SchedulerRejected {
                op: "schedulercoord_propose_block",
                result: format!("{other:?}"),
            });
        }
    };

    commit_proposed_block(scheduler, db_tip.value, proposed_block).await
}

async fn commit_proposed_block(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
    parent_value: ConsensusValue,
    proposed_block: SchedulerProposedBlock,
) -> Result<DriveOutcome, BlockProductionError> {
    let block_id = proposed_block.block_id;
    let batch_xdr_bytes = proposed_block.txs.xdr.0.len();
    let height = proposed_block.proposal.header.block_height.clone();
    let value = consensus_value(
        &parent_value,
        proposed_block.proposal.header,
        &proposed_block.proposal.transactions,
    )?;

    let outcome = match scheduler
        .schedulercoord_commit_block(&block_id, &value)
        .await
    {
        Ok(outcome) => outcome,
        Err(source) => {
            return reconcile_commit(scheduler, block_id, *height, value, batch_xdr_bytes, source)
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

    Err(BlockProductionError::CommitRejected {
        block_id,
        height: *height,
        result: format!(
            "{outcome}; previous height={}",
            *parent_value.proposal_header.block_height
        ),
    })
}

fn consensus_value(
    parent: &ConsensusValue,
    proposal_header: ProposalHeader,
    proposal_txs: &groundhog_scheduler::ProposalTxs,
) -> Result<ConsensusValue, BlockProductionError> {
    Ok(ConsensusValue {
        protocol: Protocol::new(0),
        ext: ExtensionPoint::_0,
        prev_hash: hash_consensus_value(parent)?,
        proposal_header,
        proposal_txs_hash: proposal_txs_compatibility_hash(proposal_txs)?,
        upgrades: xdrs::VecM::default(),
    })
}

// Matches hogd's temporary compatibility hash until the protocol moves to
// canonical XDR hashing for ProposalTxs.
fn proposal_txs_compatibility_hash(
    proposal_txs: &groundhog_scheduler::ProposalTxs,
) -> Result<ProposalTxsHash, BlockProductionError> {
    let mut base = proposal_txs.clone();
    base.hash.segment_tokens.0.clear();
    base.hash.segment_bytes.0.clear();
    base.hash.xdr.0.clear();
    base.nondeterministic_data.segment_tokens.0.clear();
    base.nondeterministic_data.segment_bytes.0.clear();
    base.nondeterministic_data.xdr.0.clear();

    let mut result = hash_xdr(&base)
        .map_err(|source| BlockProductionError::ConsensusHashEncoding {
            value: "proposal transactions base",
            source,
        })?
        .into_inner();
    xor_hash(
        &mut result,
        hash_parallel_batch_compatibility("hash", &proposal_txs.hash)?,
    );
    xor_hash(
        &mut result,
        hash_parallel_batch_compatibility(
            "nondeterministic_data",
            &proposal_txs.nondeterministic_data,
        )?,
    );
    Ok(ProposalTxsHash::new(SHA256Hash::from(result)))
}

fn hash_parallel_batch_compatibility(
    batch_name: &'static str,
    batch: &groundhog_scheduler::ParallelBatch,
) -> Result<[u8; 32], BlockProductionError> {
    batch
        .validate()
        .map_err(|error| BlockProductionError::MalformedSchedulerProposal {
            batch: batch_name,
            detail: error.to_string(),
        })?;

    let mut result = [0u8; 32];
    let mut offset = 0usize;
    for &segment_size in &batch.segment_bytes.0 {
        let end = offset.checked_add(segment_size as usize).ok_or_else(|| {
            BlockProductionError::MalformedSchedulerProposal {
                batch: batch_name,
                detail: "segment byte offset overflowed usize".into(),
            }
        })?;
        let segment = batch.xdr.0.get(offset..end).ok_or_else(|| {
            BlockProductionError::MalformedSchedulerProposal {
                batch: batch_name,
                detail: format!(
                    "segment range {offset}..{end} exceeds {}-byte payload",
                    batch.xdr.len()
                ),
            }
        })?;
        xor_hash(&mut result, Sha256::digest(segment).into());
        offset = end;
    }
    if offset != batch.xdr.len() {
        return Err(BlockProductionError::MalformedSchedulerProposal {
            batch: batch_name,
            detail: format!(
                "segment lengths cover {offset} bytes, payload has {} bytes",
                batch.xdr.len()
            ),
        });
    }

    let mut index = batch.clone();
    index.xdr.0.clear();
    let index_hash =
        hash_xdr(&index).map_err(|source| BlockProductionError::ConsensusHashEncoding {
            value: batch_name,
            source,
        })?;
    xor_hash(&mut result, index_hash.into_inner());
    Ok(result)
}

fn xor_hash(result: &mut [u8; 32], hash: [u8; 32]) {
    for (result_byte, hash_byte) in result.iter_mut().zip(hash) {
        *result_byte ^= hash_byte;
    }
}

fn hash_consensus_value(
    value: &ConsensusValue,
) -> Result<ConsensusValueHash, BlockProductionError> {
    hash_xdr(value)
        .map(ConsensusValueHash::new)
        .map_err(|source| BlockProductionError::ConsensusHashEncoding {
            value: "parent consensus value",
            source,
        })
}

fn hash_xdr<T: xdrs::WriteXDR>(value: &T) -> Result<SHA256Hash, xdrs::WriteError> {
    xdrs::txs::encode(value)
        .map(|encoded| SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(encoded))))
}

async fn reconcile_commit(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
    block_id: BlockID,
    height: i64,
    value: ConsensusValue,
    batch_xdr_bytes: usize,
    source: rpc_driver::RpcError,
) -> Result<DriveOutcome, BlockProductionError> {
    let reconciliation = match scheduler.schedcoord_get_tip().await {
        Ok(SchedulerGetTipResult::SchedulerGetTipOK { db_tip })
            if *db_tip.value.proposal_header.block_height == height && db_tip.value == value =>
        {
            tracing::warn!(
                block_id,
                height,
                error = %source,
                "commit RPC failed but scheduler tip confirms the proposed block"
            );
            return Ok(DriveOutcome::Committed {
                block_id,
                height,
                batch_xdr_bytes,
            });
        }
        Ok(result) => format!("scheduler tip did not confirm the proposal: {result:?}"),
        Err(error) => format!("tip lookup failed: {error}"),
    };
    Err(BlockProductionError::CommitOutcomeUnknown {
        block_id,
        height,
        source,
        reconciliation,
    })
}

#[cfg(test)]
mod hash_tests {
    use groundhog_scheduler::{
        ParallelBatch, ParallelNondeterministicProposalDataBatch, ParallelTransactionHashBatch,
        ProposalTxs,
    };
    use groundhog_xdr::types::{ExtensionPoint, SHA256Hash};
    use xdrs::{BytesM, VecM};

    use super::{BlockProductionError, hash_xdr, proposal_txs_compatibility_hash};

    #[test]
    fn proposal_txs_compatibility_hash_matches_hogd_golden_vector() {
        let proposal_txs = ProposalTxs {
            ext: ExtensionPoint::_0,
            hash: ParallelTransactionHashBatch::new(ParallelBatch {
                segment_tokens: VecM(vec![1, 2]),
                segment_bytes: VecM(vec![4, 8]),
                xdr: BytesM(vec![0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3]),
            }),
            nondeterministic_data: ParallelNondeterministicProposalDataBatch::new(ParallelBatch {
                segment_tokens: VecM(vec![1]),
                segment_bytes: VecM(vec![4]),
                xdr: BytesM(vec![0xde, 0xad, 0xbe, 0xef]),
            }),
        };

        assert_eq!(
            proposal_txs_compatibility_hash(&proposal_txs)
                .expect("bug: valid proposal transactions should hash")
                .into_inner(),
            SHA256Hash::from([
                0x3a, 0x3c, 0xe7, 0x82, 0x83, 0x94, 0x38, 0x26, 0xac, 0x43, 0x29, 0x1f, 0xab, 0x79,
                0xbd, 0x17, 0xc4, 0xad, 0x1d, 0x1e, 0xf9, 0xe2, 0x2d, 0x8c, 0x72, 0x31, 0xa9, 0x7c,
                0xb6, 0x68, 0xdc, 0xd9,
            ])
        );
    }

    #[test]
    fn proposal_txs_hash_rejects_segment_count_mismatch() {
        let proposal_txs = ProposalTxs {
            ext: ExtensionPoint::_0,
            hash: ParallelTransactionHashBatch::new(ParallelBatch {
                segment_tokens: VecM(vec![1]),
                segment_bytes: VecM(vec![]),
                xdr: BytesM(vec![]),
            }),
            nondeterministic_data: ParallelNondeterministicProposalDataBatch::new(
                ParallelBatch::empty(),
            ),
        };

        let error = proposal_txs_compatibility_hash(&proposal_txs)
            .expect_err("bug: mismatched segment metadata should fail");
        assert!(matches!(
            error,
            BlockProductionError::MalformedSchedulerProposal { batch: "hash", .. }
        ));
    }

    #[test]
    fn proposal_txs_hash_rejects_segment_byte_mismatch() {
        let proposal_txs = ProposalTxs {
            ext: ExtensionPoint::_0,
            hash: ParallelTransactionHashBatch::new(ParallelBatch {
                segment_tokens: VecM(vec![1]),
                segment_bytes: VecM(vec![2]),
                xdr: BytesM(vec![0]),
            }),
            nondeterministic_data: ParallelNondeterministicProposalDataBatch::new(
                ParallelBatch::empty(),
            ),
        };

        let error = proposal_txs_compatibility_hash(&proposal_txs)
            .expect_err("bug: segment lengths outside the payload should fail");
        assert!(matches!(
            error,
            BlockProductionError::MalformedSchedulerProposal { batch: "hash", .. }
        ));
    }

    #[test]
    fn xdr_hashing_returns_encoding_errors() {
        let over_bound = BytesM::<1>(vec![0, 1]);
        let error = hash_xdr(&over_bound).expect_err("bug: over-bound XDR should fail to encode");
        assert!(matches!(error, xdrs::WriteError::InvalidArrayLength));
    }
}
