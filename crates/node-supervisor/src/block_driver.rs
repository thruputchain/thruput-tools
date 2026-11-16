//! Local hogd-dev block production driver.
//!
//! Production deployments rely on an external coordinator to ask the scheduler
//! to nominate and commit blocks. `thruput node` deliberately avoids running
//! chuckd for the localhost topology, so the supervisor owns that small piece
//! of coordination policy for hogd-dev.
//!
//! One task owns block production in every mode. Automatic and requested
//! blocks therefore never overlap, and the task owns the block clock: the
//! timepoint each new block receives.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use thruput_node_rpc::{
    BlockID, ConsensusValue, ProposalTxsHash, SchedulerCommitOutcome, SchedulerGetTipResult,
    SchedulerProposalResult, SchedulerProposedBlock,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use thruput_xdr::{
    header::{ProposalContext, ProposalHeader},
    types::{ConsensusValueHash, ExtensionPoint, Protocol, SHA256Hash, TimePoint},
    validator::ValidatorID,
};
use tokio::{
    sync::{Notify, mpsc, oneshot},
    task::JoinHandle,
    time::{Interval, MissedTickBehavior},
};

use crate::config::{BlockDriverConfig, BlockDriverMode};

const BLOCK_REQUEST_CAPACITY: usize = 1;

/// Maximum number of blocks that one [`BlockProducer::produce_blocks`] call
/// can produce. The `dev_produceBlocks` RPC method shares this limit.
pub const MAX_BLOCKS_PER_REQUEST: u32 = thruput_json_rpc_types::requests::MAX_PRODUCE_BLOCKS;

/// Metadata returned after the scheduler commits a requested block.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProducedBlock {
    /// Scheduler-local identifier used to commit the proposed block.
    pub block_id: BlockID,
    /// Height reported by the committed proposal.
    pub height: i64,
    /// Encoded transaction-batch size reported by the proposal.
    pub batch_xdr_bytes: usize,
    /// The block's timepoint: Unix nanoseconds, as contracts read it.
    pub timepoint: TimePoint,
}

/// Errors returned while producing a local block or changing the block clock.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockProductionError {
    /// The block driver stopped before it could acknowledge the request.
    #[error("block producer has stopped")]
    DriverStopped,
    /// Shutdown interrupted a request after scheduler work may have begun.
    #[error("block production was interrupted; scheduler outcome may be unknown")]
    OutcomeUnknown,
    /// The scheduler did not complete a request before its deadline.
    #[error("block production did not complete within {timeout:?}; outcome may be unknown")]
    TimedOut {
        /// Configured request deadline.
        timeout: Duration,
    },
    /// The scheduler had no genesis block to build on.
    #[error("scheduler has no genesis block")]
    NoGenesis,
    /// A requested block timepoint is earlier than the chain tip's.
    #[error(
        "block timepoint {requested} is earlier than the chain tip's {tip}; blocks only move forward in time"
    )]
    TimepointBeforeTip {
        /// Requested Unix nanoseconds.
        requested: u64,
        /// The chain tip's Unix nanoseconds.
        tip: u64,
    },
    /// A requested block time does not fit a 64-bit nanosecond timepoint.
    #[error("block timepoint overflows 64-bit Unix nanoseconds")]
    TimepointOverflow,
    /// A request asked for no blocks or for too many.
    #[error("block count {count} is outside 1..={max}")]
    InvalidBlockCount {
        /// Requested block count.
        count: u32,
        /// Largest accepted count.
        max: u32,
    },
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

impl BlockProductionError {
    /// True when scheduler work may still be running or retained, so the
    /// driver stops rather than risk overlapping it with a new block.
    fn is_indeterminate(&self) -> bool {
        matches!(
            self,
            Self::TimedOut { .. }
                | Self::ProposalOutcomeUnknown { .. }
                | Self::CommitOutcomeUnknown { .. }
                | Self::CommitRejected { .. }
                | Self::MalformedSchedulerProposal { .. }
                | Self::ConsensusHashEncoding { .. }
        )
    }
}

