use std::sync::Arc;

use groundhog_db_gateway::DbGateway_DbGateway1_Client;
use groundhog_indexer::IndexerBlockExplorer_IndexerBlockExplorer1_Client;
use groundhog_scheduler::sched_coord::SchedulerCoord_SchedulerCoord1_Client;
use groundhog_x23_transport::{ConnectError, try_connect};
use tokio::sync::RwLock;

use crate::config::GatewayConfig;
use crate::storage_batcher::{DbGatewaySink, StorageReadBatcher, StorageReadSink};
use crate::submitter::{BatchSubmitter, SchedulerSink, SubmitSink};
use crate::tip_coalescer::{SchedulerTipSink, TipCoalescer, TipSink};

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
    /// Used for: `get_transaction_info`, `get_events` (paginated).
    pub indexer: Arc<RwLock<Option<IndexerBlockExplorer_IndexerBlockExplorer1_Client>>>,

    /// Adaptive batching submitter for `sendRawTransaction`. Accumulates
    /// incoming client submissions and packs them into `submit_unvetted`
    /// RPCs to amortize per-call overhead at high load.
    pub batch_submitter: BatchSubmitter,

    /// Adaptive batching reader for `getStorageAt`. Accumulates
    /// concurrent (`address`, `key`) reads and packs them into one
    /// `dbgateway_get_items` RPC per fired batch.
    pub storage_batcher: StorageReadBatcher,

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
    pub async fn new(config: GatewayConfig) -> Result<Self, ConnectError> {
        let config = Arc::new(config);

        // `FixedSerialPool` is 1 in-flight per connection by choice (FIFO
        // dispatch per connection, modeling x23 sync RPC; the x23 wire
        // protocol itself multiplexes via xid).  Size the pool to cover the
        // batch submitter (`max_inflight_batches` × shards) plus the tip
        // coalescer (`max_inflight_tips`) plus slack for ad-hoc RPCs.
        let sched_pool_size = scheduler_pool_size(&config);
        let scheduler = Arc::new(RwLock::new(
            try_connect(
                "scheduler",
                &config.scheduler_address,
                sched_pool_size,
                SchedulerCoord_SchedulerCoord1_Client::new,
            )
            .await?,
        ));

        let indexer = Arc::new(RwLock::new(
            try_connect(
                "indexer",
                &config.indexer_address,
                4,
                IndexerBlockExplorer_IndexerBlockExplorer1_Client::new,
            )
            .await?,
        ));

        let db_gateway = Arc::new(RwLock::new(match &config.db_gateway_address {
            Some(addr) => {
                try_connect("db_gateway", addr, 4, DbGateway_DbGateway1_Client::new).await?
            }
            None => None,
        }));

        let submit_sink: Arc<dyn SubmitSink> = Arc::new(SchedulerSink::new(scheduler.clone()));
        let batch_submitter = BatchSubmitter::spawn(&config.batch, &submit_sink);

        let storage_sink: Arc<dyn StorageReadSink> = Arc::new(DbGatewaySink::new(db_gateway));
        let storage_batcher = StorageReadBatcher::spawn(&config.storage, &storage_sink);

        let tip_sink: Arc<dyn TipSink> = Arc::new(SchedulerTipSink::new(scheduler.clone()));
        let tip_coalescer = TipCoalescer::spawn(&config.tip, &tip_sink);

        Ok(Self {
            scheduler,
            indexer,
            batch_submitter,
            storage_batcher,
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
