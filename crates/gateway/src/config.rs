use serde::Deserialize;

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
    pub indexer_address: String,

    /// x23 RPC address for hogd `DbGateway1` (latest-snapshot reads).
    ///
    /// Serves `groundhog_getStorageAt`. Optional: when absent the
    /// gateway starts in degraded mode and `getStorageAt` returns
    /// `BackendUnavailable`.
    pub db_gateway_address: Option<String>,

    /// x23 RPC address for hogd `ViewGateway1` (read-only contract execution).
    ///
    /// Serves `groundhog_view`. Optional: when absent the gateway returns
    /// `NotImplemented` for view execution.
    pub view_gateway_address: Option<String>,

    /// Maximum time to wait for one `ViewGateway` execution RPC.
    pub view_timeout_ms: u64,

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
            indexer_address: "127.0.0.1:9001".into(),
            db_gateway_address: None,
            view_gateway_address: None,
            view_timeout_ms: 30_000,
            max_view_calldata_bytes: 64 * 1024,
            view_gas_limit: groundhog_rpc_server::DEFAULT_VIEW_GAS_LIMIT,
            coordinator_address: None,
            max_events_per_page: 200,
            max_cursor_bytes: 256,
            batch: AdaptiveBatchConfig::default(),
            storage: AdaptiveBatchConfig::default(),
            tip: TipConfig::default(),
        }
    }
}
