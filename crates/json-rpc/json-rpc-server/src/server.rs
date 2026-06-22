use jsonrpsee::core::RpcResult;
use jsonrpsee::types::ErrorObjectOwned;

use groundhog_json_rpc_api::{GroundhogApiServer, WIRE_FORMAT_VERSION};
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress,
    balance::BalanceResponse,
    block::{BlockInfo, ChainTip},
    error::codes,
    events::{EventPage, EventQuery},
    hex::HexBytes,
    receipt::TransactionReceipt,
    state::{StorageItemInfo, StorageItemRead},
    transaction::{TransactionStatus, TxHash},
    version::VersionInfo,
    view::{ViewRequest, ViewResponse},
};

use crate::{
    ChainPlane, ReadPlane, RpcServerConfig, RpcServerError, SubmitPlane, ViewPlane,
    tx::validate_raw_transaction,
};

/// Generic implementation of the public Groundhog JSON-RPC API over
/// independently-typed submit, chain, read, and view backend planes.
#[derive(Debug, Clone)]
pub struct GroundhogRpcServer<S, C, R, V> {
    submit: S,
    chain: C,
    read: R,
    view: V,
    config: RpcServerConfig,
    build_info: BuildInfo,
}

/// Build-time implementation metadata exposed by `groundhog_version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    implementation_version: &'static str,
    commit_hash: Option<&'static str>,
}

impl BuildInfo {
    /// Create build metadata for the binary serving this RPC endpoint.
    pub const fn new(
        implementation_version: &'static str,
        commit_hash: Option<&'static str>,
    ) -> Self {
        Self {
            implementation_version,
            commit_hash,
        }
    }

    /// Build metadata suitable for unit tests and in-process fixtures.
    pub const fn for_tests() -> Self {
        Self::new("test", Some("test-commit"))
    }
}

impl<S, C, R, V> GroundhogRpcServer<S, C, R, V> {
    /// Create a server wrapper around independent backend planes.
    pub fn new(
        submit: S,
        chain: C,
        read: R,
        view: V,
        config: RpcServerConfig,
        build_info: BuildInfo,
    ) -> Self {
        Self {
            submit,
            chain,
            read,
            view,
            config,
            build_info,
        }
    }
}

impl<B> GroundhogRpcServer<B, B, B, B>
where
    B: Clone,
{
    /// Convenience constructor for backends that implement all four planes
    /// on a single type.
    pub fn unified(backend: B, config: RpcServerConfig, build_info: BuildInfo) -> Self {
        Self::new(
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend,
            config,
            build_info,
        )
    }
}