/// Cloneable in-process control for local block production and block time.
///
/// Requests are served one at a time by the block driver in every mode, so
/// they never overlap automatic production. Dropping a returned future before
/// the driver starts the request cancels it. Once scheduler work begins,
/// dropping the future does not cancel it.
///
/// [`BlockProductionError::TimedOut`], [`BlockProductionError::OutcomeUnknown`],
/// [`BlockProductionError::ProposalOutcomeUnknown`], and
/// [`BlockProductionError::CommitOutcomeUnknown`] mean callers must inspect
/// chain state before deciding whether to request another block. Malformed
/// proposal, commit rejection, and consensus-hash errors are also terminal
/// because the scheduler may have retained the proposed candidate. The driver
/// stops after an indeterminate result, so later requests return
/// [`BlockProductionError::DriverStopped`].
#[derive(Debug, Clone)]
pub struct BlockProducer {
    commands: mpsc::Sender<Command>,
}

impl BlockProducer {
    /// Produce one block now. The block can be empty. Its timepoint follows
    /// the block clock.
    pub async fn produce_block(&self) -> Result<ProducedBlock, BlockProductionError> {
        self.produce(1, None, None).await.map(single_block)
    }

    /// Produce one block now at `timepoint`. Later blocks continue from it.
    ///
    /// Blocks only move forward in time: `timepoint` must be at or after the
    /// chain tip's.
    pub async fn produce_block_at(
        &self,
        timepoint: TimePoint,
    ) -> Result<ProducedBlock, BlockProductionError> {
        self.produce(1, Some(*timepoint), None)
            .await
            .map(single_block)
    }

    /// Produce `count` blocks now, in order.
    ///
    /// With an `interval`, each block after the first is exactly `interval`
    /// later than the one before. Without one, every block follows the block
    /// clock. On error, blocks already produced stay committed; the error
    /// does not list them.
    pub async fn produce_blocks(
        &self,
        count: u32,
        interval: Option<Duration>,
    ) -> Result<Vec<ProducedBlock>, BlockProductionError> {
        let interval = interval.map(duration_nanos).transpose()?;
        self.produce(count, None, interval).await
    }

    /// Give the next block exactly `timepoint`. Later blocks continue from it.
    /// Returns the timepoint the next block will receive.
    pub async fn set_next_block_timepoint(
        &self,
        timepoint: TimePoint,
    ) -> Result<TimePoint, BlockProductionError> {
        self.request(|reply| Command::SetNextTimepoint {
            timepoint: *timepoint,
            reply,
        })
        .await
    }

    /// Move the block clock forward by `duration` for every later block.
    /// Returns the timepoint the next block would receive now.
    pub async fn increase_time(
        &self,
        duration: Duration,
    ) -> Result<TimePoint, BlockProductionError> {
        let nanos = duration_nanos(duration)?;
        self.request(|reply| Command::IncreaseTime { nanos, reply })
            .await
    }

    /// Change how blocks are produced while the node runs. An `interval`
    /// replaces the configured one; it only affects interval mode.
    pub async fn set_block_driver(
        &self,
        mode: BlockDriverMode,
        interval: Option<Duration>,
    ) -> Result<BlockDriverConfig, BlockProductionError> {
        self.request(|reply| Command::SetMode {
            mode,
            interval,
            reply,
        })
        .await
    }

    async fn produce(
        &self,
        count: u32,
        first: Option<u64>,
        interval: Option<u64>,
    ) -> Result<Vec<ProducedBlock>, BlockProductionError> {
        if !(1..=MAX_BLOCKS_PER_REQUEST).contains(&count) {
            return Err(BlockProductionError::InvalidBlockCount {
                count,
                max: MAX_BLOCKS_PER_REQUEST,
            });
        }
        self.request(|reply| Command::Produce {
            count,
            first,
            interval,
            reply,
        })
        .await
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, BlockProductionError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| BlockProductionError::DriverStopped)?;
        response
            .await
            .unwrap_or(Err(BlockProductionError::DriverStopped))
    }
}

