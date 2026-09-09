use std::time::{Duration, Instant};
use std::{future::Future, sync::Arc};

use jsonrpsee::core::RpcResult;
use jsonrpsee::types::ErrorObjectOwned;

use groundhog_bundle_identity::BundleIdentity;
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

use groundhog_tx_validation::NetworkConfigParams;

use crate::{
    BuildVersion, ChainPlane, ReadPlane, RpcRequestContext, RpcServerConfig, RpcServerError,
    SubmitPlane, TipReading, ViewPlane, tx::validate_raw_transaction,
};

/// Generic implementation of the public Groundhog JSON-RPC API over
/// independently-typed submit, chain, read, and view backend planes.
///
/// Holds a cached reading of the db tip (height plus the resolved network
/// config) that every `sendRawTransaction` pre-check consults. The cache is
/// refreshed by whichever path fetches a tip: a submit that finds it stale,
/// the public `chainTip` method, or the background task started by
/// [`Self::new`] that keeps it fresh on an idle server.
#[derive(Debug, Clone)]
pub struct GroundhogRpcServer<S, C, R, V> {
    submit: S,
    chain: Arc<C>,
    read: R,
    view: V,
    network_info: NetworkInfo,
    config: RpcServerConfig,
    build_info: BuildInfo,
    release_version: Option<String>,
    bundle_manifest_digest: Option<String>,
    tip_cache: TipCache,
    /// Aborts the background refresh when the last clone of the server
    /// (typically the one jsonrpsee holds) is dropped. Held only for that
    /// drop; never read.
    _tip_refresh: Option<Arc<AbortOnDrop>>,
}

/// What the submit pre-checks need from a db tip, resolved once per fetch
/// so each later access is a field read.
#[derive(Debug, Clone, Copy)]
struct TipState {
    height: i64,
    /// Every field unset when the tip's network config is a version this
    /// build cannot interpret; chain-configured checks are then skipped.
    params: NetworkConfigParams,
}

impl TipState {
    fn from_reading(reading: &TipReading) -> Self {
        let params = match NetworkConfigParams::from_network_config(&reading.network_config) {
            Ok(params) => params,
            Err(error) => {
                tracing::warn!(%error, "tip network config unreadable, chain-configured pre-checks skipped");
                NetworkConfigParams::default()
            }
        };
        Self {
            height: *reading.tip.height,
            params,
        }
    }
}

/// Rate-limited, best-effort db-tip reading shared by all
/// `sendRawTransaction` pre-checks on this server.
///
/// A submit asks the backend at most once per TTL; submits inside the
/// window reuse the last reading. The submit that finds the window expired
/// claims the refresh before awaiting it, so concurrent submits keep using
/// the stale reading instead of queuing behind the probe. Failed or
/// timed-out probes keep the previous reading: the tip only grows, so a
/// stale height can only under-detect expiry, and the config changes only
/// at explicit upgrade blocks.
#[derive(Debug, Clone, Default)]
struct TipCache {
    state: Arc<tokio::sync::Mutex<TipCacheState>>,
}

#[derive(Debug, Default)]
struct TipCacheState {
    last_attempt: Option<Instant>,
    reading: Option<TipState>,
}

impl TipCache {
    /// Record a reading obtained by any path, restarting the TTL window.
    async fn store(&self, reading: TipState) {
        let mut state = self.state.lock().await;
        state.last_attempt = Some(Instant::now());
        state.reading = Some(reading);
    }

