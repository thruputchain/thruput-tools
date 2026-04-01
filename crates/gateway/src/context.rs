use std::sync::Arc;

use crate::config::{GatewayConfig, SubmissionBackend};

/// Shared state injected into every handler.
///
/// All backend connections are behind `Arc` for cheap cloning into
/// spawned handler tasks.
///
/// This module is intentionally scaffold-oriented while the gateway shape and
/// backend contracts are being worked out.
pub struct GatewayContext {
    /// x23 RPC client to hogd scheduler.
    /// Used for: `submit_unvetted`, `get_tip`, `lookup_transactions`, `size`.
    ///
    /// Even when tx submission is routed through chuckd, read paths still use
    /// the scheduler directly.
    // TODO: wire the generated protocol-rs client:
    // pub scheduler: Arc<groundhog_scheduler::SchedulerCoord_SchedulerCoord1_Client>,

    /// x23 RPC client to hogd indexer.
    /// Used for: `get_transaction_info`, `get_contract_events`.
    // TODO: uncomment when rpc_driver types available
    // pub indexer: Arc<IndexerClient>,

    /// x23 RPC client to coordinator storage (optional).
    /// Used for: block/CV queries and optional tx submission relay path.
    // TODO: define the concrete chuckd submit/query client contract. protocol-rs
    // currently gives us a generated scheduler client, but not an equivalent
    // x23 unvetted-submit path into chuckd.
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
    /// for the selected submission topology are unreachable.
    pub fn new(config: GatewayConfig) -> Result<Self, Box<dyn std::error::Error>> {
        // TODO: create x23 RPC clients from config addresses
        // let scheduler =
        //     groundhog_scheduler::SchedulerCoord_SchedulerCoord1_Client::new(...);
        // let indexer = IndexerClient::connect(&config.indexer_address).await?;
        // let coordinator = match &config.coordinator_address {
        //     Some(addr) => Some(CoordinatorClient::connect(addr).await?),
        //     None => None,
        // };
        //
        // TODO: if config.submission_backend == SubmissionBackend::ChuckdCoordinator,
        // require coordinator_address to be set.
        let _ = SubmissionBackend::HogdScheduler;

        Ok(Self {
            config: Arc::new(config),
        })
    }
}
