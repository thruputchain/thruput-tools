use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use thruput_scheduler::SchedGatewaySubmitOutcome;
use tokio::sync::{Notify, Semaphore};
use xdr_common::VecM;

use super::*;

/// Group items the way the worker does, so a test reads the same
/// cohorts the submit loop would.
fn cohorts(items: Vec<Item>) -> Vec<Vec<Item>> {
    shared_submit::deadline_cohorts(items, MAX_DEADLINE_SKEW, |item| item.backend_deadline)
}

/// Pack copies of the items' bytes the way the worker packs them.
fn pack(items: &[Item]) -> SubmitBatch<Vec<u8>> {
    shared_submit::pack(items.iter().map(|item| item.tx_bytes.clone()).collect())
        .expect("bug: test transactions should pack")
}

struct GatedSink {
    batches: Mutex<Vec<SubmitBatch<Vec<u8>>>>,
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
    batches: Mutex<Vec<SubmitBatch<Vec<u8>>>>,
}

#[async_trait]
impl SubmitSink for RecordingSink {
    async fn submit(
        &self,
        batch: SubmitBatch<Vec<u8>>,
    ) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError> {
        let count = batch.segment_tokens.0.len();
        self.batches
            .lock()
            .expect("bug: test batch lock should be available")
            .push(batch);
        Ok(VecM(
            vec![SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted; count],
        ))
    }
}

