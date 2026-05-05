//! Adaptive batching reader for `getStorageAt`.
//!
//! Drives the shared adaptive-batching machinery in `adaptive_batch.rs`
//! with a storage-read–specific processor: pack concurrent
//! `(address, key)` reads into one `dbgateway_get_items` RPC and demux
//! the parallel response per waiter.
//!
//! Per-item demux: the outer `DbGatewayResult` errors fan out the same
//! `BackendUnavailable` to every waiter (the whole RPC failed); inside
//! a successful payload, each waiter sees its own item folded as
//! `kRPCNotFound → item: None`, `kRPCNone → Some(StorageItemInfo)`, or
//! a `BackendUnavailable` for any other per-item code.

use std::sync::Arc;

use async_trait::async_trait;
use groundhog_db_gateway::{
    AddressAndKey, AddressAndKeyBatch, DbGateway_DbGateway1_Client, DbGatewayItemResult,
    DbGatewayPayload, DbGatewayResult, RPCError, StorageKey, StorageKeyContractData,
};
use groundhog_rpc_api_types::{
    ContractAddress,
    state::{StorageItemInfo, StorageItemRead},
};
use tokio::sync::{RwLock, oneshot};
use xdr_common::VecM;

use crate::adaptive_batch::{AdaptiveBatcher, BatchProcessor, shard_index_by_prefix};
use crate::config::AdaptiveBatchConfig;
use crate::error::GatewayError;

/// Backend for batched storage reads.  In production this wraps the
/// `DbGateway1` client; tests substitute a mock.
#[async_trait]
pub trait StorageReadSink: Send + Sync + 'static {
    async fn fetch(&self, batch: AddressAndKeyBatch) -> Result<DbGatewayPayload, GatewayError>;
}

/// Production sink: forwards to hogd's `dbgateway_get_items` RPC and
/// folds the outer `DbGatewayResult` to a payload-or-error.
///
/// Holds a shared `Arc<RwLock<Option<Client>>>` so that reconnections in
/// `GatewayContext` become visible on the next call.
pub struct DbGatewaySink {
    db_gateway: Arc<RwLock<Option<DbGateway_DbGateway1_Client>>>,
}

impl DbGatewaySink {
    pub(crate) fn new(db_gateway: Arc<RwLock<Option<DbGateway_DbGateway1_Client>>>) -> Self {
        Self { db_gateway }
    }
}

#[async_trait]
impl StorageReadSink for DbGatewaySink {
    async fn fetch(&self, batch: AddressAndKeyBatch) -> Result<DbGatewayPayload, GatewayError> {
        let client = self.db_gateway.read().await.clone();
        let client = client
            .ok_or_else(|| GatewayError::BackendUnavailable("db_gateway not connected".into()))?;
        let outer = client
            .dbgateway_get_items(&batch)
            .await
            .map_err(|e| GatewayError::BackendUnavailable(format!("db_gateway: {e}")))?;
        match outer {
            DbGatewayResult::kRPCNone { payload } => Ok(payload),
            DbGatewayResult::Default { error } => Err(GatewayError::BackendUnavailable(format!(
                "db_gateway: {}",
                RPCError(error)
            ))),
            _ => Err(GatewayError::BackendUnavailable(
                "db_gateway returned an unrecognized result variant".into(),
            )),
        }
    }
}

/// A single queued client read.
struct Item {
    request: AddressAndKey,
    reply: oneshot::Sender<Result<StorageItemRead, GatewayError>>,
}

/// Per-batch processor: pack concurrent reads into one
/// `dbgateway_get_items` call and demux the response per waiter.
struct StorageReadProcessor {
    sink: Arc<dyn StorageReadSink>,
}

#[async_trait]
impl BatchProcessor for StorageReadProcessor {
    type Item = Item;

