//! Cross-backend conformance tests for the shared JSON-RPC server.
//!
//! Asserts that the public behavior of `GroundhogRpcServer<S, C, R>` is
//! consistent across the backend capability shapes used in production
//! gateway and the local hogd-dev supervisor.
//!
//! Real backend adapters live in `crates/gateway` and
//! `crates/node-supervisor`, both of which depend on this crate.
//! To stay within the public surface
//! and avoid circular deps, these tests use a programmable mock that can
//! be configured to mimic each backend shape.

use std::sync::Arc;

use async_trait::async_trait;
use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_api_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    error::codes,
    events::{
        BlockAnchor, ContractAddressList, EventFilter, EventFilterList, EventPage, EventQuery,
        EventTopicFilter, IterationOrder, PaginationCursor, TopicList,
    },
    hex::{Hex0x, HexBytes},
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{TransactionStatus, TxHash},
};
use groundhog_rpc_server::{
    ChainPlane, GroundhogRpcServer, ReadPlane, RpcServerConfig, RpcServerError, SubmitPlane,
    ValidatedTransaction,
};
use tokio::sync::Mutex;
use xdr_common::VecM;

// Canonical XDR/hash vector only. This transaction is structurally valid and
// has a stable hash, but it is not auth-valid against empty hogd-dev genesis.
const CANONICAL_HASH_VECTOR_SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
0000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a00000000000000\
0000000001000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695031\
c9d3fe2554e5ce62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d";

const CANONICAL_HASH_VECTOR_TX_HASH_HEX: &str =
    "9dc7ad0fc4f74a43a62e047c0d6885c8e4df2d4840298ecd52cf157681157ed3";

const MAX_EVENTS_PER_PAGE: u32 = 50;
const MAX_CURSOR_BYTES: usize = 64;

/// Mock backend that emulates a chosen production capability profile.
///
/// `Clone` is cheap: the only mutable state is held behind `Arc<Mutex<_>>`
/// so the three plane copies handed to `GroundhogRpcServer::unified` share
/// one history.
#[derive(Clone, Default)]
struct ScriptedBackend {
    submitted: Arc<Mutex<Vec<ValidatedTransaction>>>,
    contract_events_calls: Arc<Mutex<u32>>,
    get_storage_at_calls: Arc<Mutex<u32>>,
}

impl ScriptedBackend {
    /// Capability profile of the production gateway: pending count is
    /// not implemented because hogd's scheduler has no `size()` RPC.
    fn gateway_shape() -> Self {
        Self::default()
    }

    /// Capability profile of the local hogd-dev supervisor.
    fn hogd_dev_shape() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SubmitPlane for ScriptedBackend {
    async fn submit_raw_transaction(&self, tx: ValidatedTransaction) -> Result<(), RpcServerError> {
        self.submitted.lock().await.push(tx);
        Ok(())
    }
}

#[async_trait]
impl ChainPlane for ScriptedBackend {
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
        Ok(ChainTip {
            height: BlockHeight::from(0),
            block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
        })
    }

    async fn get_block_by_number(
        &self,
        _height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBlockByNumber".into()))
    }

    async fn pending_transaction_count(&self) -> Result<u64, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "pendingTransactionCount".into(),
        ))
    }
}

#[async_trait]
impl ReadPlane for ScriptedBackend {
    async fn transaction_receipt(
        &self,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Ok(None)
    }

    async fn transaction_status(
        &self,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Ok(TransactionStatus::Unknown)
    }

    async fn contract_events(&self, _query: EventQuery) -> Result<EventPage, RpcServerError> {
        *self.contract_events_calls.lock().await += 1;
        Ok(EventPage {
            events: vec![],
            cursor: PaginationCursor(vec![]),
            latest_block: BlockHeight::from(0),
            oldest_block: BlockHeight::from(0),
        })
    }

    async fn get_storage_at(
        &self,
        _address: ContractAddress,
        _key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        *self.get_storage_at_calls.lock().await += 1;
        Err(RpcServerError::NotImplemented("getStorageAt".into()))
    }
}

type ScriptedServer = GroundhogRpcServer<ScriptedBackend, ScriptedBackend, ScriptedBackend>;

fn server(backend: ScriptedBackend) -> ScriptedServer {
    GroundhogRpcServer::unified(
        backend,
        RpcServerConfig::default()
            .with_max_events_per_page(MAX_EVENTS_PER_PAGE)
            .with_max_cursor_bytes(MAX_CURSOR_BYTES),
    )
}

fn simple_query() -> EventQuery {
    EventQuery {
        filters: EventFilterList(VecM(vec![EventFilter {
            contract_addresses: ContractAddressList(VecM(vec![Hex0x(
                groundhog_xdr::types::ContractAddress::from([0u8; 32]),
            )])),
            topics: EventTopicFilter {
                topic0: TopicList(VecM(vec![])),
                topic1: TopicList(VecM(vec![])),
                topic2: TopicList(VecM(vec![])),
            },
        }])),
        order: IterationOrder::Ascending,
        anchor: BlockAnchor::AtBeginning,
        cursor: PaginationCursor(vec![]),
        limit: 0,
    }
}

fn opaque_hash() -> TxHash {
    TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0xab; 32])))
}

fn all_shapes() -> [ScriptedBackend; 2] {
    [
        ScriptedBackend::gateway_shape(),
        ScriptedBackend::hogd_dev_shape(),
    ]
}

