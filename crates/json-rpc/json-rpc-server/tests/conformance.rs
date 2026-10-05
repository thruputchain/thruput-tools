//! Cross-backend conformance tests for the shared JSON-RPC server.
//!
//! Asserts that the public behavior of `GroundhogRpcServer<S, C, R, V>` is
//! consistent across the backend capability shapes used in production
//! gateway and the local hogd-dev supervisor.
//!
//! Real backend adapters live in `crates/json-rpc/json-rpc-gateway` and
//! `crates/node-supervisor`, both of which depend on this crate.
//! To stay within the public surface
//! and avoid circular deps, these tests use a programmable mock that can
//! be configured to mimic each backend shape.

use std::sync::Arc;

use async_trait::async_trait;
use groundhog_json_rpc_api::{GroundhogApiServer, WIRE_FORMAT_VERSION};
use groundhog_json_rpc_server::{
    BuildInfo, ChainPlane, GroundhogRpcServer, ReadPlane, RpcRequestContext, RpcServerConfig,
    RpcServerError, SubmitPlane, TipReading, ValidatedTransaction, ViewPlane,
};
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress, NetworkHash,
    address_auth::{AddressAuthDataInfo, AddressAuthDataResponse, AuthPublicKey, AuthSigner},
    block::{BlockInfo, ChainTip},
    error::codes,
    events::{
        BlockAnchor, ContractAddressList, EventFilter, EventFilterList, EventPage, EventQuery,
        EventTopicFilter, IterationOrder, PaginationCursor, TopicList,
    },
    hex::{Hex0x, HexBytes},
    network_info::NetworkInfo,
    receipt::TransactionReceipt,
    state::{StorageItemInfo, StorageItemRead},
    transaction::{AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash},
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
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
const MAX_VIEW_CALLDATA_BYTES: usize = 32;

type StorageCall = (ContractAddress, [u8; 32]);
type StorageCalls = Arc<Mutex<Vec<StorageCall>>>;
type BalanceCalls = Arc<Mutex<Vec<ContractAddress>>>;
type AuthDataCalls = Arc<Mutex<Vec<ContractAddress>>>;
type ScriptedAuthDataRead = Arc<Mutex<Option<Result<AddressAuthDataResponse, RpcServerError>>>>;

/// Mock backend that emulates a chosen production capability profile.
///
/// `Clone` is cheap: the only mutable state is held behind `Arc<Mutex<_>>`
/// so the plane copies handed to `GroundhogRpcServer::unified` share
/// one history.
#[derive(Clone, Default)]
struct ScriptedBackend {
    submitted: Arc<Mutex<Vec<ValidatedTransaction>>>,
    contract_events_calls: Arc<Mutex<u32>>,
    get_storage_at_calls: StorageCalls,
    native_balance_calls: BalanceCalls,
    storage_read: Arc<Mutex<Option<Result<StorageItemRead, RpcServerError>>>>,
    auth_data_calls: AuthDataCalls,
    auth_data_read: ScriptedAuthDataRead,
}

#[derive(Clone)]
struct ScriptedViewBackend {
    output: RootInvocationOutput,
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

    fn with_storage_read(read: StorageItemRead) -> Self {
        Self {
            storage_read: Arc::new(Mutex::new(Some(Ok(read)))),
            ..Self::default()
        }
    }
}

impl ScriptedViewBackend {
    fn new(output: RootInvocationOutput) -> Self {
        Self { output }
    }
}

#[async_trait]
impl SubmitPlane for ScriptedBackend {
    async fn submit_raw_transaction(
        &self,
        _context: RpcRequestContext,
        tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        self.submitted.lock().await.push(tx);
        Ok(())
    }
}

#[async_trait]
impl ChainPlane for ScriptedBackend {
    async fn chain_tip(&self, _context: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        Ok(TipReading::new(
            ChainTip {
                height: BlockHeight::from(0),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
            },
            groundhog_xdr::network_config::NetworkConfig::_0 {
                v0: groundhog_xdr::network_config::NetworkConfigV0 {
                    params: VecM(vec![]),
                },
            },
        ))
    }

    async fn get_block_by_number(
        &self,
        _context: RpcRequestContext,
        _height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBlockByNumber".into()))
    }

    async fn pending_transaction_count(
        &self,
        _context: RpcRequestContext,
    ) -> Result<u64, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "pendingTransactionCount".into(),
        ))
    }
}

#[async_trait]
impl ReadPlane for ScriptedBackend {
    async fn transaction_receipt(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Ok(None)
    }

