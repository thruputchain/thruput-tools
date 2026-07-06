use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use groundhog_db_gateway::{DbGateway_DbGateway1_Client, DbGatewayIntroOutcome};
use groundhog_indexer::{
    IndexerBlockExplorer_IndexerBlockExplorer1_Client, IndexerBlockExplorerIntroOutcome,
};
use groundhog_scheduler::sched_coord::SchedulerCoord_SchedulerCoord1_Client;
use groundhog_scp::network::Protocol;
use groundhog_view::{ViewGateway_ViewGateway1_Client, ViewGatewayIntroOutcome};
use groundhog_x23_transport::{ConnectError, try_connect};
use groundhog_xdr::PROTOCOL_VERSION;
use tokio::sync::RwLock;
use xdr_common::StringM;

use crate::config::{GatewayConfig, HealthBackend};
use crate::metrics::{BackendRequestOutcome, GatewayMetrics};
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

    /// x23 RPC client to hogd `ViewGateway`.
    /// Used for: `groundhog_view`.
    pub view_gateway: Arc<RwLock<Option<ViewGateway_ViewGateway1_Client>>>,

    /// x23 RPC client to hogd `DbGateway`.
    /// Shared by storage batching and readiness probes.
    pub db_gateway: Arc<RwLock<Option<DbGateway_DbGateway1_Client>>>,

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

    /// Prometheus metrics for gateway-owned and backend x23 observations.
    pub metrics: GatewayMetrics,
}

impl GatewayContext {
    /// Create a new gateway context from configuration.
    ///
    /// Connects to the scheduler and optional read/view backends. Optional
    /// indexer, `DbGateway`, and `ViewGateway` outages or intro timeouts degrade
    /// to `None`; reachable backends that reject the intro probe fail startup as
    /// configuration or protocol mismatches.
    ///
    /// TODO: if `config.submission_backend` == `ChuckdCoordinator`,
    /// require `coordinator_address` to be set.
    pub async fn new(config: GatewayConfig, metrics: GatewayMetrics) -> Result<Self, ConnectError> {
        let config = Arc::new(config);

        // `FixedSerialPool` is 1 in-flight per connection by choice (FIFO
        // dispatch per connection, modeling x23 sync RPC; the x23 wire
        // protocol itself multiplexes via xid).  Size the pool to cover the
        // batch submitter (`max_inflight_batches` × shards) plus the tip
        // coalescer (`max_inflight_tips`) plus slack for ad-hoc RPCs.
        let sched_pool_size = scheduler_pool_size(&config);
        let scheduler = Arc::new(RwLock::new(
            connect_optional_scheduler(&config.scheduler_address, sched_pool_size).await?,
        ));

        let protocol_override = config.hogd_protocol_override.as_deref();
        let startup_timeout = Duration::from_millis(config.view_timeout_ms);

        let indexer = Arc::new(RwLock::new(
            connect_optional_indexer(
                config.indexer_address.as_deref(),
                protocol_override,
                startup_timeout,
                Some(&metrics),
            )
            .await?,
        ));

        let db_gateway_pool = db_gateway_pool_size(&config);
        let db_gateway = Arc::new(RwLock::new(
            connect_optional_db_gateway(
                config.db_gateway_address.as_deref(),
                db_gateway_pool,
                protocol_override,
                startup_timeout,
                Some(&metrics),
            )
            .await?,
        ));

        let view_gateway = Arc::new(RwLock::new(
            connect_optional_view_gateway(
                config.view_gateway_address.as_deref(),
                protocol_override,
                startup_timeout,
                Some(&metrics),
            )
            .await?,
        ));

        let submit_sink: Arc<dyn SubmitSink> =
            Arc::new(SchedulerSink::new(scheduler.clone(), metrics.clone()));
        let batch_submitter = BatchSubmitter::spawn(&config.batch, &submit_sink, metrics.clone());

        let storage_sink: Arc<dyn StorageReadSink> =
            Arc::new(DbGatewaySink::new(db_gateway.clone(), metrics.clone()));
        let storage_batcher =
            StorageReadBatcher::spawn(&config.storage, &storage_sink, metrics.clone());

        let tip_sink: Arc<dyn TipSink> =
            Arc::new(SchedulerTipSink::new(scheduler.clone(), metrics.clone()));
        let tip_coalescer = TipCoalescer::spawn(&config.tip, &tip_sink, metrics.clone());

        Ok(Self {
            scheduler,
            indexer,
            batch_submitter,
            storage_batcher,
            tip_coalescer,
            view_gateway,
            db_gateway,
            config,
            metrics,
        })
    }
}