    async fn process(&self, items: Vec<Item>) {
        let batch: AddressAndKeyBatch = VecM(items.iter().map(|i| i.request.clone()).collect());
        let result = self.sink.fetch(batch).await;
        match result {
            Err(e) => {
                for item in items {
                    let _ = item.reply.send(Err(e.clone()));
                }
            }
            Ok(payload) => {
                let block_height = payload.block_height;
                let mut responses = payload.items.0.into_iter();
                for item in items {
                    let mapped = match responses.next() {
                        Some(DbGatewayItemResult::kRPCNone { item: wire }) => Ok(StorageItemRead {
                            block_height: block_height.clone(),
                            item: Some(StorageItemInfo::from(wire)),
                        }),
                        Some(DbGatewayItemResult::Default { error }) => {
                            let err = RPCError(error);
                            if err == RPCError::kRPCNotFound {
                                Ok(StorageItemRead {
                                    block_height: block_height.clone(),
                                    item: None,
                                })
                            } else {
                                Err(GatewayError::BackendUnavailable(format!(
                                    "db_gateway item: {err}"
                                )))
                            }
                        }
                        Some(_) => Err(GatewayError::BackendUnavailable(
                            "db_gateway returned an unrecognized per-item variant".into(),
                        )),
                        None => Err(GatewayError::BackendUnavailable(
                            "db_gateway response had fewer items than the batch".into(),
                        )),
                    };
                    let _ = item.reply.send(mapped);
                }
            }
        }
    }
}

/// Handle returned to the rest of the gateway.  Clone-friendly; each
/// clone shares the same bank of shard channels.
#[derive(Clone)]
pub struct StorageReadBatcher {
    inner: AdaptiveBatcher<Item>,
}

impl StorageReadBatcher {
    /// Spawn one batcher task per shard.  Tasks live until the
    /// `StorageReadBatcher` (and thus its shard senders) are dropped.
    pub fn spawn(config: &AdaptiveBatchConfig, sink: &Arc<dyn StorageReadSink>) -> Self {
        let processor = Arc::new(StorageReadProcessor { sink: sink.clone() });
        let inner = AdaptiveBatcher::spawn("storage batcher", config, &processor);
        Self { inner }
    }

    /// Read one (`address`, `key`).  Routes to a shard by the storage
    /// key prefix (same key reads land on the same shard, so they
    /// share batch windows); returns when the fanout completes.
    /// Returns `Overloaded` if the target shard's ingress is full.
    #[tracing::instrument(skip_all, fields(?address, shard_idx))]
    pub async fn read(
        &self,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, GatewayError> {
        let shard_idx = shard_index_by_prefix(&key, self.inner.n_shards());
        tracing::Span::current().record("shard_idx", shard_idx);
        let request = AddressAndKey {
            address: address.0,
            key: StorageKey::ContractData {
                contract_data_key: StorageKeyContractData::from(key),
            },
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        self.inner.try_send(
            Item {
                request,
                reply: reply_tx,
            },
            shard_idx,
        )?;
        reply_rx
            .await
            .map_err(|_| GatewayError::BackendUnavailable("storage batcher dropped reply".into()))?
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

    use groundhog_rpc_api_types::hex::Hex0x;
    use groundhog_xdr::storage_item::WireStorageItem;
    use xdr_common::BytesM;

    /// Scripted mock sink.  Each `fetch()` call records the batch size
    /// and, if a "gate" oneshot has been pre-queued, awaits it before
    /// returning.  The gate yields the response payload (or an error)
    /// back to the batcher.
    #[derive(Default)]
    struct MockSink {
        seen: Mutex<Vec<usize>>,
        pending: Mutex<VecDeque<oneshot::Receiver<Result<DbGatewayPayload, GatewayError>>>>,
    }

    impl MockSink {
        fn arc() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn queue_gate(&self) -> oneshot::Sender<Result<DbGatewayPayload, GatewayError>> {
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().push_back(rx);
            tx
        }

        fn seen(&self) -> Vec<usize> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl StorageReadSink for MockSink {
        async fn fetch(&self, batch: AddressAndKeyBatch) -> Result<DbGatewayPayload, GatewayError> {
            let size = batch.0.len();
            self.seen.lock().unwrap().push(size);
            let gate = self.pending.lock().unwrap().pop_front();
            match gate {
                None => Err(GatewayError::BackendUnavailable("test gate missing".into())),
                Some(rx) => rx.await.unwrap_or_else(|_| {
                    Err(GatewayError::BackendUnavailable("test gate dropped".into()))
                }),
            }
        }
    }

    fn key_from_u32(x: u32) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&x.to_be_bytes());
        bytes
    }

    fn address_zero() -> ContractAddress {
        Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32]))
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

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    fn payload_at_height(height: i64, items: Vec<DbGatewayItemResult>) -> DbGatewayPayload {
        DbGatewayPayload {
            block_height: groundhog_xdr::types::BlockHeight::from(height),
            items: VecM(items),
        }
    }

