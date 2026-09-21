//! Adaptive batching submitter for `sendRawTransaction`.
//!
//! Drives the shared adaptive-batching machinery in `adaptive_batch.rs`
//! with a tx-submission–specific processor: pack N client tx-byte blobs
//! into one `ParallelSignedTransactionBatch`, call
//! `schedgateway_submit_tx_batch`, and fan out each
//! request-scoped result to its matching waiter.
//!
//! See `AdaptiveBatchConfig` for the dispatch policy (no timer,
//! BDP-style in-flight bounding, sharded for low contention) and
//! `adaptive_batch.rs` for the shared shard runner.

use std::{sync::Arc, time::Duration, time::Instant};

use async_trait::async_trait;
use groundhog_hogd_backend::{scheduler as shared_scheduler, submit as shared_submit};
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_scheduler::SchedGatewaySubmitOutcomeBatch;
use groundhog_scheduler::parallel::ParallelSignedTransactionBatch;
use tokio::sync::{RwLock, oneshot};

use crate::adaptive_batch::{
    AdaptiveBatcher, BatchProcessor, deadline_cohorts, shard_index_by_routing_key,
};
use crate::config::AdaptiveBatchConfig;
use crate::context::{AbortTaskOnDrop, SchedulerClient, clear_scheduler_if_current};
use crate::error::GatewayError;
use crate::metrics::{BackendRequestOutcome, GatewayMetrics};

/// Backend for batched submissions.  In production this wraps the
/// scheduler client; tests substitute a mock.
#[async_trait]
pub trait SubmitSink: Send + Sync + 'static {
    async fn submit(
        &self,
        batch: ParallelSignedTransactionBatch,
        deadline: Instant,
    ) -> Result<SchedGatewaySubmitOutcomeBatch, GatewayError>;
}

/// Production sink: forwards to the hogd scheduler's
/// `schedgateway_submit_tx_batch` RPC.
///
/// Holds a shared `Arc<RwLock<Option<Client>>>` so that scheduler
/// reconnections in `GatewayContext` become visible on the next call.
pub struct SchedulerSink {
    scheduler: Arc<RwLock<Option<SchedulerClient>>>,
    metrics: GatewayMetrics,
    timeout: Duration,
}

impl SchedulerSink {
    pub(crate) fn new(
        scheduler: Arc<RwLock<Option<SchedulerClient>>>,
        metrics: GatewayMetrics,
        timeout: Duration,
    ) -> Self {
        Self {
            scheduler,
            metrics,
            timeout,
        }
    }
}

#[async_trait]
impl SubmitSink for SchedulerSink {
    async fn submit(
        &self,
        batch: ParallelSignedTransactionBatch,
        deadline: Instant,
    ) -> Result<SchedGatewaySubmitOutcomeBatch, GatewayError> {
        let client = self.scheduler.read().await.clone();
        match client {
            None => Err(GatewayError::BackendUnavailable(
                "scheduler not connected".into(),
            )),
            Some(c) => {
                let generation = c.generation();
                let started_at = Instant::now();
                let timeout = self
                    .timeout
                    .min(deadline.saturating_duration_since(started_at));
                if timeout.is_zero() {
                    return Err(GatewayError::BackendTimeout);
                }
                let mut call =
                    tokio::spawn(async move { c.schedgateway_submit_tx_batch(&batch).await });
                let _abort_call_on_drop = AbortTaskOnDrop::new(&call);
                let result = if let Ok(result) = tokio::time::timeout(timeout, &mut call).await {
                    Ok(result.unwrap_or(Err(rpc_driver::RpcError::SystemError)))
                } else {
                    clear_scheduler_if_current(&self.scheduler, generation).await;
                    call.abort();
                    let _ = call.await;
                    Err(())
                };
                self.metrics.observe_backend_request(
                    crate::config::HealthBackend::Scheduler,
                    "schedgateway_submit_tx_batch",
                    match &result {
                        Ok(Ok(_)) => BackendRequestOutcome::Ok,
                        Ok(Err(_)) => BackendRequestOutcome::Error,
                        Err(()) => BackendRequestOutcome::Timeout,
                    },
                    started_at.elapsed(),
                );
                if matches!(result, Ok(Err(_))) {
                    clear_scheduler_if_current(&self.scheduler, generation).await;
                }
                result
                    .map_err(|()| GatewayError::BackendTimeout)?
                    .map_err(|e| classify_rpc_error(&e))
            }
        }
    }
}

