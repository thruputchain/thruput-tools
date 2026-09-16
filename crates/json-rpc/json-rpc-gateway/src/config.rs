use std::{fmt, str::FromStr, time::Duration};

use serde::Deserialize;

use groundhog_json_rpc_types::{
    NetworkHash, NetworkHashHex, SHA256Hash,
    network_info::NetworkInfo,
    runtime_manifest::{
        HOGD_DEV_RUNTIME_MANIFEST_KIND, HOSTED_DEVNET_RUNTIME_MANIFEST_KIND, RuntimeManifest,
    },
};

/// Indexer data connections (1..=256, default 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "usize")]
pub struct IndexerPoolSize(std::num::NonZeroUsize);

impl IndexerPoolSize {
    /// Return the validated connection count.
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for IndexerPoolSize {
    fn default() -> Self {
        Self(std::num::NonZeroUsize::new(16).expect("bug: default indexer pool size is nonzero"))
    }
}

impl TryFrom<usize> for IndexerPoolSize {
    type Error = InvalidIndexerPoolSize;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match std::num::NonZeroUsize::new(value) {
            Some(value) if value.get() <= 256 => Ok(Self(value)),
            _ => Err(InvalidIndexerPoolSize),
        }
    }
}

impl FromStr for IndexerPoolSize {
    type Err = InvalidIndexerPoolSize;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<usize>()
            .map_err(|_| InvalidIndexerPoolSize)?
            .try_into()
    }
}

/// An indexer connection count outside the supported resource bound.
#[derive(Debug, thiserror::Error)]
#[error("indexer pool size must be in 1..=256")]
pub struct InvalidIndexerPoolSize;

/// Backend names used by readiness policy.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HealthBackend {
    /// hogd scheduler/mempool backend.
    Scheduler,

    /// hogd indexer/block explorer backend.
    Indexer,

    /// hogd `DbGateway` backend.
    DbGateway,

    /// hogd `ViewGateway` backend.
    ViewGateway,
}

impl HealthBackend {
    pub(crate) const ALL: [Self; 4] = [
        Self::Scheduler,
        Self::Indexer,
        Self::DbGateway,
        Self::ViewGateway,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduler => "scheduler",
            Self::Indexer => "indexer",
            Self::DbGateway => "db_gateway",
            Self::ViewGateway => "view_gateway",
        }
    }
}

impl fmt::Display for HealthBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HealthBackend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "scheduler" => Ok(Self::Scheduler),
            "indexer" => Ok(Self::Indexer),
            "db_gateway" => Ok(Self::DbGateway),
            "view_gateway" => Ok(Self::ViewGateway),
            other => Err(format!(
                "unknown health backend `{other}`; expected scheduler,indexer,db_gateway,view_gateway"
            )),
        }
    }
}

/// Adaptive-batching policy.
///
/// Incoming client requests are accumulated and packed into one
/// backend RPC per fired batch. There is **no time-based flush**: the
/// only trigger is "a slot opened up under `max_inflight_batches`".
/// At low load a single arrival fires immediately as a batch of one
/// (zero added latency); at high load new arrivals pile up while a
/// batch is in flight and drain together when it completes.
///
/// Used for both `sendRawTransaction` (packing tx blobs into
/// `schedgateway_submit_tx_batch`) and `getStorageAt`
/// (packing item reads into `dbgateway_get_items`); see
/// `GatewayConfig::batch` and `GatewayConfig::storage` for the per-RPC
/// tuning slots.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdaptiveBatchConfig {
    /// Number of independent sharded worker tasks. Incoming requests
    /// are routed to a shard by a stable hash of the request key.
    /// Higher values reduce channel/lock contention once the gateway
    /// scales across many tokio worker threads.
    pub shards: usize,

    /// Maximum number of backend RPCs in flight per shard. Acts as a
    /// BDP ceiling: enough parallel batches to cover RTT without
    /// fragmenting them too aggressively. `1` degenerates to strictly
    /// serial batches (useful as an A/B canary).
    pub max_inflight_batches: u32,

    /// Hard cap on the number of items packed into one batch.
    pub max_size: usize,

    /// Bounded depth of the per-shard ingress channel. When full,
    /// new requests fail immediately with `Overloaded`.
    pub ingress_queue_depth: usize,
}

