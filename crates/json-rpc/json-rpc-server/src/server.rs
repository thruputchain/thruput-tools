use std::time::{Duration, Instant};
use std::{future::Future, sync::Arc};

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
    network_info::NetworkInfo,
    receipt::TransactionReceipt,
    state::{StorageItemInfo, StorageItemRead},
    transaction::{AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash},
    version::VersionInfo,
    view::{ViewRequest, ViewResponse},
};

use crate::{
    BuildVersion, ChainPlane, ReadPlane, RpcRequestContext, RpcServerConfig, RpcServerError,
    SubmitPlane, ViewPlane, tx::validate_raw_transaction,
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
    tip_probe: TipProbe,
}

/// Rate-limited chain-tip reading shared by all `sendRawTransaction`
/// expiry pre-checks on this server.
///
/// The backend is asked at most once per `tip_cache_ttl_ms`; submits
/// inside the window reuse the last reading. The submit that finds the
/// window expired claims the refresh before awaiting it, so concurrent
/// submits keep using the stale reading instead of queuing behind the
/// probe. Failed or timed-out probes keep the previous reading — the tip
/// only grows, so a stale value can only under-detect expiry.
#[derive(Debug, Clone, Default)]
struct TipProbe {
    state: Arc<tokio::sync::Mutex<TipProbeState>>,
}

#[derive(Debug, Default)]
struct TipProbeState {
    last_attempt: Option<Instant>,
    tip_height: Option<i64>,
}

/// Build-time implementation metadata exposed by `groundhog_version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    implementation_version: &'static str,
    platform_version: Option<&'static str>,
    commit_hash: Option<&'static str>,
}

impl BuildInfo {
    /// Create legacy-compatible build metadata without a packaging-platform release.
    /// New binary compositions should use [`Self::from_build_version`].
    pub const fn new(
        implementation_version: &'static str,
        commit_hash: Option<&'static str>,
    ) -> Self {
        Self {
            implementation_version,
            platform_version: None,
            commit_hash,
        }
    }

    /// Create build metadata with separate crate and packaging-platform versions.
    pub const fn from_build_version(
        build_version: BuildVersion,
        commit_hash: Option<&'static str>,
    ) -> Self {
        Self {
            implementation_version: build_version.crate_version(),
            platform_version: build_version.platform_version(),
            commit_hash,
        }
    }

    /// Build metadata suitable for unit tests and in-process fixtures.
    pub const fn for_tests() -> Self {
        Self::from_build_version(
            BuildVersion::new("test", Some("test-platform")),
            Some("test-commit"),
        )
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
            tip_probe: TipProbe::default(),
        }
    }

    fn request_context(&self) -> RpcRequestContext {
        RpcRequestContext::new(self.config.request_timeout_ms.map(Duration::from_millis))
    }

    async fn within_request_deadline<T>(
        &self,
        context: RpcRequestContext,
        future: impl Future<Output = RpcResult<T>>,
    ) -> RpcResult<T> {
        let Some(deadline) = context.deadline() else {
            return future.await;
        };
        tokio::time::timeout_at(deadline.into(), future)
            .await
            .map_err(|_| ErrorObjectOwned::from(RpcServerError::BackendTimeout))?
    }
}