    async fn transaction_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Ok(TransactionStatus::Unknown)
    }

    async fn contract_events(
        &self,
        _context: RpcRequestContext,
        _query: EventQuery,
    ) -> Result<EventPage, RpcServerError> {
        *self.contract_events_calls.lock().await += 1;
        Ok(EventPage {
            events: vec![],
            cursor: PaginationCursor(vec![]),
            latest_block: BlockHeight::from(0),
            oldest_block: BlockHeight::from(0),
        })
    }

    async fn transactions_by_address(
        &self,
        _context: RpcRequestContext,
        _query: AddressTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        Ok(TransactionRefPage {
            refs: vec![],
            cursor: PaginationCursor(vec![]),
            latest_block: BlockHeight::from(0),
            oldest_block: BlockHeight::from(0),
        })
    }

    async fn get_storage_at(
        &self,
        _context: RpcRequestContext,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        self.get_storage_at_calls.lock().await.push((address, key));
        self.storage_read
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| Err(RpcServerError::NotImplemented("getStorageAt".into())))
    }

    async fn read_native_hog_balance(
        &self,
        _context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError> {
        self.native_balance_calls.lock().await.push(address);
        self.storage_read
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| Err(RpcServerError::NotImplemented("getBalance".into())))
    }

    async fn read_address_auth_data(
        &self,
        _context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<AddressAuthDataResponse, RpcServerError> {
        self.auth_data_calls.lock().await.push(address);
        self.auth_data_read
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| Err(RpcServerError::NotImplemented("getAddressAuthData".into())))
    }
}

#[async_trait]
impl ViewPlane for ScriptedBackend {}

#[async_trait]
impl ViewPlane for ScriptedViewBackend {
    async fn view(
        &self,
        _context: RpcRequestContext,
        _request: ViewRequest,
        _gas_limit: u64,
    ) -> Result<ViewResponse, RpcServerError> {
        Ok(ViewResponse {
            output: self.output.clone(),
            events: vec![],
            height: BlockHeight::from(42),
            gas_used: 0,
        })
    }
}

type ScriptedServer =
    GroundhogRpcServer<ScriptedBackend, ScriptedBackend, ScriptedBackend, ScriptedBackend>;

fn server(backend: ScriptedBackend) -> ScriptedServer {
    GroundhogRpcServer::unified(
        backend,
        test_network_info(),
        RpcServerConfig::default()
            .with_max_events_per_page(MAX_EVENTS_PER_PAGE)
            .with_max_cursor_bytes(MAX_CURSOR_BYTES)
            .with_max_view_calldata_bytes(MAX_VIEW_CALLDATA_BYTES),
        BuildInfo::for_tests(),
    )
}

fn server_with_view(
    backend: ScriptedBackend,
    view: ScriptedViewBackend,
) -> GroundhogRpcServer<ScriptedBackend, ScriptedBackend, ScriptedBackend, ScriptedViewBackend> {
    GroundhogRpcServer::new(
        backend.clone(),
        backend.clone(),
        backend,
        view,
        test_network_info(),
        RpcServerConfig::default()
            .with_max_events_per_page(MAX_EVENTS_PER_PAGE)
            .with_max_cursor_bytes(MAX_CURSOR_BYTES)
            .with_max_view_calldata_bytes(MAX_VIEW_CALLDATA_BYTES),
        BuildInfo::for_tests(),
    )
}

