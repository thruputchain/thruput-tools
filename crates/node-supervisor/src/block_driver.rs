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
    sched_gateway::SchedGateway_SchedGateway1_Client,
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
    /// A request did not complete before its deadline.
    #[error("block driver request did not complete within {timeout:?}; outcome may be unknown")]
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
/// Each request has one deadline that includes the time it waits behind
/// automatic production or an earlier request: the configured request timeout
/// for the wait, plus the timeout for each block or tip read it runs. When it
/// expires, the request returns [`BlockProductionError::TimedOut`]. Work the
/// driver already started still completes.
///
/// [`BlockProductionError::TimedOut`], [`BlockProductionError::OutcomeUnknown`],
/// [`BlockProductionError::ProposalOutcomeUnknown`], and
/// [`BlockProductionError::CommitOutcomeUnknown`] mean callers must inspect
/// chain state before deciding whether to request another block. Malformed
/// proposal, commit rejection, and consensus-hash errors are also terminal
/// because the scheduler may have retained the proposed candidate. After such
/// a result for a requested block, the driver stops, so later requests return
/// [`BlockProductionError::DriverStopped`]. Automatic production in instant
/// and interval mode logs such a result and continues: it never cancels
/// scheduler work, and hogd refuses a new proposal while one is pending.
#[derive(Debug, Clone)]
pub struct BlockProducer {
    commands: mpsc::Sender<Command>,
    timeout: Duration,
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
        self.request(1, |reply| Command::SetNextTimepoint {
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
        self.request(1, |reply| Command::IncreaseTime { nanos, reply })
            .await
    }

    /// Give blocks without a pinned timepoint a fixed `step` from their
    /// parent, or follow the wall clock again with `None`.
    /// [`Duration::ZERO`] freezes the clock until the next pin or time
    /// increase. Returns the timepoint the next block would receive now.
    pub async fn set_block_time_step(
        &self,
        step: Option<Duration>,
    ) -> Result<TimePoint, BlockProductionError> {
        let step = step.map(duration_nanos).transpose()?;
        self.request(1, |reply| Command::SetTimeStep { step, reply })
            .await
    }

    /// Change how blocks are produced while the node runs. An `interval`
    /// replaces the configured one; it only affects interval mode.
    pub async fn set_block_driver(
        &self,
        mode: BlockDriverMode,
        interval: Option<Duration>,
    ) -> Result<BlockDriverConfig, BlockProductionError> {
        self.request(1, |reply| Command::SetMode {
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
        self.request(count, |reply| Command::Produce {
            count,
            first,
            interval,
            reply,
        })
        .await
    }

    /// Send one command and await its reply within one timeout for the wait
    /// in the queue plus one for each of the `steps` it runs.
    async fn request<T>(
        &self,
        steps: u32,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, BlockProductionError> {
        let deadline = self.timeout.saturating_mul(steps.saturating_add(1));
        let exchange = async {
            let (reply, response) = oneshot::channel();
            self.commands
                .send(command(reply))
                .await
                .map_err(|_| BlockProductionError::DriverStopped)?;
            response
                .await
                .unwrap_or(Err(BlockProductionError::DriverStopped))
        };
        tokio::time::timeout(deadline, exchange)
            .await
            .unwrap_or(Err(BlockProductionError::TimedOut { timeout: deadline }))
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
    SetTimeStep {
        step: Option<u64>,
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
            Self::SetNextTimepoint { reply, .. }
            | Self::IncreaseTime { reply, .. }
            | Self::SetTimeStep { reply, .. } => reply.is_closed(),
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
///
/// Time increased while a next timepoint is pinned leaves that block at its
/// exact timepoint, as Anvil does, and moves every block after it.
///
/// With a fixed step, a block without a pinned timepoint takes its parent's
/// timepoint plus the step, plus any time increased since the last block.
/// The offset still follows each committed block, so clearing the step
/// continues from the tip instead of jumping back to the wall clock. While
/// the parent is one of a fresh chain's initial blocks (see
/// [`Parent::is_initial`]), the next block takes the wall clock, so a step
/// does not date the first block in 1970.
#[derive(Debug, Default, Clone)]
struct BlockClock {
    offset_nanos: i128,
    next: Option<u64>,
    increase_after_next: i128,
    step: Option<u64>,
    increase_since_last: u64,
}

impl BlockClock {
    fn timepoint_after(&self, parent: Parent, now: SystemTime) -> u64 {
        let clock = match (self.next, self.step) {
            (Some(pinned), _) => pinned,
            (None, Some(step)) if !parent.is_initial() => parent
                .timepoint
                .saturating_add(step)
                .saturating_add(self.increase_since_last),
            (None, _) => {
                let shifted = unix_nanos(now).saturating_add(self.offset_nanos);
                u64::try_from(shifted.max(0)).unwrap_or(u64::MAX)
            }
        };
        clock.max(parent.timepoint)
    }

    fn pin(&mut self, timepoint: u64, now: SystemTime) {
        self.next = Some(timepoint);
        self.offset_nanos = i128::from(timepoint) - unix_nanos(now);
        self.increase_after_next = 0;
        self.increase_since_last = 0;
    }

    fn advance(&mut self, nanos: u64) {
        if self.next.is_some() {
            self.increase_after_next = self.increase_after_next.saturating_add(i128::from(nanos));
        } else {
            self.offset_nanos = self.offset_nanos.saturating_add(i128::from(nanos));
            self.increase_since_last = self.increase_since_last.saturating_add(nanos);
        }
    }

    fn set_step(&mut self, step: Option<u64>) {
        self.step = step;
    }

    fn committed(&mut self, timepoint: u64, now: SystemTime) {
        if self.next.is_some() {
            let increase = self.increase_after_next;
            self.pin(timepoint, now);
            self.next = None;
            self.offset_nanos = self.offset_nanos.saturating_add(increase);
            self.increase_since_last = u64::try_from(increase.max(0)).unwrap_or(u64::MAX);
        } else {
            if self.step.is_some() {
                self.offset_nanos = i128::from(timepoint) - unix_nanos(now);
            }
            self.increase_since_last = 0;
        }
    }
}

/// The block a new block builds on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Parent {
    height: i64,
    pub(crate) timepoint: u64,
}

impl Parent {
    /// hog-devnode commits a fresh chain's genesis block at height 0 and its
    /// genesis ceremony (the bootstrap accounts) at height 1, both at
    /// timepoint 0. Blocks a user produces start at height 2.
    ///
    /// Checked against hog-devnode 0.1.34 and 0.1.35. A runtime that commits
    /// more initial blocks would date the first stepped block near 1970;
    /// `a_step_from_startup_still_dates_the_first_block_now` in
    /// `tests/real_block_time.rs` catches that when the runtime pin changes.
    const LAST_INITIAL_HEIGHT: i64 = 1;

    fn of(header: &ProposalHeader) -> Self {
        Self {
            height: *header.block_height,
            timepoint: *header.block_timepoint,
        }
    }

    /// Whether this is one of the blocks hog-devnode commits itself when it
    /// creates a chain. A block a user pins to timepoint 0 is not.
    fn is_initial(self) -> bool {
        self.height <= Self::LAST_INITIAL_HEIGHT && self.timepoint == 0
    }
}

/// The block driver's clock and tip as of its last commit or clock change.
///
/// Contract views run at the timepoint the next block would get. The view
/// path reads it here rather than asking the driver, so a view never waits
/// behind block production or costs a scheduler call. Until the driver
/// commits or changes its clock, the next block takes the wall clock, which
/// is what views use while this is empty.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedBlockClock(Arc<std::sync::Mutex<Option<(BlockClock, Parent)>>>);

impl SharedBlockClock {
    /// A clock frozen at `timepoint`, past the initial blocks.
    #[cfg(test)]
    pub(crate) fn frozen_at(timepoint: u64) -> Self {
        let mut clock = BlockClock::default();
        clock.set_step(Some(0));
        let shared = Self::default();
        shared.publish(
            &clock,
            Parent {
                height: Parent::LAST_INITIAL_HEIGHT + 1,
                timepoint,
            },
        );
        shared
    }

    fn publish(&self, clock: &BlockClock, tip: Parent) {
        *self.lock() = Some((clock.clone(), tip));
    }

    /// The timepoint the next block would get at `now`, once the driver has
    /// seen a tip.
    pub(crate) fn next_block_timepoint(&self, now: SystemTime) -> Option<TimePoint> {
        let state = self.lock();
        let (clock, tip) = state.as_ref()?;
        Some(TimePoint::new(clock.timepoint_after(*tip, now)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(BlockClock, Parent)>> {
        // The guarded value is replaced whole, so a poisoned lock still holds
        // a consistent snapshot.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        gateway: SchedGateway_SchedGateway1_Client,
        config: BlockDriverConfig,
        shared_clock: SharedBlockClock,
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
            gateway,
            config,
            clock: BlockClock {
                // `thruput node` refuses a step longer than u64 nanoseconds
                // (about 584 years). A longer step set by another embedder
                // saturates, so the next block takes the largest timepoint.
                step: config
                    .time_step
                    .map(|step| u64::try_from(step.as_nanos()).unwrap_or(u64::MAX)),
                ..BlockClock::default()
            },
            shared_clock,
            notify,
            instant,
            ticker: None,
        };
        driver.apply_mode();
        let join = tokio::spawn(driver.run(receiver, shutdown_rx));
        Self {
            trigger,
            producer: BlockProducer {
                commands,
                timeout: config.effective_manual_request_timeout(),
            },
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
    gateway: SchedGateway_SchedGateway1_Client,
    config: BlockDriverConfig,
    clock: BlockClock,
    shared_clock: SharedBlockClock,
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
        if let Ok(DriveOutcome::Committed {
            timepoint, height, ..
        }) = &result
        {
            self.clock.committed(*timepoint, SystemTime::now());
            self.shared_clock.publish(
                &self.clock,
                Parent {
                    height: *height,
                    timepoint: *timepoint,
                },
            );
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
                    .produce_requested(count, first, interval, &reply, shutdown)
                    .await;
                let _ = reply.send(result);
                step
            }
            Command::SetNextTimepoint { timepoint, reply } => {
                let (result, step) = self
                    .with_tip(shutdown, &reply, |clock, tip| {
                        if timepoint < tip.timepoint {
                            return Err(BlockProductionError::TimepointBeforeTip {
                                requested: timepoint,
                                tip: tip.timepoint,
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
                // Change the clock only after the tip read succeeds, so a
                // failed request leaves time unchanged and is safe to retry.
                let (result, step) = self
                    .with_tip(shutdown, &reply, |clock, tip| {
                        clock.advance(nanos);
                        Ok(TimePoint::new(
                            clock.timepoint_after(tip, SystemTime::now()),
                        ))
                    })
                    .await;
                let _ = reply.send(result);
                step
            }
            Command::SetTimeStep { step, reply } => {
                // As for time increases, change the clock only after the tip
                // read succeeds.
                let (result, step_outcome) = self
                    .with_tip(shutdown, &reply, |clock, tip| {
                        clock.set_step(step);
                        Ok(TimePoint::new(
                            clock.timepoint_after(tip, SystemTime::now()),
                        ))
                    })
                    .await;
                if result.is_ok() {
                    // `BlockProducer::set_block_driver` returns this config,
                    // so keep its step current.
                    self.config.time_step = step.map(Duration::from_nanos);
                    tracing::info!(step = ?self.config.time_step, "hogd-dev block time step changed");
                }
                let _ = reply.send(result);
                step_outcome
            }
            Command::SetMode {
                mode,
                interval,
                reply,
            } => {
                // Nothing awaits between the abandoned-request check in
                // `handle` and this change, so a caller that has gone never
                // has its mode applied.
                let entering_instant = mode == BlockDriverMode::Instant
                    && self.config.mode != BlockDriverMode::Instant;
                self.config.mode = mode;
                if let Some(interval) = interval {
                    self.config.interval = interval;
                }
                self.apply_mode();
                tracing::info!(?mode, interval = ?self.config.effective_interval(), "hogd-dev block driver mode changed");
                let step = if entering_instant {
                    self.mine_pending_on_entering_instant(shutdown).await
                } else {
                    Step::Continue
                };
                let _ = reply.send(Ok(self.config));
                step
            }
        }
    }

    /// Like Anvil re-enabling automine: transactions already pending get a
    /// block at once instead of waiting for the next submit. No pending
    /// transactions means no block.
    /// The mode is already applied, so shutdown during the pending count
    /// stops the driver at once.
    async fn mine_pending_on_entering_instant(&self, shutdown: &mut oneshot::Receiver<()>) -> Step {
        let timeout = self.config.effective_manual_request_timeout();
        let count = tokio::select! {
            biased;
            _ = &mut *shutdown => return Step::Stop,
            count = tokio::time::timeout(timeout, self.gateway.schedgateway_pending_tx_count()) => count,
        };
        match count {
            Ok(Ok(count)) if count > 0 => self.notify.notify_one(),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, "pending count failed; pending transactions wait for the next submit");
            }
            Err(_) => {
                tracing::warn!(
                    "pending count timed out; pending transactions wait for the next submit"
                );
            }
        }
        Step::Continue
    }

    /// Read the tip's timepoint, then apply `update` to the clock.
    ///
    /// When the caller stops waiting during the tip read, its deadline has
    /// passed and it was told the outcome is unknown. The clock is then left
    /// unchanged, so a retry cannot apply the change twice.
    async fn with_tip<T, R>(
        &mut self,
        shutdown: &mut oneshot::Receiver<()>,
        reply: &Reply<R>,
        update: impl FnOnce(&mut BlockClock, Parent) -> Result<T, BlockProductionError>,
    ) -> (Result<T, BlockProductionError>, Step) {
        let timeout = self.config.effective_manual_request_timeout();
        let tip = tokio::select! {
            biased;
            _ = &mut *shutdown => return (Err(BlockProductionError::DriverStopped), Step::Stop),
            tip = tokio::time::timeout(timeout, tip_parent(&self.scheduler)) => tip,
        };
        if reply.is_closed() {
            return (
                Err(BlockProductionError::TimedOut { timeout }),
                Step::Continue,
            );
        }
        let result = match tip {
            Ok(Ok(tip)) => {
                let result = update(&mut self.clock, tip);
                if result.is_ok() {
                    self.shared_clock.publish(&self.clock, tip);
                }
                result
            }
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
        reply: &Reply<Vec<ProducedBlock>>,
        shutdown: &mut oneshot::Receiver<()>,
    ) -> (Result<Vec<ProducedBlock>, BlockProductionError>, Step) {
        let timeout = self.config.effective_manual_request_timeout();
        let mut blocks = Vec::new();
        let mut requested = first;
        for index in 0..count {
            // A caller that stopped waiting was told the outcome is unknown;
            // start no further blocks for it.
            if index > 0 && reply.is_closed() {
                return (
                    Err(BlockProductionError::TimedOut { timeout }),
                    Step::Continue,
                );
            }
            let result = tokio::select! {
                biased;
                _ = &mut *shutdown => {
                    return (Err(BlockProductionError::OutcomeUnknown), Step::Stop);
                }
                result = tokio::time::timeout(timeout, drive_one_block(&self.scheduler, |parent| {
                    match requested {
                        Some(requested) if requested < parent.timepoint => {
                            Err(BlockProductionError::TimepointBeforeTip {
                                requested,
                                tip: parent.timepoint,
                            })
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
            self.shared_clock.publish(
                &self.clock,
                Parent {
                    height: block.height,
                    timepoint: *block.timepoint,
                },
            );
            let another_follows = index + 1 < count;
            requested = match interval {
                Some(interval) if another_follows => {
                    match (*block.timepoint).checked_add(interval) {
                        Some(next) => Some(next),
                        None => {
                            return (Err(BlockProductionError::TimepointOverflow), Step::Continue);
                        }
                    }
                }
                _ => None,
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

pub(crate) async fn tip_parent(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
) -> Result<Parent, BlockProductionError> {
    match scheduler.schedcoord_get_tip().await.map_err(|source| {
        BlockProductionError::SchedulerRpc {
            op: "schedcoord_get_tip",
            source,
        }
    })? {
        SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => {
            Ok(Parent::of(&db_tip.value.proposal_header))
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
    timepoint: impl FnOnce(Parent) -> Result<u64, BlockProductionError>,
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

    let timepoint = timepoint(Parent::of(&db_tip.value.proposal_header))?;
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

const PROPOSAL_TXS: &[u8] = b"ProposalTxs";
const PARALLEL_BATCH: &[u8] = b"ParallelBatch";
const PROPOSAL_TXS_DOMAIN_SEPARATOR_CXS: [u8; domain_separator_cxs_size(PROPOSAL_TXS)] =
    domain_separator_cxs(PROPOSAL_TXS);
const PARALLEL_BATCH_DOMAIN_SEPARATOR_CXS: [u8; domain_separator_cxs_size(PARALLEL_BATCH)] =
    domain_separator_cxs(PARALLEL_BATCH);

/// The size of [`domain_separator_cxs`] for `name`.
const fn domain_separator_cxs_size(name: &[u8]) -> usize {
    assert!(
        name.len() <= 256,
        "a domain separator name is at most 256 bytes"
    );
    #[allow(clippy::cast_possible_truncation)] // checked above
    let length = xdrs::cxs::const_writer::size_u32(name.len() as u32);
    ExtensionPoint::_0.const_cxs_size() + length + name.len()
}

/// `cxs(DomainSeparator(name))`, as hogd builds it in
/// `hogd/crypto/domain_separators.h`.
///
/// `thruput_xdr::preimage::DomainSeparator` holds its name in a `BytesM`,
/// which a `const` cannot build, so this writes the struct's two fields
/// with the const writer. `domain_separators_encode_as_the_xdr_type` checks
/// the result against the type.
const fn domain_separator_cxs<const N: usize>(name: &[u8]) -> [u8; N] {
    let mut writer = xdrs::cxs::ConstWriter::<N>::new();
    ExtensionPoint::_0.const_write_cxs(&mut writer);
    writer.write_opaque_variable(name);
    writer.into_bytes()
}

// Matches hogd's proposal_txs_compatibility_hash (hogd/xdr/proposal_hash.h):
//   SHA-256(cxs(DomainSeparator("ProposalTxs")) || SHA-256(base) ||
//           batch_hash(signed_hash) || batch_hash(nondeterministic_data))
// where base is the ProposalTxs with both batches emptied. hogd rejects a
// commit whose hash differs, so this changes only in lockstep with hogd.
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

    let base_hash =
        hash_xdr(&base).map_err(|source| BlockProductionError::ConsensusHashEncoding {
            value: "proposal transactions base",
            source,
        })?;

    let mut hasher = Sha256::new();
    hasher.update(PROPOSAL_TXS_DOMAIN_SEPARATOR_CXS);
    hasher.update(base_hash.into_inner());
    hasher.update(hash_parallel_batch_compatibility(
        "signed_hash",
        &proposal_txs.signed_hash,
    )?);
    hasher.update(hash_parallel_batch_compatibility(
        "nondeterministic_data",
        &proposal_txs.nondeterministic_data,
    )?);
    Ok(ProposalTxsHash::new(SHA256Hash::from(<[u8; 32]>::from(
        hasher.finalize(),
    ))))
}

// SHA-256(cxs(DomainSeparator("ParallelBatch")) || SHA-256(index) ||
//         SHA-256(segment_0) || ... || SHA-256(segment_{n-1}))
// where index is the batch with its payload emptied.
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

    let mut index = batch.clone();
    index.xdr.0.clear();
    let index_hash =
        hash_xdr(&index).map_err(|source| BlockProductionError::ConsensusHashEncoding {
            value: batch_name,
            source,
        })?;

    let mut hasher = Sha256::new();
    hasher.update(PARALLEL_BATCH_DOMAIN_SEPARATOR_CXS);
    hasher.update(index_hash.into_inner());

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
        hasher.update(Sha256::digest(segment));
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
    Ok(hasher.finalize().into())
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

    use super::{BlockClock, Parent, SharedBlockClock};
    use thruput_xdr::types::TimePoint;

    const DAY: u64 = 24 * 60 * 60 * 1_000_000_000;
    const NOW: u64 = 1_760_000_000_123_456_789;

    fn at(nanos: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_nanos(nanos)
    }

    #[test]
    fn takes_the_wall_clock_in_unix_nanoseconds() {
        assert_eq!(
            BlockClock::default().timepoint_after(block(0), at(NOW)),
            NOW
        );
    }

    #[test]
    fn never_moves_behind_the_parent() {
        let clock = BlockClock::default();
        assert_eq!(clock.timepoint_after(block(NOW), at(NOW - 1)), NOW);
    }

    #[test]
    fn a_clock_before_the_epoch_keeps_the_parent() {
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(
            BlockClock::default().timepoint_after(block(42), before_epoch),
            42
        );
    }

    #[test]
    fn a_pinned_timepoint_is_used_exactly_once() {
        let mut clock = BlockClock::default();
        clock.pin(NOW + 3 * DAY, at(NOW));

        let pinned = clock.timepoint_after(block(0), at(NOW + 5));
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
            clock.timepoint_after(block(NOW + 3 * DAY), at(NOW + second)),
            NOW + 3 * DAY + second
        );
    }

    #[test]
    fn a_pinned_timepoint_in_the_past_continues_from_there() {
        let mut clock = BlockClock::default();
        clock.pin(NOW - DAY, at(NOW));
        clock.committed(NOW - DAY, at(NOW));

        assert_eq!(
            clock.timepoint_after(block(0), at(NOW + 10)),
            NOW - DAY + 10
        );
    }

    #[test]
    fn increasing_time_shifts_every_later_block() {
        let mut clock = BlockClock::default();
        clock.advance(2 * DAY);
        clock.advance(DAY);

        assert_eq!(clock.timepoint_after(block(0), at(NOW)), NOW + 3 * DAY);
        assert_eq!(
            clock.timepoint_after(block(0), at(NOW + 7)),
            NOW + 3 * DAY + 7
        );
    }

    #[test]
    fn increasing_time_keeps_a_pinned_next_block() {
        let mut clock = BlockClock::default();
        clock.pin(NOW + DAY, at(NOW));
        clock.advance(DAY);

        assert_eq!(clock.timepoint_after(block(0), at(NOW)), NOW + DAY);
    }

    #[test]
    fn an_increase_after_a_pin_moves_the_blocks_after_the_pinned_one() {
        let mut clock = BlockClock::default();
        clock.pin(NOW + DAY, at(NOW));
        clock.advance(2 * DAY);

        let pinned = clock.timepoint_after(block(0), at(NOW + 5));
        assert_eq!(pinned, NOW + DAY);
        clock.committed(pinned, at(NOW + 5));

        // The block after it is the pinned time plus the increase plus the
        // wall-clock time since the commit.
        assert_eq!(
            clock.timepoint_after(block(pinned), at(NOW + 12)),
            NOW + 3 * DAY + 7
        );
    }

    /// A parent block a user produced, at `timepoint`.
    fn block(timepoint: u64) -> Parent {
        Parent {
            height: 2,
            timepoint,
        }
    }

    fn stepped(step: u64) -> BlockClock {
        let mut clock = BlockClock::default();
        clock.set_step(Some(step));
        clock
    }

    #[test]
    fn a_step_follows_the_parent_not_the_wall_clock() {
        let mut clock = stepped(1_000);
        assert_eq!(
            clock.timepoint_after(block(NOW), at(NOW + 5 * DAY)),
            NOW + 1_000
        );
        clock.committed(NOW + 1_000, at(NOW + 5 * DAY));
        assert_eq!(
            clock.timepoint_after(block(NOW + 1_000), at(NOW + 9 * DAY)),
            NOW + 2_000
        );
    }

    #[test]
    fn a_zero_step_freezes_the_clock() {
        let mut clock = stepped(0);
        for wall in [NOW, NOW + 1, NOW + DAY] {
            assert_eq!(clock.timepoint_after(block(NOW), at(wall)), NOW);
            clock.committed(NOW, at(wall));
        }
    }

    #[test]
    fn a_pin_wins_and_the_step_continues_from_it() {
        let mut clock = stepped(0);
        clock.pin(NOW + DAY, at(NOW));
        let pinned = clock.timepoint_after(block(NOW), at(NOW + 5));
        assert_eq!(pinned, NOW + DAY);
        clock.committed(pinned, at(NOW + 5));

        // Frozen at the pinned time, however much wall time passes.
        assert_eq!(
            clock.timepoint_after(block(pinned), at(NOW + 7 * DAY)),
            NOW + DAY
        );
    }

    #[test]
    fn an_increase_moves_the_next_stepped_block_once() {
        let mut clock = stepped(0);
        clock.advance(DAY);
        let next = clock.timepoint_after(block(NOW), at(NOW));
        assert_eq!(next, NOW + DAY);
        clock.committed(next, at(NOW));
        assert_eq!(clock.timepoint_after(block(next), at(NOW + 1)), NOW + DAY);
    }

    #[test]
    fn an_increase_after_a_pin_applies_to_the_stepped_block_after_it() {
        let mut clock = stepped(0);
        clock.pin(NOW + DAY, at(NOW));
        clock.advance(2 * DAY);
        let pinned = clock.timepoint_after(block(NOW), at(NOW));
        assert_eq!(pinned, NOW + DAY);
        clock.committed(pinned, at(NOW));
        let next = clock.timepoint_after(block(pinned), at(NOW + 1));
        assert_eq!(next, NOW + 3 * DAY);
        clock.committed(next, at(NOW + 1));
        assert_eq!(
            clock.timepoint_after(block(next), at(NOW + 2)),
            NOW + 3 * DAY
        );
    }

    #[test]
    fn increases_from_wall_clock_blocks_do_not_carry_into_a_later_step() {
        let mut clock = BlockClock::default();
        clock.advance(DAY);
        let next = clock.timepoint_after(block(0), at(NOW));
        clock.committed(next, at(NOW));
        clock.set_step(Some(0));
        assert_eq!(clock.timepoint_after(block(next), at(NOW + 1)), next);
    }

    #[test]
    fn the_first_block_after_genesis_takes_the_wall_clock() {
        let clock = stepped(0);
        // The genesis block and hog-devnode's genesis ceremony block.
        for height in [0, Parent::LAST_INITIAL_HEIGHT] {
            let initial = Parent {
                height,
                timepoint: 0,
            };
            assert_eq!(clock.timepoint_after(initial, at(NOW)), NOW);
        }
    }

    #[test]
    fn a_zero_time_block_after_genesis_still_steps() {
        // `dev_produceBlock` accepts a timepoint of 0 on a fresh chain; only
        // hog-devnode's own initial blocks fall back to the wall clock.
        assert_eq!(stepped(0).timepoint_after(block(0), at(NOW)), 0);
        assert_eq!(stepped(5).timepoint_after(block(0), at(NOW)), 5);
    }

    #[test]
    fn clearing_the_step_continues_from_the_tip() {
        // A simulated chain in the past: frozen at 2000-01-01.
        const Y2000: u64 = 946_684_800_000_000_000;
        let mut clock = stepped(0);
        clock.pin(Y2000, at(NOW));
        clock.committed(Y2000, at(NOW));
        clock.committed(Y2000, at(NOW + DAY));
        clock.set_step(None);

        // Continues from the tip plus the wall time since its commit, not
        // from today.
        assert_eq!(
            clock.timepoint_after(block(Y2000), at(NOW + DAY + 9)),
            Y2000 + 9
        );
    }

    #[test]
    fn a_step_saturates_instead_of_wrapping() {
        let clock = stepped(u64::MAX);
        assert_eq!(clock.timepoint_after(block(NOW), at(NOW)), u64::MAX);
    }

    #[test]
    fn the_clock_saturates_instead_of_wrapping() {
        let mut clock = BlockClock::default();
        clock.advance(u64::MAX);
        clock.advance(u64::MAX);

        assert_eq!(clock.timepoint_after(block(0), at(NOW)), u64::MAX);
    }

    #[test]
    fn views_have_no_clock_until_the_driver_publishes_one() {
        assert_eq!(
            SharedBlockClock::default().next_block_timepoint(at(NOW)),
            None
        );
    }

    #[test]
    fn views_see_the_next_block_time_of_the_published_clock() {
        let shared = SharedBlockClock::default();
        let mut clock = stepped(0);
        shared.publish(&clock, block(NOW));
        assert_eq!(
            shared.next_block_timepoint(at(NOW + DAY)),
            Some(TimePoint::new(NOW))
        );

        clock.pin(NOW + 5, at(NOW));
        shared.publish(&clock, block(NOW));
        assert_eq!(
            shared.next_block_timepoint(at(NOW + DAY)),
            Some(TimePoint::new(NOW + 5))
        );
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
    fn domain_separators_encode_as_the_xdr_type() {
        use thruput_xdr::preimage::DomainSeparator;
        for (name, encoded) in [
            (
                super::PROPOSAL_TXS,
                &super::PROPOSAL_TXS_DOMAIN_SEPARATOR_CXS[..],
            ),
            (
                super::PARALLEL_BATCH,
                &super::PARALLEL_BATCH_DOMAIN_SEPARATOR_CXS[..],
            ),
        ] {
            let separator = DomainSeparator {
                ext: ExtensionPoint::_0,
                domain_separator: BytesM(name.to_vec()),
            };
            assert_eq!(xdrs::cxs::encode(&separator).unwrap(), encoded);
        }
    }

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
            // hogd's Mempool.ProposalTxsCompatibilityHashGoldenVector.
            SHA256Hash::from([
                0xac, 0x7e, 0x6e, 0x65, 0xe3, 0xf3, 0x6e, 0xc7, 0x88, 0x45, 0xdd, 0xf3, 0xf3, 0xd9,
                0x5a, 0xcf, 0x99, 0xea, 0xdb, 0xbd, 0xbc, 0xfd, 0x39, 0x1e, 0x44, 0x23, 0x9a, 0x7d,
                0xc4, 0xb5, 0x2c, 0x03,
            ])
        );
    }

    fn one_segment(xdr: [u8; 4]) -> ParallelBatch {
        ParallelBatch {
            segment_tokens: VecM(vec![1]),
            segment_bytes: VecM(vec![4]),
            xdr: BytesM(xdr.to_vec()),
        }
    }

    fn hash_of(signed_hash: ParallelBatch, nondeterministic_data: ParallelBatch) -> SHA256Hash {
        proposal_txs_compatibility_hash(&ProposalTxs {
            ext: ExtensionPoint::_0,
            signed_hash: ParallelSignedTransactionHashBatch::new(signed_hash),
            nondeterministic_data: ParallelNondeterministicProposalDataBatch::new(
                nondeterministic_data,
            ),
        })
        .expect("bug: valid proposal transactions should hash")
        .into_inner()
    }

    #[test]
    fn proposal_txs_hash_binds_segment_order() {
        let two_segments = |xdr: [u8; 8]| ParallelBatch {
            segment_tokens: VecM(vec![1, 1]),
            segment_bytes: VecM(vec![4, 4]),
            xdr: BytesM(xdr.to_vec()),
        };

        assert_ne!(
            hash_of(
                two_segments([0, 0, 0, 1, 0, 0, 0, 2]),
                ParallelBatch::empty()
            ),
            hash_of(
                two_segments([0, 0, 0, 2, 0, 0, 0, 1]),
                ParallelBatch::empty()
            ),
        );
    }

    #[test]
    fn proposal_txs_hash_binds_batch_positions() {
        let first = one_segment([0, 0, 0, 1]);
        let second = one_segment([0, 0, 0, 2]);

        assert_ne!(
            hash_of(first.clone(), second.clone()),
            hash_of(second, first),
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
