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
}

impl Default for RpcServerConfig {
    fn default() -> Self {
        Self {
            max_events_per_page: 200,
            max_cursor_bytes: 256,
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
}
