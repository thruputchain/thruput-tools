//! Adaptive batching submitter for `sendRawTransaction`.
//!
//! Drives the shared adaptive-batching machinery in `adaptive_batch.rs`
//! with a tx-submission–specific processor: pack N client tx-byte blobs
//! into one `ParallelSignedTransactionBatch`, call
//! `schedulercoord_submit_unvetted`, and fan out the shared result
//! across every waiter in the batch.
//!
//! See `AdaptiveBatchConfig` for the dispatch policy (no timer,
//! BDP-style in-flight bounding, sharded for low contention) and
//! `adaptive_batch.rs` for the shared shard runner.

use std::sync::Arc;

use async_trait::async_trait;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_scheduler::ParallelBatch;
use groundhog_scheduler::parallel::ParallelSignedTransactionBatch;
use groundhog_scheduler::sched_coord::SchedulerCoord_SchedulerCoord1_Client;
use tokio::sync::{RwLock, oneshot};
use xdr_common::{BytesM, VecM};

use crate::adaptive_batch::{AdaptiveBatcher, BatchProcessor, shard_index_by_routing_key};
use crate::config::AdaptiveBatchConfig;
use crate::error::GatewayError;

/// Backend for batched submissions.  In production this wraps the
/// scheduler client; tests substitute a mock.
#[async_trait]
pub trait SubmitSink: Send + Sync + 'static {
    async fn submit(&self, batch: ParallelSignedTransactionBatch) -> Result<(), GatewayError>;
}

/// Production sink: forwards to the hogd scheduler's
/// `schedulercoord_submit_unvetted` RPC.
///
/// Holds a shared `Arc<RwLock<Option<Client>>>` so that scheduler
/// reconnections in `GatewayContext` become visible on the next call.
pub struct SchedulerSink {
    scheduler: Arc<RwLock<Option<SchedulerCoord_SchedulerCoord1_Client>>>,
}

impl SchedulerSink {
    pub(crate) fn new(
        scheduler: Arc<RwLock<Option<SchedulerCoord_SchedulerCoord1_Client>>>,
    ) -> Self {
        Self { scheduler }
    }
}

#[async_trait]
impl SubmitSink for SchedulerSink {
    async fn submit(&self, batch: ParallelSignedTransactionBatch) -> Result<(), GatewayError> {
        let client = self.scheduler.read().await.clone();
        match client {
            None => Err(GatewayError::BackendUnavailable(
                "scheduler not connected".into(),
            )),
            Some(c) => c
                .schedulercoord_submit_unvetted(&batch)
                .await
                .map_err(|e| classify_rpc_error(&e)),
        }
    }
}

/// Map a scheduler RPC error to a gateway error.
///
/// v1 is intentionally conservative: the `rpc_driver::RpcError` shape
/// isn't stable enough to commit to timeout/overload detection yet, so
/// all transport failures map to `BackendUnavailable`.
fn classify_rpc_error(e: &rpc_driver::RpcError) -> GatewayError {
    GatewayError::BackendUnavailable(format!("scheduler: {e}"))
}

/// A single queued client submission.
struct Item {
    tx_bytes: Vec<u8>,
    reply: oneshot::Sender<Result<(), GatewayError>>,
}

/// Per-batch processor: pack, submit, and fan the shared result out
/// to every waiter in the drained batch.
struct SubmitProcessor {
    sink: Arc<dyn SubmitSink>,
}

#[async_trait]
impl BatchProcessor for SubmitProcessor {
    type Item = Item;

    async fn process(&self, items: Vec<Item>) {
        let packed = pack(&items);
        let result = self.sink.submit(packed).await;
        for item in items {
            let _ = item.reply.send(result.clone());
        }
    }
}