impl<S, C, R, V> GroundhogRpcServer<S, C, R, V>
where
    C: ChainPlane,
{
    /// Chain-tip height for the expiry pre-check, served from the shared
    /// rate-limited probe. Returns `None` when no reading is available
    /// (probe failed or timed out with no prior success); the caller
    /// skips the pre-check in that case.
    async fn expiry_check_tip_height(&self, context: RpcRequestContext) -> Option<i64> {
        let ttl = Duration::from_millis(self.config.tip_cache_ttl_ms);
        let now = Instant::now();
        {
            let mut state = self.tip_probe.state.lock().await;
            match state.last_attempt {
                Some(at) if now.duration_since(at) < ttl => return state.tip_height,
                _ => state.last_attempt = Some(now),
            }
        }

        let timeout = Duration::from_millis(self.config.tip_probe_timeout_ms);
        let probe_context = context.capped(timeout);
        let Some(remaining) = probe_context.remaining() else {
            return self.tip_probe.state.lock().await.tip_height;
        };
        match tokio::time::timeout(remaining, self.chain.chain_tip(probe_context)).await {
            Ok(Ok(tip)) => {
                let height = *tip.height;
                self.tip_probe.state.lock().await.tip_height = Some(height);
                Some(height)
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "chain tip unavailable, expiry pre-check uses last reading");
                self.tip_probe.state.lock().await.tip_height
            }
            Err(_) => {
                tracing::warn!("chain tip probe timed out, expiry pre-check uses last reading");
                self.tip_probe.state.lock().await.tip_height
            }
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
        let context = self.request_context();
        self.within_request_deadline(context, async {
            let tx = validate_raw_transaction(&tx_hex)?;
            // A transaction can only be included at heights strictly above the
            // current tip, so `expiry <= tip` can never commit. The tip reading
            // is rate-limited and best-effort: an unavailable tip must not block
            // submission, since the scheduler enforces expiry authoritatively.
            if let Some(tip_height) = self.expiry_check_tip_height(context).await
                && *tx.block_expiry_height <= tip_height
            {
                return Err(RpcServerError::TxRejected(format!(
                    "transaction expired: blockExpiryHeight {} is not past chain tip {}; \
                     re-sign with a later expiry height",
                    *tx.block_expiry_height, tip_height
                ))
                .into());
            }
            let tx_hash = tx.hash;
            self.submit.submit_raw_transaction(context, tx).await?;
            Ok(tx_hash)
        })
        .await
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            Ok(self.read.transaction_receipt(context, tx_hash).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn get_transaction_status(&self, tx_hash: TxHash) -> RpcResult<TransactionStatus> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            Ok(self.read.transaction_status(context, tx_hash).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> RpcResult<ChainTip> {
        let context = self.request_context();
        self.within_request_deadline(context, async { Ok(self.chain.chain_tip(context).await?) })
            .await
    }

    #[tracing::instrument(skip_all)]
    async fn network_info(&self) -> RpcResult<NetworkInfo> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            Ok(self.chain.network_info(context).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all, fields(?height))]
    async fn get_block_by_number(&self, height: BlockHeight) -> RpcResult<Option<BlockInfo>> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            Ok(self.chain.get_block_by_number(context, height).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(&self) -> RpcResult<u64> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            Ok(self.chain.pending_transaction_count(context).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all, fields(filters_len = query.filters.0.len(), limit = query.limit))]
    async fn get_events(&self, query: EventQuery) -> RpcResult<EventPage> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            validate_event_query(&query, &self.config)?;
            Ok(self.read.contract_events(context, query).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all, fields(?query.address, limit = query.limit))]
    async fn get_transactions_by_address(
        &self,
        query: AddressTransactionQuery,
    ) -> RpcResult<TransactionRefPage> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            validate_address_transaction_query(&query, &self.config)?;
            Ok(self.read.transactions_by_address(context, query).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all, fields(?address, key_len = key.0.len()))]
    async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: HexBytes,
    ) -> RpcResult<StorageItemRead> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            let key: [u8; 32] = key.0.as_slice().try_into().map_err(|_| {
                ErrorObjectOwned::owned(
                    codes::INVALID_PARAMS,
                    "storage key must be exactly 32 bytes",
                    None::<()>,
                )
            })?;
            Ok(self.read.get_storage_at(context, address, key).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all, fields(?address))]
    async fn get_balance(&self, address: ContractAddress) -> RpcResult<BalanceResponse> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            let read = self.read.read_native_hog_balance(context, address).await?;
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
        })
        .await
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
        let context = self.request_context();
        self.within_request_deadline(context, async {
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
            Ok(self
                .view
                .view(context, request, self.config.view_gas_limit)
                .await?)
        })
        .await
    }

    #[tracing::instrument(skip_all)]
    async fn version(&self) -> RpcResult<VersionInfo> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            Ok(VersionInfo::new(
                WIRE_FORMAT_VERSION,
                self.build_info.implementation_version,
                self.build_info.commit_hash,
            )
            .with_platform_version(self.build_info.platform_version))
        })
        .await
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

