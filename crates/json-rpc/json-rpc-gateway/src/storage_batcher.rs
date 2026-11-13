//! Adaptive batching reader for `DbGateway` latest-state reads:
//! `getStorage`, `getBalance`, and `getRegisteredAuth`.
//!
//! Drives the shared adaptive-batching machinery in `adaptive_batch.rs`
//! with a storage-read–specific processor: pack concurrent
//! `(address, key)` reads into one `dbgateway_get_items` RPC and demux
//! the parallel response per waiter.
//!
//! Per-item demux: the outer `DbGatewayResult` errors fan out the same
//! `BackendUnavailable` to every waiter (the whole RPC failed); inside
//! a successful payload, each waiter receives its own raw item labeled
//! with the batch's snapshot height, and projects it for its method:
//! `kRPCNotFound` is an absent key, `kRPCNone` carries the stored item,
//! and any other per-item code is a `BackendUnavailable`.

use std::{sync::Arc, time::Duration, time::Instant};

use async_trait::async_trait;
use thruput_db_gateway::AddressAndKeyBatch;
use thruput_db_gateway::{DbGatewayItemResult, DbGatewayPayload, DbGatewayResult};
use thruput_hogd_backend::db_gateway as shared_db_gateway;
use thruput_json_rpc_server::RpcRequestContext;
use thruput_json_rpc_types::{BlockHeight, ContractAddress};
use thruput_xdr::types::{AddressAndKey, StorageKey};
use tokio::sync::{RwLock, oneshot};
use tracing::Instrument;
use xdr_common::VecM;

use crate::adaptive_batch::{
    AdaptiveBatcher, BatchProcessor, deadline_cohorts, shard_index_by_routing_key,
};
use crate::config::{AdaptiveBatchConfig, HealthBackend};
use crate::context::DbGatewayClient;
use crate::handlers::DB_GATEWAY;
use crate::metrics::{BackendRequestOutcome, GatewayMetrics};
use thruput_json_rpc_server::RpcServerError;

/// Backend for batched storage reads.  In production this wraps the
/// `DbGateway1` client; tests substitute a mock.
#[async_trait]
pub trait StorageReadSink: Send + Sync + 'static {
    async fn fetch(
        &self,
        batch: AddressAndKeyBatch,
        deadline: Instant,
    ) -> Result<DbGatewayPayload, RpcServerError>;
}

/// Production sink: forwards to hogd's `dbgateway_get_items` RPC and
/// folds the outer `DbGatewayResult` to a payload-or-error.
///
/// Holds a shared `Arc<RwLock<Option<Client>>>` so that reconnections in
/// `GatewayContext` become visible on the next call.
pub struct DbGatewaySink {
    db_gateway: Arc<RwLock<Option<DbGatewayClient>>>,
    metrics: GatewayMetrics,
    timeout: Duration,
}

impl DbGatewaySink {
    pub(crate) fn new(
        db_gateway: Arc<RwLock<Option<DbGatewayClient>>>,
        metrics: GatewayMetrics,
        timeout: Duration,
    ) -> Self {
        Self {
            db_gateway,
            metrics,
            timeout,
        }
    }
}

