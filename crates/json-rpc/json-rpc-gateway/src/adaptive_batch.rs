//! Adaptive batching infrastructure shared by `submitter.rs` and
//! `storage_batcher.rs`.
//!
//! Both batchers follow the same dispatch pattern: bounded ingress
//! channel per shard, BDP-style cap on in-flight batches, drain
//! everything currently in the channel into one batch on each fire,
//! no time-based flush. At low load a single arrival fires immediately
//! as a batch of one; at high load arrivals pile up while a batch is
//! in flight and drain together when it completes.
//!
//! What differs between callsites is the *processor*: how the drained
//! `Vec<Item>` becomes a backend RPC, and how the response fans out
//! to the per-item reply channels (e.g., shared result for tx submit
//! vs per-item demux for storage reads). That lives in the
//! [`BatchProcessor`] trait the callsite implements.
//!
//! Backpressure lives in the bounded ingress channel itself: callers
//! `try_send` and see `Overloaded` on `Full`. The shard task only
//! reads from ingress when it has capacity to fire
//! (`inflight < max_inflight`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::config::AdaptiveBatchConfig;
use crate::error::GatewayError;
use crate::metrics::{GatewayMetrics, REASON_CLOSED, REASON_INGRESS_FULL};

/// Per-batch processing policy supplied by each batcher callsite.
///
/// The shard runner owns ingress + inflight accounting and hands a
/// drained batch to `process`. Implementations are responsible for
/// calling the backend, fanning out replies into each item's reply
/// channel, and any callsite-specific shaping (packing, demux).
#[async_trait]
pub(crate) trait BatchProcessor: Send + Sync + 'static {
    /// One queued client request.  Carries whatever payload the
    /// callsite needs plus its own reply oneshot — the processor owns
    /// reply distribution.
    type Item: Send + 'static;

    /// Process one drained batch.  The shard guarantees `items` is
    /// non-empty and bounded by `AdaptiveBatchConfig::max_size`.
    async fn process(&self, items: Vec<Self::Item>);
}

/// Per-shard state owned by one batcher task.  Private to the module;
/// the public surface is [`AdaptiveBatcher`].
struct Shard<P: BatchProcessor> {
    ingress: mpsc::Receiver<P::Item>,
    max_inflight: u32,
    max_size: usize,
    processor: Arc<P>,
    component: &'static str,
    metrics: GatewayMetrics,
}

impl<P: BatchProcessor> Shard<P> {
    async fn run(mut self) {
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                biased;

                Some(result) = workers.join_next(), if !workers.is_empty() => {
                    if let Err(error) = result
                        && !error.is_cancelled()
                    {
                        tracing::error!(
                            component = self.component,
                            %error,
                            "adaptive batch worker failed"
                        );
                    }
                }

                // Only read from ingress when we have capacity to fire.
                // That's the backpressure hinge: when the worker set is full,
                // new items pile up in the bounded mpsc until it fills,
                // at which point handlers see `Overloaded` from `try_send`.
                maybe_item = self.ingress.recv(), if workers.len() < self.max_inflight as usize => {
                    let Some(first) = maybe_item else { break; };
                    let mut items = Vec::with_capacity(self.max_size.min(64));
                    items.push(first);
                    while items.len() < self.max_size {
                        match self.ingress.try_recv() {
                            Ok(item) => items.push(item),
                            Err(_) => break,
                        }
                    }
                    let in_flight = self.metrics.internal_batch_in_flight_guard(self.component);
                    let processor = self.processor.clone();
                    workers.spawn(async move {
                        let _in_flight = in_flight;
                        processor.process(items).await;
                    });
                }
            }
        }
        workers.shutdown().await;
    }
}

pub(crate) fn deadline_cohorts<I>(
    mut items: Vec<I>,
    max_skew: Duration,
    deadline: impl Fn(&I) -> Instant,
) -> Vec<Vec<I>> {
    items.sort_unstable_by_key(&deadline);
    let mut cohorts: Vec<Vec<I>> = Vec::new();
    for item in items {
        let item_deadline = deadline(&item);
        let joins_current = cohorts.last().is_some_and(|cohort| {
            item_deadline.saturating_duration_since(deadline(&cohort[0])) <= max_skew
        });
        if joins_current {
            cohorts
                .last_mut()
                .expect("bug: current cohort should exist")
                .push(item);
        } else {
            cohorts.push(vec![item]);
        }
    }
    cohorts
}