    /// Current reading, refreshing it through `fetch` when the TTL has
    /// lapsed. Returns `None` only when no reading has ever succeeded.
    async fn reading<Fut>(
        &self,
        ttl: Duration,
        probe_timeout: Duration,
        context: RpcRequestContext,
        fetch: impl FnOnce(RpcRequestContext) -> Fut,
    ) -> Option<TipState>
    where
        Fut: Future<Output = Result<TipReading, RpcServerError>>,
    {
        let now = Instant::now();
        {
            let mut state = self.state.lock().await;
            match state.last_attempt {
                Some(at) if now.duration_since(at) < ttl => return state.reading,
                _ => state.last_attempt = Some(now),
            }
        }

        let probe_context = context.capped(probe_timeout);
        let Some(remaining) = probe_context.remaining() else {
            return self.state.lock().await.reading;
        };
        match tokio::time::timeout(remaining, fetch(probe_context)).await {
            Ok(Ok(reading)) => {
                let reading = TipState::from_reading(&reading);
                self.state.lock().await.reading = Some(reading);
                Some(reading)
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "chain tip unavailable, pre-checks use last reading");
                self.state.lock().await.reading
            }
            Err(_) => {
                tracing::warn!("chain tip probe timed out, pre-checks use last reading");
                self.state.lock().await.reading
            }
        }
    }
}

/// Aborts a background task when dropped.
#[derive(Debug)]
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Start the task that keeps `cache` fresh on an idle server.
///
/// Each tick refreshes only if no probe ran within the interval, so the
/// task never adds load to a server whose submits already keep the reading
/// current. Returns `None` outside a Tokio runtime, where no background
/// work can run and submits alone refresh the cache.
fn spawn_tip_refresh<C: ChainPlane>(
    chain: Arc<C>,
    cache: TipCache,
    config: &RpcServerConfig,
) -> Option<AbortOnDrop> {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::debug!("no tokio runtime, chain tip refresh task not started");
        return None;
    };
    let interval = Duration::from_millis(config.tip_refresh_interval_ms.max(1));
    let probe_timeout = Duration::from_millis(config.tip_probe_timeout_ms);
    let task = handle.spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            cache
                .reading(
                    interval,
                    probe_timeout,
                    RpcRequestContext::unbounded(),
                    |context| chain.chain_tip(context),
                )
                .await;
        }
    });
    Some(AbortOnDrop(task.abort_handle()))
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

    /// Create build metadata with separate crate and legacy packaging-platform versions.
    pub const fn from_build_version(
        build_version: BuildVersion,
        commit_hash: Option<&'static str>,
    ) -> Self {
        Self {
            implementation_version: build_version.crate_version(),
            platform_version: build_version.platform_version(),
            commit_hash: match build_version.source_commit() {
                Some(source_commit) => Some(source_commit),
                None => commit_hash,
            },
        }
    }

    /// Build metadata suitable for unit tests and in-process fixtures.
    pub const fn for_tests() -> Self {
        Self::from_build_version(BuildVersion::new("test", None), Some("test-commit"))
    }
}

impl<S, C, R, V> GroundhogRpcServer<S, C, R, V>
where
    C: ChainPlane,
{
    /// Create a server wrapper around independent backend planes.
    ///
    /// When called inside a Tokio runtime this also starts the background
    /// task that refreshes the cached db-tip reading every
    /// `tip_refresh_interval_ms`; it stops when the last clone of the
    /// server is dropped.
    pub fn new(
        submit: S,
        chain: C,
        read: R,
        view: V,
        network_info: NetworkInfo,
        config: RpcServerConfig,
        build_info: BuildInfo,
    ) -> Self {
        let chain = Arc::new(chain);
        let tip_cache = TipCache::default();
        let tip_refresh =
            spawn_tip_refresh(chain.clone(), tip_cache.clone(), &config).map(Arc::new);
        Self {
            submit,
            chain,
            read,
            view,
            network_info,
            config,
            build_info,
            release_version: None,
            bundle_manifest_digest: None,
            tip_cache,
            _tip_refresh: tip_refresh,
        }
    }
}

impl<S, C, R, V> GroundhogRpcServer<S, C, R, V> {
    /// Bind this server to the validated release identity of its running component.
    #[must_use]
    pub fn with_bundle_identity(mut self, identity: &BundleIdentity) -> Self {
        let build_version = identity.build_version();
        self.build_info =
            BuildInfo::new(build_version.crate_version(), build_version.source_commit());
        self.release_version = identity.release_version().map(str::to_owned);
        self.bundle_manifest_digest = identity.bundle_manifest_digest().map(str::to_owned);
        self
    }

