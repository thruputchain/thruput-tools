/// Default gas budget for backend `groundhog_view` execution.
pub const DEFAULT_VIEW_GAS_LIMIT: u64 = 10_000_000;

/// Server-wide bounds on JSON-RPC requests.  The shared dispatcher
/// rejects requests that exceed these before they reach a backend.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RpcServerConfig {
    /// Optional end-to-end deadline for one public JSON-RPC method.
    ///
    /// The deadline starts before shared validation and is passed explicitly to
    /// backend planes. `None` preserves the embedding application's own policy.
    pub request_timeout_ms: Option<u64>,

    /// Per-page cap on `getEvents` responses; requests with `limit`
    /// above this are rejected as `InvalidParam`.
    pub max_events_per_page: u32,

    /// Per-page cap on `getTransactionsByAddress` responses; requests
    /// with `limit` above this are rejected as `InvalidParam`.
    pub max_transactions_per_page: u32,

    /// Maximum length of the opaque pagination cursor on paginated
    /// methods; longer cursors are rejected as `InvalidParam`.
    pub max_cursor_bytes: usize,

    /// Maximum `groundhog_view` calldata byte length; larger
    /// requests are rejected as `InvalidParam` before backend dispatch.
    pub max_view_calldata_bytes: usize,

    /// Gas budget assigned to backend `groundhog_view` execution.
    ///
    /// The shared server validates JSON request shape; concrete backends
    /// pass this cap into the hogd view invocation once view execution is
    /// wired. Keep the default bounded so a view-capable backend cannot
    /// accidentally expose an unbounded root invocation.
    pub view_gas_limit: u64,

    /// How long a chain-tip reading is reused by the `sendRawTransaction`
    /// pre-checks before a submit asks the backend again. Bounds the
    /// extra backend load to one tip probe per window regardless of
    /// submission rate. Stale readings are safe: the tip only grows, so
    /// staleness can only under-detect expiry, and the network config the
    /// reading carries changes only at explicit upgrade blocks.
    pub tip_cache_ttl_ms: u64,

    /// Upper bound on a single chain-tip probe issued on behalf of the
    /// `sendRawTransaction` pre-checks, whether by a submit or by the
    /// background refresh. The pre-checks are advisory, so a slow or
    /// stalled backend must not delay submission past this bound.
    pub tip_probe_timeout_ms: u64,

    /// Interval of the background task that keeps the cached tip reading
    /// (and with it the network config) fresh on an idle server. Each tick
    /// asks the backend only if no probe ran within the interval, so a
    /// busy server pays for at most one probe per `tip_cache_ttl_ms` and
    /// an idle one for at most one per interval. Zero turns the task off;
    /// submits and `chainTip` calls alone then refresh the reading.
    pub tip_refresh_interval_ms: u64,
}

impl Default for RpcServerConfig {
    fn default() -> Self {
        Self {
            request_timeout_ms: None,
            max_events_per_page: 200,
            max_transactions_per_page: 200,
            max_cursor_bytes: 256,
            max_view_calldata_bytes: 64 * 1024,
            view_gas_limit: DEFAULT_VIEW_GAS_LIMIT,
            tip_cache_ttl_ms: 3_000,
            tip_probe_timeout_ms: 2_000,
            tip_refresh_interval_ms: 5_000,
        }
    }
}

impl RpcServerConfig {
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

    /// Override how long the submit pre-checks reuse a chain-tip reading.
    #[must_use]
    pub fn with_tip_cache_ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.tip_cache_ttl_ms = ttl_ms;
        self
    }

    /// Override the pre-checks' per-probe timeout.
    #[must_use]
    pub fn with_tip_probe_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.tip_probe_timeout_ms = timeout_ms;
        self
    }

    /// Override the background tip refresh interval; zero turns it off.
    #[must_use]
    pub fn with_tip_refresh_interval_ms(mut self, interval_ms: u64) -> Self {
        self.tip_refresh_interval_ms = interval_ms;
        self
    }
}