/// Shared, clone-friendly handle to a fleet of adaptive-batching shards.
///
/// Each shard carries its own ingress channel; `try_send` routes by
/// the caller's chosen shard index. Drop the last clone to shut shard
/// tasks down (they exit on the ingress channel closing).
struct AdaptiveBatcherInner<I: Send + 'static> {
    shards: Arc<[mpsc::Sender<I>]>,
    tasks: Box<[AbortHandle]>,
}

impl<I: Send + 'static> Drop for AdaptiveBatcherInner<I> {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub(crate) struct AdaptiveBatcher<I: Send + 'static> {
    inner: Arc<AdaptiveBatcherInner<I>>,
    /// Component name used in `Overloaded`/`BackendUnavailable` strings —
    /// e.g. "batch submitter" or "storage batcher".
    name: &'static str,
    component: &'static str,
    metrics: GatewayMetrics,
}

impl<I: Send + 'static> Clone for AdaptiveBatcher<I> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            name: self.name,
            component: self.component,
            metrics: self.metrics.clone(),
        }
    }
}

impl<I: Send + 'static> AdaptiveBatcher<I> {
    /// Spawn one shard task per shard in `config`.  `name` is used in
    /// error strings so saturation/overload messages are
    /// component-tagged (`"<name> ingress queue full"`).
    pub(crate) fn spawn<P>(
        name: &'static str,
        component: &'static str,
        config: &AdaptiveBatchConfig,
        processor: &Arc<P>,
        metrics: GatewayMetrics,
    ) -> Self
    where
        P: BatchProcessor<Item = I>,
    {
        let n_shards = config.shards.max(1);
        let mut senders = Vec::with_capacity(n_shards);
        let mut tasks = Vec::with_capacity(n_shards);
        for _ in 0..n_shards {
            let (sender, ingress) = mpsc::channel(config.ingress_queue_depth.max(1));
            let shard = Shard {
                ingress,
                max_inflight: config.max_inflight_batches.max(1),
                max_size: config.max_size.max(1),
                processor: processor.clone(),
                component,
                metrics: metrics.clone(),
            };
            let task = tokio::spawn(shard.run());
            tasks.push(task.abort_handle());
            senders.push(sender);
        }
        Self {
            inner: Arc::new(AdaptiveBatcherInner {
                shards: senders.into(),
                tasks: tasks.into(),
            }),
            name,
            component,
            metrics,
        }
    }

    /// Number of shards configured.  Stable for the life of the batcher.
    pub(crate) fn n_shards(&self) -> usize {
        self.inner.shards.len()
    }

    /// Abort shard runners and all in-flight workers. Safe to call repeatedly.
    pub(crate) fn abort(&self) {
        for task in &self.inner.tasks {
            task.abort();
        }
    }

    /// Try to enqueue one item on `shard_idx`.  Returns
    /// `BackendUnavailable` if `shard_idx >= n_shards()` (programming
    /// error in the caller — surfaced rather than panicked) or if the
    /// shard channel has been closed (shard task exited);
    /// `Overloaded` when the shard's ingress channel is full.
    pub(crate) fn try_send(&self, item: I, shard_idx: usize) -> Result<(), GatewayError> {
        let Some(sender) = self.inner.shards.get(shard_idx) else {
            return Err(GatewayError::BackendUnavailable(format!(
                "{} shard index {shard_idx} out of range (n_shards={})",
                self.name,
                self.inner.shards.len(),
            )));
        };
        sender.try_send(item).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                self.metrics
                    .observe_internal_batch_rejected(self.component, REASON_INGRESS_FULL);
                GatewayError::Overloaded(format!(
                    "gateway {} ingress queue full (shard {shard_idx})",
                    self.name
                ))
            }
            mpsc::error::TrySendError::Closed(_) => {
                self.metrics
                    .observe_internal_batch_rejected(self.component, REASON_CLOSED);
                GatewayError::BackendUnavailable(format!("{} shut down", self.name))
            }
        })
    }
}