/// Bounds-check a `getTransactionsByAddress` query before the JSON-RPC
/// dispatcher hands it to the read backend.  Rejects requests whose
/// `limit` or opaque `cursor` exceed the per-server caps in
/// `RpcServerConfig`.
///
/// `limit == 0` passes through unchanged; the indexer owns the default.
fn validate_address_transaction_query(
    query: &AddressTransactionQuery,
    config: &RpcServerConfig,
) -> Result<(), RpcServerError> {
    if query.limit > config.max_transactions_per_page {
        return Err(RpcServerError::InvalidParam(format!(
            "limit {} exceeds maximum {}",
            query.limit, config.max_transactions_per_page
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
        tx_by_address_calls: Mutex<u32>,
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
            _context: RpcRequestContext,
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
            _context: RpcRequestContext,
            tx: ValidatedTransaction,
        ) -> Result<(), RpcServerError> {
            self.submitted.lock().await.push(tx);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl ChainPlane for Arc<MockBackend> {
        async fn network_info(
            &self,
            _context: RpcRequestContext,
        ) -> Result<NetworkInfo, RpcServerError> {
            Err(RpcServerError::NotImplemented("networkInfo".into()))
        }

        async fn chain_tip(&self, _context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
            Err(RpcServerError::NotImplemented("chainTip".into()))
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

    #[async_trait::async_trait]
    impl ChainPlane for ChainOnly {
        async fn network_info(
            &self,
            _context: RpcRequestContext,
        ) -> Result<NetworkInfo, RpcServerError> {
            Err(RpcServerError::NotImplemented("networkInfo".into()))
        }

        async fn chain_tip(&self, _context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
            Ok(ChainTip {
                height: BlockHeight::from(42),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
                protocol: Some(groundhog_xdr::types::Protocol::new(1)),
            })
        }

        async fn get_block_by_number(
            &self,
            _context: RpcRequestContext,
            _height: BlockHeight,
        ) -> Result<Option<BlockInfo>, RpcServerError> {
            Ok(None)
        }

        async fn pending_transaction_count(
            &self,
            _context: RpcRequestContext,
        ) -> Result<u64, RpcServerError> {
            Ok(7)
        }
    }

    #[async_trait::async_trait]
    impl ReadPlane for Arc<MockBackend> {
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
            *self.event_calls.lock().await += 1;
            Ok(empty_page())
        }

        async fn transactions_by_address(
            &self,
            _context: RpcRequestContext,
            _query: AddressTransactionQuery,
        ) -> Result<TransactionRefPage, RpcServerError> {
            *self.tx_by_address_calls.lock().await += 1;
            Ok(empty_ref_page())
        }

        async fn get_storage_at(
            &self,
            _context: RpcRequestContext,
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
            _context: RpcRequestContext,
            _address: ContractAddress,
        ) -> Result<StorageItemRead, RpcServerError> {
            Ok(StorageItemRead {
                block_height: BlockHeight::from(0),
                item: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl ViewPlane for Arc<MockBackend> {}

    #[async_trait::async_trait]
    impl ReadPlane for ReadOnly {
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
            *self.event_calls.lock().await += 1;
            Ok(empty_page())
        }

        async fn transactions_by_address(
            &self,
            _context: RpcRequestContext,
            _query: AddressTransactionQuery,
        ) -> Result<TransactionRefPage, RpcServerError> {
            Ok(empty_ref_page())
        }

        async fn get_storage_at(
            &self,
            _context: RpcRequestContext,
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
            _context: RpcRequestContext,
            _address: ContractAddress,
        ) -> Result<StorageItemRead, RpcServerError> {
            Ok(StorageItemRead {
                block_height: BlockHeight::from(0),
                item: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl ViewPlane for ViewOnly {
        async fn view(
            &self,
            _context: RpcRequestContext,
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

    /// Chain plane whose tip is at or past the canonical vector's
    /// `block_expiry_height` of 1,000,000.
    #[derive(Clone, Copy)]
    struct ExpiredTipChain;

    #[async_trait::async_trait]
    impl ChainPlane for ExpiredTipChain {
        async fn network_info(
            &self,
            _context: RpcRequestContext,
        ) -> Result<NetworkInfo, RpcServerError> {
            Err(RpcServerError::NotImplemented("networkInfo".into()))
        }

        async fn chain_tip(&self, _context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
            Ok(ChainTip {
                height: BlockHeight::from(1_000_000),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
                protocol: Some(groundhog_xdr::types::Protocol::new(1)),
            })
        }

        async fn get_block_by_number(
            &self,
            _context: RpcRequestContext,
            _height: BlockHeight,
        ) -> Result<Option<BlockInfo>, RpcServerError> {
            Ok(None)
        }

        async fn pending_transaction_count(
            &self,
            _context: RpcRequestContext,
        ) -> Result<u64, RpcServerError> {
            Ok(0)
        }
    }

    /// Chain plane that counts `chain_tip` calls and reports a fixed
    /// pre-expiry tip.
    #[derive(Clone)]
    struct CountingTipChain {
        tip_calls: Arc<Mutex<u32>>,
    }

    #[async_trait::async_trait]
    impl ChainPlane for CountingTipChain {
        async fn network_info(
            &self,
            _context: RpcRequestContext,
        ) -> Result<NetworkInfo, RpcServerError> {
            Err(RpcServerError::NotImplemented("networkInfo".into()))
        }

        async fn chain_tip(&self, _context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
            *self.tip_calls.lock().await += 1;
            Ok(ChainTip {
                height: BlockHeight::from(42),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
                protocol: Some(groundhog_xdr::types::Protocol::new(1)),
            })
        }

        async fn get_block_by_number(
            &self,
            _context: RpcRequestContext,
            _height: BlockHeight,
        ) -> Result<Option<BlockInfo>, RpcServerError> {
            Ok(None)
        }

        async fn pending_transaction_count(
            &self,
            _context: RpcRequestContext,
        ) -> Result<u64, RpcServerError> {
            Ok(0)
        }
    }

    /// Chain plane whose `chain_tip` never completes in test time.
    #[derive(Clone, Copy)]
    struct StalledTipChain;

    #[async_trait::async_trait]
    impl ChainPlane for StalledTipChain {
        async fn network_info(
            &self,
            _context: RpcRequestContext,
        ) -> Result<NetworkInfo, RpcServerError> {
            Err(RpcServerError::NotImplemented("networkInfo".into()))
        }

        async fn chain_tip(&self, _context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
            tokio::time::sleep(std::time::Duration::from_hours(1)).await;
            unreachable!("bug: stalled tip probe should have been abandoned")
        }

        async fn get_block_by_number(
            &self,
            _context: RpcRequestContext,
            _height: BlockHeight,
        ) -> Result<Option<BlockInfo>, RpcServerError> {
            Ok(None)
        }

        async fn pending_transaction_count(
            &self,
            _context: RpcRequestContext,
        ) -> Result<u64, RpcServerError> {
            Ok(0)
        }
    }

    #[derive(Clone)]
    struct RecordingSubmit {
        contexts: Arc<Mutex<Vec<RpcRequestContext>>>,
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl SubmitPlane for RecordingSubmit {
        async fn submit_raw_transaction(
            &self,
            context: RpcRequestContext,
            _tx: ValidatedTransaction,
        ) -> Result<(), RpcServerError> {
            self.contexts.lock().await.push(context);
            tokio::time::sleep(self.delay).await;
            Ok(())
        }
    }

    #[derive(Clone)]
    struct RecordingTipChain {
        contexts: Arc<Mutex<Vec<RpcRequestContext>>>,
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl ChainPlane for RecordingTipChain {
        async fn network_info(
            &self,
            _context: RpcRequestContext,
        ) -> Result<NetworkInfo, RpcServerError> {
            Err(RpcServerError::NotImplemented("networkInfo".into()))
        }

        async fn chain_tip(&self, context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
            self.contexts.lock().await.push(context);
            tokio::time::sleep(self.delay).await;
            Ok(ChainTip {
                height: BlockHeight::from(42),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
                protocol: Some(groundhog_xdr::types::Protocol::new(1)),
            })
        }

        async fn get_block_by_number(
            &self,
            _context: RpcRequestContext,
            _height: BlockHeight,
        ) -> Result<Option<BlockInfo>, RpcServerError> {
            Ok(None)
        }

        async fn pending_transaction_count(
            &self,
            _context: RpcRequestContext,
        ) -> Result<u64, RpcServerError> {
            Ok(0)
        }
    }

    fn recording_deadline_server(
        submit: RecordingSubmit,
        chain: RecordingTipChain,
        config: RpcServerConfig,
    ) -> impl GroundhogApiServer {
        GroundhogRpcServer::new(
            submit,
            chain,
            ReadOnly {
                event_calls: Arc::new(Mutex::new(0)),
            },
            ViewOnly {
                view_gas_limits: Arc::new(Mutex::new(Vec::new())),
            },
            config,
            BuildInfo::for_tests(),
        )
    }

    fn expiry_check_server<C: ChainPlane + Clone>(
        chain: C,
        config: RpcServerConfig,
        submitted: Arc<Mutex<Vec<ValidatedTransaction>>>,
    ) -> impl GroundhogApiServer {
        GroundhogRpcServer::new(
            SubmitOnly { submitted },
            chain,
            ReadOnly {
                event_calls: Arc::new(Mutex::new(0)),
            },
            ViewOnly {
                view_gas_limits: Arc::new(Mutex::new(Vec::new())),
            },
            config,
            BuildInfo::for_tests(),
        )
    }

    #[tokio::test]
    async fn expiry_precheck_probes_tip_at_most_once_per_ttl() {
        let tip_calls = Arc::new(Mutex::new(0));
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let server = expiry_check_server(
            CountingTipChain {
                tip_calls: tip_calls.clone(),
            },
            RpcServerConfig::default(),
            submitted.clone(),
        );

        for _ in 0..3 {
            server
                .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
                .await
                .expect("bug: unexpired transaction should submit");
        }

        assert_eq!(
            *tip_calls.lock().await,
            1,
            "tip should be probed once within the cache TTL"
        );
        assert_eq!(submitted.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn expiry_precheck_probes_tip_again_after_ttl() {
        let tip_calls = Arc::new(Mutex::new(0));
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let server = expiry_check_server(
            CountingTipChain {
                tip_calls: tip_calls.clone(),
            },
            RpcServerConfig::default().with_tip_cache_ttl_ms(0),
            submitted.clone(),
        );

        for _ in 0..2 {
            server
                .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
                .await
                .expect("bug: unexpired transaction should submit");
        }

        assert_eq!(
            *tip_calls.lock().await,
            2,
            "an expired cache entry should trigger a fresh probe"
        );
    }

    #[tokio::test]
    async fn stalled_tip_probe_is_abandoned_and_submit_proceeds() {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let server = expiry_check_server(
            StalledTipChain,
            RpcServerConfig::default().with_tip_probe_timeout_ms(50),
            submitted.clone(),
        );

        let started = std::time::Instant::now();
        server
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: a stalled tip probe must not block submission");

        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "submit should not ride out the stalled probe, took {:?}",
            started.elapsed()
        );
        assert_eq!(submitted.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn validation_expiry_probe_and_submit_share_one_request_deadline() {
        let submit_contexts = Arc::new(Mutex::new(Vec::new()));
        let tip_contexts = Arc::new(Mutex::new(Vec::new()));
        let server = recording_deadline_server(
            RecordingSubmit {
                contexts: submit_contexts.clone(),
                delay: Duration::from_millis(40),
            },
            RecordingTipChain {
                contexts: tip_contexts.clone(),
                delay: Duration::from_millis(25),
            },
            RpcServerConfig::default()
                .with_request_timeout_ms(50)
                .with_tip_probe_timeout_ms(500),
        );

        let error = server
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect_err("the combined probe and submit work should exhaust one request budget");

        assert_eq!(error.code(), codes::BACKEND_TIMEOUT);
        let tip_context = tip_contexts.lock().await[0];
        let submit_context = submit_contexts.lock().await[0];
        assert_eq!(tip_context.started_at(), submit_context.started_at());
        assert_eq!(tip_context.deadline(), submit_context.deadline());
    }

    #[tokio::test]
    async fn expiry_probe_uses_a_shorter_deadline_without_resetting_request_start() {
        let submit_contexts = Arc::new(Mutex::new(Vec::new()));
        let tip_contexts = Arc::new(Mutex::new(Vec::new()));
        let server = recording_deadline_server(
            RecordingSubmit {
                contexts: submit_contexts.clone(),
                delay: Duration::ZERO,
            },
            RecordingTipChain {
                contexts: tip_contexts.clone(),
                delay: Duration::ZERO,
            },
            RpcServerConfig::default()
                .with_request_timeout_ms(500)
                .with_tip_probe_timeout_ms(50),
        );

        server
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("the bounded expiry probe and submission should complete");

        let tip_context = tip_contexts.lock().await[0];
        let submit_context = submit_contexts.lock().await[0];
        assert_eq!(tip_context.started_at(), submit_context.started_at());
        assert!(
            tip_context.deadline().expect("probe deadline")
                < submit_context.deadline().expect("request deadline"),
            "the nested expiry probe should receive the shorter deadline"
        );
    }

    #[tokio::test]
    async fn expired_transaction_is_rejected_before_submit() {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let server = GroundhogRpcServer::new(
            SubmitOnly {
                submitted: submitted.clone(),
            },
            ExpiredTipChain,
            ReadOnly {
                event_calls: Arc::new(Mutex::new(0)),
            },
            ViewOnly {
                view_gas_limits: Arc::new(Mutex::new(Vec::new())),
            },
            RpcServerConfig::default(),
            BuildInfo::for_tests(),
        );

        let err = server
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect_err("bug: expired transaction should be rejected");

        assert_eq!(
            err.code(),
            groundhog_json_rpc_types::error::codes::TX_REJECTED
        );
        assert!(
            err.message().contains("expir"),
            "message: {}",
            err.message()
        );
        assert!(submitted.lock().await.is_empty());
    }

    #[tokio::test]
    async fn chain_tip_failure_fails_open_and_submits() {
        // `MockBackend`'s chain plane errors on `chain_tip`; the expiry
        // check must not turn tip unavailability into a submit failure.
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        server
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: tip unavailability should not block submission");

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
    async fn tx_by_address_excessive_limit_is_rejected_before_backend_dispatch() {
        let mock = Arc::new(MockBackend::default());
        let cap = 5;
        let server = GroundhogRpcServer::unified(
            mock.clone(),
            RpcServerConfig::default().with_max_transactions_per_page(cap),
            BuildInfo::for_tests(),
        );

        let mut q = simple_address_query();
        q.limit = cap + 1;

        let err = server
            .get_transactions_by_address(q)
            .await
            .expect_err("bug: excessive limit should fail");

        assert_eq!(
            err.code(),
            groundhog_json_rpc_types::error::codes::INVALID_PARAMS
        );
        assert_eq!(*mock.tx_by_address_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn tx_by_address_oversized_cursor_is_rejected_before_backend_dispatch() {
        let mock = Arc::new(MockBackend::default());
        let cap = 16;
        let server = GroundhogRpcServer::unified(
            mock.clone(),
            RpcServerConfig::default().with_max_cursor_bytes(cap),
            BuildInfo::for_tests(),
        );

        let mut q = simple_address_query();
        q.cursor = PaginationCursor(vec![0u8; cap + 1]);

        let err = server
            .get_transactions_by_address(q)
            .await
            .expect_err("bug: oversized cursor should fail");

        assert_eq!(
            err.code(),
            groundhog_json_rpc_types::error::codes::INVALID_PARAMS
        );
        assert_eq!(*mock.tx_by_address_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn tx_by_address_limit_zero_passes_through_as_server_default_marker() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        server
            .get_transactions_by_address(simple_address_query())
            .await
            .expect("bug: limit=0 should pass through");

        assert_eq!(*mock.tx_by_address_calls.lock().await, 1);
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
            BuildInfo::from_build_version(
                BuildVersion::new("9.8.7", Some("1.2.3")),
                Some("abc123"),
            ),
        );

        let version = server
            .version()
            .await
            .expect("bug: version metadata should be infallible");

        assert_eq!(version.wire_format, WIRE_FORMAT_VERSION);
        assert_eq!(version.implementation, "9.8.7");
        assert_eq!(version.platform_version.as_deref(), Some("1.2.3"));
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

    fn simple_address_query() -> AddressTransactionQuery {
        AddressTransactionQuery {
            address: Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32])),
            order: IterationOrder::Ascending,
            anchor: BlockAnchor::AtBeginning,
            cursor: PaginationCursor(vec![]),
            limit: 0,
        }
    }

    fn empty_ref_page() -> TransactionRefPage {
        TransactionRefPage {
            refs: vec![],
            cursor: PaginationCursor(vec![]),
            latest_block: BlockHeight::from(0),
            oldest_block: BlockHeight::from(0),
        }
    }
}