pub(crate) async fn connect_optional_scheduler(
    address: &str,
    pool_size: usize,
) -> Result<Option<SchedulerCoord_SchedulerCoord1_Client>, ConnectError> {
    try_connect(
        "scheduler",
        address,
        pool_size,
        SchedulerCoord_SchedulerCoord1_Client::new,
    )
    .await
}

pub(crate) async fn connect_optional_indexer(
    address: Option<&str>,
    protocol_override: Option<&str>,
    startup_timeout: Duration,
    metrics: Option<&GatewayMetrics>,
) -> Result<Option<IndexerBlockExplorer_IndexerBlockExplorer1_Client>, ConnectError> {
    let Some(address) = address else {
        return Ok(None);
    };

    let connect_started_at = Instant::now();
    let connect_result = tokio::time::timeout(
        startup_timeout,
        try_connect(
            "indexer",
            address,
            4,
            IndexerBlockExplorer_IndexerBlockExplorer1_Client::new,
        ),
    )
    .await;
    let Some(client) = (match connect_result {
        Err(_) => {
            tracing::warn!(
                addr = address,
                service = "indexer",
                timeout = ?startup_timeout,
                "optional backend connect timed out, starting in degraded mode"
            );
            return Ok(None);
        }
        Ok(result) => result?,
    }) else {
        return Ok(None);
    };
    let Some(intro_timeout) = startup_timeout.checked_sub(connect_started_at.elapsed()) else {
        tracing::warn!(
            addr = address,
            service = "indexer_block_explorer_intro",
            timeout = ?startup_timeout,
            "optional backend connect consumed startup timeout, starting in degraded mode"
        );
        return Ok(None);
    };

    let protocol_version = protocol_override.unwrap_or(PROTOCOL_VERSION);
    let protocol: Protocol = StringM(protocol_version.as_bytes().to_vec());
    let started_at = Instant::now();
    let intro_result = tokio::time::timeout(
        intro_timeout,
        client.indexer_block_explorer_intro(&protocol),
    )
    .await;
    if let Some(metrics) = metrics {
        let outcome = match &intro_result {
            Err(_) => BackendRequestOutcome::Timeout,
            Ok(Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)) => BackendRequestOutcome::Ok,
            Ok(Ok(_)) => BackendRequestOutcome::Rejected,
            Ok(Err(_)) => BackendRequestOutcome::Error,
        };
        metrics.observe_backend_request(
            HealthBackend::Indexer,
            "indexer_block_explorer_intro",
            outcome,
            started_at.elapsed(),
        );
    }
    let Ok(intro_outcome) = intro_result else {
        tracing::warn!(
            addr = address,
            service = "indexer_block_explorer_intro",
            timeout = ?startup_timeout,
            "optional backend intro timed out, starting in degraded mode"
        );
        return Ok(None);
    };

    match intro_outcome {
        Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess) => Ok(Some(client)),
        Ok(outcome) => Err(ConnectError::StartupRejected {
            service: "indexer",
            addr: address.to_string(),
            reason: indexer_intro_rejection_reason(outcome, protocol_version),
        }),
        Err(source) => {
            tracing::warn!(
                addr = address,
                service = "indexer_block_explorer_intro",
                error = %source,
                "optional backend intro failed, starting in degraded mode"
            );
            Ok(None)
        }
    }
}