#[async_trait]
impl StorageReadSink for DbGatewaySink {
    async fn fetch(
        &self,
        batch: AddressAndKeyBatch,
        deadline: Instant,
    ) -> Result<DbGatewayPayload, RpcServerError> {
        let client = self.db_gateway.read().await.clone();
        let client = client
            .ok_or_else(|| RpcServerError::BackendUnavailable("db_gateway not connected".into()))?;
        let started_at = Instant::now();
        let timeout = self
            .timeout
            .min(deadline.saturating_duration_since(started_at));
        if timeout.is_zero() {
            return Err(RpcServerError::BackendTimeout);
        }
        let outer_result = tokio::time::timeout(timeout, client.dbgateway_get_items(&batch))
            .instrument(crate::telemetry::backend_span(
                "db_gateway",
                "dbgateway_get_items",
            ))
            .await;
        self.metrics.observe_backend_request(
            HealthBackend::DbGateway,
            "dbgateway_get_items",
            match &outer_result {
                Ok(Ok(DbGatewayResult::kRPCNone { .. })) => BackendRequestOutcome::Ok,
                Ok(Ok(_)) => BackendRequestOutcome::Rejected,
                Ok(Err(_)) => BackendRequestOutcome::Error,
                Err(_) => BackendRequestOutcome::Timeout,
            },
            started_at.elapsed(),
        );
        let outer = outer_result
            .map_err(|_| RpcServerError::BackendTimeout)?
            .map_err(|e| RpcServerError::BackendUnavailable(format!("db_gateway: {e}")))?;
        Ok(shared_db_gateway::payload(DB_GATEWAY, outer)?)
    }
}

/// A single queued client read.
struct Item {
    request: AddressAndKey,
    reply: oneshot::Sender<Result<ItemAnswer, RpcServerError>>,
    backend_deadline: Instant,
}

/// One waiter's unprojected answer: its per-item result and the snapshot
/// height of the batch it rode in.
struct ItemAnswer {
    block_height: BlockHeight,
    item: DbGatewayItemResult,
    complete_batch: bool,
}

/// Per-batch processor: pack concurrent reads into one
/// `dbgateway_get_items` call and demux the response per waiter.
struct StorageReadProcessor {
    sink: Arc<dyn StorageReadSink>,
    metrics: GatewayMetrics,
    max_deadline_skew: Duration,
}

#[async_trait]
impl BatchProcessor for StorageReadProcessor {
    type Item = Item;

    async fn process(&self, items: Vec<Item>) {
        let now = Instant::now();
        let mut live = Vec::with_capacity(items.len());
        for item in items {
            if item.reply.is_closed() {
                continue;
            }
            if item.backend_deadline <= now {
                let _ = item.reply.send(Err(RpcServerError::BackendTimeout));
            } else {
                live.push(item);
            }
        }
        for mut cohort in
            deadline_cohorts(live, self.max_deadline_skew, |item| item.backend_deadline)
        {
            let now = Instant::now();
            cohort.retain(|item| !item.reply.is_closed());
            if cohort.is_empty() {
                continue;
            }
            let deadline = cohort[0].backend_deadline;
            if deadline <= now {
                for item in cohort {
                    let _ = item.reply.send(Err(RpcServerError::BackendTimeout));
                }
                continue;
            }
            self.metrics
                .observe_internal_batch_size("storage_batcher", cohort.len());
            let batch: AddressAndKeyBatch =
                VecM(cohort.iter().map(|i| i.request.clone()).collect());
            let result = self.sink.fetch(batch, deadline).await;
            match result {
                Err(e) => {
                    for item in cohort {
                        let _ = item.reply.send(Err(e.clone()));
                    }
                }
                Ok(payload) => {
                    let complete_batch = payload.items.0.len() == cohort.len();
                    let block_height = payload.block_height;
                    let mut responses = payload.items.0.into_iter();
                    for item in cohort {
                        let answer = responses
                            .next()
                            .map(|response| ItemAnswer {
                                block_height: block_height.clone(),
                                item: response,
                                complete_batch,
                            })
                            .ok_or_else(|| shared_db_gateway::missing_item_error(DB_GATEWAY));
                        let _ = item.reply.send(answer);
                    }
                }
            }
        }
    }
}

fn validate_native_answer(answer: &ItemAnswer) -> Result<(), RpcServerError> {
    if !answer.complete_batch {
        return Err(RpcServerError::invalid_data(
            thruput_json_rpc_types::error::native::BackendDataInvalidReason::InvalidCardinality,
            "storage batch response count differs from request count",
        ));
    }
    Ok(())
}

