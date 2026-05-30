/// Default gas budget for backend `groundhog_view` execution.
pub const DEFAULT_VIEW_GAS_LIMIT: u64 = 10_000_000;

/// Server-wide bounds on JSON-RPC requests.  The shared dispatcher
/// rejects requests that exceed these before they reach a backend.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RpcServerConfig {
    /// Per-page cap on `getEvents` responses; requests with `limit`
    /// above this are rejected as `InvalidParam`.
    pub max_events_per_page: u32,

    /// Maximum length of the opaque `getEvents` pagination cursor;
    /// longer cursors are rejected as `InvalidParam`.
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
}

impl Default for RpcServerConfig {
    fn default() -> Self {
        Self {
            max_events_per_page: 200,
            max_cursor_bytes: 256,
            max_view_calldata_bytes: 64 * 1024,
            view_gas_limit: DEFAULT_VIEW_GAS_LIMIT,
        }
    }
}

impl RpcServerConfig {
    /// Override the per-page event limit.
    #[must_use]
    pub fn with_max_events_per_page(mut self, max: u32) -> Self {
        self.max_events_per_page = max;
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
}
