use serde::Deserialize;

use groundhog_json_rpc_types::runtime_manifest::RuntimeManifest;

/// Backend used for transaction submission.
///
/// This only affects `groundhog_sendRawTransaction`. Read paths still use
/// hogd scheduler/indexer directly.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionBackend {
    /// Gateway forwards signed transactions straight to hogd scheduler.
    #[default]
    HogdScheduler,

    /// Gateway forwards signed transactions to chuckd, which relays them to hogd.
    ChuckdCoordinator,
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
/// `submit_unvetted`) and `getStorageAt` (packing item reads into
/// `dbgateway_get_items`); see `GatewayConfig::batch` and
/// `GatewayConfig::storage` for the per-RPC tuning slots.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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
/// A single task serializes backend `schedcoord_get_tip` calls. While
/// one call is in flight, subsequent `chainTip` requests accumulate in
/// a waiter list; when the call returns, every waiter receives the
/// same `BlockTip` via fanout. Linearizable: each waiter's result
/// comes from an RPC initiated strictly after they arrived.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    /// Listen address for the JSON-RPC server.
    pub listen_address: String,

    /// Request timeout in milliseconds.
    pub request_timeout_ms: u64,

    /// Maximum simultaneous TCP/WebSocket connections.
    pub max_connections: u32,

    /// Backend used for `groundhog_sendRawTransaction`.
    pub submission_backend: SubmissionBackend,

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

    /// x23 RPC address for hogd `DbGateway1` (latest-snapshot reads).
    ///
    /// Serves `groundhog_getStorageAt`. Optional: when absent the
    /// gateway starts in degraded mode and `getStorageAt` returns
    /// `BackendUnavailable`.
    pub db_gateway_address: Option<String>,

    /// x23 RPC address for hogd `ViewGateway1` (sandboxed contract simulation).
    ///
    /// Serves `groundhog_view`. Optional: when absent the gateway returns
    /// `NotImplemented` for view execution. When configured, connection or
    /// startup-probe failures are hard startup errors.
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

    /// x23 RPC address for coordinator (for block/CV queries).
    ///
    /// Also used for tx submission when `submission_backend=chuckd_coordinator`.
    /// Optional if block data comes from indexer and tx submission goes direct
    /// to hogd scheduler. See `docs/gateway/system-design.md`.
    pub coordinator_address: Option<String>,

    /// Per-page cap on `getEvents` responses; requests with `limit`
    /// above this are rejected as `InvalidParam`.
    pub max_events_per_page: u32,

    /// Maximum length of the opaque `getEvents` pagination cursor;
    /// longer cursors are rejected as `InvalidParam`.
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
            listen_address: "0.0.0.0:8545".into(),
            request_timeout_ms: 60_000,
            max_connections: 2000,
            submission_backend: SubmissionBackend::HogdScheduler,
            scheduler_address: "127.0.0.1:9000".into(),
            indexer_address: Some("127.0.0.1:9001".into()),
            db_gateway_address: None,
            view_gateway_address: None,
            view_timeout_ms: 30_000,
            hogd_protocol_override: None,
            max_view_calldata_bytes: 64 * 1024,
            view_gas_limit: groundhog_json_rpc_server::DEFAULT_VIEW_GAS_LIMIT,
            coordinator_address: None,
            max_events_per_page: 200,
            max_cursor_bytes: 256,
            batch: AdaptiveBatchConfig::default(),
            storage: AdaptiveBatchConfig::default(),
            tip: TipConfig::default(),
        }
    }
}

impl GatewayConfig {
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
        let defaults = GatewayConfig::default();
        let scheduler_address = manifest.endpoints.scheduler.to_string();
        let indexer_address = manifest.endpoints.indexer.map(|addr| addr.to_string());
        let db_gateway_address = manifest.endpoints.db_gateway.map(|addr| addr.to_string());
        let view_gateway_address = manifest.endpoints.view_gateway.map(|addr| addr.to_string());
        let hogd_protocol_override = Some(manifest.protocol.clone());

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

        self.scheduler_address = scheduler_address;
        self.indexer_address = indexer_address;
        self.db_gateway_address = db_gateway_address;
        self.view_gateway_address = view_gateway_address;
        self.hogd_protocol_override = hogd_protocol_override;
        Ok(())
    }
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

    /// The config file already set a backend field to a conflicting value.
    #[error("runtime manifest conflicts with configured field `{field}`")]
    TomlConflict {
        /// Conflicting config field.
        field: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::{hex::Hex0x, runtime_manifest::RuntimeManifest};
    use groundhog_xdr::types::SHA256Hash;
    use serde_json::json;

    use super::{GatewayConfig, RuntimeManifestConfigError};

    #[test]
    fn runtime_manifest_overlays_gateway_backend_endpoints() {
        let mut config = GatewayConfig::default();
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
            "indexer": "127.0.0.1:1001",
            "dbGateway": "127.0.0.1:1002",
            "viewGateway": "127.0.0.1:1003",
        }));

        config
            .apply_runtime_manifest(&manifest)
            .expect("bug: manifest should configure gateway");

        assert_eq!(config.scheduler_address, "127.0.0.1:1000");
        assert_eq!(config.indexer_address.as_deref(), Some("127.0.0.1:1001"));
        assert_eq!(config.db_gateway_address.as_deref(), Some("127.0.0.1:1002"));
        assert_eq!(
            config.view_gateway_address.as_deref(),
            Some("127.0.0.1:1003")
        );
        assert_eq!(config.hogd_protocol_override.as_deref(), Some("hogp/test"));
    }

    #[test]
    fn runtime_manifest_can_omit_indexer_for_gateway() {
        let mut config = GatewayConfig::default();
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
        }));

        config
            .apply_runtime_manifest(&manifest)
            .expect("bug: scheduler-only manifest should configure gateway");

        assert_eq!(config.scheduler_address, "127.0.0.1:1000");
        assert_eq!(config.indexer_address, None);
        assert_eq!(config.db_gateway_address, None);
        assert_eq!(config.view_gateway_address, None);
        assert_eq!(config.hogd_protocol_override.as_deref(), Some("hogp/test"));
    }

    #[test]
    fn runtime_manifest_rejects_unsupported_schema_version() {
        let mut config = GatewayConfig::default();
        let mut manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
        }));
        manifest.schema_version = 2;

        let err = config
            .apply_runtime_manifest(&manifest)
            .expect_err("bug: unsupported schema should fail");

        assert_eq!(
            err,
            RuntimeManifestConfigError::UnsupportedSchemaVersion { actual: 2 }
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
            scheduler_address: "127.0.0.1:1000".into(),
            ..GatewayConfig::default()
        };
        let manifest = manifest_with_endpoints(&json!({
            "scheduler": "127.0.0.1:1000",
        }));

        config
            .apply_runtime_manifest(&manifest)
            .expect("bug: matching scheduler should not conflict");

        assert_eq!(config.scheduler_address, "127.0.0.1:1000");
    }

    fn manifest_with_endpoints(endpoints: &serde_json::Value) -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": hash(0x42),
            "producer": {
                "binary": "hogd-dev",
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
                    "artifactHint": "./dev-wallet.wasm",
                    "deployEntrypoint": "0xadac498222148e4f",
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