    #[cfg(test)]
    fn with_test_release_identity(mut self) -> Self {
        self.release_version = Some("0.1.25-rc.4".to_owned());
        self.bundle_manifest_digest = Some("sha256:test-manifest".to_owned());
        self
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
    /// Db-tip reading for the submit pre-checks, served from the shared
    /// rate-limited cache. Returns `None` when no reading is available
    /// (probe failed or timed out with no prior success); the caller then
    /// runs only the protocol-constant checks.
    async fn submit_tip_state(&self, context: RpcRequestContext) -> Option<TipState> {
        self.tip_cache
            .reading(
                Duration::from_millis(self.config.tip_cache_ttl_ms),
                Duration::from_millis(self.config.tip_probe_timeout_ms),
                context,
                |probe_context| self.chain.chain_tip(probe_context),
            )
            .await
    }
}

impl<B> GroundhogRpcServer<B, B, B, B>
where
    B: Clone + ChainPlane,
{
    /// Convenience constructor for backends that implement all four planes
    /// on a single type.
    pub fn unified(
        backend: B,
        network_info: NetworkInfo,
        config: RpcServerConfig,
        build_info: BuildInfo,
    ) -> Self {
        Self::new(
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend,
            network_info,
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
            // The pre-checks are advisory mirrors of what the scheduler and
            // executor enforce authoritatively: an unavailable tip reading
            // skips the chain-dependent checks rather than blocking submission.
            let tip = self.submit_tip_state(context).await;
            let params = tip.map(|tip| tip.params).unwrap_or_default();
            let tx = validate_raw_transaction(&tx_hex, &params)?;
            // A transaction can only be included at heights strictly above the
            // current tip, so `expiry <= tip` can never commit.
            if let Some(tip_height) = tip.map(|tip| tip.height)
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
    async fn network_info(&self) -> RpcResult<NetworkInfo> {
        Ok(self.network_info.clone())
    }

    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> RpcResult<ChainTip> {
        let context = self.request_context();
        self.within_request_deadline(context, async {
            let reading = self.chain.chain_tip(context).await?;
            // Every fetched tip refreshes the pre-check cache, config included.
            self.tip_cache.store(TipState::from_reading(&reading)).await;
            Ok(reading.tip)
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
            .with_platform_version(self.build_info.platform_version)
            .with_release_identity(
                self.release_version.clone(),
                self.bundle_manifest_digest.clone(),
            ))
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
    use groundhog_json_rpc_types::NetworkHash;
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
        GroundhogRpcServer::unified(
            mock,
            test_network_info(),
            RpcServerConfig::default(),
            BuildInfo::for_tests(),
        )
    }

    /// A tip reading whose network config states no parameters.
    fn tip_reading(height: i64) -> TipReading {
        tip_reading_with_params(height, vec![])
    }

    /// A tip reading whose network config states only `maxNewContractsPerTx`.
    fn tip_reading_with_max_contracts(height: i64, max_new_contracts_per_tx: u32) -> TipReading {
        tip_reading_with_params(
            height,
            vec![
                groundhog_xdr::network_config::ConfigParam::kConfigParamMaxNewContractsPerTx {
                    max_new_contracts_per_tx,
                },
            ],
        )
    }

    fn tip_reading_with_params(
        height: i64,
        params: Vec<groundhog_xdr::network_config::ConfigParam>,
    ) -> TipReading {
        TipReading::new(
            ChainTip {
                height: BlockHeight::from(height),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
            },
            groundhog_xdr::network_config::NetworkConfig::_0 {
                v0: groundhog_xdr::network_config::NetworkConfigV0 {
                    params: VecM(params),
                },
            },
        )
    }

    fn test_network_info() -> NetworkInfo {
        NetworkInfo::new(NetworkHash::from(groundhog_xdr::types::SHA256Hash::from(
            [0x07; 32],
        )))
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
        async fn chain_tip(
            &self,
            _context: RpcRequestContext,
        ) -> Result<TipReading, RpcServerError> {
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
        async fn chain_tip(
            &self,
            _context: RpcRequestContext,
        ) -> Result<TipReading, RpcServerError> {
            Ok(tip_reading(42))
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
        async fn chain_tip(
            &self,
            _context: RpcRequestContext,
        ) -> Result<TipReading, RpcServerError> {
            Ok(tip_reading(1_000_000))
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
        async fn chain_tip(
            &self,
            _context: RpcRequestContext,
        ) -> Result<TipReading, RpcServerError> {
            *self.tip_calls.lock().await += 1;
            Ok(tip_reading(42))
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
        async fn chain_tip(
            &self,
            _context: RpcRequestContext,
        ) -> Result<TipReading, RpcServerError> {
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
        async fn chain_tip(
            &self,
            context: RpcRequestContext,
        ) -> Result<TipReading, RpcServerError> {
            self.contexts.lock().await.push(context);
            tokio::time::sleep(self.delay).await;
            Ok(tip_reading(42))
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
            test_network_info(),
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
            test_network_info(),
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

    /// Chain plane whose tip carries a chosen `maxNewContractsPerTx` and
    /// that counts how often it is asked, with a pre-expiry tip.
    #[derive(Clone)]
    struct ConfigChain {
        max_new_contracts_per_tx: u32,
        tip_calls: Arc<Mutex<u32>>,
    }

    impl ConfigChain {
        fn new(max_new_contracts_per_tx: u32) -> Self {
            Self {
                max_new_contracts_per_tx,
                tip_calls: Arc::new(Mutex::new(0)),
            }
        }
    }

    #[async_trait::async_trait]
    impl ChainPlane for ConfigChain {
        async fn chain_tip(
            &self,
            _context: RpcRequestContext,
        ) -> Result<TipReading, RpcServerError> {
            *self.tip_calls.lock().await += 1;
            Ok(tip_reading_with_max_contracts(
                42,
                self.max_new_contracts_per_tx,
            ))
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

    /// Chain plane whose `chain_tip` always fails.
    #[derive(Clone, Copy)]
    struct UnavailableChain;

    #[async_trait::async_trait]
    impl ChainPlane for UnavailableChain {
        async fn chain_tip(
            &self,
            _context: RpcRequestContext,
        ) -> Result<TipReading, RpcServerError> {
            Err(RpcServerError::BackendUnavailable("scheduler down".into()))
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

    fn rv32_contract(size: usize, fill: u8) -> groundhog_xdr::contract::Contract {
        groundhog_xdr::contract::Contract::kRv32 {
            rv32_contract: groundhog_xdr::contract::ContractBlob {
                data: xdr_common::BytesM(vec![fill; size]),
            },
        }
    }

    fn signed_tx_hex_with_contracts(contracts: Vec<groundhog_xdr::contract::Contract>) -> String {
        format!(
            "0x{}",
            hex::encode(crate::tx::test_fixtures::signed_tx_bytes_with_contracts(
                contracts
            ))
        )
    }

    #[tokio::test]
    async fn config_reading_rides_on_the_tip_probe() {
        let chain = ConfigChain::new(4);
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let server =
            expiry_check_server(chain.clone(), RpcServerConfig::default(), submitted.clone());

        for _ in 0..3 {
            server
                .send_raw_transaction(signed_tx_hex_with_contracts(vec![rv32_contract(8, 1)]))
                .await
                .expect("bug: one attached contract is within a cap of four");
        }

        assert_eq!(
            *chain.tip_calls.lock().await,
            1,
            "count check should use the tip reading, not a second backend call"
        );
        assert_eq!(submitted.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn too_many_contracts_is_rejected_with_structured_data_before_submit() {
        let chain = ConfigChain::new(1);
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let server = expiry_check_server(chain, RpcServerConfig::default(), submitted.clone());

        let error = server
            .send_raw_transaction(signed_tx_hex_with_contracts(vec![
                rv32_contract(8, 1),
                rv32_contract(8, 2),
            ]))
            .await
            .expect_err("bug: two contracts should exceed a cap of one");

        assert_eq!(error.code(), codes::TX_INVALID);
        assert_eq!(
            error.message(),
            "invalid transaction: transaction attaches 2 contracts; chain permits 1"
        );
        let data: serde_json::Value =
            serde_json::from_str(error.data().expect("bound violation carries data").get())
                .unwrap();
        assert_eq!(
            data,
            serde_json::json!({"reason": "tooManyContracts", "attached": 2, "max": 1})
        );
        assert!(submitted.lock().await.is_empty());
    }

    #[tokio::test]
    async fn oversized_contract_is_rejected_when_tip_is_unavailable() {
        // Without a tip reading the chain-configured count check is skipped,
        // but the protocol-constant size cap still applies.
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let server = expiry_check_server(
            UnavailableChain,
            RpcServerConfig::default(),
            submitted.clone(),
        );
        let oversized = groundhog_tx_validation::MAX_CONTRACT_SIZE_BYTES as usize + 1;

        let error = server
            .send_raw_transaction(signed_tx_hex_with_contracts(vec![rv32_contract(
                oversized, 1,
            )]))
            .await
            .expect_err("bug: oversized contract should be rejected");
        server
            .send_raw_transaction(signed_tx_hex_with_contracts(
                (1..=8).map(|fill| rv32_contract(1, fill)).collect(),
            ))
            .await
            .expect("bug: without a tip reading the count check is skipped");

        assert_eq!(error.code(), codes::TX_INVALID);
        let data: serde_json::Value =
            serde_json::from_str(error.data().expect("bound violation carries data").get())
                .unwrap();
        assert_eq!(data["reason"], "contractTooLarge");
        assert_eq!(data["index"], 0);
        assert_eq!(submitted.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn chain_tip_rpc_refreshes_the_precheck_cache() {
        let chain = ConfigChain::new(1);
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let server = expiry_check_server(chain.clone(), RpcServerConfig::default(), submitted);

        server
            .chain_tip()
            .await
            .expect("bug: tip should be readable");
        let error = server
            .send_raw_transaction(signed_tx_hex_with_contracts(vec![
                rv32_contract(8, 1),
                rv32_contract(8, 2),
            ]))
            .await
            .expect_err("bug: the config fetched by chainTip should reject two contracts");

        assert_eq!(error.code(), codes::TX_INVALID);
        assert_eq!(
            *chain.tip_calls.lock().await,
            1,
            "the submit should reuse the reading the chainTip call stored"
        );
    }

    #[tokio::test]
    async fn background_refresh_fetches_the_tip_without_submissions() {
        let chain = ConfigChain::new(4);
        let _server = expiry_check_server(
            chain.clone(),
            RpcServerConfig::default().with_tip_refresh_interval_ms(10),
            Arc::new(Mutex::new(Vec::new())),
        );

        let started = std::time::Instant::now();
        while *chain.tip_calls.lock().await == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "background refresh should have probed the tip by now"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn background_refresh_stops_when_the_server_is_dropped() {
        let chain = ConfigChain::new(4);
        let server = expiry_check_server(
            chain.clone(),
            RpcServerConfig::default().with_tip_refresh_interval_ms(5),
            Arc::new(Mutex::new(Vec::new())),
        );
        while *chain.tip_calls.lock().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        drop(server);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let after_drop = *chain.tip_calls.lock().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            *chain.tip_calls.lock().await,
            after_drop,
            "no probes should run after the last server clone is dropped"
        );
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
            test_network_info(),
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
            test_network_info(),
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
            test_network_info(),
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
            test_network_info(),
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
            test_network_info(),
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
            test_network_info(),
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
            test_network_info(),
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
            test_network_info(),
            RpcServerConfig::default(),
            BuildInfo::for_tests(),
        )
        .with_test_release_identity();

        let version = server
            .version()
            .await
            .expect("bug: version metadata should be infallible");

        assert_eq!(version.wire_format, WIRE_FORMAT_VERSION);
        assert_eq!(version.implementation, "test");
        assert_eq!(version.platform_version, None);
        assert_eq!(version.release_version.as_deref(), Some("0.1.25-rc.4"));
        assert_eq!(
            version.bundle_manifest_digest.as_deref(),
            Some("sha256:test-manifest")
        );
        assert_eq!(version.commit_hash.as_deref(), Some("test-commit"));
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
