use serde::Deserialize;

/// Gateway configuration, loaded from TOML.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    /// Listen address for the JSON-RPC server.
    pub listen_address: String,

    /// Request timeout in milliseconds.
    pub request_timeout_ms: u64,

    /// Maximum concurrent in-flight requests.
    pub max_in_flight_requests: u32,

    /// x23 RPC address for hogd scheduler.
    pub scheduler_address: String,

    /// x23 RPC address for hogd indexer.
    pub indexer_address: String,

    /// x23 RPC address for coordinator (for block/CV queries).
    /// Optional — may not be needed if block data comes from indexer.
    /// See `docs/system-design.md` question #1.
    pub coordinator_address: Option<String>,

    /// Maximum height range for `getContractEvents` queries.
    /// Requests exceeding this return `RANGE_TOO_LARGE` (-32011).
    /// Hogd has no built-in limits, so the gateway must enforce bounds.
    pub max_event_height_range: u64,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            listen_address: "0.0.0.0:8545".into(),
            request_timeout_ms: 60_000,
            max_in_flight_requests: 2000,
            scheduler_address: "127.0.0.1:9000".into(),
            indexer_address: "127.0.0.1:9001".into(),
            coordinator_address: None,
            max_event_height_range: 1000,
        }
    }
}