impl AdaptiveBatchConfig {
    /// Maximum number of backend RPCs this batcher can have in flight
    /// at once: `shards × max_inflight_batches`.  Use this to size
    /// transport-layer connection pools so that the batcher's own
    /// `Overloaded` is the saturation signal, not transport queuing.
    pub fn max_concurrent_rpcs(&self) -> usize {
        self.shards
            .max(1)
            .saturating_mul(self.max_inflight_batches.max(1) as usize)
    }
}

impl Default for AdaptiveBatchConfig {
    fn default() -> Self {
        Self {
            shards: 1,
            max_inflight_batches: 4,
            max_size: 1000,
            ingress_queue_depth: 10_000,
        }
    }
}

/// Request-coalescing config for `chainTip`.
///
/// A single task serializes backend `schedgateway_get_tip` calls. While
/// one call is in flight, subsequent `chainTip` requests accumulate in
/// a waiter list; when the call returns, every waiter receives the
/// same `BlockTip` via fanout. Linearizable: each waiter's result
/// comes from an RPC initiated strictly after they arrived.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TipConfig {
    /// Maximum number of overlapping `get_tip` RPCs. Covers RTT under
    /// burst load; `1` means strictly serial.
    pub max_inflight_tips: u32,

    /// Bounded depth of the ingress channel. When full, new `chainTip`
    /// requests fail immediately with `Overloaded`.
    pub ingress_queue_depth: usize,
}

impl Default for TipConfig {
    fn default() -> Self {
        Self {
            max_inflight_tips: 4,
            ingress_queue_depth: 10_000,
        }
    }
}