/// Pack N client tx-byte blobs into one `ParallelSignedTransactionBatch`.
///
/// One segment per tx — same shape as today's single-tx handler, scaled up.
fn pack(items: &[Item]) -> ParallelSignedTransactionBatch {
    let n = items.len();
    let mut segment_tokens = Vec::with_capacity(n);
    let mut segment_bytes = Vec::with_capacity(n);
    let total_bytes: usize = items.iter().map(|i| i.tx_bytes.len()).sum();
    let mut xdr = Vec::with_capacity(total_bytes);
    for item in items {
        segment_tokens.push(1u32);
        // Bounded by `max_size` and the handler's own size check
        // (`send_raw_transaction` rejects inputs larger than u32::MAX
        // before handing them to the submitter).
        let tx_len = u32::try_from(item.tx_bytes.len())
            .expect("bug: tx_bytes > u32::MAX reached the submitter");
        segment_bytes.push(tx_len);
        xdr.extend_from_slice(&item.tx_bytes);
    }
    ParallelSignedTransactionBatch::new(ParallelBatch {
        segment_tokens: VecM(segment_tokens),
        segment_bytes: VecM(segment_bytes),
        xdr: BytesM(xdr),
    })
}

/// Handle returned to the rest of the gateway.  Clone-friendly; each
/// clone shares the same bank of shard channels.
#[derive(Clone)]
pub struct BatchSubmitter {
    inner: AdaptiveBatcher<Item>,
}

impl BatchSubmitter {
    /// Spawn one submitter task per shard.  Tasks live until the
    /// `BatchSubmitter` (and thus its shard senders) are dropped.
    pub fn spawn(config: &AdaptiveBatchConfig, sink: &Arc<dyn SubmitSink>) -> Self {
        let processor = Arc::new(SubmitProcessor { sink: sink.clone() });
        let inner = AdaptiveBatcher::spawn("batch submitter", config, &processor);
        Self { inner }
    }