fn single_block(mut blocks: Vec<ProducedBlock>) -> ProducedBlock {
    blocks
        .pop()
        .expect("bug: a one-block request returns one block")
}

fn duration_nanos(duration: Duration) -> Result<u64, BlockProductionError> {
    u64::try_from(duration.as_nanos()).map_err(|_| BlockProductionError::TimepointOverflow)
}

type Reply<T> = oneshot::Sender<Result<T, BlockProductionError>>;

#[derive(Debug)]
enum Command {
    Produce {
        count: u32,
        first: Option<u64>,
        interval: Option<u64>,
        reply: Reply<Vec<ProducedBlock>>,
    },
    SetNextTimepoint {
        timepoint: u64,
        reply: Reply<TimePoint>,
    },
    IncreaseTime {
        nanos: u64,
        reply: Reply<TimePoint>,
    },
    SetMode {
        mode: BlockDriverMode,
        interval: Option<Duration>,
        reply: Reply<BlockDriverConfig>,
    },
}

impl Command {
    fn is_abandoned(&self) -> bool {
        match self {
            Self::Produce { reply, .. } => reply.is_closed(),
            Self::SetNextTimepoint { reply, .. } | Self::IncreaseTime { reply, .. } => {
                reply.is_closed()
            }
            Self::SetMode { reply, .. } => reply.is_closed(),
        }
    }
}

/// The timepoint each new block receives.
///
/// A block takes the pinned timepoint when one is set, and otherwise the wall
/// clock plus an offset. It never moves behind its parent, because hogd
/// refuses a block older than its committed tip. Pinning a timepoint also
/// moves the offset, so later blocks continue from the pinned time instead of
/// falling back to the wall clock.
#[derive(Debug, Default)]
struct BlockClock {
    offset_nanos: i128,
    next: Option<u64>,
}

impl BlockClock {
    fn timepoint_after(&self, parent: u64, now: SystemTime) -> u64 {
        let clock = self.next.unwrap_or_else(|| {
            let shifted = unix_nanos(now).saturating_add(self.offset_nanos);
            u64::try_from(shifted.max(0)).unwrap_or(u64::MAX)
        });
        clock.max(parent)
    }

    fn pin(&mut self, timepoint: u64, now: SystemTime) {
        self.next = Some(timepoint);
        self.offset_nanos = i128::from(timepoint) - unix_nanos(now);
    }

    fn advance(&mut self, nanos: u64) {
        self.offset_nanos = self.offset_nanos.saturating_add(i128::from(nanos));
    }

    fn committed(&mut self, timepoint: u64, now: SystemTime) {
        if self.next.is_some() {
            self.pin(timepoint, now);
            self.next = None;
        }
    }
}

fn unix_nanos(now: SystemTime) -> i128 {
    match now.duration_since(UNIX_EPOCH) {
        Ok(since) => i128::try_from(since.as_nanos()).unwrap_or(i128::MAX),
        Err(before) => -i128::try_from(before.duration().as_nanos()).unwrap_or(i128::MAX),
    }
}

/// Cheap wake handle held by submit-plane adapters.
#[derive(Clone)]
pub(crate) struct BlockDriverTrigger {
    wake: Option<(Arc<Notify>, Arc<AtomicBool>)>,
}

impl fmt::Debug for BlockDriverTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockDriverTrigger")
            .field("enabled", &self.wake.is_some())
            .finish_non_exhaustive()
    }
}

impl BlockDriverTrigger {
    /// A trigger that always wakes `notify`, as in instant mode.
    #[cfg(test)]
    pub(crate) fn new(notify: Arc<Notify>) -> Self {
        Self {
            wake: Some((notify, Arc::new(AtomicBool::new(true)))),
        }
    }

    #[cfg(test)]
    pub(crate) fn noop() -> Self {
        Self { wake: None }
    }

