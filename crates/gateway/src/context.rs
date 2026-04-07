use std::sync::Arc;

use groundhog_indexer::IndexerBlockExplorer_IndexerBlockExplorer1_Client;
use groundhog_scheduler::sched_coord::SchedulerCoord_SchedulerCoord1_Client;
use rpc_driver::FixedSerialPool;
use tokio::sync::RwLock;

use crate::config::GatewayConfig;

/// Startup errors for the gateway context.
#[derive(Debug, thiserror::Error)]
pub enum GatewayStartupError {
    #[error("invalid address '{addr}': {source}")]
    BadAddress {
        addr: String,
        source: std::net::AddrParseError,
    },

    #[error("failed to connect to {service} at {addr}: {source}")]
    ConnectFailed {
        service: &'static str,
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
    pub indexer: RwLock<Option<IndexerBlockExplorer_IndexerBlockExplorer1_Client>>,

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
    /// Attempts to connect to scheduler and indexer. If either is
    /// unreachable, starts in degraded mode (that backend = None).
    ///
    /// TODO: if config.submission_backend == ChuckdCoordinator,
    /// require coordinator_address to be set.
    pub async fn new(config: GatewayConfig) -> Result<Self, GatewayStartupError> {
        let config = Arc::new(config);

        let scheduler = try_connect("scheduler", &config.scheduler_address, 4).await?;

        let indexer = try_connect("indexer", &config.indexer_address, 4).await?;

        Ok(Self {
            scheduler: RwLock::new(scheduler),
            indexer: RwLock::new(indexer),
            config,
        })
    }
}

/// Try to connect to a backend.
///
/// Fails hard on configuration errors (bad address). Tolerates transient
/// connection failures (returns None, logs warning).
async fn try_connect<C: FromPool>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
) -> Result<Option<C>, GatewayStartupError> {
    match connect(service, addr, pool_size).await {
        Ok(client) => {
            tracing::info!(addr, service, "connected");
            Ok(Some(client))
        }
        Err(e @ GatewayStartupError::BadAddress { .. }) => Err(e),
        Err(e) => {
            tracing::warn!(addr, service, error = %e, "unreachable, starting in degraded mode");
            Ok(None)
        }
    }
}

async fn connect<C: FromPool>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
) -> Result<C, GatewayStartupError> {
    let socket_addr: std::net::SocketAddr =
        addr.parse().map_err(|e| GatewayStartupError::BadAddress {
            addr: addr.to_string(),
            source: e,
        })?;
    let pool = Arc::new(
        FixedSerialPool::connect(&socket_addr, pool_size)
            .await
            .map_err(|e| GatewayStartupError::ConnectFailed {
                service,
                addr: addr.to_string(),
                source: e,
            })?,
    );
    Ok(C::from_pool(pool))
}

/// Trait to construct an RPC client from a connection pool.
trait FromPool {
    fn from_pool(pool: Arc<FixedSerialPool>) -> Self;
}

impl FromPool for SchedulerCoord_SchedulerCoord1_Client {
    fn from_pool(pool: Arc<FixedSerialPool>) -> Self {
        Self::new(pool)
    }
}

impl FromPool for IndexerBlockExplorer_IndexerBlockExplorer1_Client {
    fn from_pool(pool: Arc<FixedSerialPool>) -> Self {
        Self::new(pool)
    }
}