pub(crate) async fn connect_optional_db_gateway(
    address: Option<&str>,
    pool_size: usize,
    protocol_override: Option<&str>,
    startup_timeout: Duration,
    metrics: Option<&GatewayMetrics>,
) -> Result<Option<DbGateway_DbGateway1_Client>, ConnectError> {
    let Some(address) = address else {
        return Ok(None);
    };

    let connect_started_at = Instant::now();
    let connect_result = tokio::time::timeout(
        startup_timeout,
        try_connect(
            "db_gateway",
            address,
            pool_size,
            DbGateway_DbGateway1_Client::new,
        ),
    )
    .await;
    let Some(client) = (match connect_result {
        Err(_) => {
            tracing::warn!(
                addr = address,
                service = "db_gateway",
                timeout = ?startup_timeout,
                "optional backend connect timed out, starting in degraded mode"
            );
            return Ok(None);
        }
        Ok(result) => result?,
    }) else {
        return Ok(None);
    };
    let Some(intro_timeout) = startup_timeout.checked_sub(connect_started_at.elapsed()) else {
        tracing::warn!(
            addr = address,
            service = "dbgateway_intro",
            timeout = ?startup_timeout,
            "optional backend connect consumed startup timeout, starting in degraded mode"
        );
        return Ok(None);
    };

    let protocol_version = protocol_override.unwrap_or(PROTOCOL_VERSION);
    let protocol: Protocol = StringM(protocol_version.as_bytes().to_vec());
    let started_at = Instant::now();
    let intro_result = tokio::time::timeout(intro_timeout, client.dbgateway_intro(&protocol)).await;
    if let Some(metrics) = metrics {
        let outcome = match &intro_result {
            Err(_) => BackendRequestOutcome::Timeout,
            Ok(Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)) => BackendRequestOutcome::Ok,
            Ok(Ok(_)) => BackendRequestOutcome::Rejected,
            Ok(Err(_)) => BackendRequestOutcome::Error,
        };
        metrics.observe_backend_request(
            HealthBackend::DbGateway,
            "dbgateway_intro",
            outcome,
            started_at.elapsed(),
        );
    }
    let Ok(intro_outcome) = intro_result else {
        tracing::warn!(
            addr = address,
            service = "dbgateway_intro",
            timeout = ?startup_timeout,
            "optional backend intro timed out, starting in degraded mode"
        );
        return Ok(None);
    };

    match intro_outcome {
        Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess) => Ok(Some(client)),
        Ok(outcome) => Err(ConnectError::StartupRejected {
            service: "db_gateway",
            addr: address.to_string(),
            reason: db_gateway_intro_rejection_reason(outcome, protocol_version),
        }),
        Err(source) => {
            tracing::warn!(
                addr = address,
                service = "dbgateway_intro",
                error = %source,
                "optional backend intro failed, starting in degraded mode"
            );
            Ok(None)
        }
    }
}

pub(crate) async fn connect_optional_view_gateway(
    address: Option<&str>,
    protocol_override: Option<&str>,
    startup_timeout: Duration,
    metrics: Option<&GatewayMetrics>,
) -> Result<Option<ViewGateway_ViewGateway1_Client>, ConnectError> {
    let Some(address) = address else {
        return Ok(None);
    };

    let connect_started_at = Instant::now();
    let connect_result = tokio::time::timeout(
        startup_timeout,
        try_connect(
            "view_gateway",
            address,
            view_gateway_pool_size(),
            ViewGateway_ViewGateway1_Client::new,
        ),
    )
    .await;
    let Some(client) = (match connect_result {
        Err(_) => {
            tracing::warn!(
                addr = address,
                service = "view_gateway",
                timeout = ?startup_timeout,
                "optional backend connect timed out, starting in degraded mode"
            );
            return Ok(None);
        }
        Ok(result) => result?,
    }) else {
        return Ok(None);
    };
    let Some(intro_timeout) = startup_timeout.checked_sub(connect_started_at.elapsed()) else {
        tracing::warn!(
            addr = address,
            service = "view_gateway_intro",
            timeout = ?startup_timeout,
            "optional backend connect consumed startup timeout, starting in degraded mode"
        );
        return Ok(None);
    };

    let protocol_version = protocol_override.unwrap_or(PROTOCOL_VERSION);
    let protocol: Protocol = StringM(protocol_version.as_bytes().to_vec());
    let started_at = Instant::now();
    let intro_result =
        tokio::time::timeout(intro_timeout, client.viewgateway_intro(&protocol)).await;
    if let Some(metrics) = metrics {
        let outcome = match &intro_result {
            Err(_) => BackendRequestOutcome::Timeout,
            Ok(Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)) => BackendRequestOutcome::Ok,
            Ok(Ok(_)) => BackendRequestOutcome::Rejected,
            Ok(Err(_)) => BackendRequestOutcome::Error,
        };
        metrics.observe_backend_request(
            HealthBackend::ViewGateway,
            "viewgateway_intro",
            outcome,
            started_at.elapsed(),
        );
    }
    let Ok(intro_outcome) = intro_result else {
        tracing::warn!(
            addr = address,
            service = "view_gateway_intro",
            timeout = ?startup_timeout,
            "optional backend intro timed out, starting in degraded mode"
        );
        return Ok(None);
    };

    match intro_outcome {
        Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess) => Ok(Some(client)),
        Ok(outcome) => Err(ConnectError::StartupRejected {
            service: "view_gateway",
            addr: address.to_string(),
            reason: view_gateway_intro_rejection_reason(outcome, protocol_version),
        }),
        Err(source) => {
            tracing::warn!(
                addr = address,
                service = "view_gateway_intro",
                error = %source,
                "optional backend intro failed, starting in degraded mode"
            );
            Ok(None)
        }
    }
}