    /// Submit one tx.  Routes to a shard by tx hash; returns when the
    /// fanout completes.  Returns `Overloaded` if the target shard's
    /// ingress is full (client should retry with backoff).
    #[tracing::instrument(skip_all, fields(shard_idx, tx_bytes_len = tx_bytes.len()))]
    pub async fn submit(&self, tx_bytes: Vec<u8>, hash: TxHash) -> Result<(), GatewayError> {
        let bytes: &[u8] = hash.0.0.as_ref();
        let shard_idx = shard_index_by_routing_key(bytes, self.inner.n_shards());
        tracing::Span::current().record("shard_idx", shard_idx);
        let (reply_tx, reply_rx) = oneshot::channel();
        self.inner.try_send(
            Item {
                tx_bytes,
                reply: reply_tx,
            },
            shard_idx,
        )?;
        reply_rx
            .await
            .map_err(|_| GatewayError::BackendUnavailable("batch submitter dropped reply".into()))?
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Scripted mock sink.  Each `submit()` call records the batch size
    /// and, if a "gate" oneshot has been pre-queued, awaits it before
    /// returning.  If no gates remain, returns `Ok` immediately.
    #[derive(Default)]
    struct MockSink {
        seen: Mutex<Vec<usize>>,
        pending: Mutex<VecDeque<oneshot::Receiver<Result<(), GatewayError>>>>,
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
    }

    #[async_trait]
    impl SubmitSink for MockSink {
        async fn submit(&self, batch: ParallelSignedTransactionBatch) -> Result<(), GatewayError> {
            let size = batch.segment_bytes.0.len();
            self.seen.lock().unwrap().push(size);
            let gate = self.pending.lock().unwrap().pop_front();
            match gate {
                None => Ok(()),
                Some(rx) => rx.await.unwrap_or_else(|_| {
                    Err(GatewayError::BackendUnavailable("test gate dropped".into()))
                }),
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

    #[tokio::test]
    async fn batch_of_1_at_low_load() {
        let mock = MockSink::arc();
        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
        );

        submitter
            .submit(vec![0xaa; 8], hash_from_u32(0))
            .await
            .unwrap();

        assert_eq!(mock.seen(), vec![1], "exactly one batch of size 1");
    }

    #[tokio::test]
    async fn adaptive_batching_drains_on_completion() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 1, 1000, 100), // strict serial batches
            &(mock.clone() as Arc<dyn SubmitSink>),
        );

        // First tx starts a gated batch of 1.
        let first = tokio::spawn({
            let s = submitter.clone();
            async move { s.submit(vec![0x01], hash_from_u32(0)).await }
        });
        settle().await;
        assert_eq!(mock.seen(), vec![1], "first batch in flight");

        // Pile up 5 more while the first is blocked.
        let mut waiters = vec![];
        for i in 1..=5u32 {
            let s = submitter.clone();
            waiters.push(tokio::spawn(async move {
                s.submit(vec![u8::try_from(i).unwrap()], hash_from_u32(i))
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
    async fn max_inflight_is_respected() {
        let mock = MockSink::arc();
        let gate_a = mock.queue_gate();
        let gate_b = mock.queue_gate();

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 2, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
        );

        let w1 = {
            let s = submitter.clone();
            tokio::spawn(async move { s.submit(vec![0x01], hash_from_u32(1)).await })
        };
        settle().await;
        let w2 = {
            let s = submitter.clone();
            tokio::spawn(async move { s.submit(vec![0x02], hash_from_u32(2)).await })
        };
        settle().await;
        assert_eq!(mock.seen(), vec![1, 1], "two concurrent in-flight batches");

        // A third should sit queued until one completes.
        let w3 = {
            let s = submitter.clone();
            tokio::spawn(async move { s.submit(vec![0x03], hash_from_u32(3)).await })
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
        );

        let first = {
            let s = submitter.clone();
            tokio::spawn(async move { s.submit(vec![0x01], hash_from_u32(0)).await })
        };
        settle().await;

        let mut waiters = vec![];
        for i in 1..=10u32 {
            let s = submitter.clone();
            waiters.push(tokio::spawn(async move {
                s.submit(vec![u8::try_from(i).unwrap()], hash_from_u32(i))
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
        );

        // First item is pulled from mpsc and starts the gated batch.
        let w1 = {
            let s = submitter.clone();
            tokio::spawn(async move { s.submit(vec![0x01], hash_from_u32(0)).await })
        };
        settle().await;

        // Second item: try_send succeeds (channel has 1 slot, currently empty
        // because the submitter already pulled item 1).  But with
        // max_inflight=1 and gate held, the submitter's ingress branch is
        // disabled, so this item sits in the channel.
        let w2 = {
            let s = submitter.clone();
            tokio::spawn(async move { s.submit(vec![0x02], hash_from_u32(1)).await })
        };
        settle().await;

        // Third item: channel is now full (item 2 occupies the only slot).
        // try_send → Full → Overloaded.
        let err = submitter
            .submit(vec![0x99], hash_from_u32(99))
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
    async fn backend_error_fans_out_to_every_waiter() {
        let mock = MockSink::arc();
        let err_gate = mock.queue_gate();

        let submitter = BatchSubmitter::spawn(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn SubmitSink>),
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
                s.submit(vec![u8::try_from(i).unwrap()], hash_from_u32(i))
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
    async fn pack_shape_is_correct() {
        let items: Vec<Item> = [vec![0x11; 3], vec![0x22; 5], vec![0x33; 2]]
            .into_iter()
            .map(|bytes| {
                let (tx, _rx) = oneshot::channel();
                Item {
                    tx_bytes: bytes,
                    reply: tx,
                }
            })
            .collect();
        let packed = pack(&items);
        assert_eq!(packed.segment_tokens.0, vec![1, 1, 1]);
        assert_eq!(packed.segment_bytes.0, vec![3, 5, 2]);
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0x11; 3]);
        expected.extend_from_slice(&[0x22; 5]);
        expected.extend_from_slice(&[0x33; 2]);
        assert_eq!(packed.xdr.0, expected);
    }
}