/// Handle returned to the rest of the gateway.  Clone-friendly; each
/// clone shares the same bank of shard channels.
#[derive(Clone)]
pub struct StorageReadBatcher {
    inner: AdaptiveBatcher<Item>,
    request_timeout: Duration,
    backend_timeout: Duration,
}

impl StorageReadBatcher {
    /// Spawn one batcher task per shard.  Tasks live until the
    /// `StorageReadBatcher` (and thus its shard senders) are dropped.
    #[cfg(test)]
    pub fn spawn(
        config: &AdaptiveBatchConfig,
        sink: &Arc<dyn StorageReadSink>,
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
        sink: &Arc<dyn StorageReadSink>,
        metrics: GatewayMetrics,
        request_timeout: Duration,
        backend_timeout: Duration,
        max_deadline_skew: Duration,
    ) -> Self {
        let processor = Arc::new(StorageReadProcessor {
            sink: sink.clone(),
            metrics: metrics.clone(),
            max_deadline_skew,
        });
        let inner = AdaptiveBatcher::spawn(
            "storage batcher",
            "storage_batcher",
            config,
            &processor,
            metrics,
        );
        Self {
            inner,
            request_timeout,
            backend_timeout,
        }
    }

    /// Abort storage shard runners and all in-flight reads.
    pub(crate) fn abort(&self) {
        self.inner.abort();
    }

    /// Native reads retain and validate the original batch cardinality.
    pub(crate) async fn native_storage(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: thruput_json_rpc_server::native::read::StateKey,
    ) -> Result<thruput_json_rpc_types::responses::StorageResult, RpcServerError> {
        use thruput_json_rpc_server::native::read::StateKey;
        let (storage_key, routing_key) = match key {
            StateKey::ContractData(key) => (shared_db_gateway::contract_data_key(key), key),
            StateKey::FreeNativeTokenBalance => {
                (StorageKey::FreeNativeTokenBalance, address.0.into_inner())
            }
        };
        let answer = self
            .read_key(context, address, storage_key, &routing_key)
            .await?;
        validate_native_answer(&answer)?;
        thruput_hogd_backend::native::storage(answer.block_height, answer.item)
    }