fn indexer_intro_rejection_reason(
    outcome: IndexerBlockExplorerIntroOutcome,
    protocol_version: &str,
) -> String {
    if outcome == IndexerBlockExplorerIntroOutcome::IntroMismatchedProtocol {
        return format!(
            "IndexerBlockExplorer intro mismatch: gateway speaks protocol {protocol_version}, \
             but the backend rejected it ({outcome}); pass `--hogd-protocol`/\
             `hogd_protocol_override` for a matching backend protocol, or rebuild the gateway \
             against a matching groundhog-xdr"
        );
    }

    format!(
        "IndexerBlockExplorer intro rejected with outcome {outcome}; gateway speaks protocol \
         {protocol_version}"
    )
}

fn db_gateway_intro_rejection_reason(
    outcome: DbGatewayIntroOutcome,
    protocol_version: &str,
) -> String {
    if outcome == DbGatewayIntroOutcome::DbGatewayIntroMismatchedProtocol {
        return format!(
            "DbGateway intro mismatch: gateway speaks protocol {protocol_version}, \
             but the backend rejected it ({outcome}); pass `--hogd-protocol`/\
             `hogd_protocol_override` for a matching backend protocol, or rebuild the gateway \
             against a matching groundhog-xdr"
        );
    }

    format!(
        "DbGateway intro rejected with outcome {outcome}; gateway speaks protocol \
         {protocol_version}"
    )
}

fn view_gateway_intro_rejection_reason(
    outcome: ViewGatewayIntroOutcome,
    protocol_version: &str,
) -> String {
    if outcome == ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol {
        return format!(
            "ViewGateway intro mismatch: gateway speaks protocol {protocol_version}, \
             but the backend rejected it ({outcome}); pass `--hogd-protocol`/\
             `hogd_protocol_override` for a matching backend protocol, or rebuild the gateway \
             against a matching groundhog-xdr"
        );
    }

    format!(
        "ViewGateway intro rejected with outcome {outcome}; gateway speaks protocol \
         {protocol_version}"
    )
}

/// Pick a scheduler pool size that covers every concurrent RPC the gateway
/// may have in flight against it.  x23 serializes one RPC per connection.
/// Slack covers ad-hoc calls (`lookup_transactions`, `pending_transaction_count`).
pub(crate) fn scheduler_pool_size(config: &GatewayConfig) -> usize {
    const SLACK: usize = 4;
    let tip = config.tip.max_inflight_tips.max(1) as usize;
    config
        .batch
        .max_concurrent_rpcs()
        .saturating_add(tip)
        .saturating_add(SLACK)
}

/// Pick a `db_gateway` pool size that matches the storage batcher's own
/// in-flight cap, so the batcher's `Overloaded` is the saturation
/// signal rather than transport-layer queuing.  Slack covers transient
/// reconnection overlap; no ad-hoc reads share this pool today.
pub(crate) fn db_gateway_pool_size(config: &GatewayConfig) -> usize {
    const SLACK: usize = 2;
    config.storage.max_concurrent_rpcs().saturating_add(SLACK)
}

