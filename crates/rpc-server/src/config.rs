/// Shared JSON-RPC server configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RpcServerConfig {
    /// Maximum inclusive height range accepted by `getContractEvents`.
    pub max_event_height_range: u64,
}

impl Default for RpcServerConfig {
    fn default() -> Self {
        Self {
            // Keep in sync with the production gateway default unless a
            // backend explicitly chooses a different bound.
            max_event_height_range: 1000,
        }
    }
}

impl RpcServerConfig {
    /// Override the inclusive height range cap used by `getContractEvents`.
    #[must_use]
    pub fn with_max_event_height_range(mut self, max: u64) -> Self {
        self.max_event_height_range = max;
        self
    }
}
