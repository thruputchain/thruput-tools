//! Bounded internal batching for local-node transaction submission.

use std::{
    fmt,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use groundhog_json_rpc_server::{RpcRequestContext, RpcServerError};
use groundhog_scheduler::{
    ParallelBatch, parallel::ParallelSignedTransactionBatch,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::AbortHandle,
};
use xdr_common::{BytesM, VecM};

use crate::{block_driver::BlockDriverTrigger, request_deadline::within_request_deadline};
use groundhog_x23_transport::HOGD_LARGE_MESSAGE_SIZE;

const MAX_BATCH_SIZE: usize = 1_000;
const INGRESS_QUEUE_DEPTH: usize = 10_000;
const XDR_ENVELOPE_HEADROOM: usize = 64 * 1024;
const MAX_BATCH_BYTES: usize = HOGD_LARGE_MESSAGE_SIZE as usize - XDR_ENVELOPE_HEADROOM;
const INGRESS_QUEUE_BYTES: usize = HOGD_LARGE_MESSAGE_SIZE as usize;
const BACKEND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DEADLINE_SKEW: Duration = Duration::from_secs(1);

#[async_trait]
trait SubmitSink: Send + Sync + 'static {
    async fn submit(&self, batch: ParallelSignedTransactionBatch) -> Result<(), RpcServerError>;
}

struct SchedulerSink {
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
}

#[async_trait]
impl SubmitSink for SchedulerSink {
    async fn submit(&self, batch: ParallelSignedTransactionBatch) -> Result<(), RpcServerError> {
        self.scheduler
            .schedulercoord_submit_unvetted(&batch)
            .await
            .map_err(|error| {
                RpcServerError::BackendUnavailable(format!(
                    "hogd-dev scheduler submit failed: {error}"
                ))
            })
    }
}

struct Item {
    tx_bytes: Vec<u8>,
    backend_deadline: Instant,
    reply: oneshot::Sender<Result<(), RpcServerError>>,
    _ingress_bytes: OwnedSemaphorePermit,
}

struct Inner {
    ingress: mpsc::Sender<Item>,
    ingress_bytes: Arc<Semaphore>,
    block_driver: Arc<RwLock<Option<BlockDriverTrigger>>>,
    worker: AbortHandle,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

/// Cloneable handle that combines concurrent public submissions into scheduler batches.
#[derive(Clone)]
pub(crate) struct SubmitBatcher {
    inner: Arc<Inner>,
}

impl fmt::Debug for SubmitBatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubmitBatcher").finish_non_exhaustive()
    }
}

impl SubmitBatcher {
    /// Start a bounded submission worker on the current Tokio runtime.
    ///
    /// Clones share one queue and worker, with at most one scheduler batch in flight.
    /// Dropping the last handle aborts the worker rather than draining queued work.
    pub(crate) fn spawn(scheduler: SchedulerCoord_SchedulerCoord1_Client) -> Self {
        Self::spawn_with(
            Arc::new(SchedulerSink { scheduler }),
            MAX_BATCH_SIZE,
            MAX_BATCH_BYTES,
            INGRESS_QUEUE_DEPTH,
            INGRESS_QUEUE_BYTES,
        )
    }

    fn spawn_with(
        sink: Arc<dyn SubmitSink>,
        max_batch_size: usize,
        max_batch_bytes: usize,
        ingress_queue_depth: usize,
        ingress_queue_bytes: usize,
    ) -> Self {
        let (ingress_tx, ingress_rx) = mpsc::channel(ingress_queue_depth);
        let ingress_bytes = Arc::new(Semaphore::new(ingress_queue_bytes));
        let block_driver = Arc::new(RwLock::new(None));
        let worker = tokio::spawn(run(
            ingress_rx,
            sink,
            max_batch_size,
            max_batch_bytes,
            block_driver.clone(),
        ))
        .abort_handle();
        Self {
            inner: Arc::new(Inner {
                ingress: ingress_tx,
                ingress_bytes,
                block_driver,
                worker,
            }),
        }
    }