/// Keep `ViewGateway` concurrency bounded independently from JSON-RPC
/// `max_connections`. View calls are unbatched and may be expensive, so this
/// intentionally starts conservative until the gateway has per-method
/// concurrency limits or view-specific load data.
pub(crate) fn view_gateway_pool_size() -> usize {
    4
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use groundhog_db_gateway::{
        AddressAndKeyBatch, DbGateway_DbGateway1, DbGateway_DbGateway1_Dispatcher, DbGatewayResult,
    };
    use groundhog_indexer::{
        AddressTransactionQuery, EventPage as IdxEventPage, EventQuery,
        IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher,
        TransactionPage as IdxTransactionPage, TransactionQuery, TransactionRefPage, TxInfoResult,
    };
    use groundhog_view::{
        ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Dispatcher, ViewGatewayResult,
    };
    use groundhog_xdr::transaction::SignedTransactionHash;
    use groundhog_xdr::view::TransactionViewQuery;
    use rpc_driver::{ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;

    use super::*;

    struct IntroRejectedViewGateway;
    #[derive(Clone, Copy)]
    enum IntroMode {
        Reject,
        Expect(&'static str),
        Slow,
        RpcFail,
    }
    struct TestIndexer {
        mode: IntroMode,
    }
    struct TestDbGateway {
        mode: IntroMode,
    }
    struct ProtocolCheckingViewGateway {
        expected: &'static str,
    }
    struct SlowIntroViewGateway;

    #[async_trait]
    impl ViewGateway_ViewGateway1 for IntroRejectedViewGateway {
        async fn viewgateway_intro(
            &self,
            _arg0: Protocol,
        ) -> Result<ViewGatewayIntroOutcome, ProgramError> {
            Ok(ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol)
        }

        async fn viewgateway_view(
            &self,
            _arg0: TransactionViewQuery,
        ) -> Result<ViewGatewayResult, ProgramError> {
            panic!("bug: intro rejection test should not issue view RPC")
        }
    }

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for TestIndexer {
        async fn indexer_block_explorer_intro(
            &self,
            arg0: Protocol,
        ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
            match self.mode {
                IntroMode::Reject => Ok(IndexerBlockExplorerIntroOutcome::IntroMismatchedProtocol),
                IntroMode::Expect(expected) if arg0.0 == expected.as_bytes() => {
                    Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
                }
                IntroMode::Expect(_) => {
                    Ok(IndexerBlockExplorerIntroOutcome::IntroMismatchedProtocol)
                }
                IntroMode::Slow => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
                }
                IntroMode::RpcFail => Err(ProgramError::msg("fake indexer intro rpc-fail")),
            }
        }

        async fn get_transaction_info(
            &self,
            _arg0: SignedTransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }

        async fn get_events(&self, _arg0: EventQuery) -> Result<IdxEventPage, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }

        async fn get_transactions(
            &self,
            _arg0: TransactionQuery,
        ) -> Result<IdxTransactionPage, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }

        async fn get_transactions_by_address(
            &self,
            _arg0: AddressTransactionQuery,
        ) -> Result<TransactionRefPage, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }
    }

    #[async_trait]
    impl DbGateway_DbGateway1 for TestDbGateway {
        async fn dbgateway_intro(
            &self,
            arg0: Protocol,
        ) -> Result<DbGatewayIntroOutcome, ProgramError> {
            match self.mode {
                IntroMode::Expect(expected) if arg0.0 == expected.as_bytes() => {
                    Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)
                }
                IntroMode::Reject | IntroMode::Expect(_) => {
                    Ok(DbGatewayIntroOutcome::DbGatewayIntroMismatchedProtocol)
                }
                IntroMode::Slow => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)
                }
                IntroMode::RpcFail => Err(ProgramError::msg("fake DbGateway intro rpc-fail")),
            }
        }

        async fn dbgateway_get_items(
            &self,
            _arg0: AddressAndKeyBatch,
        ) -> Result<DbGatewayResult, ProgramError> {
            panic!("bug: intro rejection test should not issue db gateway data RPC")
        }
    }

    #[async_trait]
    impl ViewGateway_ViewGateway1 for ProtocolCheckingViewGateway {
        async fn viewgateway_intro(
            &self,
            arg0: Protocol,
        ) -> Result<ViewGatewayIntroOutcome, ProgramError> {
            if arg0.0 == self.expected.as_bytes() {
                Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)
            } else {
                Ok(ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol)
            }
        }

        async fn viewgateway_view(
            &self,
            _arg0: TransactionViewQuery,
        ) -> Result<ViewGatewayResult, ProgramError> {
            panic!("bug: intro protocol test should not issue view RPC")
        }
    }

    #[async_trait]
    impl ViewGateway_ViewGateway1 for SlowIntroViewGateway {
        async fn viewgateway_intro(
            &self,
            _arg0: Protocol,
        ) -> Result<ViewGatewayIntroOutcome, ProgramError> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)
        }

        async fn viewgateway_view(
            &self,
            _arg0: TransactionViewQuery,
        ) -> Result<ViewGatewayResult, ProgramError> {
            panic!("bug: intro timeout test should not issue view RPC")
        }
    }

    async fn start_intro_rejected_view_gateway() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test ViewGateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test ViewGateway should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(ViewGateway_ViewGateway1_Dispatcher::new(
            Box::new(IntroRejectedViewGateway),
        )));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    async fn start_test_indexer(
        mode: IntroMode,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test indexer should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test indexer should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(TestIndexer {
                mode,
            })),
        ));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    async fn start_test_db_gateway(
        mode: IntroMode,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test DbGateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test DbGateway should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(DbGateway_DbGateway1_Dispatcher::new(Box::new(
            TestDbGateway { mode },
        ))));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    async fn start_protocol_checking_view_gateway(
        expected: &'static str,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test ViewGateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test ViewGateway should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(ViewGateway_ViewGateway1_Dispatcher::new(
            Box::new(ProtocolCheckingViewGateway { expected }),
        )));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    async fn start_slow_intro_view_gateway() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test ViewGateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test ViewGateway should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(ViewGateway_ViewGateway1_Dispatcher::new(
            Box::new(SlowIntroViewGateway),
        )));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    #[tokio::test]
    async fn optional_view_gateway_without_address_is_absent() {
        let client = connect_optional_view_gateway(None, None, Duration::from_secs(1), None)
            .await
            .expect("missing optional ViewGateway should not fail startup");

        assert!(client.is_none());
    }

    #[tokio::test]
    async fn optional_indexer_and_db_gateway_without_address_are_absent() {
        let indexer = connect_optional_indexer(None, None, Duration::from_secs(1), None)
            .await
            .expect("missing optional indexer should not fail startup");
        let db_gateway = connect_optional_db_gateway(None, 1, None, Duration::from_secs(1), None)
            .await
            .expect("missing optional DbGateway should not fail startup");

        assert!(indexer.is_none());
        assert!(db_gateway.is_none());
    }

    #[tokio::test]
    async fn configured_view_gateway_intro_rejection_fails_startup_probe() {
        let (addr, _server) = start_intro_rejected_view_gateway().await;

        let Err(err) = connect_optional_view_gateway(
            Some(&addr.to_string()),
            None,
            Duration::from_secs(1),
            None,
        )
        .await
        else {
            panic!("configured ViewGateway intro rejection should fail startup");
        };

        match err {
            ConnectError::StartupRejected { reason, .. } => {
                assert!(reason.contains("ViewGateway intro mismatch"));
                assert!(reason.contains(PROTOCOL_VERSION));
                assert!(reason.contains("ViewGatewayIntroMismatchedProtocol"));
                assert!(reason.contains("--hogd-protocol"));
            }
            other => panic!("expected StartupRejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn configured_indexer_intro_rejection_fails_startup_probe() {
        let (addr, _server) = start_test_indexer(IntroMode::Reject).await;

        let Err(err) =
            connect_optional_indexer(Some(&addr.to_string()), None, Duration::from_secs(1), None)
                .await
        else {
            panic!("configured indexer intro rejection should fail startup");
        };

        match err {
            ConnectError::StartupRejected { reason, .. } => {
                assert!(reason.contains("IndexerBlockExplorer intro mismatch"));
                assert!(reason.contains(PROTOCOL_VERSION));
                assert!(reason.contains("IntroMismatchedProtocol"));
                assert!(reason.contains("--hogd-protocol"));
            }
            other => panic!("expected StartupRejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn configured_db_gateway_intro_rejection_fails_startup_probe() {
        let (addr, _server) = start_test_db_gateway(IntroMode::Reject).await;

        let Err(err) = connect_optional_db_gateway(
            Some(&addr.to_string()),
            1,
            None,
            Duration::from_secs(1),
            None,
        )
        .await
        else {
            panic!("configured DbGateway intro rejection should fail startup");
        };

        match err {
            ConnectError::StartupRejected { reason, .. } => {
                assert!(reason.contains("DbGateway intro mismatch"));
                assert!(reason.contains(PROTOCOL_VERSION));
                assert!(reason.contains("DbGatewayIntroMismatchedProtocol"));
                assert!(reason.contains("--hogd-protocol"));
            }
            other => panic!("expected StartupRejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn configured_view_gateway_uses_protocol_override_for_startup_probe() {
        let (addr, _server) = start_protocol_checking_view_gateway("hogp/test-smoke").await;

        let client = connect_optional_view_gateway(
            Some(&addr.to_string()),
            Some("hogp/test-smoke"),
            Duration::from_secs(1),
            None,
        )
        .await
        .expect("matching protocol override should pass startup")
        .expect("configured ViewGateway should be connected");

        drop(client);
    }

    #[tokio::test]
    async fn configured_indexer_and_db_gateway_use_protocol_override_for_startup_probe() {
        let (indexer_addr, _indexer_server) =
            start_test_indexer(IntroMode::Expect("hogp/test-smoke")).await;
        let (db_addr, _db_server) =
            start_test_db_gateway(IntroMode::Expect("hogp/test-smoke")).await;

        let indexer = connect_optional_indexer(
            Some(&indexer_addr.to_string()),
            Some("hogp/test-smoke"),
            Duration::from_secs(1),
            None,
        )
        .await
        .expect("matching indexer protocol override should pass startup")
        .expect("configured indexer should be connected");
        let db_gateway = connect_optional_db_gateway(
            Some(&db_addr.to_string()),
            1,
            Some("hogp/test-smoke"),
            Duration::from_secs(1),
            None,
        )
        .await
        .expect("matching DbGateway protocol override should pass startup")
        .expect("configured DbGateway should be connected");

        drop(indexer);
        drop(db_gateway);
    }

    #[tokio::test]
    async fn configured_view_gateway_intro_timeout_degrades_startup() {
        let (addr, _server) = start_slow_intro_view_gateway().await;

        let view_gateway = connect_optional_view_gateway(
            Some(&addr.to_string()),
            None,
            Duration::from_millis(10),
            None,
        )
        .await
        .expect("slow ViewGateway intro should preserve degraded startup");

        assert!(view_gateway.is_none());
    }

    #[tokio::test]
    async fn configured_indexer_and_db_gateway_intro_timeout_degrades_startup() {
        let (indexer_addr, _indexer_server) = start_test_indexer(IntroMode::Slow).await;
        let (db_addr, _db_server) = start_test_db_gateway(IntroMode::Slow).await;

        let indexer = connect_optional_indexer(
            Some(&indexer_addr.to_string()),
            None,
            Duration::from_millis(10),
            None,
        )
        .await
        .expect("slow indexer intro should preserve degraded startup");
        let db_gateway = connect_optional_db_gateway(
            Some(&db_addr.to_string()),
            1,
            None,
            Duration::from_millis(10),
            None,
        )
        .await
        .expect("slow DbGateway intro should preserve degraded startup");

        assert!(indexer.is_none());
        assert!(db_gateway.is_none());
    }

    #[tokio::test]
    async fn configured_indexer_and_db_gateway_intro_rpc_errors_degrade_startup() {
        let (indexer_addr, _indexer_server) = start_test_indexer(IntroMode::RpcFail).await;
        let (db_addr, _db_server) = start_test_db_gateway(IntroMode::RpcFail).await;

        let indexer = connect_optional_indexer(
            Some(&indexer_addr.to_string()),
            None,
            Duration::from_secs(1),
            None,
        )
        .await
        .expect("indexer intro RPC errors should preserve degraded startup");
        let db_gateway = connect_optional_db_gateway(
            Some(&db_addr.to_string()),
            1,
            None,
            Duration::from_secs(1),
            None,
        )
        .await
        .expect("DbGateway intro RPC errors should preserve degraded startup");

        assert!(indexer.is_none());
        assert!(db_gateway.is_none());
    }
}