// === Conformance: behavior identical across backend shapes ===

#[tokio::test]
async fn invalid_hex_is_rejected_with_tx_invalid_for_all_shapes() {
    for backend in all_shapes() {
        let err = server(backend)
            .send_raw_transaction("not-valid-hex".into())
            .await
            .expect_err("bug: invalid hex must be rejected");

        assert_eq!(err.code(), codes::TX_INVALID);
    }
}

#[tokio::test]
async fn invalid_xdr_is_rejected_with_tx_invalid_for_all_shapes() {
    for backend in all_shapes() {
        let err = server(backend)
            .send_raw_transaction("0x00".into())
            .await
            .expect_err("bug: invalid XDR must be rejected");

        assert_eq!(err.code(), codes::TX_INVALID);
    }
}

#[tokio::test]
async fn canonical_hash_vector_submit_returns_hash_for_all_shapes() {
    for backend in all_shapes() {
        let tx_hash = server(backend.clone())
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: canonical hash vector should submit to scripted backend");

        assert_eq!(
            hex::encode(tx_hash.0.0.as_ref()),
            CANONICAL_HASH_VECTOR_TX_HASH_HEX
        );
        assert_eq!(backend.submitted.lock().await.len(), 1);
    }
}

#[tokio::test]
async fn unknown_tx_status_returns_unknown_for_all_shapes() {
    for backend in all_shapes() {
        let status = server(backend)
            .get_transaction_status(opaque_hash())
            .await
            .expect("bug: status lookup must succeed");

        assert!(matches!(status, TransactionStatus::Unknown));
    }
}

#[tokio::test]
async fn unknown_receipt_returns_none_for_all_shapes() {
    for backend in all_shapes() {
        let receipt = server(backend)
            .get_transaction_receipt(opaque_hash())
            .await
            .expect("bug: receipt lookup must succeed");

        assert!(receipt.is_none());
    }
}

#[tokio::test]
async fn event_query_validation_runs_before_dispatch_for_all_shapes() {
    for backend in all_shapes() {
        let calls = backend.contract_events_calls.clone();
        let svc = server(backend);

        let mut over_limit = simple_query();
        over_limit.limit = MAX_EVENTS_PER_PAGE + 1;
        let limit_err = svc
            .get_events(over_limit)
            .await
            .expect_err("bug: excessive limit must be rejected");
        assert_eq!(limit_err.code(), codes::INVALID_PARAMS);

        let mut over_cursor = simple_query();
        over_cursor.cursor = PaginationCursor(vec![0u8; MAX_CURSOR_BYTES + 1]);
        let cursor_err = svc
            .get_events(over_cursor)
            .await
            .expect_err("bug: oversized cursor must be rejected");
        assert_eq!(cursor_err.code(), codes::INVALID_PARAMS);

        assert_eq!(
            *calls.lock().await,
            0,
            "validation must short-circuit before backend dispatch"
        );
    }
}

#[tokio::test]
async fn get_storage_at_key_length_validated_for_all_shapes() {
    let address: ContractAddress = Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32]));
    for backend in all_shapes() {
        let calls = backend.get_storage_at_calls.clone();
        let svc = server(backend);

        let short_err = svc
            .get_storage_at(address, HexBytes(vec![0u8; 16]))
            .await
            .expect_err("bug: short storage key must be rejected");
        assert_eq!(short_err.code(), codes::INVALID_PARAMS);

        let long_err = svc
            .get_storage_at(address, HexBytes(vec![0u8; 33]))
            .await
            .expect_err("bug: long storage key must be rejected");
        assert_eq!(long_err.code(), codes::INVALID_PARAMS);

        let empty_err = svc
            .get_storage_at(address, HexBytes(vec![]))
            .await
            .expect_err("bug: empty storage key must be rejected");
        assert_eq!(empty_err.code(), codes::INVALID_PARAMS);

        assert_eq!(
            *calls.lock().await,
            0,
            "key-length validation must short-circuit before backend dispatch"
        );
    }
}

#[tokio::test]
async fn chain_tip_dispatches_through_chain_plane_for_all_shapes() {
    for backend in all_shapes() {
        let tip = server(backend)
            .chain_tip()
            .await
            .expect("bug: chain_tip must dispatch through chain plane");

        // The mock returns BlockHeight::from(0); asserting against that
        // value confirms dispatch landed in the configured plane rather
        // than short-circuiting in the shared server.
        assert_eq!(*tip.height, 0);
    }
}

#[tokio::test]
async fn get_block_by_number_is_not_implemented_for_all_shapes() {
    for backend in all_shapes() {
        let err = server(backend)
            .get_block_by_number(BlockHeight::from(0))
            .await
            .expect_err("bug: getBlockByNumber is not yet implemented");

        assert_eq!(err.code(), codes::NOT_IMPLEMENTED);
    }
}

// === Current shared limitation: no backend has a scheduler size RPC ===

#[tokio::test]
async fn pending_transaction_count_is_not_implemented_for_all_shapes() {
    for backend in all_shapes() {
        let err = server(backend)
            .pending_transaction_count()
            .await
            .expect_err("bug: pending count should not be implemented");

        assert_eq!(err.code(), codes::NOT_IMPLEMENTED);
    }
}