/// Gateway configuration, loaded from TOML.
///
/// Unknown keys are rejected so a typo in an operator config fails
/// startup instead of silently running with defaults.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    /// Logical network identifier used by the hosted faucet API.
    pub network_id: Option<String>,

    /// Hash of the network name bound into transaction signatures.
    pub network_hash: Option<NetworkHashHex>,

    /// Materialized genesis hash used only by the hosted faucet adapter.
    pub genesis_hash: Option<SHA256Hash>,

    /// Optional hosted faucet base URL. The airdrop RPC is absent when unset.
    pub faucet_url: Option<String>,

    /// Listen address for the JSON-RPC server.
    pub listen_address: String,

    /// End-to-end HTTP/WebSocket JSON-RPC timeout in milliseconds.
    pub request_timeout_ms: u64,

    /// Maximum simultaneous TCP/WebSocket connections.
    pub max_connections: u32,

    /// Active plus buffered entries per JSON-RPC array (1..=1024, default 128).
    pub rpc_batch_concurrency: groundhog_json_rpc_server::batch::RpcBatchConcurrency,

    /// Private health/metrics HTTP listener.
    pub observability_listen: String,

    /// Backends required for readiness status `ok`.
    pub health_required_backends: Vec<HealthBackend>,

    /// Maximum allowed age of the last successful chain-tip probe.
    pub health_chain_tip_max_age_ms: u64,

    /// Interval for background readiness probes.
    pub health_probe_interval_ms: u64,

    /// Maximum time to build one backend connection pool at startup or after
    /// disconnect.
    ///
    /// This is separate from the health probe interval because reconnecting a
    /// multi-connection x23 pool can take longer than one readiness cycle.
    pub backend_connect_timeout_ms: u64,

    /// Delay scheduler tip probes after initially building or rebuilding its
    /// connection pool.
    ///
    /// Older hogd schedulers accept TCP connections before coordinator
    /// registration makes `schedgateway_get_tip` safe to call.
    pub scheduler_reconnect_grace_ms: u64,

    /// Maximum total time to drain gateway servers and background health work
    /// after receiving a shutdown signal.
    pub shutdown_grace_ms: u64,

    /// x23 RPC address for hogd scheduler.
    ///
    /// Required for scheduler-backed submission and for read paths such as
    /// `chainTip`, `pendingTransactionCount`, and pending tx lookup.
    pub scheduler_address: String,

    /// x23 RPC address for hogd indexer.
    ///
    /// Optional in runtime-manifest mode. When absent, read APIs backed by the
    /// indexer return `BackendUnavailable`.
    pub indexer_address: Option<String>,

    /// Number of serial x23 data connections to the indexer (1..=256, default 16).
    ///
    /// Applies at startup and reconnect. The separate health connection is
    /// excluded; each data connection handles one ordinary RPC at a time.
    pub indexer_pool_size: IndexerPoolSize,

    /// x23 RPC address for hogd `DbGateway1` (latest-snapshot reads).
    ///
    /// Serves `groundhog_getStorageAt`. Optional: when absent the
    /// gateway starts in degraded mode and `getStorageAt` returns
    /// `BackendUnavailable`.
    pub db_gateway_address: Option<String>,

    /// x23 RPC address for hogd `ViewGateway1` (sandboxed contract simulation).
    ///
    /// Serves `groundhog_view`. Optional: when absent the gateway returns
    /// `NotImplemented` for view execution. When configured, transient
    /// connection or intro timeout/RPC failures degrade startup; intro
    /// rejections remain hard startup errors because they usually indicate a
    /// protocol/config mismatch.
    pub view_gateway_address: Option<String>,

    /// Maximum time to wait for one `ViewGateway` execution RPC.
    pub view_timeout_ms: u64,

    /// Optional protocol string used for hogd intro handshakes.
    ///
    /// Defaults to the protocol embedded in the linked `groundhog-xdr` crate.
    /// This override is intended for local cross-repo smoke tests before a
    /// matching Rust protocol crate has been published.
    pub hogd_protocol_override: Option<String>,

    /// Maximum `groundhog_view` calldata byte length.
    pub max_view_calldata_bytes: usize,

    /// Gas budget assigned to backend `groundhog_view` execution.
    pub view_gas_limit: u64,

    /// Per-page cap on `getEvents` responses; requests with `limit`
    /// above this are rejected as `InvalidParam`.
    pub max_events_per_page: u32,

    /// Per-page cap on `getTransactionsByAddress` responses; requests
    /// with `limit` above this are rejected as `InvalidParam`.
    pub max_transactions_per_page: u32,

    /// Maximum length of the opaque pagination cursor on paginated
    /// methods; longer cursors are rejected as `InvalidParam`.
    pub max_cursor_bytes: usize,

    /// Adaptive batching for `sendRawTransaction`.
    pub batch: AdaptiveBatchConfig,

    /// Adaptive batching for `getStorageAt`.
    pub storage: AdaptiveBatchConfig,

    /// Request coalescing for `chainTip`.
    pub tip: TipConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            network_id: None,
            network_hash: None,
            genesis_hash: None,
            faucet_url: None,
            listen_address: "127.0.0.1:8545".into(),
            request_timeout_ms: 60_000,
            max_connections: 2000,
            rpc_batch_concurrency: groundhog_json_rpc_server::batch::RpcBatchConcurrency::default(),
            observability_listen: "127.0.0.1:9646".into(),
            health_required_backends: vec![HealthBackend::Scheduler, HealthBackend::Indexer],
            health_chain_tip_max_age_ms: 30_000,
            health_probe_interval_ms: 5_000,
            backend_connect_timeout_ms: 30_000,
            scheduler_reconnect_grace_ms: 30_000,
            shutdown_grace_ms: 10_000,
            scheduler_address: "127.0.0.1:9000".into(),
            indexer_address: Some("127.0.0.1:9001".into()),
            indexer_pool_size: IndexerPoolSize::default(),
            db_gateway_address: None,
            view_gateway_address: None,
            view_timeout_ms: 30_000,
            hogd_protocol_override: None,
            max_view_calldata_bytes: 64 * 1024,
            view_gas_limit: groundhog_json_rpc_server::DEFAULT_VIEW_GAS_LIMIT,
            max_events_per_page: 200,
            max_transactions_per_page: 200,
            max_cursor_bytes: 256,
            batch: AdaptiveBatchConfig::default(),
            storage: AdaptiveBatchConfig::default(),
            tip: TipConfig::default(),
        }
    }
}

impl GatewayConfig {
    pub(crate) fn batch_service<S>(
        &self,
        inner: S,
        max_response_body_size: u32,
    ) -> groundhog_json_rpc_server::batch::ConcurrentBatchService<S> {
        groundhog_json_rpc_server::batch::ConcurrentBatchService::new(
            inner,
            max_response_body_size,
            self.rpc_batch_concurrency,
        )
    }