fn test_network_info() -> NetworkInfo {
    NetworkInfo::new(NetworkHash::from(groundhog_xdr::types::SHA256Hash::from(
        [0x07; 32],
    )))
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

fn assert_view_output_eq(actual: &RootInvocationOutput, expected: &RootInvocationOutput) {
    match (actual, expected) {
        (
            RootInvocationOutput::Return {
                return_data: actual,
            },
            RootInvocationOutput::Return {
                return_data: expected,
            },
        )
        | (
            RootInvocationOutput::Abort { abort_data: actual },
            RootInvocationOutput::Abort {
                abort_data: expected,
            },
        ) => assert_eq!(actual.0, expected.0),
        (RootInvocationOutput::Fault, RootInvocationOutput::Fault) => {}
        (actual, expected) => panic!("wrong view output: actual={actual:?}, expected={expected:?}"),
    }
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
async fn version_returns_shared_metadata_without_backend_dispatch_for_all_shapes() {
    for backend in all_shapes() {
        let submitted = backend.submitted.clone();
        let event_calls = backend.contract_events_calls.clone();
        let storage_calls = backend.get_storage_at_calls.clone();

        let version = server(backend)
            .version()
            .await
            .expect("bug: version metadata must be available for every backend shape");

        assert_eq!(version.wire_format, WIRE_FORMAT_VERSION);
        assert_eq!(version.implementation, "test");
        assert_eq!(version.commit_hash.as_deref(), Some("test-commit"));
        assert!(submitted.lock().await.is_empty());
        assert_eq!(*event_calls.lock().await, 0);
        assert!(storage_calls.lock().await.is_empty());
    }
}

#[tokio::test]
async fn event_filters_require_a_named_contract_for_all_shapes() {
    for backend in all_shapes() {
        let calls = backend.contract_events_calls.clone();
        let svc = server(backend);
        let mut empty = simple_query();
        empty.filters.0.0.clear();
        let mut no_contracts = simple_query();
        no_contracts.filters.0.0[0].contract_addresses.0.0.clear();
        let mut mixed = simple_query();
        mixed.filters.0.0.push(no_contracts.filters.0.0[0].clone());
        for query in [empty, no_contracts, mixed] {
            let error = svc
                .get_events(query)
                .await
                .expect_err("bug: empty event filters must be rejected");
            assert_eq!(error.code(), codes::INVALID_PARAMS);
        }
        assert_eq!(*calls.lock().await, 0);
        // No topic constraints is a valid wildcard within a named contract.
        let page = svc
            .get_events(simple_query())
            .await
            .expect("bug: a named contract with wildcard topics must be accepted");
        assert!(page.events.is_empty());
        assert_eq!(*calls.lock().await, 1);
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
        let storage_calls = backend.get_storage_at_calls.clone();
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

        assert!(
            storage_calls.lock().await.is_empty(),
            "key-length validation must short-circuit before backend dispatch"
        );
    }
}

#[tokio::test]
async fn get_balance_reads_native_hog_balance_storage_for_all_shapes() {
    for backend in all_shapes() {
        let address: ContractAddress =
            Hex0x(groundhog_xdr::types::ContractAddress::from([7u8; 32]));
        let backend = ScriptedBackend {
            storage_read: Arc::new(Mutex::new(Some(Ok(StorageItemRead {
                block_height: BlockHeight::from(123),
                item: Some(StorageItemInfo::NonnegativeInt {
                    value: "10000000".into(),
                }),
            })))),
            ..backend
        };
        let storage_calls = backend.get_storage_at_calls.clone();
        let balance_calls = backend.native_balance_calls.clone();

        let balance = server(backend)
            .get_balance(address)
            .await
            .expect("bug: scripted native HOG balance should return successfully");

        assert_eq!(balance.address, address);
        assert_eq!(balance.raw_balance, "10000000");
        assert_eq!(*balance.height, 123);
        assert!(storage_calls.lock().await.is_empty());
        assert_eq!(*balance_calls.lock().await, vec![address]);
    }
}

#[tokio::test]
async fn get_balance_returns_zero_for_missing_balance_item() {
    let address: ContractAddress = Hex0x(groundhog_xdr::types::ContractAddress::from([8u8; 32]));
    let backend = ScriptedBackend::with_storage_read(StorageItemRead {
        block_height: BlockHeight::from(456),
        item: None,
    });

    let balance = server(backend)
        .get_balance(address)
        .await
        .expect("bug: missing native HOG balance should return zero");

    assert_eq!(balance.address, address);
    assert_eq!(balance.raw_balance, "0");
    assert_eq!(*balance.height, 456);
}

#[tokio::test]
async fn get_balance_errors_for_unexpected_storage_item_kind() {
    let address: ContractAddress = Hex0x(groundhog_xdr::types::ContractAddress::from([9u8; 32]));
    let backend = ScriptedBackend::with_storage_read(StorageItemRead {
        block_height: BlockHeight::from(789),
        item: Some(StorageItemInfo::AuthData),
    });

    let err = server(backend)
        .get_balance(address)
        .await
        .expect_err("bug: unexpected native HOG balance item kind should fail");

    assert_eq!(err.code(), codes::BACKEND_UNAVAILABLE);
}

#[tokio::test]
async fn get_address_auth_data_reads_auth_data_for_all_shapes() {
    for backend in all_shapes() {
        let address: ContractAddress =
            Hex0x(groundhog_xdr::types::ContractAddress::from([10u8; 32]));
        let read = AddressAuthDataResponse {
            address,
            height: BlockHeight::from(321),
            auth_data: Some(AddressAuthDataInfo {
                signers: vec![AuthSigner {
                    key_id: 0,
                    public_key: AuthPublicKey::Ed25519 {
                        key: Hex0x(groundhog_xdr::types::Ed25519PublicKey::from([3u8; 32])),
                    },
                    max_weight: 1,
                }],
                required_weight: 1,
            }),
        };
        let backend = ScriptedBackend {
            auth_data_read: Arc::new(Mutex::new(Some(Ok(read.clone())))),
            ..backend
        };
        let storage_calls = backend.get_storage_at_calls.clone();
        let balance_calls = backend.native_balance_calls.clone();
        let auth_data_calls = backend.auth_data_calls.clone();

        let response = server(backend)
            .get_address_auth_data(address)
            .await
            .expect("bug: scripted auth data read should return successfully");

        assert_eq!(response, read);
        assert_eq!(*auth_data_calls.lock().await, vec![address]);
        assert!(storage_calls.lock().await.is_empty());
        assert!(balance_calls.lock().await.is_empty());
    }
}

#[tokio::test]
async fn get_address_auth_data_maps_backend_failures_to_rpc_errors() {
    let address: ContractAddress = Hex0x(groundhog_xdr::types::ContractAddress::from([11u8; 32]));
    for (failure, code) in [
        (
            RpcServerError::BackendUnavailable("db_gateway item: kRPCUnrecoverable".into()),
            codes::BACKEND_UNAVAILABLE,
        ),
        (RpcServerError::BackendTimeout, codes::BACKEND_TIMEOUT),
        (
            RpcServerError::NotImplemented("getAddressAuthData".into()),
            codes::NOT_IMPLEMENTED,
        ),
    ] {
        let backend = ScriptedBackend {
            auth_data_read: Arc::new(Mutex::new(Some(Err(failure)))),
            ..ScriptedBackend::default()
        };

        let err = server(backend)
            .get_address_auth_data(address)
            .await
            .expect_err("bug: scripted auth data failure should surface");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn view_entrypoint_length_validated_for_all_shapes() {
    for backend in all_shapes() {
        for entrypoint_len in [7, 9] {
            let svc = server(backend.clone());
            let request = ViewRequest {
                contract: Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32])),
                entrypoint: HexBytes(vec![0u8; entrypoint_len]),
                calldata: HexBytes(vec![]),
                caller: None,
                gas_limit: None,
            };

            let err = svc
                .view(request)
                .await
                .expect_err("bug: invalid view entrypoint length must be rejected");

            assert_eq!(err.code(), codes::INVALID_PARAMS);
        }
    }
}

#[tokio::test]
async fn view_calldata_length_validated_for_all_shapes() {
    for backend in all_shapes() {
        let svc = server(backend);
        let request = ViewRequest {
            contract: Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32])),
            entrypoint: HexBytes(vec![0u8; 8]),
            calldata: HexBytes(vec![0u8; MAX_VIEW_CALLDATA_BYTES + 1]),
            caller: None,
            gas_limit: None,
        };

        let err = svc
            .view(request)
            .await
            .expect_err("bug: oversized view calldata must be rejected");

        assert_eq!(err.code(), codes::INVALID_PARAMS);
    }
}