/// Map a scheduler RPC error to a gateway error.
///
/// Driver lifecycle and protocol errors do not imply a gateway deadline or
/// admission failure, so they all map to `BackendUnavailable`.
fn classify_rpc_error(e: &rpc_driver::RpcError) -> GatewayError {
    GatewayError::BackendUnavailable(format!("scheduler: {e}"))
}

/// A single queued client submission.
struct Item {
    tx_bytes: Vec<u8>,
    reply: oneshot::Sender<Result<(), GatewayError>>,
    backend_deadline: Instant,
}

/// Per-batch processor: pack, submit, and fan the shared result out
/// to every waiter in the drained batch.
struct SubmitProcessor {
    sink: Arc<dyn SubmitSink>,
    metrics: GatewayMetrics,
    max_deadline_skew: Duration,
}

#[async_trait]
impl BatchProcessor for SubmitProcessor {
    type Item = Item;

    async fn process(&self, items: Vec<Item>) {
        let now = Instant::now();
        let mut live = Vec::with_capacity(items.len());
        for item in items {
            if item.reply.is_closed() {
                continue;
            }
            if item.backend_deadline <= now {
                self.metrics
                    .observe_submitted_transaction("backend_timeout");
                let _ = item.reply.send(Err(GatewayError::BackendTimeout));
            } else {
                live.push(item);
            }
        }
        for mut cohort in
            deadline_cohorts(live, self.max_deadline_skew, |item| item.backend_deadline)
                .into_iter()
                .rev()
        {
            let now = Instant::now();
            cohort.retain(|item| !item.reply.is_closed());
            if cohort.is_empty() {
                continue;
            }
            let deadline = cohort[0].backend_deadline;
            if deadline <= now {
                for item in cohort {
                    self.metrics
                        .observe_submitted_transaction("backend_timeout");
                    let _ = item.reply.send(Err(GatewayError::BackendTimeout));
                }
                continue;
            }
            self.metrics
                .observe_internal_batch_size("submit_batcher", cohort.len());
            let packed = match pack(&cohort) {
                Ok(packed) => packed,
                Err(error) => {
                    let outcome = outcome_for_submit_error(&error);
                    for item in cohort {
                        self.metrics.observe_submitted_transaction(outcome);
                        let _ = item.reply.send(Err(error.clone()));
                    }
                    continue;
                }
            };
            match self.sink.submit(packed, deadline).await {
                Err(error) => {
                    let outcome = outcome_for_submit_error(&error);
                    for item in cohort {
                        self.metrics.observe_submitted_transaction(outcome);
                        let _ = item.reply.send(Err(error.clone()));
                    }
                }
                Ok(outcomes) if outcomes.0.len() != cohort.len() => {
                    let error = GatewayError::BackendUnavailable(format!(
                        "scheduler returned {} submission outcomes for {} transactions",
                        outcomes.0.len(),
                        cohort.len()
                    ));
                    let outcome = outcome_for_submit_error(&error);
                    for item in cohort {
                        self.metrics.observe_submitted_transaction(outcome);
                        let _ = item.reply.send(Err(error.clone()));
                    }
                }
                Ok(outcomes) => {
                    for (item, scheduler_outcome) in cohort.into_iter().zip(outcomes.0) {
                        let result = shared_scheduler::submit_result(scheduler_outcome)
                            .map_err(GatewayError::from);
                        self.metrics
                            .observe_submitted_transaction(outcome_for_submit_result(&result));
                        let _ = item.reply.send(result);
                    }
                }
            }
        }
    }
}

/// Pack N client tx-byte blobs into one `ParallelSignedTransactionBatch`.
///
/// One segment per tx — same shape as today's single-tx handler, scaled up.
fn pack(items: &[Item]) -> Result<ParallelSignedTransactionBatch, GatewayError> {
    shared_submit::pack(items.iter().map(|item| item.tx_bytes.as_slice()))
        .map_err(GatewayError::from)
}

/// Handle returned to the rest of the gateway.  Clone-friendly; each
/// clone shares the same bank of shard channels.
#[derive(Clone)]
pub struct BatchSubmitter {
    inner: AdaptiveBatcher<Item>,
    metrics: GatewayMetrics,
    request_timeout: Duration,
    backend_timeout: Duration,
}