    /// Resolve the immutable identity required before the gateway starts.
    pub fn network_info(&self) -> Result<NetworkInfo, NetworkIdentityConfigError> {
        let network_hash = self
            .network_hash
            .as_ref()
            .ok_or(NetworkIdentityConfigError::MissingNetworkHash)?;
        Ok(NetworkInfo::new(NetworkHash::from(
            network_hash.to_network_hash().into_inner(),
        )))
    }

    pub(crate) fn request_timeout(&self) -> Duration {
        // Keep backend, application, and HTTP deadlines strictly ordered even
        // for malformed or test-only sub-millisecond configurations.
        Duration::from_millis(self.request_timeout_ms.max(3))
    }

    pub(crate) fn backend_request_timeout(&self) -> Duration {
        self.request_timeout()
            .saturating_sub(self.request_timeout_margin())
            .max(Duration::from_millis(1))
    }

    pub(crate) fn http_request_timeout(&self) -> Duration {
        self.request_timeout()
            .saturating_add(self.request_timeout_margin())
    }

    pub(crate) fn request_timeout_margin(&self) -> Duration {
        (self.request_timeout() / 10).clamp(Duration::from_millis(1), Duration::from_secs(1))
    }

    /// Overlay backend endpoint configuration from a runtime manifest.
    pub fn apply_runtime_manifest(
        &mut self,
        manifest: &RuntimeManifest,
    ) -> Result<(), RuntimeManifestConfigError> {
        if manifest.schema_version
            != groundhog_json_rpc_types::runtime_manifest::RUNTIME_MANIFEST_SCHEMA_VERSION
        {
            return Err(RuntimeManifestConfigError::UnsupportedSchemaVersion {
                actual: manifest.schema_version,
            });
        }
        match manifest.kind.as_str() {
            HOGD_DEV_RUNTIME_MANIFEST_KIND if manifest.chain_state.is_none() => {
                return Err(RuntimeManifestConfigError::MissingChainState);
            }
            HOGD_DEV_RUNTIME_MANIFEST_KIND | HOSTED_DEVNET_RUNTIME_MANIFEST_KIND => {}
            _ => {
                return Err(RuntimeManifestConfigError::UnexpectedKind {
                    actual: manifest.kind.clone(),
                });
            }
        }
        let defaults = GatewayConfig::default();
        let network_hash = Some(manifest.network_hash.clone());
        let scheduler_address = manifest
            .endpoints
            .scheduler_gateway
            .ok_or(RuntimeManifestConfigError::MissingSchedulerGateway)?
            .to_string();
        let indexer_address = manifest.endpoints.indexer.map(|addr| addr.to_string());
        let db_gateway_address = manifest.endpoints.db_gateway.map(|addr| addr.to_string());
        let view_gateway_address = manifest.endpoints.view_gateway.map(|addr| addr.to_string());
        let hogd_protocol_override = Some(manifest.protocol.clone());

        reject_manifest_conflict(
            "network_hash",
            &self.network_hash,
            &defaults.network_hash,
            &network_hash,
        )?;
        reject_manifest_conflict(
            "scheduler_address",
            &self.scheduler_address,
            &defaults.scheduler_address,
            &scheduler_address,
        )?;
        reject_manifest_conflict(
            "indexer_address",
            &self.indexer_address,
            &defaults.indexer_address,
            &indexer_address,
        )?;
        reject_manifest_conflict(
            "db_gateway_address",
            &self.db_gateway_address,
            &defaults.db_gateway_address,
            &db_gateway_address,
        )?;
        reject_manifest_conflict(
            "view_gateway_address",
            &self.view_gateway_address,
            &defaults.view_gateway_address,
            &view_gateway_address,
        )?;
        reject_manifest_conflict(
            "hogd_protocol_override",
            &self.hogd_protocol_override,
            &defaults.hogd_protocol_override,
            &hogd_protocol_override,
        )?;

        self.network_hash = network_hash;
        self.scheduler_address = scheduler_address;
        self.indexer_address = indexer_address;
        self.db_gateway_address = db_gateway_address;
        self.view_gateway_address = view_gateway_address;
        self.hogd_protocol_override = hogd_protocol_override;
        Ok(())
    }
}

/// Invalid or incomplete immutable gateway identity configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NetworkIdentityConfigError {
    /// No protocol network hash was configured.
    #[error("missing required gateway field `network_hash`")]
    MissingNetworkHash,
}

