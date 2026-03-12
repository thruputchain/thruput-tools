use std::sync::Arc;

use crate::config::GatewayConfig;

/// Shared state injected into every handler.
///
/// All backend connections are behind `Arc` for cheap cloning into
/// spawned handler tasks.
///
/// Backend client fields are commented out until `rpc_driver` types
/// are available from the private registry.
pub struct GatewayContext {
    /// x23 RPC client to hogd scheduler.
    /// Used for: `submit_unvetted`, `get_tip`, `lookup_transactions`, `size`.
    // TODO: uncomment when rpc_driver types available
    // pub scheduler: Arc<SchedulerClient>,

    /// x23 RPC client to hogd indexer.
    /// Used for: `get_transaction_info`, `get_contract_events`.
    // TODO: uncomment when rpc_driver types available
    // pub indexer: Arc<IndexerClient>,

    /// x23 RPC client to coordinator storage (optional).
    /// Used for: block/CV queries — pending data source decision
    /// (see `docs/system-design.md` question #1).
    // TODO: uncomment when rpc_driver types available + block data source decided
    // pub coordinator: Option<Arc<CoordinatorClient>>,

    /// Prometheus metrics for the gateway.
    // TODO: define GatewayMetrics struct (histograms, counters per method)
    // pub metrics: Arc<GatewayMetrics>,

    /// Server configuration (timeouts, limits, backend addresses).
    pub config: Arc<GatewayConfig>,
}

impl GatewayContext {
    /// Create a new gateway context from configuration.
    ///
    /// Once x23 clients are available, this will establish connections
    /// to scheduler and indexer backends and fail if required backends
    /// are unreachable.
    pub fn new(config: GatewayConfig) -> Result<Self, Box<dyn std::error::Error>> {
        // TODO: create x23 RPC clients from config addresses
        // let scheduler = SchedulerClient::connect(&config.scheduler_address).await?;
        // let indexer = IndexerClient::connect(&config.indexer_address).await?;
        // let coordinator = match &config.coordinator_address {
        //     Some(addr) => Some(CoordinatorClient::connect(addr).await?),
        //     None => None,
        // };

        Ok(Self {
            config: Arc::new(config),
        })
    }
}