/// Pick a local batcher shard by the first four bytes of `routing_key`.
/// Stable per routing key: repeated requests with the same prefix hit the same shard.
pub(crate) fn shard_index_by_routing_key(routing_key: &[u8], n_shards: usize) -> usize {
    if n_shards <= 1 || routing_key.len() < 4 {
        return 0;
    }
    let prefix = u32::from_be_bytes([
        routing_key[0],
        routing_key[1],
        routing_key[2],
        routing_key[3],
    ]);
    (prefix as usize) % n_shards
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use tokio::sync::{Notify, oneshot};

    use super::{AdaptiveBatcher, BatchProcessor, deadline_cohorts};
    use crate::config::AdaptiveBatchConfig;
    use crate::metrics::GatewayMetrics;

    struct TestItem {
        reply: oneshot::Sender<()>,
    }

    struct PanicOnceProcessor {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl BatchProcessor for PanicOnceProcessor {
        type Item = TestItem;

        async fn process(&self, items: Vec<TestItem>) {
            assert_ne!(
                self.calls.fetch_add(1, Ordering::SeqCst),
                0,
                "intentional test panic"
            );
            for item in items {
                let _ = item.reply.send(());
            }
        }
    }

    struct HangingProcessor {
        started: Arc<Notify>,
    }

    #[async_trait]
    impl BatchProcessor for HangingProcessor {
        type Item = TestItem;

        async fn process(&self, _items: Vec<TestItem>) {
            self.started.notify_one();
            std::future::pending::<()>().await;
        }
    }

    fn test_config() -> AdaptiveBatchConfig {
        AdaptiveBatchConfig {
            shards: 1,
            max_inflight_batches: 1,
            max_size: 10,
            ingress_queue_depth: 10,
        }
    }

    #[tokio::test]
    async fn panicking_processor_releases_inflight_capacity() {
        let processor = Arc::new(PanicOnceProcessor {
            calls: AtomicUsize::new(0),
        });
        let batcher = AdaptiveBatcher::spawn(
            "test batcher",
            "test_batcher",
            &test_config(),
            &processor,
            GatewayMetrics::for_tests(),
        );
        let (first_tx, first_rx) = oneshot::channel();
        batcher
            .try_send(TestItem { reply: first_tx }, 0)
            .expect("bug: first item should enqueue");
        assert!(first_rx.await.is_err(), "panicked worker should drop reply");

        let (second_tx, second_rx) = oneshot::channel();
        batcher
            .try_send(TestItem { reply: second_tx }, 0)
            .expect("bug: second item should enqueue");
        tokio::time::timeout(Duration::from_secs(1), second_rx)
            .await
            .expect("bug: replacement worker should run")
            .expect("bug: replacement worker should send its reply");
    }

    #[tokio::test]
    async fn dropping_last_handle_aborts_inflight_processor() {
        let started = Arc::new(Notify::new());
        let processor = Arc::new(HangingProcessor {
            started: started.clone(),
        });
        let batcher = AdaptiveBatcher::spawn(
            "test batcher",
            "test_batcher",
            &test_config(),
            &processor,
            GatewayMetrics::for_tests(),
        );
        let final_handle = batcher.clone();
        let (reply_tx, mut reply_rx) = oneshot::channel();
        batcher
            .try_send(TestItem { reply: reply_tx }, 0)
            .expect("bug: item should enqueue");
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("bug: processor should start");

        drop(batcher);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut reply_rx)
                .await
                .is_err(),
            "dropping a non-final handle must not cancel work"
        );
        drop(final_handle);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), reply_rx)
                .await
                .expect("bug: final handle drop should cancel worker")
                .is_err(),
            "cancelled worker should drop its reply"
        );
    }

    #[test]
    fn deadline_cohorts_bound_skew_from_each_anchor() {
        let base = Instant::now();
        let items = vec![
            (3, base + Duration::from_millis(21)),
            (1, base),
            (2, base + Duration::from_millis(10)),
            (4, base + Duration::from_millis(30)),
        ];

        let cohorts = deadline_cohorts(items, Duration::from_millis(10), |item| item.1);

        assert_eq!(cohorts.len(), 2);
        assert_eq!(
            cohorts[0].iter().map(|item| item.0).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            cohorts[1].iter().map(|item| item.0).collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn deadline_cohorts_do_not_chain_skew_transitively() {
        let base = Instant::now();
        let items = vec![
            base,
            base + Duration::from_millis(9),
            base + Duration::from_millis(18),
        ];
        let expected = vec![vec![items[0], items[1]], vec![items[2]]];

        let cohorts = deadline_cohorts(items, Duration::from_millis(10), |deadline| *deadline);

        assert_eq!(cohorts, expected);
    }
}