impl BatchSubmitter {
    /// Spawn one submitter task per shard.  Tasks live until the
    /// `BatchSubmitter` (and thus its shard senders) are dropped.
    #[cfg(test)]
    pub fn spawn(
        config: &AdaptiveBatchConfig,
        sink: &Arc<dyn SubmitSink>,
        metrics: GatewayMetrics,
    ) -> Self {
        Self::spawn_with_deadlines(
            config,
            sink,
            metrics,
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
    }

    pub(crate) fn spawn_with_deadlines(
        config: &AdaptiveBatchConfig,
        sink: &Arc<dyn SubmitSink>,
        metrics: GatewayMetrics,
        request_timeout: Duration,
        backend_timeout: Duration,
        max_deadline_skew: Duration,
    ) -> Self {
        let processor = Arc::new(SubmitProcessor {
            sink: sink.clone(),
            metrics: metrics.clone(),
            max_deadline_skew,
        });
        let inner = AdaptiveBatcher::spawn(
            "batch submitter",
            "submit_batcher",
            config,
            &processor,
            metrics.clone(),
        );
        Self {
            inner,
            metrics,
            request_timeout,
            backend_timeout,
        }
    }

    /// Abort submitter shard runners and all in-flight submissions.
    pub(crate) fn abort(&self) {
        self.inner.abort();
    }

    /// Submit one tx.  Routes to a shard by tx hash; returns when the
    /// fanout completes.  Returns `Overloaded` if the target shard's
    /// ingress is full (client should retry with backoff).
    #[tracing::instrument(skip_all, fields(shard_idx, tx_bytes_len = tx_bytes.len()))]
    pub async fn submit(
        &self,
        context: RpcRequestContext,
        tx_bytes: Vec<u8>,
        hash: TxHash,
    ) -> Result<(), GatewayError> {
        let bytes: &[u8] = hash.0.0.as_ref();
        let shard_idx = shard_index_by_routing_key(bytes, self.inner.n_shards());
        tracing::Span::current().record("shard_idx", shard_idx);
        let (reply_tx, reply_rx) = oneshot::channel();
        let request_deadline = context
            .capped_from_start(self.request_timeout)
            .deadline()
            .expect("operation timeout should bound submission");
        let backend_deadline = context
            .capped_from_start(self.backend_timeout)
            .deadline()
            .expect("backend timeout should bound submission");
        if backend_deadline <= Instant::now() {
            let err = GatewayError::BackendTimeout;
            self.metrics
                .observe_submitted_transaction(outcome_for_submit_error(&err));
            return Err(err);
        }
        if let Err(err) = self.inner.try_send(
            Item {
                tx_bytes,
                reply: reply_tx,
                backend_deadline,
            },
            shard_idx,
        ) {
            self.metrics
                .observe_submitted_transaction(outcome_for_submit_error(&err));
            return Err(err);
        }
        match tokio::time::timeout_at(request_deadline.into(), reply_rx).await {
            Err(_) => {
                let err = GatewayError::BackendTimeout;
                self.metrics
                    .observe_submitted_transaction(outcome_for_submit_error(&err));
                Err(err)
            }
            Ok(Err(_)) => {
                let err = GatewayError::BackendUnavailable("batch submitter dropped reply".into());
                self.metrics
                    .observe_submitted_transaction(outcome_for_submit_error(&err));
                Err(err)
            }
            Ok(Ok(result)) => result,
        }
    }
}

fn outcome_for_submit_result(result: &Result<(), GatewayError>) -> &'static str {
    match result {
        Ok(()) => "ok",
        Err(err) => outcome_for_submit_error(err),
    }
}