    /// Wake the block driver after the scheduler submit RPC succeeds. Only
    /// instant mode produces a block for a submit.
    pub(crate) fn trigger_after_submit(&self) {
        if let Some((notify, instant)) = &self.wake
            && instant.load(Ordering::Acquire)
        {
            notify.notify_one();
        }
    }
}

/// Background block production task for the hogd-dev scheduler.
#[derive(Debug)]
pub(crate) struct BlockDriverTask {
    trigger: BlockDriverTrigger,
    producer: BlockProducer,
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl BlockDriverTask {
    /// Start the block driver for the supplied scheduler client.
    pub(crate) fn start(
        scheduler: SchedulerCoord_SchedulerCoord1_Client,
        config: BlockDriverConfig,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (commands, receiver) = mpsc::channel(BLOCK_REQUEST_CAPACITY);
        let notify = Arc::new(Notify::new());
        let instant = Arc::new(AtomicBool::new(false));
        let trigger = BlockDriverTrigger {
            wake: Some((Arc::clone(&notify), Arc::clone(&instant))),
        };
        let mut driver = Driver {
            scheduler,
            config,
            clock: BlockClock::default(),
            notify,
            instant,
            ticker: None,
        };
        driver.apply_mode();
        let join = tokio::spawn(driver.run(receiver, shutdown_rx));
        Self {
            trigger,
            producer: BlockProducer { commands },
            shutdown: Some(shutdown_tx),
            join,
        }
    }

    /// Wake handle for scheduler submit adapters.
    pub(crate) fn trigger(&self) -> BlockDriverTrigger {
        self.trigger.clone()
    }

    /// In-process block production and block clock control.
    pub(crate) fn producer(&self) -> BlockProducer {
        self.producer.clone()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Continue,
    Stop,
}

enum Wake {
    Automatic,
    Command(Command),
}

struct Driver {
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
    config: BlockDriverConfig,
    clock: BlockClock,
    notify: Arc<Notify>,
    instant: Arc<AtomicBool>,
    ticker: Option<Interval>,
}

impl Driver {
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        loop {
            let wake = tokio::select! {
                biased;
                _ = &mut shutdown => break,
                command = commands.recv() => match command {
                    Some(command) => Wake::Command(command),
                    None => break,
                },
                () = self.notify.notified(), if self.config.mode == BlockDriverMode::Instant => {
                    Wake::Automatic
                }
                () = tick(&mut self.ticker) => Wake::Automatic,
            };
            let step = match wake {
                Wake::Automatic => self.produce_automatic(&mut shutdown).await,
                Wake::Command(command) => self.handle(command, &mut shutdown).await,
            };
            if step == Step::Stop {
                break;
            }
        }
        tracing::debug!("hogd-dev block driver stopping");
    }

    fn apply_mode(&mut self) {
        self.instant.store(
            self.config.mode == BlockDriverMode::Instant,
            Ordering::Release,
        );
        self.ticker = (self.config.mode == BlockDriverMode::Interval).then(|| {
            let interval = self.config.effective_interval();
            // `interval` ticks immediately on creation. Start one period from
            // now so the first block opportunity reflects the cadence.
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            ticker
        });
    }

    async fn produce_automatic(&mut self, shutdown: &mut oneshot::Receiver<()>) -> Step {
        let result = tokio::select! {
            biased;
            _ = &mut *shutdown => return Step::Stop,
            result = drive_one_block(&self.scheduler, |parent| {
                Ok(self.clock.timepoint_after(parent, SystemTime::now()))
            }) => result,
        };
        if let Ok(DriveOutcome::Committed { timepoint, .. }) = &result {
            self.clock.committed(*timepoint, SystemTime::now());
        }
        log_drive_result(result);
        Step::Continue
    }

    async fn handle(&mut self, command: Command, shutdown: &mut oneshot::Receiver<()>) -> Step {
        if command.is_abandoned() {
            return Step::Continue;
        }
        match command {
            Command::Produce {
                count,
                first,
                interval,
                reply,
            } => {
                let (result, step) = self
                    .produce_requested(count, first, interval, shutdown)
                    .await;
                let _ = reply.send(result);
                step
            }
            Command::SetNextTimepoint { timepoint, reply } => {
                let (result, step) = self
                    .with_tip(shutdown, |clock, tip| {
                        if timepoint < tip {
                            return Err(BlockProductionError::TimepointBeforeTip {
                                requested: timepoint,
                                tip,
                            });
                        }
                        clock.pin(timepoint, SystemTime::now());
                        Ok(TimePoint::new(timepoint))
                    })
                    .await;
                let _ = reply.send(result);
                step
            }
            Command::IncreaseTime { nanos, reply } => {
                self.clock.advance(nanos);
                let (result, step) = self
                    .with_tip(shutdown, |clock, tip| {
                        Ok(TimePoint::new(
                            clock.timepoint_after(tip, SystemTime::now()),
                        ))
                    })
                    .await;
                let _ = reply.send(result);
                step
            }
            Command::SetMode {
                mode,
                interval,
                reply,
            } => {
                self.config.mode = mode;
                if let Some(interval) = interval {
                    self.config.interval = interval;
                }
                self.apply_mode();
                tracing::info!(?mode, interval = ?self.config.effective_interval(), "hogd-dev block driver mode changed");
                let _ = reply.send(Ok(self.config));
                Step::Continue
            }
        }
    }

    /// Read the tip's timepoint, then apply `update` to the clock.
    async fn with_tip<T>(
        &mut self,
        shutdown: &mut oneshot::Receiver<()>,
        update: impl FnOnce(&mut BlockClock, u64) -> Result<T, BlockProductionError>,
    ) -> (Result<T, BlockProductionError>, Step) {
        let timeout = self.config.effective_manual_request_timeout();
        let tip = tokio::select! {
            biased;
            _ = &mut *shutdown => return (Err(BlockProductionError::DriverStopped), Step::Stop),
            tip = tokio::time::timeout(timeout, tip_timepoint(&self.scheduler)) => tip,
        };
        let result = match tip {
            Ok(Ok(tip)) => update(&mut self.clock, tip),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(BlockProductionError::TimedOut { timeout }),
        };
        (result, Step::Continue)
    }

    async fn produce_requested(
        &mut self,
        count: u32,
        first: Option<u64>,
        interval: Option<u64>,
        shutdown: &mut oneshot::Receiver<()>,
    ) -> (Result<Vec<ProducedBlock>, BlockProductionError>, Step) {
        let timeout = self.config.effective_manual_request_timeout();
        let mut blocks = Vec::new();
        let mut requested = first;
        for _ in 0..count {
            let result = tokio::select! {
                biased;
                _ = &mut *shutdown => {
                    return (Err(BlockProductionError::OutcomeUnknown), Step::Stop);
                }
                result = tokio::time::timeout(timeout, drive_one_block(&self.scheduler, |parent| {
                    match requested {
                        Some(requested) if requested < parent => {
                            Err(BlockProductionError::TimepointBeforeTip { requested, tip: parent })
                        }
                        Some(requested) => Ok(requested),
                        None => Ok(self.clock.timepoint_after(parent, SystemTime::now())),
                    }
                })) => result.unwrap_or(Err(BlockProductionError::TimedOut { timeout })),
            };
            let block = match produced_block(result) {
                Ok(block) => block,
                Err(error) => {
                    tracing::warn!(%error, "requested hogd-dev block production failed");
                    if error.is_indeterminate() {
                        tracing::warn!("block driver stopping after indeterminate outcome");
                        return (Err(error), Step::Stop);
                    }
                    return (Err(error), Step::Continue);
                }
            };
            if requested.is_some() {
                self.clock.pin(*block.timepoint, SystemTime::now());
            }
            self.clock.committed(*block.timepoint, SystemTime::now());
            requested = match interval {
                Some(interval) => match (*block.timepoint).checked_add(interval) {
                    Some(next) => Some(next),
                    None => return (Err(BlockProductionError::TimepointOverflow), Step::Continue),
                },
                None => None,
            };
            blocks.push(block);
        }
        (Ok(blocks), Step::Continue)
    }
}

async fn tick(ticker: &mut Option<Interval>) {
    match ticker {
        Some(ticker) => {
            ticker.tick().await;
        }
        None => std::future::pending().await,
    }
}

fn produced_block(
    result: Result<DriveOutcome, BlockProductionError>,
) -> Result<ProducedBlock, BlockProductionError> {
    match result {
        Ok(DriveOutcome::Committed {
            block_id,
            height,
            batch_xdr_bytes,
            timepoint,
        }) => Ok(ProducedBlock {
            block_id,
            height,
            batch_xdr_bytes,
            timepoint: TimePoint::new(timepoint),
        }),
        Ok(DriveOutcome::NoGenesis) => Err(BlockProductionError::NoGenesis),
        Err(error) => Err(error),
    }
}

async fn tip_timepoint(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
) -> Result<u64, BlockProductionError> {
    match scheduler.schedcoord_get_tip().await.map_err(|source| {
        BlockProductionError::SchedulerRpc {
            op: "schedcoord_get_tip",
            source,
        }
    })? {
        SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => {
            Ok(*db_tip.value.proposal_header.block_timepoint)
        }
        SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => {
            Err(BlockProductionError::NoGenesis)
        }
        other => Err(BlockProductionError::UnexpectedTipResult {
            result: format!("{other:?}"),
        }),
    }
}

fn log_drive_result(result: Result<DriveOutcome, BlockProductionError>) {
    match result {
        Ok(DriveOutcome::Committed {
            block_id,
            height,
            batch_xdr_bytes,
            timepoint,
        }) => {
            tracing::info!(
                block_id,
                height,
                batch_xdr_bytes,
                timepoint,
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
        timepoint: u64,
    },
    NoGenesis,
}

/// Propose and commit one block on the scheduler tip. `timepoint` chooses the
/// block's timepoint from its parent's.
async fn drive_one_block(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
    timepoint: impl FnOnce(u64) -> Result<u64, BlockProductionError>,
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

    let timepoint = timepoint(*db_tip.value.proposal_header.block_timepoint)?;
    let proposal_context = ProposalContext {
        protocol: Protocol::new(0),
        proposer: ValidatorID::default(),
        block_timepoint: TimePoint::new(timepoint),
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
    let timepoint = *proposed_block.proposal.header.block_timepoint;
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
            timepoint,
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
    proposal_txs: &thruput_node_rpc::ProposalTxs,
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
    proposal_txs: &thruput_node_rpc::ProposalTxs,
) -> Result<ProposalTxsHash, BlockProductionError> {
    let mut base = proposal_txs.clone();
    base.signed_hash.segment_tokens.0.clear();
    base.signed_hash.segment_bytes.0.clear();
    base.signed_hash.xdr.0.clear();
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
        hash_parallel_batch_compatibility("signed_hash", &proposal_txs.signed_hash)?,
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
    batch: &thruput_node_rpc::ParallelBatch,
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
                timepoint: *value.proposal_header.block_timepoint,
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
mod clock_tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::BlockClock;

    const DAY: u64 = 24 * 60 * 60 * 1_000_000_000;
    const NOW: u64 = 1_760_000_000_123_456_789;

    fn at(nanos: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_nanos(nanos)
    }

    #[test]
    fn takes_the_wall_clock_in_unix_nanoseconds() {
        assert_eq!(BlockClock::default().timepoint_after(0, at(NOW)), NOW);
    }

    #[test]
    fn never_moves_behind_the_parent() {
        let clock = BlockClock::default();
        assert_eq!(clock.timepoint_after(NOW, at(NOW - 1)), NOW);
    }

    #[test]
    fn a_clock_before_the_epoch_keeps_the_parent() {
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(BlockClock::default().timepoint_after(42, before_epoch), 42);
    }

    #[test]
    fn a_pinned_timepoint_is_used_exactly_once() {
        let mut clock = BlockClock::default();
        clock.pin(NOW + 3 * DAY, at(NOW));

        let pinned = clock.timepoint_after(0, at(NOW + 5));
        assert_eq!(pinned, NOW + 3 * DAY);
        clock.committed(pinned, at(NOW + 5));

        assert!(clock.next.is_none());
    }

    #[test]
    fn later_blocks_continue_from_a_pinned_timepoint() {
        let mut clock = BlockClock::default();
        clock.pin(NOW + 3 * DAY, at(NOW));
        clock.committed(NOW + 3 * DAY, at(NOW));

        // One second of wall-clock time later, the clock is one second past
        // the pinned time: it neither falls back nor freezes.
        let second = 1_000_000_000;
        assert_eq!(
            clock.timepoint_after(NOW + 3 * DAY, at(NOW + second)),
            NOW + 3 * DAY + second
        );
    }

    #[test]
    fn a_pinned_timepoint_in_the_past_continues_from_there() {
        let mut clock = BlockClock::default();
        clock.pin(NOW - DAY, at(NOW));
        clock.committed(NOW - DAY, at(NOW));

        assert_eq!(clock.timepoint_after(0, at(NOW + 10)), NOW - DAY + 10);
    }

    #[test]
    fn increasing_time_shifts_every_later_block() {
        let mut clock = BlockClock::default();
        clock.advance(2 * DAY);
        clock.advance(DAY);

        assert_eq!(clock.timepoint_after(0, at(NOW)), NOW + 3 * DAY);
        assert_eq!(clock.timepoint_after(0, at(NOW + 7)), NOW + 3 * DAY + 7);
    }

    #[test]
    fn increasing_time_keeps_a_pinned_next_block() {
        let mut clock = BlockClock::default();
        clock.pin(NOW + DAY, at(NOW));
        clock.advance(DAY);

        assert_eq!(clock.timepoint_after(0, at(NOW)), NOW + DAY);
    }

    #[test]
    fn the_clock_saturates_instead_of_wrapping() {
        let mut clock = BlockClock::default();
        clock.advance(u64::MAX);
        clock.advance(u64::MAX);

        assert_eq!(clock.timepoint_after(0, at(NOW)), u64::MAX);
    }
}

#[cfg(test)]
mod hash_tests {
    use thruput_node_rpc::{
        ParallelBatch, ParallelNondeterministicProposalDataBatch,
        ParallelSignedTransactionHashBatch, ProposalTxs,
    };
    use thruput_xdr::types::{ExtensionPoint, SHA256Hash};
    use xdrs::{BytesM, VecM};

    use super::{BlockProductionError, hash_xdr, proposal_txs_compatibility_hash};

    #[test]
    fn proposal_txs_compatibility_hash_matches_hogd_golden_vector() {
        let proposal_txs = ProposalTxs {
            ext: ExtensionPoint::_0,
            signed_hash: ParallelSignedTransactionHashBatch::new(ParallelBatch {
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
            signed_hash: ParallelSignedTransactionHashBatch::new(ParallelBatch {
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
            BlockProductionError::MalformedSchedulerProposal {
                batch: "signed_hash",
                ..
            }
        ));
    }

    #[test]
    fn proposal_txs_hash_rejects_segment_byte_mismatch() {
        let proposal_txs = ProposalTxs {
            ext: ExtensionPoint::_0,
            signed_hash: ParallelSignedTransactionHashBatch::new(ParallelBatch {
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
            BlockProductionError::MalformedSchedulerProposal {
                batch: "signed_hash",
                ..
            }
        ));
    }

    #[test]
    fn xdr_hashing_returns_encoding_errors() {
        let over_bound = BytesM::<1>(vec![0, 1]);
        let error = hash_xdr(&over_bound).expect_err("bug: over-bound XDR should fail to encode");
        assert!(matches!(error, xdrs::WriteError::InvalidArrayLength));
    }
}