#[async_trait::async_trait]
impl<S, C, R, V> GroundhogApiServer for GroundhogRpcServer<S, C, R, V>
where
    S: SubmitPlane,
    C: ChainPlane,
    R: ReadPlane,
    V: ViewPlane,
{
    #[tracing::instrument(skip_all, fields(tx_hex_len = tx_hex.len()))]
    async fn send_raw_transaction(&self, tx_hex: String) -> RpcResult<TxHash> {
        let tx = validate_raw_transaction(&tx_hex)?;
        let tx_hash = tx.hash;
        self.submit.submit_raw_transaction(tx).await?;
        Ok(tx_hash)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>> {
        Ok(self.read.transaction_receipt(tx_hash).await?)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn get_transaction_status(&self, tx_hash: TxHash) -> RpcResult<TransactionStatus> {
        Ok(self.read.transaction_status(tx_hash).await?)
    }

    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> RpcResult<ChainTip> {
        Ok(self.chain.chain_tip().await?)
    }

    #[tracing::instrument(skip_all, fields(?height))]
    async fn get_block_by_number(&self, height: BlockHeight) -> RpcResult<Option<BlockInfo>> {
        Ok(self.chain.get_block_by_number(height).await?)
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(&self) -> RpcResult<u64> {
        Ok(self.chain.pending_transaction_count().await?)
    }

    #[tracing::instrument(skip_all, fields(filters_len = query.filters.0.len(), limit = query.limit))]
    async fn get_events(&self, query: EventQuery) -> RpcResult<EventPage> {
        validate_event_query(&query, &self.config)?;
        Ok(self.read.contract_events(query).await?)
    }

    #[tracing::instrument(skip_all, fields(?address, key_len = key.0.len()))]
    async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: HexBytes,
    ) -> RpcResult<StorageItemRead> {
        let key: [u8; 32] = key.0.as_slice().try_into().map_err(|_| {
            ErrorObjectOwned::owned(
                codes::INVALID_PARAMS,
                "storage key must be exactly 32 bytes",
                None::<()>,
            )
        })?;
        Ok(self.read.get_storage_at(address, key).await?)
    }

    #[tracing::instrument(skip_all, fields(?address))]
    async fn get_balance(&self, address: ContractAddress) -> RpcResult<BalanceResponse> {
        let read = self.read.read_native_hog_balance(address).await?;
        let raw_balance = match read.item {
            Some(StorageItemInfo::NonnegativeInt { value }) => value,
            None => "0".into(),
            Some(other) => {
                return Err(RpcServerError::BackendUnavailable(format!(
                    "native HOG balance storage had unexpected item kind: {other:?}"
                ))
                .into());
            }
        };
        Ok(BalanceResponse {
            address,
            raw_balance,
            height: read.block_height,
        })
    }

    #[tracing::instrument(
        skip_all,
        fields(
            ?request.contract,
            entrypoint_len = request.entrypoint.0.len(),
            calldata_len = request.calldata.0.len(),
        )
    )]
    async fn view(&self, request: ViewRequest) -> RpcResult<ViewResponse> {
        if request.entrypoint.0.len() != 8 {
            return Err(ErrorObjectOwned::owned(
                codes::INVALID_PARAMS,
                "view entrypoint must be exactly 8 bytes",
                None::<()>,
            ));
        }
        if request.calldata.0.len() > self.config.max_view_calldata_bytes {
            return Err(ErrorObjectOwned::owned(
                codes::INVALID_PARAMS,
                format!(
                    "view calldata length {} exceeds maximum {}",
                    request.calldata.0.len(),
                    self.config.max_view_calldata_bytes
                ),
                None::<()>,
            ));
        }
        Ok(self.view.view(request, self.config.view_gas_limit).await?)
    }

    #[tracing::instrument(skip_all)]
    async fn version(&self) -> RpcResult<VersionInfo> {
        Ok(VersionInfo::new(
            WIRE_FORMAT_VERSION,
            self.build_info.implementation_version,
            self.build_info.commit_hash,
        ))
    }
}