#[async_trait]
impl SubmitSink for GatedSink {
    async fn submit(
        &self,
        batch: SubmitBatch<Vec<u8>>,
    ) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError> {
        let count = batch.segment_tokens.0.len();
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
        Ok(VecM(
            vec![SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted; count],
        ))
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
    let batch = pack(&[item(vec![1, 2]), item(vec![3, 4, 5])]);

    assert_eq!(batch.segment_tokens.0, vec![1, 1]);
    assert_eq!(batch.segment_bytes.0, vec![2, 3]);
    assert_eq!(batch.xdr.0, vec![vec![1, 2], vec![3, 4, 5]]);
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
        .map(|batch| batch.xdr.0.concat())
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
        .map(|batch| batch.xdr.0.concat())
        .collect();
    assert_eq!(batches, vec![vec![1, 1, 1], vec![2, 2, 3, 3]]);
    assert!(
        replies
            .into_iter()
            .all(|mut reply| reply.try_recv().is_ok())
    );
}

#[test]
fn the_tightest_deadline_cohort_is_submitted_first() {
    let now = Instant::now();
    let items = vec![
        item_with_reply(vec![1], now + Duration::from_secs(10)).0,
        item_with_reply(vec![2], now + Duration::from_secs(5)).0,
        item_with_reply(vec![3], now + Duration::from_secs(10)).0,
    ];

    let order: Vec<Vec<u8>> = cohorts(items)
        .into_iter()
        .map(|cohort| cohort.into_iter().map(|item| item.tx_bytes[0]).collect())
        .collect();

    assert_eq!(order, vec![vec![2], vec![1, 3]]);
}

#[test]
fn a_cohort_spans_no_more_than_the_permitted_deadline_skew() {
    let now = Instant::now();
    // Deadlines arrive out of order; grouping is by deadline, not arrival.
    // Offsets are chosen so the skew boundary is exercised inclusively
    // (2_500 joins an anchor at 1_500) and exclusively (2_501 does not).
    let offsets = [2_000u64, 1_500, 2_500, 2_501, 3_501, 3_502];
    let items = (0u8..)
        .zip(offsets)
        .map(|(index, millis)| item_with_reply(vec![index], now + Duration::from_millis(millis)).0)
        .collect();

    let grouped: Vec<Vec<u8>> = cohorts(items)
        .into_iter()
        .map(|cohort| cohort.into_iter().map(|item| item.tx_bytes[0]).collect())
        .collect();

    assert_eq!(grouped, vec![vec![1, 0, 2], vec![3, 4], vec![5]]);
    assert!(cohorts(Vec::new()).is_empty());
}

#[test]
fn expired_item_in_later_cohort_does_not_discard_live_item() {
    let now = Instant::now();
    let (first, _first_reply) = item_with_reply(vec![1], now + Duration::from_secs(30));
    let (expired, mut expired_reply) = item_with_reply(vec![2], now + Duration::from_secs(5));
    let (live, mut live_reply) = item_with_reply(vec![3], now + Duration::from_millis(5_500));
    let mut grouped = cohorts(live_items(vec![first, expired, live], now));
    assert_eq!(grouped.len(), 2);

    // Model time reaching the tightest cohort's deadline before it is packed.
    let tightest = live_items(grouped.remove(0), now + Duration::from_secs(5));

    assert!(matches!(
        expired_reply.try_recv(),
        Ok(Err(RpcServerError::BackendTimeout))
    ));
    assert!(matches!(
        live_reply.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(pack(&tightest).xdr.0.concat(), vec![3]);
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
    // The tighter deadline is packed first, so the looser one is still
    // queued when its caller goes away.
    let (submitted, submitted_reply) = item_with_reply(vec![1], now + Duration::from_secs(10));
    let (canceled, canceled_reply) = item_with_reply(vec![2], now + Duration::from_secs(30));
    ingress_tx
        .try_send(submitted)
        .expect("bug: first test item should enter queue");
    ingress_tx
        .try_send(canceled)
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

    drop(canceled_reply);
    sink.permits.add_permits(1);
    submitted_reply
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
            batch: SubmitBatch<Vec<u8>>,
        ) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError> {
            let count = batch.segment_tokens.0.len();
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Ok(VecM(
                vec![SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted; count],
            ))
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
            _batch: SubmitBatch<Vec<u8>>,
        ) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError> {
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

struct OutcomeSink(Vec<SchedGatewaySubmitOutcome>);

#[async_trait]
impl SubmitSink for OutcomeSink {
    async fn submit(
        &self,
        _batch: SubmitBatch<Vec<u8>>,
    ) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError> {
        Ok(VecM(self.0.clone()))
    }
}

async fn outcome_batch(
    outcomes: Vec<SchedGatewaySubmitOutcome>,
    count: usize,
) -> (Vec<Result<(), RpcServerError>>, bool) {
    let (tx, rx) = mpsc::channel(count);
    let mut replies = Vec::new();
    for _ in 0..count {
        let (item, reply) = item_with_reply(vec![1], Instant::now() + BACKEND_TIMEOUT);
        tx.try_send(item).unwrap();
        replies.push(reply);
    }
    drop(tx);
    let notify = Arc::new(Notify::new());
    run(
        rx,
        Arc::new(OutcomeSink(outcomes)),
        count,
        100,
        Arc::new(RwLock::new(Some(BlockDriverTrigger::new(notify.clone())))),
    )
    .await;
    let mut results = Vec::new();
    for reply in replies {
        results.push(reply.await.unwrap());
    }
    let woke = tokio::time::timeout(Duration::from_millis(10), notify.notified())
        .await
        .is_ok();
    (results, woke)
}

#[tokio::test]
async fn mixed_outcomes_reach_their_own_waiters_and_wake_block_driver() {
    use SchedGatewaySubmitOutcome as O;
    let (results, woke) = outcome_batch(
        vec![
            O::SchedGatewaySubmitAuthenticationRejected,
            O::SchedGatewaySubmitAdmitted,
            O::SchedGatewaySubmitRetryable,
            O::SchedGatewaySubmitInternalError,
            O(99),
        ],
        5,
    )
    .await;
    assert!(matches!(results[0], Err(RpcServerError::TxRejected(_))));
    assert!(results[1].is_ok());
    assert!(matches!(results[2], Err(RpcServerError::Overloaded(_))));
    assert!(matches!(
        results[3],
        Err(RpcServerError::BackendUnavailable(_))
    ));
    assert!(matches!(
        results[4],
        Err(RpcServerError::BackendUnavailable(_))
    ));
    assert!(woke);
}

#[tokio::test]
async fn malformed_cardinality_fails_every_waiter_without_waking_driver() {
    for count in [0, 1, 3] {
        let (results, woke) = outcome_batch(
            vec![SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted; count],
            2,
        )
        .await;
        assert!(
            results
                .iter()
                .all(|r| matches!(r, Err(RpcServerError::BackendUnavailable(_))))
        );
        assert!(!woke);
    }
}

#[tokio::test]
async fn rejected_batch_does_not_wake_block_driver() {
    let (results, woke) = outcome_batch(
        vec![SchedGatewaySubmitOutcome::SchedGatewaySubmitAuthenticationRejected; 2],
        2,
    )
    .await;
    assert!(
        results
            .iter()
            .all(|r| matches!(r, Err(RpcServerError::TxRejected(_))))
    );
    assert!(!woke);
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