fn reject_manifest_conflict<T>(
    field: &'static str,
    current: &T,
    default: &T,
    manifest: &T,
) -> Result<(), RuntimeManifestConfigError>
where
    T: PartialEq,
{
    if current != default && current != manifest {
        return Err(RuntimeManifestConfigError::TomlConflict { field });
    }
    Ok(())
}

/// Runtime manifest configuration error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeManifestConfigError {
    /// The manifest uses a schema version this gateway does not understand.
    #[error("unsupported runtime manifest schemaVersion {actual}")]
    UnsupportedSchemaVersion {
        /// Actual manifest schema version.
        actual: u32,
    },

    /// The manifest was not produced by a supported runtime.
    #[error(
        "unexpected runtime manifest kind `{actual}`; expected `hogd-dev-runtime` or \
         `hosted-devnet-runtime`"
    )]
    UnexpectedKind {
        /// Actual manifest kind.
        actual: String,
    },

    /// The manifest does not identify whether the chain was initialized or recovered.
    #[error("hog-devnode runtime manifest is missing chainState")]
    MissingChainState,

    /// The manifest predates the separate scheduler gateway endpoint.
    #[error("runtime manifest is missing required endpoint `schedulerGateway`")]
    MissingSchedulerGateway,

    /// The config file already set a backend field to a conflicting value.
    #[error("runtime manifest conflicts with configured field `{field}`")]
    TomlConflict {
        /// Conflicting config field.
        field: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use groundhog_json_rpc_types::{NetworkHashHex, hex::Hex0x, runtime_manifest::RuntimeManifest};
    use groundhog_xdr::types::SHA256Hash;
    use serde_json::json;

    use super::{GatewayConfig, NetworkIdentityConfigError, RuntimeManifestConfigError};

    #[test]
    fn rpc_batch_concurrency_toml_defaults_and_bounds() {
        assert_eq!(
            toml::from_str::<super::GatewayConfig>("")
                .unwrap()
                .rpc_batch_concurrency
                .get(),
            128
        );
        for value in [0, 1025] {
            let error =
                toml::from_str::<super::GatewayConfig>(&format!("rpc_batch_concurrency = {value}"))
                    .unwrap_err();
            assert!(error.to_string().contains("1..=1024"));
        }
        for value in [1, 17, 1024] {
            assert_eq!(
                toml::from_str::<super::GatewayConfig>(&format!("rpc_batch_concurrency = {value}"))
                    .unwrap()
                    .rpc_batch_concurrency
                    .get(),
                value
            );
        }
    }
    #[test]
    fn toml_with_unknown_top_level_key_is_rejected() {
        let error = toml::from_str::<GatewayConfig>(
            r#"
            listen_address = "127.0.0.1:8545"
            listen_adress = "0.0.0.0:8545"
            "#,
        )
        .expect_err("bug: unknown config key should fail");

        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn toml_with_unknown_batch_or_tip_key_is_rejected() {
        let batch_error = toml::from_str::<GatewayConfig>(
            r"
            [batch]
            max_sizes = 10
            ",
        )
        .expect_err("bug: unknown batch key should fail");
        assert!(batch_error.to_string().contains("unknown field"));

        let tip_error = toml::from_str::<GatewayConfig>(
            r"
            [tip]
            max_inflight_tip = 1
            ",
        )
        .expect_err("bug: unknown tip key should fail");
        assert!(tip_error.to_string().contains("unknown field"));
    }

    #[test]
    fn toml_with_known_keys_parses() {
        let config = toml::from_str::<GatewayConfig>(
            r#"
            listen_address = "127.0.0.1:9999"

            [batch]
            max_size = 10

            [tip]
            max_inflight_tips = 2
            "#,
        )
        .expect("bug: known config keys should parse");

        assert_eq!(config.listen_address, "127.0.0.1:9999");
        assert_eq!(config.batch.max_size, 10);
        assert_eq!(config.tip.max_inflight_tips, 2);
    }

    #[test]
    fn request_deadlines_are_nested() {
        let config = GatewayConfig {
            request_timeout_ms: 10_000,
            ..GatewayConfig::default()
        };

        assert_eq!(config.backend_request_timeout(), Duration::from_secs(9));
        assert_eq!(config.request_timeout(), Duration::from_secs(10));
        assert_eq!(config.http_request_timeout(), Duration::from_secs(11));

        let minimum = GatewayConfig {
            request_timeout_ms: 0,
            ..GatewayConfig::default()
        };
        assert_eq!(minimum.backend_request_timeout(), Duration::from_millis(2));
        assert_eq!(minimum.request_timeout(), Duration::from_millis(3));
        assert_eq!(minimum.http_request_timeout(), Duration::from_millis(4));
    }

    #[test]
    fn gateway_network_identity_is_required_and_preserved() {
        let expected_network_hash = NetworkHashHex::from([0x12; 32]);
        let config = GatewayConfig {
            network_hash: Some(expected_network_hash),
            ..GatewayConfig::default()
        };

        let network_info = config
            .network_info()
            .expect("bug: complete gateway identity should resolve");

        assert_eq!(network_info.network_hash.as_ref(), &[0x12; 32]);
    }

    #[test]
    fn gateway_network_identity_rejects_missing_network_hash() {
        assert_eq!(
            GatewayConfig::default().network_info(),
            Err(NetworkIdentityConfigError::MissingNetworkHash)
        );
    }

    #[test]
    fn gateway_network_identity_rejects_malformed_hashes() {
        let error = toml::from_str::<GatewayConfig>(
            r#"
genesis_hash = "0x1234"
"#,
        )
        .expect_err("bug: malformed genesis hash should fail TOML decoding");

        assert!(error.to_string().contains("genesis_hash"));

        let error = toml::from_str::<GatewayConfig>(
            r#"
network_hash = "0x1234"
"#,
        )
        .expect_err("bug: malformed network hash should fail TOML decoding");

        assert!(error.to_string().contains("network_hash"));
    }

    #[test]
    fn runtime_manifest_overlays_gateway_backend_endpoints() {
        let mut config = GatewayConfig::default();
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
            "indexer": "127.0.0.1:1001",
            "dbGateway": "127.0.0.1:1002",
            "viewGateway": "127.0.0.1:1003",
        }));

        config
            .apply_runtime_manifest(&manifest)
            .expect("bug: manifest should configure gateway");

        assert_eq!(config.scheduler_address, "127.0.0.1:1010");
        assert_eq!(config.network_hash, Some(manifest.network_hash.clone()));
        assert_eq!(config.indexer_address.as_deref(), Some("127.0.0.1:1001"));
        assert_eq!(config.db_gateway_address.as_deref(), Some("127.0.0.1:1002"));
        assert_eq!(
            config.view_gateway_address.as_deref(),
            Some("127.0.0.1:1003")
        );
        assert_eq!(
            config.hogd_protocol_override.as_deref(),
            Some("hogp/test00000000000")
        );
    }

    #[test]
    fn runtime_manifest_can_omit_indexer_for_gateway() {
        let mut config = GatewayConfig::default();
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
        }));

        config
            .apply_runtime_manifest(&manifest)
            .expect("bug: scheduler-only manifest should configure gateway");

        assert_eq!(config.scheduler_address, "127.0.0.1:1010");
        assert_eq!(config.indexer_address, None);
        assert_eq!(config.db_gateway_address, None);
        assert_eq!(config.view_gateway_address, None);
        assert_eq!(
            config.hogd_protocol_override.as_deref(),
            Some("hogp/test00000000000")
        );
    }

    #[test]
    fn runtime_manifest_requires_scheduler_gateway() {
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
        }));

        assert_eq!(
            GatewayConfig::default().apply_runtime_manifest(&manifest),
            Err(RuntimeManifestConfigError::MissingSchedulerGateway)
        );
    }

    #[test]
    fn runtime_manifest_rejects_unsupported_schema_version() {
        let mut config = GatewayConfig::default();
        let mut manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
        }));
        manifest.schema_version = 3;

        let err = config
            .apply_runtime_manifest(&manifest)
            .expect_err("bug: unsupported schema should fail");

        assert_eq!(
            err,
            RuntimeManifestConfigError::UnsupportedSchemaVersion { actual: 3 }
        );
    }

    #[test]
    fn runtime_manifest_rejects_unexpected_kind_and_missing_chain_state() {
        let mut wrong_kind = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
        }));
        wrong_kind.kind = "other-runtime".into();
        assert_eq!(
            GatewayConfig::default().apply_runtime_manifest(&wrong_kind),
            Err(RuntimeManifestConfigError::UnexpectedKind {
                actual: "other-runtime".into(),
            })
        );

        let mut missing_chain_state = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
        }));
        missing_chain_state.chain_state = None;
        assert_eq!(
            GatewayConfig::default().apply_runtime_manifest(&missing_chain_state),
            Err(RuntimeManifestConfigError::MissingChainState)
        );
    }

    #[test]
    fn hosted_runtime_manifest_configures_gateway_without_chain_state() {
        let mut config = GatewayConfig::default();
        let mut manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
            "indexer": "127.0.0.1:1001",
            "dbGateway": "127.0.0.1:1002",
            "viewGateway": "127.0.0.1:1003",
        }));
        manifest.kind = "hosted-devnet-runtime".into();
        manifest.chain_state = None;

        config
            .apply_runtime_manifest(&manifest)
            .expect("bug: hosted runtime manifest should configure gateway");

        assert_eq!(config.scheduler_address, "127.0.0.1:1010");
        assert_eq!(config.network_hash, Some(manifest.network_hash.clone()));
        assert_eq!(config.indexer_address.as_deref(), Some("127.0.0.1:1001"));
        assert_eq!(config.db_gateway_address.as_deref(), Some("127.0.0.1:1002"));
        assert_eq!(
            config.view_gateway_address.as_deref(),
            Some("127.0.0.1:1003")
        );
        assert_eq!(
            config.hogd_protocol_override.as_deref(),
            Some("hogp/test00000000000")
        );
    }

    #[test]
    fn runtime_manifest_conflicts_with_toml_endpoint_config() {
        let mut config = GatewayConfig {
            scheduler_address: "127.0.0.1:9999".into(),
            ..GatewayConfig::default()
        };
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
        }));

        let err = config
            .apply_runtime_manifest(&manifest)
            .expect_err("bug: conflicting scheduler should fail");

        assert_eq!(
            err,
            RuntimeManifestConfigError::TomlConflict {
                field: "scheduler_address",
            }
        );
    }

    #[test]
    fn runtime_manifest_allows_matching_config_value() {
        let mut config = GatewayConfig {
            network_hash: Some(NetworkHashHex::from([0; 32])),
            genesis_hash: Some(hash(0x42)),
            scheduler_address: "127.0.0.1:1010".into(),
            ..GatewayConfig::default()
        };
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
        }));

        config
            .apply_runtime_manifest(&manifest)
            .expect("bug: matching scheduler should not conflict");

        assert_eq!(config.scheduler_address, "127.0.0.1:1010");
    }

    #[test]
    fn runtime_manifest_rejects_conflicting_network_identity() {
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
        }));
        let mut network_config = GatewayConfig {
            network_hash: Some(NetworkHashHex::from([0x41; 32])),
            ..GatewayConfig::default()
        };
        assert_eq!(
            network_config.apply_runtime_manifest(&manifest),
            Err(RuntimeManifestConfigError::TomlConflict {
                field: "network_hash",
            })
        );
    }

    fn manifest_with_endpoints(endpoints: &serde_json::Value) -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 2,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test00000000000",
            "producer": {
                "binary": "hog-devnode",
                "version": "0.1.0",
                "commitHash": "abcdef0",
            },
            "endpoints": endpoints,
            "bootstrapAccountDerivation": {
                "scheme": "groundhog-bootstrap-account-v1",
                "seed": "groundhog-dev-accounts",
            },
            "bootstrapAccounts": [],
            "bootstrap": {
                "devWallet": {
                    "contentHash": hash(0x51),
                    "deployedCodeHash": hash(0x52),
                    "artifactHint": "./dev_wallet.rv32a",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "createWalletEntrypoint": "0x6372656174655761",
                    "doActionEntrypoint": "0x646f416374696f6e",
                },
                "storageCostModel": {
                    "hash": hash(0x53),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234",
                },
            },
        }))
        .expect("bug: manifest JSON should parse")
    }

    fn hash(byte: u8) -> groundhog_json_rpc_types::SHA256Hash {
        Hex0x(SHA256Hash::from([byte; 32]))
    }
}