fn outcome_for_submit_error(err: &GatewayError) -> &'static str {
    match err {
        GatewayError::Overloaded(_) => "overloaded",
        GatewayError::BackendTimeout => "backend_timeout",
        GatewayError::BackendUnavailable(_) => "backend_unavailable",
        GatewayError::TxInvalid(_) => "tx_invalid",
        GatewayError::TxRejected(_) => "tx_rejected",
        GatewayError::TxNotFound | GatewayError::BlockNotFound => "not_found",
        GatewayError::InvalidParam(_) => "invalid_params",
        GatewayError::NotImplemented(_) => "not_implemented",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use groundhog_scheduler::SchedGatewaySubmitOutcome;
    use xdr_common::VecM;

    use super::*;
    use crate::health::{GatewayHealthSnapshot, HealthStatus};
    use std::collections::VecDeque;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    #[test]
    fn driver_lifecycle_errors_remain_backend_unavailable() {
        for error in [
            rpc_driver::RpcError::ConnectFailed,
            rpc_driver::RpcError::PoolClosed,
            rpc_driver::RpcError::Canceled,
            rpc_driver::RpcError::InvalidPoolConfiguration,
        ] {
            assert!(matches!(
                classify_rpc_error(&error),
                GatewayError::BackendUnavailable(_)
            ));
        }
    }

    /// Scripted mock sink.  Each `submit()` call records the batch size
    /// and, if a "gate" oneshot has been pre-queued, awaits it before
    /// returning.  If no gates remain, returns `Ok` immediately.
    #[derive(Default)]
    struct MockSink {
        seen: Mutex<Vec<usize>>,
        deadlines: Mutex<Vec<Instant>>,
        pending: Mutex<VecDeque<oneshot::Receiver<Result<(), GatewayError>>>>,
        outcomes: Mutex<VecDeque<Vec<SchedGatewaySubmitOutcome>>>,
    }

    impl MockSink {
        fn arc() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn queue_gate(&self) -> oneshot::Sender<Result<(), GatewayError>> {
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().push_back(rx);
            tx
        }

        fn seen(&self) -> Vec<usize> {
            self.seen.lock().unwrap().clone()
        }

        fn deadlines(&self) -> Vec<Instant> {
            self.deadlines.lock().unwrap().clone()
        }

        fn queue_outcomes(&self, outcomes: Vec<SchedGatewaySubmitOutcome>) {
            self.outcomes.lock().unwrap().push_back(outcomes);
        }
    }

    #[async_trait]
    impl SubmitSink for MockSink {
        async fn submit(
            &self,
            batch: ParallelSignedTransactionBatch,
            deadline: Instant,
        ) -> Result<SchedGatewaySubmitOutcomeBatch, GatewayError> {
            let size = batch.segment_bytes.0.len();
            self.seen.lock().unwrap().push(size);
            self.deadlines.lock().unwrap().push(deadline);
            let gate = self.pending.lock().unwrap().pop_front();
            if let Some(rx) = gate {
                rx.await.unwrap_or_else(|_| {
                    Err(GatewayError::BackendUnavailable("test gate dropped".into()))
                })?;
            }
            let outcomes = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    vec![SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted; size]
                });
            Ok(VecM(outcomes))
        }
    }

    struct TimeoutInvalidatesSink {
        cutoff: Instant,
        valid: AtomicBool,
        deadlines: Mutex<Vec<Instant>>,
    }

    #[async_trait]
    impl SubmitSink for TimeoutInvalidatesSink {
        async fn submit(
            &self,
            _batch: ParallelSignedTransactionBatch,
            deadline: Instant,
        ) -> Result<SchedGatewaySubmitOutcomeBatch, GatewayError> {
            self.deadlines.lock().unwrap().push(deadline);
            if deadline < self.cutoff {
                self.valid.store(false, Ordering::SeqCst);
                Err(GatewayError::BackendTimeout)
            } else if self.valid.load(Ordering::SeqCst) {
                Ok(VecM(vec![
                    SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted,
                ]))
            } else {
                Err(GatewayError::BackendUnavailable(
                    "scheduler invalidated by older timeout".into(),
                ))
            }
        }
    }

    fn hash_from_u32(x: u32) -> TxHash {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&x.to_be_bytes());
        TxHash(groundhog_json_rpc_types::hex::Hex0x(
            groundhog_xdr::types::SHA256Hash::from(bytes),
        ))
    }

    fn config_for_tests(
        shards: usize,
        max_inflight: u32,
        max_size: usize,
        ingress: usize,
    ) -> AdaptiveBatchConfig {
        AdaptiveBatchConfig {
            shards,
            max_inflight_batches: max_inflight,
            max_size,
            ingress_queue_depth: ingress,
        }
    }

    /// A small "settle" delay to let spawned tasks make progress.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    fn request_context() -> RpcRequestContext {
        RpcRequestContext::unbounded()
    }

    #[tokio::test]
    async fn batch_of_1_at_low_load() {
        let mock = MockSink::arc();
        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
        );

        submitter
            .submit(request_context(), vec![0xaa; 8], hash_from_u32(0))
            .await
            .unwrap();

        assert_eq!(mock.seen(), vec![1], "exactly one batch of size 1");
    }

    #[tokio::test]
    async fn request_deadline_reaches_submission_sink() {
        let mock = MockSink::arc();
        let submitter = BatchSubmitter::spawn_with_deadlines(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
            Duration::from_secs(5),
            Duration::from_secs(4),
            Duration::from_secs(1),
        );
        let context = RpcRequestContext::new(Some(Duration::from_secs(5)));
        let expected = context
            .started_at()
            .checked_add(Duration::from_secs(4))
            .expect("backend deadline should fit");

        submitter
            .submit(context, vec![0xaa; 8], hash_from_u32(0))
            .await
            .expect("submission should complete");

        assert_eq!(mock.deadlines(), vec![expected]);
    }

    #[tokio::test]
    async fn expired_submission_is_not_dispatched_and_is_counted() {
        let mock = MockSink::arc();
        let metrics = GatewayMetrics::for_tests();
        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
            metrics.clone(),
        );

        let result = submitter
            .submit(
                RpcRequestContext::new(Some(Duration::ZERO)),
                vec![0xaa],
                hash_from_u32(0),
            )
            .await;

        assert!(matches!(result, Err(GatewayError::BackendTimeout)));
        assert!(mock.seen().is_empty());
        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let body = metrics.gather(&snapshot).expect("metrics should encode");
        assert!(body.contains(
            "groundhog_gateway_submitted_transactions_total{outcome=\"backend_timeout\"} 1"
        ));
    }

    #[tokio::test]
    async fn adaptive_batching_drains_on_completion() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 1, 1000, 100), // strict serial batches
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
        );

        // First tx starts a gated batch of 1.
        let first = tokio::spawn({
            let s = submitter.clone();
            async move {
                s.submit(request_context(), vec![0x01], hash_from_u32(0))
                    .await
            }
        });
        settle().await;
        assert_eq!(mock.seen(), vec![1], "first batch in flight");

        // Pile up 5 more while the first is blocked.
        let mut waiters = vec![];
        for i in 1..=5u32 {
            let s = submitter.clone();
            waiters.push(tokio::spawn(async move {
                s.submit(
                    request_context(),
                    vec![u8::try_from(i).unwrap()],
                    hash_from_u32(i),
                )
                .await
            }));
        }
        settle().await;
        assert_eq!(mock.seen(), vec![1], "still just the first batch");

        // Release; the next batch should drain all 5 at once.
        let _ = gate.send(Ok(()));
        first.await.unwrap().unwrap();
        for w in waiters {
            w.await.unwrap().unwrap();
        }
        assert_eq!(mock.seen(), vec![1, 5], "second batch carried all 5");
    }

    #[tokio::test]
    async fn older_deadline_cohort_does_not_timeout_newer_submissions() {
        let mock = MockSink::arc();
        let first_gate = mock.queue_gate();
        let newer_gate = mock.queue_gate();
        let old_gate = mock.queue_gate();
        let submitter = BatchSubmitter::spawn_with_deadlines(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_millis(10),
        );

        let first = tokio::spawn({
            let submitter = submitter.clone();
            async move {
                submitter
                    .submit(request_context(), vec![0], hash_from_u32(0))
                    .await
            }
        });
        settle().await;

        let old = tokio::spawn({
            let submitter = submitter.clone();
            async move {
                submitter
                    .submit(
                        RpcRequestContext::new(Some(Duration::from_secs(2))),
                        vec![1],
                        hash_from_u32(1),
                    )
                    .await
            }
        });
        let newer = tokio::spawn({
            let submitter = submitter.clone();
            async move {
                submitter
                    .submit(
                        RpcRequestContext::new(Some(Duration::from_secs(5))),
                        vec![2],
                        hash_from_u32(2),
                    )
                    .await
            }
        });
        settle().await;

        first_gate.send(Ok(())).expect("first gate should be open");
        first.await.unwrap().unwrap();
        settle().await;
        newer_gate
            .send(Ok(()))
            .expect("newer cohort gate should be open");
        newer
            .await
            .unwrap()
            .expect("newer cohort should complete independently");
        old_gate
            .send(Err(GatewayError::BackendTimeout))
            .expect("old cohort gate should be open");

        assert!(matches!(
            old.await.unwrap(),
            Err(GatewayError::BackendTimeout)
        ));
        assert_eq!(mock.seen(), vec![1, 1, 1]);
    }

    #[tokio::test]
    async fn canceled_older_cohort_is_skipped_after_newer_work_completes() {
        let mock = MockSink::arc();
        let newer_gate = mock.queue_gate();
        let processor = SubmitProcessor {
            sink: mock.clone(),
            metrics: GatewayMetrics::for_tests(),
            max_deadline_skew: Duration::from_millis(10),
        };
        let (old_reply, old_waiter) = oneshot::channel();
        let (newer_reply, newer_waiter) = oneshot::channel();
        let now = Instant::now();
        let processing = tokio::spawn(async move {
            processor
                .process(vec![
                    Item {
                        tx_bytes: vec![1],
                        reply: old_reply,
                        backend_deadline: now + Duration::from_secs(2),
                    },
                    Item {
                        tx_bytes: vec![2],
                        reply: newer_reply,
                        backend_deadline: now + Duration::from_secs(5),
                    },
                ])
                .await;
        });
        settle().await;

        drop(old_waiter);
        newer_gate
            .send(Ok(()))
            .expect("newer cohort gate should be open");

        newer_waiter
            .await
            .expect("newer reply should be sent")
            .expect("newer cohort should succeed");
        processing.await.expect("processor should finish");
        assert_eq!(mock.seen(), vec![1]);
    }

    #[tokio::test]
    async fn newer_submission_finishes_before_old_timeout_invalidates_scheduler() {
        let now = Instant::now();
        let sink = Arc::new(TimeoutInvalidatesSink {
            cutoff: now + Duration::from_secs(3),
            valid: AtomicBool::new(true),
            deadlines: Mutex::new(Vec::new()),
        });
        let processor = SubmitProcessor {
            sink: sink.clone(),
            metrics: GatewayMetrics::for_tests(),
            max_deadline_skew: Duration::from_millis(10),
        };
        let (old_reply, old_waiter) = oneshot::channel();
        let (newer_reply, newer_waiter) = oneshot::channel();

        processor
            .process(vec![
                Item {
                    tx_bytes: vec![1],
                    reply: old_reply,
                    backend_deadline: now + Duration::from_secs(2),
                },
                Item {
                    tx_bytes: vec![2],
                    reply: newer_reply,
                    backend_deadline: now + Duration::from_secs(5),
                },
            ])
            .await;

        newer_waiter
            .await
            .expect("newer reply should be sent")
            .expect("newer cohort should finish before invalidation");
        assert!(matches!(
            old_waiter.await.expect("old reply should be sent"),
            Err(GatewayError::BackendTimeout)
        ));
        assert_eq!(
            sink.deadlines.lock().unwrap().as_slice(),
            &[now + Duration::from_secs(5), now + Duration::from_secs(2)]
        );
    }

    #[tokio::test]
    async fn max_inflight_is_respected() {
        let mock = MockSink::arc();
        let gate_a = mock.queue_gate();
        let gate_b = mock.queue_gate();

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 2, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
        );

        let w1 = {
            let s = submitter.clone();
            tokio::spawn(async move {
                s.submit(request_context(), vec![0x01], hash_from_u32(1))
                    .await
            })
        };
        settle().await;
        let w2 = {
            let s = submitter.clone();
            tokio::spawn(async move {
                s.submit(request_context(), vec![0x02], hash_from_u32(2))
                    .await
            })
        };
        settle().await;
        assert_eq!(mock.seen(), vec![1, 1], "two concurrent in-flight batches");

        // A third should sit queued until one completes.
        let w3 = {
            let s = submitter.clone();
            tokio::spawn(async move {
                s.submit(request_context(), vec![0x03], hash_from_u32(3))
                    .await
            })
        };
        settle().await;
        assert_eq!(mock.seen(), vec![1, 1], "third batch still held");

        let _ = gate_a.send(Ok(()));
        w1.await.unwrap().unwrap();
        settle().await;
        assert_eq!(
            mock.seen(),
            vec![1, 1, 1],
            "third batch fires on completion"
        );

        let _ = gate_b.send(Ok(()));
        w2.await.unwrap().unwrap();
        w3.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn max_size_caps_batch() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 1, 4, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
        );

        let first = {
            let s = submitter.clone();
            tokio::spawn(async move {
                s.submit(request_context(), vec![0x01], hash_from_u32(0))
                    .await
            })
        };
        settle().await;

        let mut waiters = vec![];
        for i in 1..=10u32 {
            let s = submitter.clone();
            waiters.push(tokio::spawn(async move {
                s.submit(
                    request_context(),
                    vec![u8::try_from(i).unwrap()],
                    hash_from_u32(i),
                )
                .await
            }));
        }

        let _ = gate.send(Ok(()));
        first.await.unwrap().unwrap();
        for w in waiters {
            w.await.unwrap().unwrap();
        }

        // First gated batch of 1, then drain 10 in batches of up to 4.
        assert_eq!(mock.seen(), vec![1, 4, 4, 2]);
    }

    #[tokio::test]
    async fn ingress_full_returns_overloaded() {
        let mock = MockSink::arc();
        let gate_holder = mock.queue_gate(); // never fire: holds the in-flight batch open

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 1, 1, 1), // 1 in flight, max batch 1, 1 queue slot
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
        );

        // First item is pulled from mpsc and starts the gated batch.
        let w1 = {
            let s = submitter.clone();
            tokio::spawn(async move {
                s.submit(request_context(), vec![0x01], hash_from_u32(0))
                    .await
            })
        };
        settle().await;

        // Second item: try_send succeeds (channel has 1 slot, currently empty
        // because the submitter already pulled item 1).  But with
        // max_inflight=1 and gate held, the submitter's ingress branch is
        // disabled, so this item sits in the channel.
        let w2 = {
            let s = submitter.clone();
            tokio::spawn(async move {
                s.submit(request_context(), vec![0x02], hash_from_u32(1))
                    .await
            })
        };
        settle().await;

        // Third item: channel is now full (item 2 occupies the only slot).
        // try_send → Full → Overloaded.
        let err = submitter
            .submit(request_context(), vec![0x99], hash_from_u32(99))
            .await
            .expect_err("should be overloaded");
        assert!(
            matches!(err, GatewayError::Overloaded(_)),
            "got {err:?}, expected Overloaded"
        );

        // Release the gate path so w1/w2 don't hang forever on test exit.
        drop(gate_holder); // drop the unused sender → mock gate receiver sees Err.
        let _ = w1.await;
        let _ = w2.await;
    }

    #[tokio::test]
    async fn admitted_submission_is_counted_if_caller_cancels() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();
        let metrics = GatewayMetrics::for_tests();

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
            metrics.clone(),
        );

        let waiter = tokio::spawn({
            let s = submitter.clone();
            async move {
                s.submit(request_context(), vec![0x01], hash_from_u32(0))
                    .await
            }
        });
        settle().await;
        assert_eq!(mock.seen(), vec![1], "submission reached the backend");

        waiter.abort();
        let _ = waiter.await;
        let _ = gate.send(Ok(()));
        settle().await;

        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let body = metrics.gather(&snapshot).expect("metrics should encode");
        assert!(body.contains("groundhog_gateway_submitted_transactions_total{outcome=\"ok\"} 1"));
    }

    #[tokio::test]
    async fn backend_error_fans_out_to_every_waiter() {
        let mock = MockSink::arc();
        let err_gate = mock.queue_gate();

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
        );

        // Spawn four submissions back-to-back with no intervening yield.
        // On `current_thread` (the default `#[tokio::test]` runtime), all
        // four tasks run to their `try_send` + `await reply_rx` yield
        // point before the submitter task gets to poll — so all four
        // items land in the shard's mpsc, and the submitter drains them
        // into one batch on its first ingress pull.  That batch is the
        // one gated on `err_gate`, so every waiter in the batch will
        // receive the same error.
        let mut waiters = vec![];
        for i in 0..4u32 {
            let s = submitter.clone();
            waiters.push(tokio::spawn(async move {
                s.submit(
                    request_context(),
                    vec![u8::try_from(i).unwrap()],
                    hash_from_u32(i),
                )
                .await
            }));
        }
        settle().await;

        // Fail the one batch that holds all four items.
        let _ = err_gate.send(Err(GatewayError::BackendUnavailable("mock".into())));
        for (i, w) in waiters.into_iter().enumerate() {
            let r = w.await.unwrap();
            assert!(
                matches!(r, Err(GatewayError::BackendUnavailable(_))),
                "waiter {i}: expected BackendUnavailable, got {r:?}"
            );
        }

        // And verify the batch really was size 4 (one call, four items).
        assert_eq!(
            mock.seen(),
            vec![4],
            "all four waiters should have shared a single batch"
        );
    }

    #[tokio::test]
    async fn mixed_scheduler_outcomes_fan_out_by_transaction_position() {
        let mock = MockSink::arc();
        mock.queue_outcomes(vec![
            SchedGatewaySubmitOutcome::SchedGatewaySubmitAuthenticationRejected,
            SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted,
            SchedGatewaySubmitOutcome::SchedGatewaySubmitRetryable,
            SchedGatewaySubmitOutcome::SchedGatewaySubmitInternalError,
        ]);
        let metrics = GatewayMetrics::for_tests();
        let processor = SubmitProcessor {
            sink: mock.clone(),
            metrics: metrics.clone(),
            max_deadline_skew: Duration::from_secs(1),
        };
        let mut items = Vec::new();
        let mut replies = Vec::new();
        for byte in 0..4 {
            let (reply, waiter) = oneshot::channel();
            items.push(Item {
                tx_bytes: vec![byte],
                reply,
                backend_deadline: Instant::now() + Duration::from_secs(1),
            });
            replies.push(waiter);
        }

        processor.process(items).await;

        let rejected = replies.remove(0).await.expect("rejected reply");
        let admitted = replies.remove(0).await.expect("admitted reply");
        let retryable = replies.remove(0).await.expect("retryable reply");
        let internal = replies.remove(0).await.expect("internal reply");
        assert!(matches!(rejected, Err(GatewayError::TxRejected(_))));
        assert!(admitted.is_ok());
        assert!(matches!(retryable, Err(GatewayError::Overloaded(_))));
        assert!(matches!(internal, Err(GatewayError::BackendUnavailable(_))));
        assert_eq!(mock.seen(), vec![4]);

        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let body = metrics.gather(&snapshot).expect("metrics should encode");
        for expected in [
            "groundhog_gateway_submitted_transactions_total{outcome=\"ok\"} 1",
            "groundhog_gateway_submitted_transactions_total{outcome=\"tx_rejected\"} 1",
            "groundhog_gateway_submitted_transactions_total{outcome=\"overloaded\"} 1",
            "groundhog_gateway_submitted_transactions_total{outcome=\"backend_unavailable\"} 1",
        ] {
            assert!(body.contains(expected), "missing metric `{expected}`");
        }
    }

    #[tokio::test]
    async fn outcome_cardinality_mismatch_fails_every_waiter() {
        let mock = MockSink::arc();
        mock.queue_outcomes(vec![
            SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted,
            SchedGatewaySubmitOutcome::SchedGatewaySubmitAuthenticationRejected,
        ]);
        let processor = SubmitProcessor {
            sink: mock,
            metrics: GatewayMetrics::for_tests(),
            max_deadline_skew: Duration::from_secs(1),
        };
        let mut items = Vec::new();
        let mut replies = Vec::new();
        for byte in 0..3 {
            let (reply, waiter) = oneshot::channel();
            items.push(Item {
                tx_bytes: vec![byte],
                reply,
                backend_deadline: Instant::now() + Duration::from_secs(1),
            });
            replies.push(waiter);
        }

        processor.process(items).await;

        for waiter in replies {
            let result = waiter.await.expect("reply should be sent");
            assert!(matches!(result, Err(GatewayError::BackendUnavailable(_))));
        }
    }

    #[tokio::test]
    async fn shards_distribute_across_hashes() {
        let n_shards = 4;
        let mut hits = vec![0usize; n_shards];
        for i in 0..10_000u32 {
            let h = hash_from_u32(i.wrapping_mul(2_654_435_761)); // scatter
            let bytes: &[u8] = h.0.0.as_ref();
            hits[shard_index_by_routing_key(bytes, n_shards)] += 1;
        }
        for (i, c) in hits.iter().enumerate() {
            assert!(*c > 1500 && *c < 3500, "shard {i} got {c} (expected ~2500)");
        }
    }

    #[tokio::test]
    async fn queued_request_deadline_skips_stale_backend_work() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();
        let submitter = BatchSubmitter::spawn_with_deadlines(
            &config_for_tests(1, 1, 1, 10),
            &(mock.clone() as Arc<dyn SubmitSink>),
            GatewayMetrics::for_tests(),
            Duration::from_millis(25),
            Duration::from_millis(25),
            Duration::from_millis(1),
        );

        let first = {
            let submitter = submitter.clone();
            tokio::spawn(async move {
                submitter
                    .submit(request_context(), vec![1], hash_from_u32(1))
                    .await
            })
        };
        settle().await;
        let second = submitter
            .submit(request_context(), vec![2], hash_from_u32(2))
            .await;

        assert!(matches!(
            first.await.unwrap(),
            Err(GatewayError::BackendTimeout)
        ));
        assert!(matches!(second, Err(GatewayError::BackendTimeout)));
        gate.send(Ok(())).unwrap();
        settle().await;
        assert_eq!(mock.seen(), vec![1]);
    }

    #[tokio::test]
    async fn pack_shape_is_correct() {
        let items: Vec<Item> = [vec![0x11; 3], vec![0x22; 5], vec![0x33; 2]]
            .into_iter()
            .map(|bytes| {
                let (tx, _rx) = oneshot::channel();
                Item {
                    tx_bytes: bytes,
                    reply: tx,
                    backend_deadline: Instant::now() + Duration::from_secs(1),
                }
            })
            .collect();
        let packed = pack(&items).expect("test transactions should pack");
        assert_eq!(packed.segment_tokens.0, vec![1, 1, 1]);
        assert_eq!(packed.segment_bytes.0, vec![3, 5, 2]);
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0x11; 3]);
        expected.extend_from_slice(&[0x22; 5]);
        expected.extend_from_slice(&[0x33; 2]);
        assert_eq!(packed.xdr.0, expected);
    }
}
