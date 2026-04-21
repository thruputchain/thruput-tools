use std::sync::Arc;

use groundhog_indexer::IndexerBlockExplorer_IndexerBlockExplorer1_Client;
use groundhog_scheduler::sched_coord::SchedulerCoord_SchedulerCoord1_Client;
use rpc_driver::FixedSerialPool;
use tokio::sync::RwLock;

use crate::config::GatewayConfig;
use crate::submitter::{BatchSubmitter, SchedulerSink, SubmitSink};
use crate::tip_coalescer::{SchedulerTipSink, TipCoalescer, TipSink};

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
/// Backend connections are behind `Arc<RwLock<Option<...>>>` to
/// support lazy connection and reconnection, and to let long-lived
/// submitter/coalescer tasks observe the same state. Handlers clone
/// the client out of the lock before making RPC calls, so the read
/// guard is not held across `.await`.
pub struct GatewayContext {
    /// x23 RPC client to hogd scheduler.
    /// Used for: `submit_unvetted`, `get_tip`, `lookup_transactions`, `size`.
    ///
    /// Even when tx submission is routed through chuckd, read paths still use
    /// the scheduler directly.
    pub scheduler: Arc<RwLock<Option<SchedulerCoord_SchedulerCoord1_Client>>>,

    /// x23 RPC client to hogd indexer.
    /// Used for: `get_transaction_info`, `get_contract_events`.
    pub indexer: Arc<RwLock<Option<IndexerBlockExplorer_IndexerBlockExplorer1_Client>>>,

    /// Adaptive batching submitter for `sendRawTransaction`. Accumulates
    /// incoming client submissions and packs them into `submit_unvetted`
    /// RPCs to amortize per-call overhead at high load.
    pub batch_submitter: BatchSubmitter,

    /// Request coalescer for `chainTip`. One in-flight `get_tip` RPC
    /// can satisfy many concurrent client requests.
    pub tip_coalescer: TipCoalescer,

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
    /// TODO: if `config.submission_backend` == `ChuckdCoordinator`,
    /// require `coordinator_address` to be set.
    pub async fn new(config: GatewayConfig) -> Result<Self, GatewayStartupError> {
        let config = Arc::new(config);

        // Each x23 connection serves one in-flight RPC at a time (no HTTP/2-style
        // multiplexing).  The scheduler sees concurrent calls from both the
        // batch submitter (up to `max_inflight_batches` per shard × shards)
        // and the tip coalescer (up to `max_inflight_tips`), so size the pool
        // to cover the sum plus a small slack for unrelated RPCs on the same
        // client (lookup_transactions, pending_transaction_count).
        let sched_pool_size = scheduler_pool_size(&config);
        let scheduler = Arc::new(RwLock::new(
            try_connect("scheduler", &config.scheduler_address, sched_pool_size).await?,
        ));

        let indexer = Arc::new(RwLock::new(
            try_connect("indexer", &config.indexer_address, 4).await?,
        ));

        let submit_sink: Arc<dyn SubmitSink> = Arc::new(SchedulerSink::new(scheduler.clone()));
        let batch_submitter = BatchSubmitter::spawn(&config.batch, &submit_sink);

        let tip_sink: Arc<dyn TipSink> = Arc::new(SchedulerTipSink::new(scheduler.clone()));
        let tip_coalescer = TipCoalescer::spawn(&config.tip, &tip_sink);

        Ok(Self {
            scheduler,
            indexer,
            batch_submitter,
            tip_coalescer,
            config,
        })
    }
}

/// Pick a scheduler pool size that covers every concurrent RPC the gateway
/// may have in flight against it.  x23 serializes one RPC per connection.
/// Slack covers ad-hoc calls (`lookup_transactions`, `pending_transaction_count`).
fn scheduler_pool_size(config: &GatewayConfig) -> usize {
    const SLACK: usize = 4;
    let batcher = config
        .batch
        .shards
        .max(1)
        .saturating_mul(config.batch.max_inflight_batches.max(1) as usize);
    let tip = config.tip.max_inflight_tips.max(1) as usize;
    batcher.saturating_add(tip).saturating_add(SLACK)
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