    /// Reserved deployment metadata uses the same committed, complete batch reads.
    pub(crate) async fn native_contract_deployment(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<thruput_json_rpc_types::responses::ContractDeploymentResult, RpcServerError> {
        let answer = self
            .read_key(context, address, StorageKey::Metadata, address.0.as_ref())
            .await?;
        validate_native_answer(&answer)?;
        thruput_hogd_backend::native::contract_deployment(address, answer.block_height, answer.item)
    }

    /// Native authorization reads share queues but never accept a partial batch.
    pub(crate) async fn native_registered_auth(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<thruput_json_rpc_types::responses::RegisteredAuthResult, RpcServerError> {
        let answer = self
            .read_key(context, address, StorageKey::AuthData, address.0.as_ref())
            .await?;
        validate_native_answer(&answer)?;
        thruput_hogd_backend::native::registered_auth(address, answer.block_height, answer.item)
    }

    // `routing_key` is only for local AdaptiveBatcher queue routing; it is not
    // sent to hogd. Use bytes that identify the logical item well enough to keep
    // repeated reads stable while spreading different reads across queues.
    async fn read_key(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        storage_key: StorageKey,
        routing_key: &[u8],
    ) -> Result<ItemAnswer, RpcServerError> {
        let shard_idx = shard_index_by_routing_key(routing_key, self.inner.n_shards());
        tracing::Span::current().record("shard_idx", shard_idx);
        let request = AddressAndKey {
            address: address.0,
            key: storage_key,
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        let request_deadline = context
            .capped_from_start(self.request_timeout)
            .deadline()
            .expect("operation timeout should bound storage read");
        let backend_deadline = context
            .capped_from_start(self.backend_timeout)
            .deadline()
            .expect("backend timeout should bound storage read");
        if backend_deadline <= Instant::now() {
            return Err(RpcServerError::BackendTimeout);
        }
        self.inner.try_send(
            Item {
                request,
                reply: reply_tx,
                backend_deadline,
            },
            shard_idx,
        )?;
        tokio::time::timeout_at(request_deadline.into(), reply_rx)
            .await
            .map_err(|_| RpcServerError::BackendTimeout)?
            .map_err(|_| {
                RpcServerError::BackendUnavailable("storage batcher dropped reply".into())
            })?
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use thruput_node_rpc::RPCError;

    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;
    use thruput_json_rpc_server::native::read::StateKey;
    use thruput_json_rpc_types::responses;

    use thruput_json_rpc_types::base58::Base58;
    use thruput_xdr::address_auth::{AddressAuthData, PublicKeyConfig, PublicKeyConfigOpt};
    use thruput_xdr::storage_item::WireStorageItem;
    use thruput_xdr::types::{Ed25519PublicKey, PublicKey};
    use xdr_common::BytesM;

    /// Scripted mock sink.  Each `fetch()` call records the batch size
    /// and, if a "gate" oneshot has been pre-queued, awaits it before
    /// returning.  The gate yields the response payload (or an error)
    /// back to the batcher.
    #[derive(Default)]
    struct MockSink {
        seen: Mutex<Vec<usize>>,
        batches: Mutex<Vec<AddressAndKeyBatch>>,
        deadlines: Mutex<Vec<Instant>>,
        pending: Mutex<VecDeque<oneshot::Receiver<Result<DbGatewayPayload, RpcServerError>>>>,
    }

    impl MockSink {
        fn arc() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn queue_gate(&self) -> oneshot::Sender<Result<DbGatewayPayload, RpcServerError>> {
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().push_back(rx);
            tx
        }

        fn seen(&self) -> Vec<usize> {
            self.seen.lock().unwrap().clone()
        }

        fn batches(&self) -> Vec<AddressAndKeyBatch> {
            self.batches.lock().unwrap().clone()
        }

        fn deadlines(&self) -> Vec<Instant> {
            self.deadlines.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl StorageReadSink for MockSink {
        async fn fetch(
            &self,
            batch: AddressAndKeyBatch,
            deadline: Instant,
        ) -> Result<DbGatewayPayload, RpcServerError> {
            let size = batch.0.len();
            self.seen.lock().unwrap().push(size);
            self.batches.lock().unwrap().push(batch);
            self.deadlines.lock().unwrap().push(deadline);
            let gate = self.pending.lock().unwrap().pop_front();
            match gate {
                None => Err(RpcServerError::BackendUnavailable(
                    "test gate missing".into(),
                )),
                Some(rx) => rx.await.unwrap_or_else(|_| {
                    Err(RpcServerError::BackendUnavailable(
                        "test gate dropped".into(),
                    ))
                }),
            }
        }
    }

    impl StorageReadBatcher {
        async fn read_contract_data(
            &self,
            context: RpcRequestContext,
            address: ContractAddress,
            key: [u8; 32],
        ) -> Result<responses::StorageResult, RpcServerError> {
            self.native_storage(context, address, StateKey::ContractData(key))
                .await
        }

        async fn read_balance(
            &self,
            context: RpcRequestContext,
            address: ContractAddress,
        ) -> Result<responses::StorageResult, RpcServerError> {
            self.native_storage(context, address, StateKey::FreeNativeTokenBalance)
                .await
        }
    }

    fn key_from_u32(x: u32) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&x.to_be_bytes());
        bytes
    }

    fn address_zero() -> ContractAddress {
        Base58(thruput_xdr::types::ContractAddress::from([0u8; 32]))
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

    fn request_context() -> RpcRequestContext {
        RpcRequestContext::unbounded()
    }

    fn payload_at_height(height: i64, items: Vec<DbGatewayItemResult>) -> DbGatewayPayload {
        DbGatewayPayload {
            block_height: thruput_xdr::types::BlockHeight::from(height),
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
            error: RPCError::kRPCNotFound,
        }
    }

    fn other_error_item(code: i32) -> DbGatewayItemResult {
        DbGatewayItemResult::Default {
            error: thruput_node_rpc::RPCError(code),
        }
    }

    #[tokio::test]
    async fn batch_of_1_at_low_load() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();
        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
        );

        let read = tokio::spawn({
            let b = batcher.clone();
            async move {
                b.read_contract_data(request_context(), address_zero(), key_from_u32(0))
                    .await
            }
        });
        settle().await;
        let _ = gate.send(Ok(payload_at_height(
            7,
            vec![bytestring_item(b"hi".to_vec())],
        )));
        let result = read.await.unwrap().unwrap();

        assert_eq!(mock.seen(), vec![1], "exactly one batch of size 1");
        assert_eq!(*result.state_block_height, 7);
        match result.item {
            Some(responses::StorageItem::Bytestring { bytes }) => {
                assert_eq!(bytes.0.as_slice(), b"hi");
            }
            other => panic!("expected Bytestring, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_deadline_reaches_storage_sink() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();
        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
        );
        let context = RpcRequestContext::new(Some(Duration::from_secs(5)));
        let expected = context.deadline().expect("bounded request deadline");

        let read = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .read_contract_data(context, address_zero(), key_from_u32(0))
                    .await
            }
        });
        settle().await;
        gate.send(Ok(payload_at_height(7, vec![not_found_item()])))
            .expect("storage gate should be open");
        read.await
            .expect("storage task should join")
            .expect("storage read should complete");

        assert_eq!(mock.deadlines(), vec![expected]);
    }

    #[tokio::test]
    async fn expired_storage_read_is_not_dispatched() {
        let mock = MockSink::arc();
        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
        );

        let result = batcher
            .read_contract_data(
                RpcRequestContext::new(Some(Duration::ZERO)),
                address_zero(),
                key_from_u32(0),
            )
            .await;

        assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
        assert!(mock.seen().is_empty());
    }

