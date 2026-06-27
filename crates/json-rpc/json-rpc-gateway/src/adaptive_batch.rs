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

use async_trait::async_trait;
use tokio::sync::mpsc;

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
    completion_rx: mpsc::UnboundedReceiver<()>,
    completion_tx: mpsc::UnboundedSender<()>,
    inflight: u32,
    max_inflight: u32,
    max_size: usize,
    processor: Arc<P>,
    component: &'static str,
    metrics: GatewayMetrics,
}

impl<P: BatchProcessor> Shard<P> {
    async fn run(mut self) {
        loop {
            tokio::select! {
                // Prefer completions so `inflight` is decremented before we
                // consider firing another batch.
                biased;

                Some(()) = self.completion_rx.recv() => {
                    self.inflight = self.inflight.saturating_sub(1);
                }

                // Only read from ingress when we have capacity to fire.
                // That's the backpressure hinge: when `inflight == max`,
                // new items pile up in the bounded mpsc until it fills,
                // at which point handlers see `Overloaded` from `try_send`.
                maybe_item = self.ingress.recv(), if self.inflight < self.max_inflight => {
                    let Some(first) = maybe_item else { break; };
                    let mut items = Vec::with_capacity(self.max_size.min(64));
                    items.push(first);
                    while items.len() < self.max_size {
                        match self.ingress.try_recv() {
                            Ok(item) => items.push(item),
                            Err(_) => break,
                        }
                    }
                    self.inflight += 1;
                    self.metrics.observe_internal_batch_size(self.component, items.len());
                    let in_flight = self.metrics.internal_batch_in_flight_guard(self.component);
                    let processor = self.processor.clone();
                    let done = self.completion_tx.clone();
                    tokio::spawn(async move {
                        let _in_flight = in_flight;
                        processor.process(items).await;
                        let _ = done.send(());
                    });
                }
            }
        }
    }
}

/// Shared, clone-friendly handle to a fleet of adaptive-batching shards.
///
/// Each shard carries its own ingress channel; `try_send` routes by
/// the caller's chosen shard index. Drop the last clone to shut shard
/// tasks down (they exit on the ingress channel closing).
pub(crate) struct AdaptiveBatcher<I: Send + 'static> {
    shards: Arc<[mpsc::Sender<I>]>,
    /// Component name used in `Overloaded`/`BackendUnavailable` strings —
    /// e.g. "batch submitter" or "storage batcher".
    name: &'static str,
    component: &'static str,
    metrics: GatewayMetrics,
}

impl<I: Send + 'static> Clone for AdaptiveBatcher<I> {
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
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
        for _ in 0..n_shards {
            let (sender, ingress) = mpsc::channel(config.ingress_queue_depth.max(1));
            let (completion_tx, completion_rx) = mpsc::unbounded_channel();
            let shard = Shard {
                ingress,
                completion_tx,
                completion_rx,
                inflight: 0,
                max_inflight: config.max_inflight_batches.max(1),
                max_size: config.max_size.max(1),
                processor: processor.clone(),
                component,
                metrics: metrics.clone(),
            };
            tokio::spawn(shard.run());
            senders.push(sender);
        }
        Self {
            shards: senders.into(),
            name,
            component,
            metrics,
        }
    }

    /// Number of shards configured.  Stable for the life of the batcher.
    pub(crate) fn n_shards(&self) -> usize {
        self.shards.len()
    }

    /// Try to enqueue one item on `shard_idx`.  Returns
    /// `BackendUnavailable` if `shard_idx >= n_shards()` (programming
    /// error in the caller — surfaced rather than panicked) or if the
    /// shard channel has been closed (shard task exited);
    /// `Overloaded` when the shard's ingress channel is full.
    pub(crate) fn try_send(&self, item: I, shard_idx: usize) -> Result<(), GatewayError> {
        let Some(sender) = self.shards.get(shard_idx) else {
            return Err(GatewayError::BackendUnavailable(format!(
                "{} shard index {shard_idx} out of range (n_shards={})",
                self.name,
                self.shards.len(),
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
