/// Default gas budget for backend `groundhog_view` execution.
pub const DEFAULT_VIEW_GAS_LIMIT: u64 = 10_000_000;

/// Operator policy for one shared-snapshot request, not consensus parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewBatchConfig {
    /// Maximum number of independent calls in a group.
    pub max_calls: u32,
    /// Sum of calldata bytes across calls.
    pub max_calldata_bytes: u32,
    /// Sum of declared gas limits in each attempt.
    pub max_gas: u64,
    /// Encoded backend response, including events; the JSON response cap also applies.
    pub max_response_bytes: u32,
}

impl Default for ViewBatchConfig {
    fn default() -> Self {
        Self {
            max_calls: 16,
            max_calldata_bytes: 65_536,
            max_gas: 160_000_000,
            max_response_bytes: 1_048_576,
        }
    }
}

impl ViewBatchConfig {
    /// Describe the policy advertised by the public endpoint.
    pub const fn server_limits(
        self,
        timeout_ms: u64,
    ) -> thruput_json_rpc_types::server_limits::ViewBatchLimits {
        thruput_json_rpc_types::server_limits::ViewBatchLimits {
            max_calls: self.max_calls,
            max_calldata_bytes: self.max_calldata_bytes,
            max_gas: self.max_gas,
            max_response_bytes: self.max_response_bytes,
            timeout_ms,
        }
    }
}

/// Server-wide bounds on JSON-RPC requests.  The shared dispatcher
/// rejects requests that exceed these before they reach a backend.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RpcServerConfig {
    /// Aggregate view-batch policy. Per-call view limits also apply.
    pub view_batch: ViewBatchConfig,
    /// Maximum shared-snapshot response wait, measured from request start.
    pub view_timeout_ms: u64,
    /// Optional end-to-end deadline for one public JSON-RPC method.
    ///
    /// The deadline starts before shared validation and is passed explicitly to
    /// backend planes. `None` preserves the embedding application's own policy.
    pub request_timeout_ms: Option<u64>,

    /// Per-page cap on `getEvents` responses; requests with `limit`
    /// above this are rejected as `InvalidParam`.
    pub max_events_per_page: u32,

    /// Per-page cap on transaction-reference queries. The effective maximum
    /// is at most the indexer's 200; larger request limits are invalid params.
    pub max_transactions_per_page: u32,

    /// Maximum length of the opaque pagination cursor on paginated
    /// methods; longer cursors are rejected as `InvalidParam`.
    pub max_cursor_bytes: usize,

    /// Maximum `groundhog_view` calldata byte length; larger
    /// requests are rejected as `InvalidParam` before backend dispatch.
    pub max_view_calldata_bytes: usize,

    /// Gas budget assigned to backend `groundhog_view` execution when the
    /// request names no `gasLimit`, and the ceiling a requested `gasLimit`
    /// may not exceed.
    ///
    /// The shared server validates JSON request shape; concrete backends
    /// pass this cap into the hogd view invocation once view execution is
    /// wired. Keep the default bounded so a view-capable backend cannot
    /// accidentally expose an unbounded root invocation.
    pub view_gas_limit: u64,

    /// Upper bound on a single chain-tip probe issued on behalf of the
    /// `sendRawTransaction` pre-checks, whether by a submit or by the
    /// background refresh. The pre-checks are advisory, so a slow or
    /// stalled backend must not delay submission past this bound.
    pub tip_probe_timeout_ms: u64,

    /// How often the background task refreshes the chain-tip reading the
    /// `sendRawTransaction` pre-checks read. A reading stays usable for
    /// this plus `tip_probe_timeout_ms`, so a refresh always lands first
    /// and a submit never fetches on its own latency path. Zero disables
    /// both the task and reuse. Staleness is safe: the tip only grows, and
    /// the config it carries changes only at upgrade blocks.
    pub tip_refresh_interval_ms: u64,
}

impl Default for RpcServerConfig {
    fn default() -> Self {
        Self {
            view_batch: ViewBatchConfig::default(),
            view_timeout_ms: 30_000,
            request_timeout_ms: None,
            max_events_per_page: 200,
            max_transactions_per_page: 200,
            max_cursor_bytes: 256,
            max_view_calldata_bytes: 64 * 1024,
            view_gas_limit: DEFAULT_VIEW_GAS_LIMIT,
            tip_probe_timeout_ms: 2_000,
            tip_refresh_interval_ms: 5_000,
        }
    }
}

impl RpcServerConfig {
    /// Set the endpoint's aggregate shared-read policy.
    #[must_use]
    pub const fn with_view_batch(mut self, limits: ViewBatchConfig) -> Self {
        self.view_batch = limits;
        self
    }

    #[must_use]
    pub const fn with_view_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.view_timeout_ms = timeout_ms;
        self
    }
    /// Set an end-to-end timeout for every public JSON-RPC request.
    #[must_use]
    pub fn with_request_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.request_timeout_ms = Some(timeout_ms.max(1));
        self
    }

    /// Override the per-page event limit.
    #[must_use]
    pub fn with_max_events_per_page(mut self, max: u32) -> Self {
        self.max_events_per_page = max;
        self
    }

    /// Override the per-page transaction-ref limit.
    #[must_use]
    pub fn with_max_transactions_per_page(mut self, max: u32) -> Self {
        self.max_transactions_per_page = max;
        self
    }

    /// Effective event page cap, bounded by the indexer's limit.
    pub fn event_page_cap(&self) -> u32 {
        self.max_events_per_page.min(200)
    }

    /// Effective transaction-reference page cap, bounded by the indexer's limit.
    pub fn transaction_page_cap(&self) -> u32 {
        self.max_transactions_per_page.min(200)
    }

    /// Override the cursor byte length cap.
    #[must_use]
    pub fn with_max_cursor_bytes(mut self, max: usize) -> Self {
        self.max_cursor_bytes = max;
        self
    }

    /// Override the view calldata byte length cap.
    #[must_use]
    pub fn with_max_view_calldata_bytes(mut self, max: usize) -> Self {
        self.max_view_calldata_bytes = max;
        self
    }

    /// Override the view execution gas limit.
    #[must_use]
    pub fn with_view_gas_limit(mut self, gas_limit: u64) -> Self {
        self.view_gas_limit = gas_limit;
        self
    }

    /// Override the pre-checks' per-probe timeout.
    #[must_use]
    pub fn with_tip_probe_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.tip_probe_timeout_ms = timeout_ms;
        self
    }

    /// Override how often the background task refreshes the tip reading.
    /// The reuse window follows it, plus `tip_probe_timeout_ms`.
    #[must_use]
    pub fn with_tip_refresh_interval_ms(mut self, interval_ms: u64) -> Self {
        self.tip_refresh_interval_ms = interval_ms;
        self
    }
}