    #[tokio::test]
    async fn native_token_balance_read_uses_free_native_token_balance_key() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();
        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
        );

        let address = address_zero();
        let read = tokio::spawn({
            let b = batcher.clone();
            async move { b.read_balance(request_context(), address).await }
        });
        settle().await;
        let _ = gate.send(Ok(payload_at_height(9, vec![not_found_item()])));
        let result = read.await.unwrap().unwrap();

        assert_eq!(*result.state_block_height, 9);
        assert!(result.item.is_none());
        let recorded = mock.batches();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0.len(), 1);
        assert_eq!(recorded[0].0[0].address, address.0);
        assert_eq!(recorded[0].0[0].key, StorageKey::FreeNativeTokenBalance);
    }

    #[tokio::test]
    async fn address_auth_data_read_uses_auth_data_key() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();
        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 4, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
        );

        let address = address_zero();
        let read = tokio::spawn({
            let b = batcher.clone();
            async move { b.native_registered_auth(request_context(), address).await }
        });
        settle().await;
        let _ = gate.send(Ok(payload_at_height(11, vec![not_found_item()])));
        let result = read.await.unwrap().unwrap();

        assert_eq!(result.address, address);
        assert_eq!(*result.state_block_height, 11);
        assert!(result.configuration.is_none());
        let recorded = mock.batches();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0.len(), 1);
        assert_eq!(recorded[0].0[0].address, address.0);
        assert_eq!(recorded[0].0[0].key, StorageKey::AuthData);
    }

    #[tokio::test]
    async fn auth_data_and_balance_reads_share_a_batch_and_project_their_own_items() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();
        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
        );

        let address = address_zero();
        let balance = tokio::spawn({
            let b = batcher.clone();
            async move { b.read_balance(request_context(), address).await }
        });
        let auth = tokio::spawn({
            let b = batcher.clone();
            async move { b.native_registered_auth(request_context(), address).await }
        });
        settle().await;

        let mut public_keys: [PublicKeyConfigOpt; 21] =
            std::array::from_fn(|_| PublicKeyConfigOpt::new(None));
        public_keys[2] = PublicKeyConfigOpt::new(Some(PublicKeyConfig {
            key: PublicKey::Ed25519 {
                ed25519: Ed25519PublicKey::from([0xcd; 32]),
            },
            max_weight: 4,
        }));
        let _ = gate.send(Ok(payload_at_height(
            13,
            vec![
                DbGatewayItemResult::kRPCNone {
                    item: WireStorageItem::kItemTypeNonnegativeInt {
                        nnint: thruput_xdr::types::int128_xdr {
                            highbits: 0,
                            lowbits: 500,
                        },
                    },
                },
                DbGatewayItemResult::kRPCNone {
                    item: WireStorageItem::kItemTypeAuthData {
                        auth_data: AddressAuthData {
                            public_keys,
                            required_weight: 3,
                            last_written_height: thruput_xdr::types::BlockHeight::from(2),
                        },
                    },
                },
            ],
        )));

        let balance = balance.await.unwrap().unwrap();
        assert_eq!(*balance.state_block_height, 13);
        assert!(matches!(
            balance.item,
            Some(responses::StorageItem::NonnegativeInt { value: 500 })
        ));

        let auth = auth.await.unwrap().unwrap();
        assert_eq!(*auth.state_block_height, 13);
        let auth_data = auth.configuration.expect("auth data is registered");
        assert_eq!(auth_data.required_weight, 3);
        assert_eq!(auth_data.signers.len(), 1);
        assert_eq!(auth_data.signers[0].key_id, 2);
        assert_eq!(auth_data.signers[0].max_weight, 4);

        let recorded = mock.batches();
        assert_eq!(mock.seen(), vec![2], "both reads shared one batch");
        assert_eq!(recorded[0].0[0].key, StorageKey::FreeNativeTokenBalance);
        assert_eq!(recorded[0].0[1].key, StorageKey::AuthData);
    }

    #[tokio::test]
    async fn per_item_demux_is_independent() {
        let mock = MockSink::arc();
        let gate = mock.queue_gate();

        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
        );

        // Three concurrent reads land in a single batch (no yields between
        // spawns under the current_thread runtime), and the mock returns
        // a 3-item payload demuxing each waiter independently.
        let r0 = tokio::spawn({
            let b = batcher.clone();
            async move {
                b.read_contract_data(request_context(), address_zero(), key_from_u32(0))
                    .await
            }
        });
        let r1 = tokio::spawn({
            let b = batcher.clone();
            async move {
                b.read_contract_data(request_context(), address_zero(), key_from_u32(1))
                    .await
            }
        });
        let r2 = tokio::spawn({
            let b = batcher.clone();
            async move {
                b.read_contract_data(request_context(), address_zero(), key_from_u32(2))
                    .await
            }
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
            Some(responses::StorageItem::Bytestring { bytes }) => {
                assert_eq!(bytes.0.as_slice(), b"present");
            }
            other => panic!("r0 expected Bytestring, got {other:?}"),
        }
        assert_eq!(*res0.state_block_height, 42);

        let res1 = r1.await.unwrap().expect("r1 should be Ok(None)");
        assert!(res1.item.is_none(), "r1 expected None, got {:?}", res1.item);
        assert_eq!(*res1.state_block_height, 42);

        let err2 = r2
            .await
            .unwrap()
            .expect_err("r2 should be a per-item error");
        assert!(
            matches!(err2, RpcServerError::BackendUnavailable(_)),
            "r2 expected BackendUnavailable, got {err2:?}"
        );

        assert_eq!(mock.seen(), vec![3], "all three reads shared one batch");
    }

    #[tokio::test]
    async fn older_deadline_cohort_does_not_timeout_newer_storage_reads() {
        let mock = MockSink::arc();
        let first_gate = mock.queue_gate();
        let old_gate = mock.queue_gate();
        let newer_gate = mock.queue_gate();
        let batcher = StorageReadBatcher::spawn_with_deadlines(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_millis(10),
        );

        let first = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .read_contract_data(request_context(), address_zero(), key_from_u32(0))
                    .await
            }
        });
        settle().await;

        let old = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .read_contract_data(
                        RpcRequestContext::new(Some(Duration::from_secs(2))),
                        address_zero(),
                        key_from_u32(1),
                    )
                    .await
            }
        });
        let newer = tokio::spawn({
            let batcher = batcher.clone();
            async move {
                batcher
                    .read_contract_data(
                        RpcRequestContext::new(Some(Duration::from_secs(5))),
                        address_zero(),
                        key_from_u32(2),
                    )
                    .await
            }
        });
        settle().await;

        first_gate
            .send(Ok(payload_at_height(1, vec![not_found_item()])))
            .expect("first gate should be open");
        first.await.unwrap().unwrap();
        settle().await;
        old_gate
            .send(Err(RpcServerError::BackendTimeout))
            .expect("old cohort gate should be open");
        assert!(matches!(
            old.await.unwrap(),
            Err(RpcServerError::BackendTimeout)
        ));
        newer_gate
            .send(Ok(payload_at_height(2, vec![not_found_item()])))
            .expect("newer cohort gate should be open");
        newer
            .await
            .unwrap()
            .expect("newer cohort should complete independently");
        assert_eq!(mock.seen(), vec![1, 1, 1]);
    }

    #[tokio::test]
    async fn outer_error_fans_out_to_every_waiter() {
        let mock = MockSink::arc();
        let err_gate = mock.queue_gate();

        let batcher = StorageReadBatcher::spawn(
            &config_for_tests(1, 1, 1000, 100),
            &(mock.clone() as Arc<dyn StorageReadSink>),
            GatewayMetrics::for_tests(),
        );

        let mut waiters = vec![];
        for i in 0..4u32 {
            let b = batcher.clone();
            waiters.push(tokio::spawn(async move {
                b.read_contract_data(request_context(), address_zero(), key_from_u32(i))
                    .await
            }));
        }
        settle().await;

        let _ = err_gate.send(Err(RpcServerError::BackendUnavailable("mock".into())));
        for (i, w) in waiters.into_iter().enumerate() {
            let r = w.await.unwrap();
            assert!(
                matches!(r, Err(RpcServerError::BackendUnavailable(_))),
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
            hits[shard_index_by_routing_key(&key, n_shards)] += 1;
        }
        for (i, c) in hits.iter().enumerate() {
            assert!(*c > 1500 && *c < 3500, "shard {i} got {c} (expected ~2500)");
        }
    }
    #[tokio::test]
    async fn native_reads_reject_short_and_extra_batches_before_demux() {
        use thruput_json_rpc_server::native::read::StateKey;
        use thruput_json_rpc_types::error::native::BackendDataInvalidReason;
        for count in [0, 1, 3] {
            let mock = MockSink::arc();
            let gate = mock.queue_gate();
            let batcher = StorageReadBatcher::spawn(
                &config_for_tests(1, 4, 1000, 100),
                &(mock.clone() as Arc<dyn StorageReadSink>),
                GatewayMetrics::for_tests(),
            );
            let reads: Vec<_> = (0..2)
                .map(|index| {
                    let batcher = batcher.clone();
                    tokio::spawn(async move {
                        batcher
                            .native_storage(
                                request_context(),
                                address_zero(),
                                StateKey::ContractData(key_from_u32(index)),
                            )
                            .await
                    })
                })
                .collect();
            settle().await;
            gate.send(Ok(payload_at_height(7, vec![not_found_item(); count])))
                .unwrap();
            for read in reads {
                assert!(matches!(
                    read.await.unwrap(),
                    Err(RpcServerError::BackendDataInvalid {
                        reason: BackendDataInvalidReason::InvalidCardinality,
                        ..
                    })
                ));
            }
            assert_eq!(mock.seen(), vec![2]);
        }
    }
}