    fn bytestring_item(bytes: Vec<u8>) -> DbGatewayItemResult {
        DbGatewayItemResult::kRPCNone {
            item: WireStorageItem::kItemTypeBytestring {
                bytestring: BytesM(bytes),
            },
        }
    }

    fn not_found_item() -> DbGatewayItemResult {
        DbGatewayItemResult::Default {
            error: RPCError::kRPCNotFound.0,
        }
    }

    fn other_error_item(code: i32) -> DbGatewayItemResult {
        DbGatewayItemResult::Default { error: code }
    }

    #[tokio::test]
    async fn batch_of_1_at_low_load() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();
        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
        );

        let read = tokio::spawn({
            let b = batcher.clone();
            async move { b.read(address_zero(), key_from_u32(0)).await }
        });
        settle().await;
        let _ = gate.send(Ok(payload_at_height(
            7,
            vec![bytestring_item(b"hi".to_vec())],
        )));
        let result = read.await.unwrap().unwrap();

        assert_eq!(mock.seen(), vec![1], "exactly one batch of size 1");
        assert_eq!(*result.block_height, 7);
        match result.item {
            Some(StorageItemInfo::Bytestring { bytes }) => {
                assert_eq!(bytes.0.as_slice(), b"hi");
            }
            other => panic!("expected Bytestring, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn per_item_demux_is_independent() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();

        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
        );

        // Three concurrent reads land in a single batch (no yields between
        // spawns under the current_thread runtime), and the mock returns
        // a 3-item payload demuxing each waiter independently.
        let r0 = tokio::spawn({
            let b = batcher.clone();
            async move { b.read(address_zero(), key_from_u32(0)).await }
        });
        let r1 = tokio::spawn({
            let b = batcher.clone();
            async move { b.read(address_zero(), key_from_u32(1)).await }
        });
        let r2 = tokio::spawn({
            let b = batcher.clone();
            async move { b.read(address_zero(), key_from_u32(2)).await }
        });
        settle().await;

        let _ = gate.send(Ok(payload_at_height(
            42,
            vec![
                bytestring_item(b"present".to_vec()),
                not_found_item(),
                other_error_item(RPCError::kRPCStorageWriteFailed.0),
            ],
        )));

        let res0 = r0.await.unwrap().unwrap();
        match res0.item {
            Some(StorageItemInfo::Bytestring { bytes }) => {
                assert_eq!(bytes.0.as_slice(), b"present");
            }
            other => panic!("r0 expected Bytestring, got {other:?}"),
        }
        assert_eq!(*res0.block_height, 42);

        let res1 = r1.await.unwrap().expect("r1 should be Ok(None)");
        assert!(res1.item.is_none(), "r1 expected None, got {:?}", res1.item);
        assert_eq!(*res1.block_height, 42);

        let err2 = r2
            .await
            .unwrap()
            .expect_err("r2 should be a per-item error");
        assert!(
            matches!(err2, GatewayError::BackendUnavailable(_)),
            "r2 expected BackendUnavailable, got {err2:?}"
        );

        assert_eq!(mock.seen(), vec![3], "all three reads shared one batch");
    }

    #[tokio::test]
    async fn outer_error_fans_out_to_every_waiter() {
        let mock = MockSink::arc();
        let err_gate = mock.queue_gate();

        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
        );

        let mut waiters = vec![];
        for i in 0..4u32 {
            let b = batcher.clone();
            waiters.push(tokio::spawn(async move {
                b.read(address_zero(), key_from_u32(i)).await
            }));
        }
        settle().await;

        let _ = err_gate.send(Err(GatewayError::BackendUnavailable("mock".into())));
        for (i, w) in waiters.into_iter().enumerate() {
            let r = w.await.unwrap();
            assert!(
                matches!(r, Err(GatewayError::BackendUnavailable(_))),
                "waiter {i}: expected BackendUnavailable, got {r:?}"
            );
        }
        assert_eq!(
            mock.seen(),
            vec![4],
            "all four waiters shared a single batch"
        );
    }

    #[tokio::test]
    async fn shards_distribute_across_keys() {
        let n_shards = 4;
        let mut hits = vec![0usize; n_shards];
        for i in 0..10_000u32 {
            let key = key_from_u32(i.wrapping_mul(2_654_435_761)); // scatter
            hits[shard_index_by_prefix(&key, n_shards)] += 1;
        }
        for (i, c) in hits.iter().enumerate() {
            assert!(*c > 1500 && *c < 3500, "shard {i} got {c} (expected ~2500)");
        }
    }
}