#[tokio::test]
async fn view_returns_not_implemented_until_backend_support_lands_for_all_shapes() {
    for backend in all_shapes() {
        let svc = server(backend);
        let request = ViewRequest {
            contract: Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32])),
            entrypoint: HexBytes(vec![0u8; 8]),
            calldata: HexBytes(vec![]),
            caller: None,
            gas_limit: None,
        };

        let err = svc
            .view(request)
            .await
            .expect_err("bug: current backends should not implement view");

        assert_eq!(err.code(), codes::NOT_IMPLEMENTED);
    }
}

#[tokio::test]
async fn view_success_outputs_round_trip_for_all_shapes() {
    let cases = [
        RootInvocationOutput::Return {
            return_data: HexBytes(vec![0x01, 0x02]),
        },
        RootInvocationOutput::Abort {
            abort_data: HexBytes(vec![0x0a, 0x0b]),
        },
        RootInvocationOutput::Fault,
    ];

    for backend in all_shapes() {
        for output in cases.clone() {
            let svc = server_with_view(backend.clone(), ScriptedViewBackend::new(output.clone()));
            let request = ViewRequest {
                contract: Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32])),
                entrypoint: HexBytes(vec![0u8; 8]),
                calldata: HexBytes(vec![]),
                caller: None,
                gas_limit: None,
            };

            let response = svc
                .view(request)
                .await
                .expect("bug: scripted view backend should return a successful response");

            assert_view_output_eq(&response.output, &output);
            assert!(response.events.is_empty());
            assert_eq!(*response.height, 42);
        }
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
async fn chain_config_dispatches_through_chain_plane_for_all_shapes() {
    for backend in all_shapes() {
        let config = server(backend)
            .chain_config()
            .await
            .expect("bug: chain_config must dispatch through chain plane");

        // The mock's config states nothing, so no parameters come back.
        assert_eq!(config.stated.max_contract_size_bytes, None);
    }
}

#[tokio::test]
async fn get_block_by_number_is_not_implemented_for_all_shapes() {
    for backend in all_shapes() {
        let err = server(backend)
            .get_block_by_number(BlockHeight::from(0).into())
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
