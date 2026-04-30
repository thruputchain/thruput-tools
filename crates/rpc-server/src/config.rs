/// Shared JSON-RPC server configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RpcServerConfig {
    /// Maximum number of events the server will return per `getEvents`
    /// page.  The indexer also caps server-side; this is a defensive
    /// pre-XDR check.  See `IndexDB::clamp_events_limit` on the
    /// hogd side (currently 200).
    pub max_events_per_page: u32,

    /// Maximum byte length for a `PaginationCursor` value accepted by
    /// `getEvents`.  Mirrors the hogd-internal `kMaxCursorBytes`
    /// (currently 256).  Pre-XDR `DoS` guard against absurd inputs.
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
