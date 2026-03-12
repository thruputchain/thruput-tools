use std::sync::Arc;

use crate::config::GatewayConfig;

/// Shared state injected into every handler.
/// All backend connections are behind Arc for cheap cloning
/// into spawned handler tasks.
pub struct GatewayContext {
    /// x23 RPC client to hogd scheduler.
    /// Used for: submit_unvetted, get_tip, find_tx, size.
    // pub scheduler: Arc<SchedulerClient>,

    /// x23 RPC client to hogd indexer.
    /// Used for: get_transaction_info, get_contract_events.
    // pub indexer: Arc<IndexerClient>,

    /// x23 RPC client to coordinator storage.
    /// Used for: lookup_cv_by_height, lookup_cv.
    // pub coordinator: Option<Arc<CoordinatorClient>>,

    /// Prometheus metrics registry.
    // pub metrics: Arc<GatewayMetrics>,

    /// Server configuration (timeouts, limits, etc.).
    pub config: Arc<GatewayConfig>,
}