/// Bounds-check a `getEvents` query before the JSON-RPC dispatcher
/// hands it to the read backend.  Rejects requests whose `limit` or
/// opaque `cursor` exceed the per-server caps in `RpcServerConfig`.
///
/// `limit == 0` and empty `filters` pass through unchanged; the
/// indexer owns those defaults.
fn validate_event_query(
    query: &EventQuery,
    config: &RpcServerConfig,
) -> Result<(), RpcServerError> {
    if query.limit > config.max_events_per_page {
        return Err(RpcServerError::InvalidParam(format!(
            "limit {} exceeds maximum {}",
            query.limit, config.max_events_per_page
        )));
    }
    if query.cursor.0.len() > config.max_cursor_bytes {
        return Err(RpcServerError::InvalidParam(format!(
            "cursor length {} exceeds maximum {}",
            query.cursor.0.len(),
            config.max_cursor_bytes
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::*;
    use crate::{ValidatedTransaction, ViewPlane};
    use groundhog_json_rpc_types::events::{
        BlockAnchor, ContractAddressList, EventFilter, EventFilterList, EventTopicFilter,
        IterationOrder, PaginationCursor, TopicList,
    };
    use groundhog_json_rpc_types::hex::Hex0x;
    use xdr_common::VecM;

    // Canonical XDR/hash vector only. This transaction is structurally valid
    // and has a stable hash, but it is not auth-valid against empty hogd-dev
    // genesis.
    const CANONICAL_HASH_VECTOR_SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
0000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a00000000000000\
0000000001000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695031\
c9d3fe2554e5ce62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d";
    const CANONICAL_HASH_VECTOR_TX_HASH_HEX: &str =
        "9dc7ad0fc4f74a43a62e047c0d6885c8e4df2d4840298ecd52cf157681157ed3";

    #[derive(Default)]
    struct MockBackend {
        submitted: Mutex<Vec<ValidatedTransaction>>,
        event_calls: Mutex<u32>,
    }

    #[derive(Clone)]
    struct SubmitOnly {
        submitted: Arc<Mutex<Vec<ValidatedTransaction>>>,
    }

    #[derive(Clone, Copy)]
    struct ChainOnly;

    #[derive(Clone)]
    struct ReadOnly {
        event_calls: Arc<Mutex<u32>>,
    }

    #[derive(Clone)]
    struct ViewOnly {
        view_gas_limits: Arc<Mutex<Vec<u64>>>,
    }

    fn server(
        mock: Arc<MockBackend>,
    ) -> GroundhogRpcServer<Arc<MockBackend>, Arc<MockBackend>, Arc<MockBackend>, Arc<MockBackend>>
    {
        GroundhogRpcServer::unified(mock, RpcServerConfig::default(), BuildInfo::for_tests())
    }

    #[async_trait::async_trait]
    impl SubmitPlane for Arc<MockBackend> {
        async fn submit_raw_transaction(
            &self,
            tx: ValidatedTransaction,
        ) -> Result<(), RpcServerError> {
            self.submitted.lock().await.push(tx);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl SubmitPlane for SubmitOnly {
        async fn submit_raw_transaction(
            &self,
            tx: ValidatedTransaction,
        ) -> Result<(), RpcServerError> {
            self.submitted.lock().await.push(tx);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl ChainPlane for Arc<MockBackend> {
        async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
            Err(RpcServerError::NotImplemented("chainTip".into()))
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

    #[async_trait::async_trait]
    impl ChainPlane for ChainOnly {
        async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
            Ok(ChainTip {
                height: BlockHeight::from(42),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
            })
        }

        async fn get_block_by_number(
            &self,
            _height: BlockHeight,
        ) -> Result<Option<BlockInfo>, RpcServerError> {
            Ok(None)
        }

        async fn pending_transaction_count(&self) -> Result<u64, RpcServerError> {
            Ok(7)
        }
    }

    #[async_trait::async_trait]
    impl ReadPlane for Arc<MockBackend> {
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
            *self.event_calls.lock().await += 1;
            Ok(empty_page())
        }

        async fn get_storage_at(
            &self,
            _address: ContractAddress,
            _key: [u8; 32],
        ) -> Result<StorageItemRead, RpcServerError> {
            Ok(StorageItemRead {
                block_height: BlockHeight::from(0),
                item: None,
            })
        }

        async fn read_native_hog_balance(
            &self,
            address: ContractAddress,
        ) -> Result<StorageItemRead, RpcServerError> {
            self.get_storage_at(address, [0u8; 32]).await
        }
    }

    #[async_trait::async_trait]
    impl ViewPlane for Arc<MockBackend> {}

    #[async_trait::async_trait]
    impl ReadPlane for ReadOnly {
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
            *self.event_calls.lock().await += 1;
            Ok(empty_page())
        }

        async fn get_storage_at(
            &self,
            _address: ContractAddress,
            _key: [u8; 32],
        ) -> Result<StorageItemRead, RpcServerError> {
            Ok(StorageItemRead {
                block_height: BlockHeight::from(0),
                item: None,
            })
        }

        async fn read_native_hog_balance(
            &self,
            address: ContractAddress,
        ) -> Result<StorageItemRead, RpcServerError> {
            self.get_storage_at(address, [0u8; 32]).await
        }
    }

    #[async_trait::async_trait]
    impl ViewPlane for ViewOnly {
        async fn view(
            &self,
            _request: ViewRequest,
            gas_limit: u64,
        ) -> Result<ViewResponse, RpcServerError> {
            self.view_gas_limits.lock().await.push(gas_limit);
            Ok(ViewResponse {
                output: groundhog_json_rpc_types::view::RootInvocationOutput::Return {
                    return_data: HexBytes(vec![]),
                },
                events: vec![],
                height: BlockHeight::from(0),
            })
        }
    }

    #[tokio::test]
    async fn canonical_hash_vector_submit_calls_backend_and_returns_hash() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        let tx_hash = server
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: canonical hash vector should submit to mock backend");

        assert_eq!(
            hex::encode(tx_hash.0.0.as_ref()),
            CANONICAL_HASH_VECTOR_TX_HASH_HEX
        );
        assert_eq!(mock.submitted.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn distinct_backend_plane_types_are_accepted() {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let event_calls = Arc::new(Mutex::new(0));
        let view_gas_limits = Arc::new(Mutex::new(Vec::new()));
        let server = GroundhogRpcServer::new(
            SubmitOnly {
                submitted: submitted.clone(),
            },
            ChainOnly,
            ReadOnly {
                event_calls: event_calls.clone(),
            },
            ViewOnly {
                view_gas_limits: view_gas_limits.clone(),
            },
            RpcServerConfig::default(),
            BuildInfo::for_tests(),
        );

        let tx_hash = server
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: canonical hash vector should submit through submit plane");

        assert_eq!(
            hex::encode(tx_hash.0.0.as_ref()),
            CANONICAL_HASH_VECTOR_TX_HASH_HEX
        );
        assert_eq!(submitted.lock().await.len(), 1);
        assert_eq!(
            server
                .pending_transaction_count()
                .await
                .expect("bug: chain plane should answer pending count"),
            7
        );

        server
            .get_events(simple_query())
            .await
            .expect("bug: read plane should answer events");

        assert_eq!(*event_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn view_passes_configured_gas_limit_to_view_plane() {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let event_calls = Arc::new(Mutex::new(0));
        let view_gas_limits = Arc::new(Mutex::new(Vec::new()));
        let gas_limit = 123_456;
        let server = GroundhogRpcServer::new(
            SubmitOnly {
                submitted: submitted.clone(),
            },
            ChainOnly,
            ReadOnly {
                event_calls: event_calls.clone(),
            },
            ViewOnly {
                view_gas_limits: view_gas_limits.clone(),
            },
            RpcServerConfig::default().with_view_gas_limit(gas_limit),
            BuildInfo::for_tests(),
        );

        let request = ViewRequest {
            contract: Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32])),
            entrypoint: HexBytes(vec![0u8; 8]),
            calldata: HexBytes(vec![]),
            caller: None,
        };

        server
            .view(request)
            .await
            .expect("bug: valid view request should dispatch to view plane");

        assert_eq!(*view_gas_limits.lock().await, vec![gas_limit]);
        assert!(submitted.lock().await.is_empty());
        assert_eq!(*event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn excessive_limit_is_rejected_before_backend_dispatch() {
        let mock = Arc::new(MockBackend::default());
        let cap = 5;
        let server = GroundhogRpcServer::unified(
            mock.clone(),
            RpcServerConfig::default().with_max_events_per_page(cap),
            BuildInfo::for_tests(),
        );

        let mut q = simple_query();
        q.limit = cap + 1;

        let err = server
            .get_events(q)
            .await
            .expect_err("bug: excessive limit should fail");

        assert_eq!(
            err.code(),
            groundhog_json_rpc_types::error::codes::INVALID_PARAMS
        );
        assert_eq!(*mock.event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn oversized_cursor_is_rejected_before_backend_dispatch() {
        let mock = Arc::new(MockBackend::default());
        let cap = 16;
        let server = GroundhogRpcServer::unified(
            mock.clone(),
            RpcServerConfig::default().with_max_cursor_bytes(cap),
            BuildInfo::for_tests(),
        );

        let mut q = simple_query();
        q.cursor = PaginationCursor(vec![0u8; cap + 1]);

        let err = server
            .get_events(q)
            .await
            .expect_err("bug: oversized cursor should fail");

        assert_eq!(
            err.code(),
            groundhog_json_rpc_types::error::codes::INVALID_PARAMS
        );
        assert_eq!(*mock.event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn limit_zero_passes_through_as_server_default_marker() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        // limit=0 is the XDR convention for "use server default"; the
        // json-rpc-server layer must let it through unchanged.
        server
            .get_events(simple_query())
            .await
            .expect("bug: limit=0 should pass through");

        assert_eq!(*mock.event_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn backend_not_implemented_maps_to_public_code() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock);

        let err = server
            .pending_transaction_count()
            .await
            .expect_err("bug: mock pending count should be unimplemented");

        assert_eq!(
            err.code(),
            groundhog_json_rpc_types::error::codes::NOT_IMPLEMENTED
        );
    }

    #[tokio::test]
    async fn version_uses_wire_format_and_build_info_without_backend_dispatch() {
        let mock = Arc::new(MockBackend::default());
        let server = GroundhogRpcServer::unified(
            mock.clone(),
            RpcServerConfig::default(),
            BuildInfo::new("9.8.7", Some("abc123")),
        );

        let version = server
            .version()
            .await
            .expect("bug: version metadata should be infallible");

        assert_eq!(version.wire_format, WIRE_FORMAT_VERSION);
        assert_eq!(version.implementation, "9.8.7");
        assert_eq!(version.commit_hash.as_deref(), Some("abc123"));
        assert!(mock.submitted.lock().await.is_empty());
        assert_eq!(*mock.event_calls.lock().await, 0);
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

    fn empty_page() -> EventPage {
        EventPage {
            events: vec![],
            cursor: PaginationCursor(vec![]),
            latest_block: BlockHeight::from(0),
            oldest_block: BlockHeight::from(0),
        }
    }
}
