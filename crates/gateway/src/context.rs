use std::sync::Arc;

use groundhog_scheduler::sched_coord::SchedulerCoord_SchedulerCoord1_Client;
use rpc_driver::FixedSerialPool;
use tokio::sync::RwLock;

use crate::config::GatewayConfig;

/// Startup errors for the gateway context.
#[derive(Debug, thiserror::Error)]
pub enum GatewayStartupError {
    #[error("invalid scheduler address '{addr}': {source}")]
    BadSchedulerAddress {
        addr: String,
        source: std::net::AddrParseError,
    },

    #[error("failed to connect to scheduler at {addr}: {source}")]
    SchedulerConnect {
        addr: String,
        source: rpc_driver::RpcError,
    },
}

/// Shared state injected into every handler.
///
/// Backend connections are behind `RwLock<Option<...>>` to support
/// lazy connection and reconnection. Handlers clone the client out of
/// the lock before making RPC calls, so the read guard is not held
/// across `.await`.
pub struct GatewayContext {
    /// x23 RPC client to hogd scheduler.
    /// Used for: `submit_unvetted`, `get_tip`, `lookup_transactions`, `size`.
    ///
    /// Even when tx submission is routed through chuckd, read paths still use
    /// the scheduler directly.
    pub scheduler: RwLock<Option<SchedulerCoord_SchedulerCoord1_Client>>,

    /// x23 RPC client to hogd indexer.
    /// Used for: `get_transaction_info`, `get_contract_events`.
    /// Not yet available in Rust — blocked on indexer Rust client crate.
    // TODO: pub indexer: RwLock<Option<IndexerClient>>,

    /// x23 RPC client to coordinator storage (optional).
    /// Used for: block/CV queries and optional tx submission relay path.
    // TODO: define the concrete chuckd submit/query client contract. protocol-rs
    // currently gives us a generated scheduler client, but not an equivalent
    // x23 unvetted-submit path into chuckd.
    // pub coordinator: Option<...>,

    /// Prometheus metrics for the gateway.
    /// Phase 4: per-method histograms, counters, backend latency.
    // TODO: pub metrics: Arc<GatewayMetrics>,

    /// Server configuration (timeouts, limits, backend addresses).
    pub config: Arc<GatewayConfig>,
}

impl GatewayContext {
    /// Create a new gateway context from configuration.
    ///
    /// Attempts to connect to the scheduler. If the scheduler is
    /// unreachable, starts in degraded mode (scheduler = None).
    ///
    /// TODO: if config.submission_backend == ChuckdCoordinator,
    /// require coordinator_address to be set.
    pub async fn new(config: GatewayConfig) -> Result<Self, GatewayStartupError> {
        let config = Arc::new(config);

        // Try to connect to scheduler; log and continue if unreachable.
        let scheduler = match connect_scheduler(&config.scheduler_address).await {
            Ok(client) => {
                tracing::info!(addr = %config.scheduler_address, "connected to scheduler");
                Some(client)
            }
            Err(e) => {
                tracing::warn!(
                    addr = %config.scheduler_address,
                    error = %e,
                    "scheduler unreachable, starting in degraded mode"
                );
                None
            }
        };

        Ok(Self {
            scheduler: RwLock::new(scheduler),
            config,
        })
    }
}

async fn connect_scheduler(
    addr: &str,
) -> Result<SchedulerCoord_SchedulerCoord1_Client, GatewayStartupError> {
    let socket_addr: std::net::SocketAddr =
        addr.parse()
            .map_err(|e| GatewayStartupError::BadSchedulerAddress {
                addr: addr.to_string(),
                source: e,
            })?;
    let pool = Arc::new(
        FixedSerialPool::connect(&socket_addr, 4)
            .await
            .map_err(|e| GatewayStartupError::SchedulerConnect {
                addr: addr.to_string(),
                source: e,
            })?,
    );
    Ok(SchedulerCoord_SchedulerCoord1_Client::new(pool))
}