    /// Queue validated transaction bytes and await the shared scheduler submission result.
    ///
    /// Exhausted queue capacity or payload-byte budget returns `Overloaded` immediately.
    /// The backend deadline is capped at 30 seconds from request start, or the earlier
    /// request deadline. Canceled callers are skipped before packing where possible;
    /// canceling this future does not cancel a scheduler batch already in flight.
    /// Success confirms scheduler RPC completion, not individual transaction commitment.
    #[tracing::instrument(skip_all, fields(tx_bytes_len = tx_bytes.len()))]
    pub(crate) async fn submit(
        &self,
        context: RpcRequestContext,
        tx_bytes: Vec<u8>,
    ) -> Result<(), RpcServerError> {
        if tx_bytes.len() > MAX_BATCH_BYTES {
            return Err(RpcServerError::TxInvalid("transaction too large".into()));
        }
        let backend_deadline = context
            .capped_from_start(BACKEND_TIMEOUT)
            .deadline()
            .ok_or_else(|| {
                RpcServerError::BackendUnavailable(
                    "dev-node submit deadline could not be calculated".into(),
                )
            })?;
        if backend_deadline <= Instant::now() {
            return Err(RpcServerError::BackendTimeout);
        }
        let byte_count = u32::try_from(tx_bytes.len())
            .map_err(|_| RpcServerError::TxInvalid("transaction too large".into()))?;
        let ingress_bytes = self
            .inner
            .ingress_bytes
            .clone()
            .try_acquire_many_owned(byte_count)
            .map_err(|_| RpcServerError::Overloaded("dev-node submit queue full".into()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.inner
            .ingress
            .try_send(Item {
                tx_bytes,
                backend_deadline,
                reply: reply_tx,
                _ingress_bytes: ingress_bytes,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    RpcServerError::Overloaded("dev-node submit queue full".into())
                }
                mpsc::error::TrySendError::Closed(_) => {
                    RpcServerError::BackendUnavailable("dev-node submit batcher stopped".into())
                }
            })?;

        within_request_deadline(context, async {
            reply_rx.await.map_err(|_| {
                RpcServerError::BackendUnavailable("dev-node submit batcher stopped".into())
            })?
        })
        .await
    }

    /// Install or replace the block-driver trigger shared by all batcher clones.
    ///
    /// The worker invokes the current trigger once after each successful scheduler batch.
    pub(crate) fn set_block_driver(&self, block_driver: BlockDriverTrigger) {
        *self
            .inner
            .block_driver
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(block_driver);
    }

    #[cfg(test)]
    fn abort(&self) {
        self.inner.worker.abort();
    }
}

async fn run(
    mut ingress: mpsc::Receiver<Item>,
    sink: Arc<dyn SubmitSink>,
    max_batch_size: usize,
    max_batch_bytes: usize,
    block_driver: Arc<RwLock<Option<BlockDriverTrigger>>>,
) {
    let mut deferred = None;
    loop {
        let first = if let Some(item) = deferred.take() {
            item
        } else {
            let Some(item) = ingress.recv().await else {
                return;
            };
            item
        };
        let mut items = Vec::with_capacity(max_batch_size.min(64));
        let mut batch_bytes = first.tx_bytes.len();
        items.push(first);
        while items.len() < max_batch_size {
            let Ok(item) = ingress.try_recv() else {
                break;
            };
            if item.reply.is_closed() {
                continue;
            }
            if item.backend_deadline <= Instant::now() {
                let _ = item.reply.send(Err(RpcServerError::BackendTimeout));
                continue;
            }
            let Some(next_bytes) = batch_bytes.checked_add(item.tx_bytes.len()) else {
                deferred = Some(item);
                break;
            };
            if next_bytes > max_batch_bytes {
                deferred = Some(item);
                break;
            }
            batch_bytes = next_bytes;
            items.push(item);
        }

        let live = live_items(items, Instant::now());
        if live.is_empty() {
            continue;
        }

        for cohort in deadline_cohorts(live) {
            let cohort = live_items(cohort, Instant::now());
            if cohort.is_empty() {
                continue;
            }
            let deadline = cohort
                .iter()
                .map(|item| item.backend_deadline)
                .min()
                .expect("bug: non-empty deadline cohort should have a deadline");
            tracing::debug!(
                batch_size = cohort.len(),
                "submitting dev-node transaction batch"
            );
            let result = match pack(&cohort) {
                Ok(batch) => submit_before(&sink, batch, deadline).await,
                Err(error) => Err(error),
            };
            if result.is_ok()
                && let Some(trigger) = block_driver
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            {
                trigger.trigger_after_submit();
            }
            for item in cohort {
                let _ = item.reply.send(result.clone());
            }
        }
    }
}

async fn submit_before(
    sink: &Arc<dyn SubmitSink>,
    batch: ParallelSignedTransactionBatch,
    deadline: Instant,
) -> Result<(), RpcServerError> {
    let submit = sink.submit(batch);
    tokio::time::timeout_at(deadline.into(), submit)
        .await
        .map_err(|_| RpcServerError::BackendTimeout)?
}

fn live_items(items: Vec<Item>, now: Instant) -> Vec<Item> {
    items
        .into_iter()
        .filter_map(|item| {
            if item.reply.is_closed() {
                return None;
            }
            if item.backend_deadline <= now {
                let _ = item.reply.send(Err(RpcServerError::BackendTimeout));
                return None;
            }
            Some(item)
        })
        .collect()
}

fn deadline_cohorts(items: Vec<Item>) -> Vec<Vec<Item>> {
    let mut cohorts: Vec<Vec<Item>> = Vec::new();
    let mut bounds: Option<(Instant, Instant)> = None;
    for item in items {
        let deadline = item.backend_deadline;
        let (min, max) = bounds.map_or((deadline, deadline), |(min, max)| {
            (min.min(deadline), max.max(deadline))
        });
        if bounds.is_some() && max.saturating_duration_since(min) <= MAX_DEADLINE_SKEW {
            bounds = Some((min, max));
            cohorts
                .last_mut()
                .expect("bug: current deadline cohort should exist")
                .push(item);
        } else {
            bounds = Some((deadline, deadline));
            cohorts.push(vec![item]);
        }
    }
    cohorts
}

fn pack(items: &[Item]) -> Result<ParallelSignedTransactionBatch, RpcServerError> {
    let mut segment_tokens = Vec::with_capacity(items.len());
    let mut segment_bytes = Vec::with_capacity(items.len());
    let total_bytes = items.iter().map(|item| item.tx_bytes.len()).sum();
    let mut xdr = Vec::with_capacity(total_bytes);
    for item in items {
        let tx_len = u32::try_from(item.tx_bytes.len())
            .map_err(|_| RpcServerError::TxInvalid("transaction too large".into()))?;
        segment_tokens.push(1);
        segment_bytes.push(tx_len);
        xdr.extend_from_slice(&item.tx_bytes);
    }
    Ok(ParallelSignedTransactionBatch::new(ParallelBatch {
        segment_tokens: VecM(segment_tokens),
        segment_bytes: VecM(segment_bytes),
        xdr: BytesM(xdr),
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::sync::{Notify, Semaphore};

    use super::*;

    struct GatedSink {
        batches: Mutex<Vec<ParallelSignedTransactionBatch>>,
        called: Notify,
        permits: Semaphore,
    }

    impl GatedSink {
        fn new() -> Self {
            Self {
                batches: Mutex::new(Vec::new()),
                called: Notify::new(),
                permits: Semaphore::new(0),
            }
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        batches: Mutex<Vec<ParallelSignedTransactionBatch>>,
    }

    #[async_trait]
    impl SubmitSink for RecordingSink {
        async fn submit(
            &self,
            batch: ParallelSignedTransactionBatch,
        ) -> Result<(), RpcServerError> {
            self.batches
                .lock()
                .expect("bug: test batch lock should be available")
                .push(batch);
            Ok(())
        }
    }

    #[async_trait]
    impl SubmitSink for GatedSink {
        async fn submit(
            &self,
            batch: ParallelSignedTransactionBatch,
        ) -> Result<(), RpcServerError> {
            self.batches
                .lock()
                .expect("bug: test batch lock should be available")
                .push(batch);
            self.called.notify_one();
            self.permits
                .acquire()
                .await
                .expect("bug: test semaphore should remain open")
                .forget();
            Ok(())
        }
    }

    fn item(tx_bytes: Vec<u8>) -> Item {
        item_with_reply(tx_bytes, Instant::now() + BACKEND_TIMEOUT).0
    }

    fn item_with_reply(
        tx_bytes: Vec<u8>,
        backend_deadline: Instant,
    ) -> (Item, oneshot::Receiver<Result<(), RpcServerError>>) {
        let (reply, receiver) = oneshot::channel();
        let ingress_bytes = Arc::new(Semaphore::new(tx_bytes.len()))
            .try_acquire_many_owned(u32::try_from(tx_bytes.len()).expect("bug: test byte length"))
            .expect("bug: test byte permit should be available");
        (
            Item {
                tx_bytes,
                backend_deadline,
                reply,
                _ingress_bytes: ingress_bytes,
            },
            receiver,
        )
    }

    async fn wait_for_queue_capacity(batcher: &SubmitBatcher, expected: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while batcher.inner.ingress.capacity() != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bug: test submissions should reach the queue");
    }

    #[test]
    fn pack_preserves_transaction_segments_and_bytes() {
        let batch = pack(&[item(vec![1, 2]), item(vec![3, 4, 5])])
            .expect("bug: test transactions should pack");

        assert_eq!(batch.segment_tokens.0, vec![1, 1]);
        assert_eq!(batch.segment_bytes.0, vec![2, 3]);
        assert_eq!(batch.xdr.0, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn count_limit_splits_drained_items_without_reordering() {
        let sink = Arc::new(RecordingSink::default());
        let (ingress_tx, ingress_rx) = mpsc::channel(3);
        let mut replies = Vec::new();
        for byte in 1..=3 {
            let (item, reply) = item_with_reply(vec![byte], Instant::now() + BACKEND_TIMEOUT);
            ingress_tx
                .try_send(item)
                .expect("bug: test item should enter queue");
            replies.push(reply);
        }
        drop(ingress_tx);

        run(
            ingress_rx,
            sink.clone(),
            2,
            100,
            Arc::new(RwLock::new(None)),
        )
        .await;

        let batches: Vec<_> = sink
            .batches
            .lock()
            .expect("bug: test batch lock should be available")
            .iter()
            .map(|batch| batch.xdr.0.clone())
            .collect();
        assert_eq!(batches, vec![vec![1, 2], vec![3]]);
        assert!(
            replies
                .into_iter()
                .all(|mut reply| reply.try_recv().is_ok())
        );
    }

    #[tokio::test]
    async fn byte_limit_splits_drained_items_without_reordering() {
        let sink = Arc::new(RecordingSink::default());
        let (ingress_tx, ingress_rx) = mpsc::channel(3);
        let mut replies = Vec::new();
        for bytes in [vec![1, 1, 1], vec![2, 2], vec![3, 3]] {
            let (item, reply) = item_with_reply(bytes, Instant::now() + BACKEND_TIMEOUT);
            ingress_tx
                .try_send(item)
                .expect("bug: test item should enter queue");
            replies.push(reply);
        }
        drop(ingress_tx);

        run(ingress_rx, sink.clone(), 10, 4, Arc::new(RwLock::new(None))).await;

        let batches: Vec<_> = sink
            .batches
            .lock()
            .expect("bug: test batch lock should be available")
            .iter()
            .map(|batch| batch.xdr.0.clone())
            .collect();
        assert_eq!(batches, vec![vec![1, 1, 1], vec![2, 2, 3, 3]]);
        assert!(
            replies
                .into_iter()
                .all(|mut reply| reply.try_recv().is_ok())
        );
    }

    #[test]
    fn deadline_cohorts_preserve_ingress_order() {
        let now = Instant::now();
        let items = vec![
            item_with_reply(vec![1], now + Duration::from_secs(10)).0,
            item_with_reply(vec![2], now + Duration::from_secs(5)).0,
            item_with_reply(vec![3], now + Duration::from_secs(10)).0,
        ];

        let order: Vec<_> = deadline_cohorts(items)
            .into_iter()
            .flatten()
            .map(|item| item.tx_bytes[0])
            .collect();

        assert_eq!(order, vec![1, 2, 3]);
    }

    #[test]
    fn deadline_cohorts_bound_min_and_max_without_reordering() {
        let now = Instant::now();
        // Exercise both bound updates, inclusive skew, and reset after each split.
        for offsets in [
            [2_000, 1_500, 2_500, 2_501, 3_501, 3_502],
            [2_000, 2_500, 1_500, 1_499, 499, 498],
        ] {
            let items = (0u8..)
                .zip(offsets)
                .map(|(index, millis)| {
                    item_with_reply(vec![index], now + Duration::from_millis(millis)).0
                })
                .collect();

            let cohorts: Vec<Vec<_>> = deadline_cohorts(items)
                .into_iter()
                .map(|cohort| cohort.into_iter().map(|item| item.tx_bytes[0]).collect())
                .collect();

            assert_eq!(cohorts, vec![vec![0, 1, 2], vec![3, 4], vec![5]]);
        }
        assert!(deadline_cohorts(Vec::new()).is_empty());
    }

    #[test]
    fn expired_item_in_later_cohort_does_not_discard_live_item() {
        let now = Instant::now();
        let (first, _first_reply) = item_with_reply(vec![1], now + Duration::from_secs(30));
        let (expired, mut expired_reply) = item_with_reply(vec![2], now + Duration::from_secs(5));
        let (live, mut live_reply) = item_with_reply(vec![3], now + Duration::from_millis(5_500));
        let mut cohorts = deadline_cohorts(live_items(vec![first, expired, live], now));
        assert_eq!(cohorts.len(), 2);

        // Model the first submission completing at the later cohort's earliest deadline.
        let later = live_items(cohorts.pop().unwrap(), now + Duration::from_secs(5));

        assert!(matches!(
            expired_reply.try_recv(),
            Ok(Err(RpcServerError::BackendTimeout))
        ));
        assert!(matches!(
            live_reply.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(pack(&later).unwrap().xdr.0, vec![3]);
    }

    #[tokio::test]
    async fn concurrent_submissions_are_combined_after_inflight_batch() {
        let sink = Arc::new(GatedSink::new());
        let batcher = SubmitBatcher::spawn_with(sink.clone(), 10, 100, 10, 100);
        let first_batch = sink.called.notified();
        let first = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![0])
                    .await
            }
        });
        first_batch.await;

        let mut queued = Vec::new();
        for byte in 1..=3 {
            let batcher = batcher.clone();
            queued.push(tokio::spawn(async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![byte])
                    .await
            }));
        }
        wait_for_queue_capacity(&batcher, 7).await;

        let second_batch = sink.called.notified();
        sink.permits.add_permits(1);
        first
            .await
            .expect("bug: first submit task should join")
            .expect("bug: first submit should succeed");
        second_batch.await;
        sink.permits.add_permits(1);
        for task in queued {
            task.await
                .expect("bug: queued submit task should join")
                .expect("bug: queued submit should succeed");
        }

        let sizes: Vec<_> = sink
            .batches
            .lock()
            .expect("bug: test batch lock should be available")
            .iter()
            .map(|batch| batch.segment_tokens.0.len())
            .collect();
        assert_eq!(sizes, vec![1, 3]);
    }

    #[tokio::test]
    async fn full_ingress_queue_returns_overloaded() {
        let sink = Arc::new(GatedSink::new());
        let batcher = SubmitBatcher::spawn_with(sink.clone(), 1, 100, 1, 100);
        let first_batch = sink.called.notified();
        let first = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![1])
                    .await
            }
        });
        first_batch.await;
        let second = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![2])
                    .await
            }
        });
        wait_for_queue_capacity(&batcher, 0).await;

        let error = batcher
            .submit(RpcRequestContext::unbounded(), vec![3])
            .await
            .expect_err("third submit should exceed the test queue");

        sink.permits.add_permits(2);
        first
            .await
            .expect("bug: first submit task should join")
            .expect("bug: first submit should succeed");
        second
            .await
            .expect("bug: second submit task should join")
            .expect("bug: second submit should succeed");
        assert!(matches!(error, RpcServerError::Overloaded(_)));
    }

    #[tokio::test]
    async fn full_ingress_byte_budget_returns_overloaded() {
        let sink = Arc::new(GatedSink::new());
        let batcher = SubmitBatcher::spawn_with(sink.clone(), 10, 100, 10, 2);
        let first_batch = sink.called.notified();
        let first = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![1, 2])
                    .await
            }
        });
        first_batch.await;

        let error = batcher
            .submit(RpcRequestContext::unbounded(), vec![3])
            .await
            .expect_err("second submit should exceed the test byte budget");

        sink.permits.add_permits(1);
        first
            .await
            .expect("bug: first submit task should join")
            .expect("bug: first submit should succeed");
        assert!(matches!(error, RpcServerError::Overloaded(_)));
    }

    #[tokio::test]
    async fn expired_request_returns_timeout_before_queue_overload() {
        let sink = Arc::new(GatedSink::new());
        let batcher = SubmitBatcher::spawn_with(sink.clone(), 10, 100, 10, 1);
        let first_batch = sink.called.notified();
        let first = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![1])
                    .await
            }
        });
        first_batch.await;

        let error = batcher
            .submit(RpcRequestContext::new(Some(Duration::ZERO)), vec![2])
            .await
            .expect_err("expired request should time out before checking queue capacity");

        sink.permits.add_permits(1);
        first
            .await
            .expect("bug: first submit task should join")
            .expect("bug: first submit should succeed");
        assert!(matches!(error, RpcServerError::BackendTimeout));
    }

    #[tokio::test]
    async fn timed_out_queued_request_is_not_submitted() {
        let sink = Arc::new(GatedSink::new());
        let batcher = SubmitBatcher::spawn_with(sink.clone(), 10, 100, 10, 100);
        let first_batch = sink.called.notified();
        let first = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![1])
                    .await
            }
        });
        first_batch.await;

        let error = batcher
            .submit(
                RpcRequestContext::new(Some(std::time::Duration::from_millis(1))),
                vec![2],
            )
            .await
            .expect_err("queued request should time out");
        sink.permits.add_permits(1);
        first
            .await
            .expect("bug: first submit task should join")
            .expect("bug: first submit should succeed");
        wait_for_queue_capacity(&batcher, 10).await;

        assert!(matches!(error, RpcServerError::BackendTimeout));
        assert_eq!(
            sink.batches
                .lock()
                .expect("bug: test batch lock should be available")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn canceled_later_cohort_is_not_submitted() {
        let sink = Arc::new(GatedSink::new());
        let (ingress_tx, ingress_rx) = mpsc::channel(2);
        let now = Instant::now();
        let (first, first_reply) = item_with_reply(vec![1], now + Duration::from_secs(30));
        let (second, second_reply) = item_with_reply(vec![2], now + Duration::from_secs(10));
        ingress_tx
            .try_send(first)
            .expect("bug: first test item should enter queue");
        ingress_tx
            .try_send(second)
            .expect("bug: second test item should enter queue");
        drop(ingress_tx);
        let worker = tokio::spawn(run(
            ingress_rx,
            sink.clone(),
            10,
            100,
            Arc::new(RwLock::new(None)),
        ));
        sink.called.notified().await;

        drop(second_reply);
        sink.permits.add_permits(1);
        first_reply
            .await
            .expect("bug: first test reply should arrive")
            .expect("bug: first test submit should succeed");
        worker.await.expect("bug: test worker should stop");

        assert_eq!(
            sink.batches
                .lock()
                .expect("bug: test batch lock should be available")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn scheduler_timeout_does_not_wedge_later_submissions() {
        struct SlowFirstSink {
            calls: AtomicUsize,
        }

        #[async_trait]
        impl SubmitSink for SlowFirstSink {
            async fn submit(
                &self,
                _batch: ParallelSignedTransactionBatch,
            ) -> Result<(), RpcServerError> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Ok(())
            }
        }

        let sink = Arc::new(SlowFirstSink {
            calls: AtomicUsize::new(0),
        });
        let batcher = SubmitBatcher::spawn_with(sink.clone(), 10, 100, 10, 100);
        let first = batcher
            .submit(
                RpcRequestContext::new(Some(std::time::Duration::from_millis(5))),
                vec![1],
            )
            .await;
        let second = batcher
            .submit(RpcRequestContext::unbounded(), vec![2])
            .await;

        assert!(matches!(first, Err(RpcServerError::BackendTimeout)));
        assert!(second.is_ok());
        assert_eq!(sink.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn successful_batch_wakes_block_driver_once() {
        let sink = Arc::new(GatedSink::new());
        let batcher = SubmitBatcher::spawn_with(sink.clone(), 10, 100, 10, 100);
        let notify = Arc::new(Notify::new());
        batcher.set_block_driver(BlockDriverTrigger::new(notify.clone()));
        let called = sink.called.notified();
        let wake = notify.notified();
        let submit = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![1])
                    .await
            }
        });
        called.await;
        sink.permits.add_permits(1);
        submit
            .await
            .expect("bug: submit task should join")
            .expect("bug: submit should succeed");

        tokio::time::timeout(std::time::Duration::from_secs(1), wake)
            .await
            .expect("block driver should be notified");
    }

    #[tokio::test]
    async fn scheduler_failure_is_returned_to_every_batched_request() {
        struct FailingSink;

        #[async_trait]
        impl SubmitSink for FailingSink {
            async fn submit(
                &self,
                _batch: ParallelSignedTransactionBatch,
            ) -> Result<(), RpcServerError> {
                Err(RpcServerError::BackendUnavailable("test failure".into()))
            }
        }

        let (ingress_tx, ingress_rx) = mpsc::channel(2);
        let ingress_bytes = Arc::new(Semaphore::new(2));
        let (first_reply, first_result) = oneshot::channel();
        let (second_reply, second_result) = oneshot::channel();
        ingress_tx
            .try_send(Item {
                tx_bytes: vec![1],
                backend_deadline: Instant::now() + BACKEND_TIMEOUT,
                reply: first_reply,
                _ingress_bytes: ingress_bytes
                    .clone()
                    .try_acquire_owned()
                    .expect("bug: first test byte permit should be available"),
            })
            .expect("bug: first test item should enter queue");
        ingress_tx
            .try_send(Item {
                tx_bytes: vec![2],
                backend_deadline: Instant::now() + BACKEND_TIMEOUT,
                reply: second_reply,
                _ingress_bytes: ingress_bytes
                    .try_acquire_owned()
                    .expect("bug: second test byte permit should be available"),
            })
            .expect("bug: second test item should enter queue");
        drop(ingress_tx);
        run(
            ingress_rx,
            Arc::new(FailingSink),
            10,
            100,
            Arc::new(RwLock::new(None)),
        )
        .await;

        for result in [first_result, second_result] {
            assert!(matches!(
                result.await,
                Ok(Err(RpcServerError::BackendUnavailable(_)))
            ));
        }
    }

    #[tokio::test]
    async fn abort_resolves_waiting_request() {
        let sink = Arc::new(GatedSink::new());
        let batcher = SubmitBatcher::spawn_with(sink.clone(), 10, 100, 10, 100);
        let called = sink.called.notified();
        let submit = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .submit(RpcRequestContext::unbounded(), vec![1])
                    .await
            }
        });
        called.await;

        batcher.abort();
        let result = submit.await.expect("bug: submit task should join");

        assert!(matches!(result, Err(RpcServerError::BackendUnavailable(_))));
    }
}
