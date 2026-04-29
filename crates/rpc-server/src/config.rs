/// Shared JSON-RPC server configuration.
#[derive(Debug, Clone)]
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
