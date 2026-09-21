use std::{
    ops::Deref,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use groundhog_db_gateway::{DbGateway_DbGateway1_Client, DbGatewayIntroOutcome};
use groundhog_hogd_backend::{IntroOutcome, intro};
use groundhog_indexer::{
    IndexerBlockExplorer_IndexerBlockExplorer1_Client, IndexerBlockExplorerIntroOutcome,
};
use groundhog_json_rpc_types::protocol_token::{protocol_from_text, protocol_text};
use groundhog_scheduler::sched_gateway::{
    SchedGateway_SchedGateway1_Client, SchedGatewayIntroOutcome,
};
use groundhog_scp::network::ProtocolCommit;
use groundhog_view::{ViewGateway_ViewGateway1_Client, ViewGatewayIntroOutcome};
use groundhog_x23_transport::{
    ConnectError, hogd_large_message_config, try_connect, try_connect_with_canceled_call_policy,
    try_connect_with_config, try_connect_with_config_and_canceled_call_policy,
};
use groundhog_xdr::PROTOCOL_VERSION;
use rpc_driver::{CanceledCallPolicy, ConnectionConfig, RpcSender};
use tokio::{
    sync::{RwLock, Semaphore},
    task::{AbortHandle, JoinHandle},
    time::Instant,
};

use crate::config::{GatewayConfig, HealthBackend};
use crate::metrics::{BackendRequestOutcome, GatewayMetrics};
use crate::storage_batcher::{DbGatewaySink, StorageReadBatcher, StorageReadSink};
use crate::submitter::{BatchSubmitter, SchedulerSink, SubmitSink};
use crate::tip_coalescer::{SchedulerTipSink, TipCoalescer, TipSink};

static NEXT_SCHEDULER_GENERATION: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
pub(crate) const SCHEDULER_LOOKUP_CONCURRENCY: usize = 4;
pub(crate) use crate::config::SCHEDULER_LOOKUP_MAX_OUTSTANDING;
const HEALTH_PROBE_POOL_SIZE: usize = 1;

#[derive(Clone)]
pub(crate) struct BackendClient<C> {
    client: C,
    probe_client: C,
}

impl<C> BackendClient<C> {
    pub(crate) fn new(client: C, probe_client: C) -> Self {
        Self {
            client,
            probe_client,
        }
    }

    pub(crate) fn probe_client(&self) -> &C {
        &self.probe_client
    }
}

impl<C> Deref for BackendClient<C> {
    type Target = C;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

pub(crate) type IndexerClient = BackendClient<IndexerBlockExplorer_IndexerBlockExplorer1_Client>;
pub(crate) type DbGatewayClient = BackendClient<DbGateway_DbGateway1_Client>;
pub(crate) type ViewGatewayClient = BackendClient<ViewGateway_ViewGateway1_Client>;

#[derive(Clone)]
pub(crate) struct SchedulerClient {
    client: SchedGateway_SchedGateway1_Client,
    lookup_client: SchedGateway_SchedGateway1_Client,
    probe_client: SchedGateway_SchedGateway1_Client,
    generation: u64,
}

impl SchedulerClient {
    pub(crate) fn new(
        client: SchedGateway_SchedGateway1_Client,
        lookup_client: SchedGateway_SchedGateway1_Client,
        probe_client: SchedGateway_SchedGateway1_Client,
    ) -> Self {
        Self {
            client,
            lookup_client,
            probe_client,
            generation: NEXT_SCHEDULER_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn probe_client(&self) -> &SchedGateway_SchedGateway1_Client {
        &self.probe_client
    }

    pub(crate) fn lookup_client(&self) -> &SchedGateway_SchedGateway1_Client {
        &self.lookup_client
    }
}

impl Deref for SchedulerClient {
    type Target = SchedGateway_SchedGateway1_Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

pub(crate) async fn clear_scheduler_if_current(
    scheduler: &RwLock<Option<SchedulerClient>>,
    generation: u64,
) {
    let mut current = scheduler.write().await;
    if current
        .as_ref()
        .is_some_and(|client| client.generation() == generation)
    {
        *current = None;
    }
}

pub(crate) struct AbortTaskOnDrop {
    handle: AbortHandle,
}

impl AbortTaskOnDrop {
    pub(crate) fn new<T>(task: &JoinHandle<T>) -> Self {
        Self {
            handle: task.abort_handle(),
        }
    }
}

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Shared state injected into every handler.
///
/// Backend connections are behind `Arc<RwLock<Option<...>>>` to
/// support lazy connection and reconnection, and to let long-lived
/// submitter/coalescer tasks observe the same state. Handlers clone
/// the client out of the lock before making RPC calls, so the read
/// guard is not held across `.await`.
pub struct GatewayContext {
    /// The protocol token this gateway offers at intro: the configured
    /// override when there is one, otherwise this build's own. Resolved once
    /// at construction, so nothing downstream re-parses operator text.
    pub(crate) protocol: ProtocolCommit,

    /// x23 RPC client to hogd scheduler gateway.
    /// Used for: transaction submission, tip, lookup, and pending count.
    pub scheduler: Arc<RwLock<Option<SchedulerClient>>>,

    /// Running plus waiting transaction-status scheduler lookups, shared across callers.
    pub(crate) scheduler_lookup_admission: Arc<Semaphore>,

    /// Execution slots matching the dedicated scheduler lookup pool.
    pub(crate) scheduler_lookup_execution: Arc<Semaphore>,

    /// Scheduler pool held away from request handlers until its readiness
    /// grace period has elapsed.
    pub(crate) scheduler_quarantine: Arc<RwLock<Option<QuarantinedScheduler>>>,

    /// x23 RPC client to hogd indexer.
    /// Used for: `get_transaction_info`, `get_events` (paginated).
    pub indexer: Arc<RwLock<Option<IndexerClient>>>,

    /// Adaptive batching submitter for `sendRawTransaction`. Accumulates
    /// incoming client submissions and packs them into
    /// `schedgateway_submit_tx_batch` RPCs to amortize
    /// per-call overhead at high load.
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
    pub view_gateway: Arc<RwLock<Option<ViewGatewayClient>>>,

    /// x23 RPC client to hogd `DbGateway`.
    pub db_gateway: Arc<RwLock<Option<DbGatewayClient>>>,

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
    pub async fn new(config: GatewayConfig, metrics: GatewayMetrics) -> Result<Self, ConnectError> {
        let config = Arc::new(config);

        // Resolved once, here, so a malformed override fails startup with the
        // text the operator wrote rather than reaching a backend as some other
        // token and coming back as a protocol mismatch.
        let protocol_override = config
            .hogd_protocol_override
            .as_deref()
            .map(protocol_from_text)
            .transpose()
            .map_err(|source| ConnectError::InvalidProtocolOverride {
                reason: source.to_string(),
            })?;
        let protocol = protocol_override.unwrap_or(PROTOCOL_VERSION);

        // `FixedSerialPool` is 1 in-flight per connection by choice (FIFO
        // dispatch per connection, modeling x23 sync RPC; the x23 wire
        // protocol itself multiplexes via xid).  Size the pool to cover the
        // batch submitter (`max_inflight_batches` × shards) plus the tip
        // coalescer (`max_inflight_tips`) plus slack for ad-hoc RPCs.
        let startup_timeout = Duration::from_millis(config.backend_connect_timeout_ms.max(1));
        let sched_pool_size = scheduler_pool_size(&config);
        let scheduler_candidate = if let Ok(result) = tokio::time::timeout(
            startup_timeout,
            connect_optional_scheduler(
                &config.scheduler_address,
                sched_pool_size,
                config.scheduler_lookup_pool_size.get(),
                protocol,
                Some(&metrics),
            ),
        )
        .await
        {
            result?
        } else {
            tracing::warn!(
                addr = %config.scheduler_address,
                timeout = ?startup_timeout,
                "scheduler connect timed out, starting in degraded mode"
            );
            None
        };
        let scheduler = Arc::new(RwLock::new(None));

        let indexer = Arc::new(RwLock::new(tolerate_optional_startup_timeout(
            connect_optional_indexer(
                config.indexer_address.as_deref(),
                config.indexer_pool_size.get(),
                protocol_override,
                startup_timeout,
                Some(&metrics),
            )
            .await,
        )?));

        let db_gateway_pool = db_gateway_pool_size(&config);
        let db_gateway = Arc::new(RwLock::new(tolerate_optional_startup_timeout(
            connect_optional_db_gateway(
                config.db_gateway_address.as_deref(),
                db_gateway_pool,
                protocol_override,
                startup_timeout,
                Some(&metrics),
            )
            .await,
        )?));

        let view_gateway = Arc::new(RwLock::new(tolerate_optional_startup_timeout(
            connect_optional_view_gateway(
                config.view_gateway_address.as_deref(),
                protocol_override,
                startup_timeout,
                Some(&metrics),
            )
            .await,
        )?));

        let context = Self::assemble(
            protocol,
            scheduler,
            indexer,
            view_gateway,
            db_gateway,
            config,
            metrics,
        );
        if let Some(client) = scheduler_candidate {
            let ready_at =
                Instant::now() + Duration::from_millis(context.config.scheduler_reconnect_grace_ms);
            *context.scheduler_quarantine.write().await =
                Some(QuarantinedScheduler { client, ready_at });
        }
        Ok(context)
    }

    /// Assemble a context from already-connected (or absent) backend
    /// clients, spawning the batching and coalescing tasks.
    pub(crate) fn assemble(
        protocol: ProtocolCommit,
        scheduler: Arc<RwLock<Option<SchedulerClient>>>,
        indexer: Arc<RwLock<Option<IndexerClient>>>,
        view_gateway: Arc<RwLock<Option<ViewGatewayClient>>>,
        db_gateway: Arc<RwLock<Option<DbGatewayClient>>>,
        config: Arc<GatewayConfig>,
        metrics: GatewayMetrics,
    ) -> Self {
        let request_timeout = config.request_timeout();
        let backend_request_timeout = config.backend_request_timeout();
        let request_timeout_margin = config.request_timeout_margin();
        let submit_sink: Arc<dyn SubmitSink> = Arc::new(SchedulerSink::new(
            scheduler.clone(),
            metrics.clone(),
            backend_request_timeout,
        ));
        let batch_submitter = BatchSubmitter::spawn_with_deadlines(
            &config.batch,
            &submit_sink,
            metrics.clone(),
            request_timeout,
            backend_request_timeout,
            request_timeout_margin,
        );

        let storage_sink: Arc<dyn StorageReadSink> = Arc::new(DbGatewaySink::new(
            db_gateway.clone(),
            metrics.clone(),
            backend_request_timeout,
        ));
        let storage_batcher = StorageReadBatcher::spawn_with_deadlines(
            &config.storage,
            &storage_sink,
            metrics.clone(),
            request_timeout,
            backend_request_timeout,
            request_timeout_margin,
        );

        let tip_sink: Arc<dyn TipSink> = Arc::new(SchedulerTipSink::new(
            scheduler.clone(),
            metrics.clone(),
            backend_request_timeout,
        ));
        let tip_coalescer = TipCoalescer::spawn_with_deadlines(
            &config.tip,
            &tip_sink,
            metrics.clone(),
            request_timeout,
            backend_request_timeout,
            request_timeout_margin,
        );

        Self {
            scheduler,
            scheduler_lookup_admission: Arc::new(Semaphore::new(SCHEDULER_LOOKUP_MAX_OUTSTANDING)),
            scheduler_lookup_execution: Arc::new(Semaphore::new(
                config.scheduler_lookup_pool_size.get(),
            )),
            scheduler_quarantine: Arc::new(RwLock::new(None)),
            indexer,
            batch_submitter,
            protocol,
            storage_batcher,
            tip_coalescer,
            view_gateway,
            db_gateway,
            config,
            metrics,
        }
    }

    /// Abort request-batching workers after the RPC server has stopped accepting work.
    pub(crate) fn abort_request_workers(&self) {
        self.batch_submitter.abort();
        self.storage_batcher.abort();
        self.tip_coalescer.abort();
    }
}

fn tolerate_optional_startup_timeout<T>(
    result: Result<Option<T>, ConnectError>,
) -> Result<Option<T>, ConnectError> {
    match result {
        Err(ConnectError::StartupProbeTimedOut { .. }) => Ok(None),
        result => result,
    }
}

#[derive(Clone)]
pub(crate) struct QuarantinedScheduler {
    pub(crate) client: SchedulerClient,
    pub(crate) ready_at: Instant,
}

pub(crate) async fn connect_optional_scheduler(
    address: &str,
    pool_size: usize,
    lookup_pool_size: usize,
    protocol: ProtocolCommit,
    metrics: Option<&GatewayMetrics>,
) -> Result<Option<SchedulerClient>, ConnectError> {
    let Some(client) = try_connect_with_canceled_call_policy(
        "scheduler",
        address,
        pool_size,
        CanceledCallPolicy::PoisonConnection,
        SchedGateway_SchedGateway1_Client::new,
    )
    .await?
    else {
        return Ok(None);
    };
    let Some(lookup_client) = try_connect_with_config_and_canceled_call_policy(
        "scheduler_lookup",
        address,
        lookup_pool_size,
        hogd_large_message_config(),
        CanceledCallPolicy::ReplaceConnection,
        SchedGateway_SchedGateway1_Client::new,
    )
    .await?
    else {
        return Ok(None);
    };
    let Some(probe_client) = try_connect_with_canceled_call_policy(
        "scheduler_health",
        address,
        1,
        CanceledCallPolicy::PoisonConnection,
        SchedGateway_SchedGateway1_Client::new,
    )
    .await?
    else {
        return Ok(None);
    };
    let client = SchedulerClient::new(client, lookup_client, probe_client);
    let protocol_version = protocol_text(&protocol);
    let started_at = Instant::now();
    let intro_result = client.probe_client().schedgateway_intro(&protocol).await;
    if let Some(metrics) = metrics {
        let outcome = match &intro_result {
            Ok(SchedGatewayIntroOutcome::SchedGatewayIntroSuccess) => BackendRequestOutcome::Ok,
            Ok(_) => BackendRequestOutcome::Rejected,
            Err(_) => BackendRequestOutcome::Error,
        };
        metrics.observe_backend_request(
            HealthBackend::Scheduler,
            "schedgateway_intro",
            outcome,
            started_at.elapsed(),
        );
    }
    match intro_result {
        Ok(outcome) if intro::sched_gateway(outcome).accepted() => Ok(Some(client)),
        Ok(outcome) => Err(ConnectError::StartupRejected {
            service: "scheduler_gateway",
            addr: address.to_string(),
            reason: intro_rejection_reason(intro::sched_gateway(outcome), &protocol_version),
        }),
        Err(source) => {
            tracing::warn!(
                addr = address,
                service = "schedgateway_intro",
                error = %source,
                "scheduler gateway intro failed, starting in degraded mode"
            );
            Ok(None)
        }
    }
}

/// Explain why a hogd service rejected the protocol this gateway speaks.
///
/// A mismatch is the one rejection an operator can fix without touching the
/// backend, so it names both the override flag and the rebuild path.
fn intro_rejection_reason(outcome: IntroOutcome, protocol_version: &str) -> String {
    let service = outcome.service();
    let name = outcome.name();
    if outcome.mismatched_protocol() {
        return format!(
            "{service} intro mismatch: gateway speaks protocol {protocol_version}, \
             but the backend rejected it ({name}); pass `--hogd-protocol`/\
             `hogd_protocol_override` for a matching backend protocol, or rebuild the gateway \
             against a matching groundhog-xdr"
        );
    }

    format!(
        "{service} intro rejected with outcome {name}; gateway speaks protocol \
         {protocol_version}"
    )
}

async fn connect_backend_pair<C, F>(
    service: &'static str,
    probe_service: &'static str,
    address: &str,
    pool_size: usize,
    connection_config: ConnectionConfig,
    startup_timeout: Duration,
    build: F,
) -> Result<Option<BackendClient<C>>, ConnectError>
where
    F: Fn(Arc<dyn RpcSender>) -> C + Copy,
{
    let result = tokio::time::timeout(startup_timeout, async {
        let Some(client) =
            try_connect_with_config(service, address, pool_size, connection_config, build).await?
        else {
            return Ok(None);
        };
        let Some(probe_client) =
            try_connect(probe_service, address, HEALTH_PROBE_POOL_SIZE, build).await?
        else {
            return Ok(None);
        };
        Ok(Some(BackendClient::new(client, probe_client)))
    })
    .await;
    if let Ok(result) = result {
        result
    } else {
        tracing::warn!(
            addr = address,
            service,
            timeout = ?startup_timeout,
            "optional backend connection pools timed out, starting in degraded mode"
        );
        Err(ConnectError::StartupProbeTimedOut {
            service,
            addr: address.to_string(),
            timeout: startup_timeout,
        })
    }
}

pub(crate) async fn connect_optional_indexer(
    address: Option<&str>,
    pool_size: usize,
    protocol_override: Option<ProtocolCommit>,
    startup_timeout: Duration,
    metrics: Option<&GatewayMetrics>,
) -> Result<Option<IndexerClient>, ConnectError> {
    let Some(address) = address else {
        return Ok(None);
    };

    let connect_started_at = Instant::now();
    let Some(client) = connect_backend_pair(
        "indexer",
        "indexer_health",
        address,
        pool_size,
        hogd_large_message_config(),
        startup_timeout,
        IndexerBlockExplorer_IndexerBlockExplorer1_Client::new,
    )
    .await?
    else {
        return Ok(None);
    };
    let Some(intro_timeout) = startup_timeout.checked_sub(connect_started_at.elapsed()) else {
        tracing::warn!(
            addr = address,
            service = "indexer_block_explorer_intro",
            timeout = ?startup_timeout,
            "optional backend connect consumed startup timeout, starting in degraded mode"
        );
        return Err(ConnectError::StartupProbeTimedOut {
            service: "indexer",
            addr: address.to_string(),
            timeout: startup_timeout,
        });
    };

    let protocol = protocol_override.unwrap_or(PROTOCOL_VERSION);
    // The text form of what is actually sent, so a mismatch message quotes the
    // bytes the backend rejected.
    let protocol_version = protocol_text(&protocol);
    let started_at = Instant::now();
    let intro_result = tokio::time::timeout(
        intro_timeout,
        client
            .probe_client()
            .indexer_block_explorer_intro(&protocol),
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
        return Err(ConnectError::StartupProbeTimedOut {
            service: "indexer",
            addr: address.to_string(),
            timeout: startup_timeout,
        });
    };

    match intro_outcome {
        Ok(outcome) if intro::indexer_block_explorer(outcome).accepted() => Ok(Some(client)),
        Ok(outcome) => Err(ConnectError::StartupRejected {
            service: "indexer",
            addr: address.to_string(),
            reason: intro_rejection_reason(
                intro::indexer_block_explorer(outcome),
                &protocol_version,
            ),
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
    protocol_override: Option<ProtocolCommit>,
    startup_timeout: Duration,
    metrics: Option<&GatewayMetrics>,
) -> Result<Option<DbGatewayClient>, ConnectError> {
    let Some(address) = address else {
        return Ok(None);
    };

    let connect_started_at = Instant::now();
    let Some(client) = connect_backend_pair(
        "db_gateway",
        "db_gateway_health",
        address,
        pool_size,
        ConnectionConfig::default(),
        startup_timeout,
        DbGateway_DbGateway1_Client::new,
    )
    .await?
    else {
        return Ok(None);
    };
    let Some(intro_timeout) = startup_timeout.checked_sub(connect_started_at.elapsed()) else {
        tracing::warn!(
            addr = address,
            service = "dbgateway_intro",
            timeout = ?startup_timeout,
            "optional backend connect consumed startup timeout, starting in degraded mode"
        );
        return Err(ConnectError::StartupProbeTimedOut {
            service: "db_gateway",
            addr: address.to_string(),
            timeout: startup_timeout,
        });
    };

    let protocol = protocol_override.unwrap_or(PROTOCOL_VERSION);
    // The text form of what is actually sent, so a mismatch message quotes the
    // bytes the backend rejected.
    let protocol_version = protocol_text(&protocol);
    let started_at = Instant::now();
    let intro_result = tokio::time::timeout(
        intro_timeout,
        client.probe_client().dbgateway_intro(&protocol),
    )
    .await;
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
        return Err(ConnectError::StartupProbeTimedOut {
            service: "db_gateway",
            addr: address.to_string(),
            timeout: startup_timeout,
        });
    };

    match intro_outcome {
        Ok(outcome) if intro::db_gateway(outcome).accepted() => Ok(Some(client)),
        Ok(outcome) => Err(ConnectError::StartupRejected {
            service: "db_gateway",
            addr: address.to_string(),
            reason: intro_rejection_reason(intro::db_gateway(outcome), &protocol_version),
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
    protocol_override: Option<ProtocolCommit>,
    startup_timeout: Duration,
    metrics: Option<&GatewayMetrics>,
) -> Result<Option<ViewGatewayClient>, ConnectError> {
    let Some(address) = address else {
        return Ok(None);
    };

    let connect_started_at = Instant::now();
    let Some(client) = connect_backend_pair(
        "view_gateway",
        "view_gateway_health",
        address,
        view_gateway_pool_size(),
        ConnectionConfig::default(),
        startup_timeout,
        ViewGateway_ViewGateway1_Client::new,
    )
    .await?
    else {
        return Ok(None);
    };
    let Some(intro_timeout) = startup_timeout.checked_sub(connect_started_at.elapsed()) else {
        tracing::warn!(
            addr = address,
            service = "view_gateway_intro",
            timeout = ?startup_timeout,
            "optional backend connect consumed startup timeout, starting in degraded mode"
        );
        return Err(ConnectError::StartupProbeTimedOut {
            service: "view_gateway",
            addr: address.to_string(),
            timeout: startup_timeout,
        });
    };

    let protocol = protocol_override.unwrap_or(PROTOCOL_VERSION);
    // The text form of what is actually sent, so a mismatch message quotes the
    // bytes the backend rejected.
    let protocol_version = protocol_text(&protocol);
    let started_at = Instant::now();
    let intro_result = tokio::time::timeout(
        intro_timeout,
        client.probe_client().viewgateway_intro(&protocol),
    )
    .await;
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
        return Err(ConnectError::StartupProbeTimedOut {
            service: "view_gateway",
            addr: address.to_string(),
            timeout: startup_timeout,
        });
    };

    match intro_outcome {
        Ok(outcome) if intro::view_gateway(outcome).accepted() => Ok(Some(client)),
        Ok(outcome) => Err(ConnectError::StartupRejected {
            service: "view_gateway",
            addr: address.to_string(),
            reason: intro_rejection_reason(intro::view_gateway(outcome), &protocol_version),
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

/// Pick a scheduler pool size that covers every concurrent RPC the gateway
/// may have in flight against it.  x23 serializes one RPC per connection.
/// Transaction lookups use their own admission-matched pool.
pub(crate) fn scheduler_pool_size(config: &GatewayConfig) -> usize {
    let tip = config.tip.max_inflight_tips.max(1) as usize;
    config.batch.max_concurrent_rpcs().saturating_add(tip)
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
    use groundhog_scp::network::ProtocolCommit as Protocol;
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use groundhog_db_gateway::{
        AddressAndKeyBatch, DbGateway_DbGateway1, DbGateway_DbGateway1_Dispatcher, DbGatewayResult,
    };
    use groundhog_indexer::{
        AddressTransactionQuery, BlockLookupResult, BlockTransactionQuery, EventPageResult,
        EventQuery, IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, TransactionPageResult,
        TransactionQuery, TransactionRefPageResult, TxInfoResult,
    };
    use groundhog_scheduler::{
        ExpectedDBTip, ParallelSignedTransactionBatch, PendingTxCount,
        SchedGatewaySubmitOutcomeBatch,
        sched_gateway::{SchedGateway_SchedGateway1, SchedGateway_SchedGateway1_Dispatcher},
    };
    use groundhog_view::{
        ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Dispatcher, ViewGatewayResult,
    };
    use groundhog_xdr::transaction::{
        OptSignedTransactionBatch, TransactionHash, TransactionHashBatch,
    };
    use groundhog_xdr::view::TransactionViewQuery;
    use rpc_driver::{ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;

    use super::*;

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn abort_task_on_drop_cancels_spawned_task() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task_dropped = dropped.clone();
        let task = tokio::spawn(async move {
            let _marker = DropMarker(task_dropped);
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.expect("bug: spawned task should start");
        let guard = AbortTaskOnDrop::new(&task);

        drop(guard);

        let error = task.await.expect_err("bug: guarded task should be aborted");
        assert!(error.is_cancelled());
        assert!(dropped.load(Ordering::SeqCst));
    }

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
    struct TestSchedulerGateway {
        mode: IntroMode,
    }
    struct ProtocolCheckingViewGateway {
        expected: &'static str,
    }
    struct SlowIntroViewGateway;

    #[async_trait]
    impl SchedGateway_SchedGateway1 for TestSchedulerGateway {
        async fn schedgateway_intro(
            &self,
            protocol: Protocol,
        ) -> Result<SchedGatewayIntroOutcome, ProgramError> {
            match self.mode {
                IntroMode::Expect(expected)
                    if protocol_from_text(expected).is_ok_and(|want| protocol == want) =>
                {
                    Ok(SchedGatewayIntroOutcome::SchedGatewayIntroSuccess)
                }
                IntroMode::Reject | IntroMode::Expect(_) => {
                    Ok(SchedGatewayIntroOutcome::SchedGatewayIntroMismatchedProtocol)
                }
                IntroMode::Slow => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(SchedGatewayIntroOutcome::SchedGatewayIntroSuccess)
                }
                IntroMode::RpcFail => {
                    Err(ProgramError::msg("fake scheduler gateway intro failure"))
                }
            }
        }

        async fn schedgateway_submit_tx_batch(
            &self,
            _batch: ParallelSignedTransactionBatch,
        ) -> Result<SchedGatewaySubmitOutcomeBatch, ProgramError> {
            panic!("bug: intro test should not submit")
        }

        async fn schedgateway_get_tip(&self) -> Result<ExpectedDBTip, ProgramError> {
            panic!("bug: intro test should not read the tip")
        }

        async fn schedgateway_lookup_tx(
            &self,
            _hashes: TransactionHashBatch,
        ) -> Result<OptSignedTransactionBatch, ProgramError> {
            panic!("bug: intro test should not look up transactions")
        }

        async fn schedgateway_pending_tx_count(&self) -> Result<PendingTxCount, ProgramError> {
            panic!("bug: intro test should not read pending count")
        }
    }

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
                IntroMode::Expect(expected)
                    if protocol_from_text(expected).is_ok_and(|want| arg0 == want) =>
                {
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
            _arg0: TransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }

        async fn get_block_by_height(
            &self,
            _arg0: groundhog_xdr::types::BlockHeight,
        ) -> Result<BlockLookupResult, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }

        async fn get_events(&self, _arg0: EventQuery) -> Result<EventPageResult, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }

        async fn get_transactions(
            &self,
            _arg0: TransactionQuery,
        ) -> Result<TransactionPageResult, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }

        async fn get_transactions_by_address(
            &self,
            _arg0: AddressTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("bug: intro rejection test should not issue indexer data RPC")
        }

        async fn get_transactions_by_block(
            &self,
            _arg0: BlockTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
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
                IntroMode::Expect(expected)
                    if protocol_from_text(expected).is_ok_and(|want| arg0 == want) =>
                {
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
            if protocol_from_text(self.expected).is_ok_and(|want| arg0 == want) {
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

    async fn start_test_scheduler_gateway(
        mode: IntroMode,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test scheduler gateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test scheduler gateway should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(SchedGateway_SchedGateway1_Dispatcher::new(
            Box::new(TestSchedulerGateway { mode }),
        )));
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
        let indexer = connect_optional_indexer(None, 4, None, Duration::from_secs(1), None)
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
                assert!(reason.contains(&protocol_text(&PROTOCOL_VERSION)));
                assert!(reason.contains("ViewGatewayIntroMismatchedProtocol"));
                assert!(reason.contains("--hogd-protocol"));
            }
            other => panic!("expected StartupRejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn configured_scheduler_gateway_intro_rejection_fails_startup_probe() {
        let (addr, _server) = start_test_scheduler_gateway(IntroMode::Reject).await;

        let Err(err) =
            connect_optional_scheduler(&addr.to_string(), 1, 4, PROTOCOL_VERSION, None).await
        else {
            panic!("configured scheduler gateway intro rejection should fail startup");
        };

        match err {
            ConnectError::StartupRejected { reason, .. } => {
                assert!(reason.contains("SchedGateway intro mismatch"));
                assert!(reason.contains(&protocol_text(&PROTOCOL_VERSION)));
                assert!(reason.contains("SchedGatewayIntroMismatchedProtocol"));
                assert!(reason.contains("--hogd-protocol"));
            }
            other => panic!("expected StartupRejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn configured_indexer_intro_rejection_fails_startup_probe() {
        let (addr, _server) = start_test_indexer(IntroMode::Reject).await;

        let Err(err) = connect_optional_indexer(
            Some(&addr.to_string()),
            4,
            None,
            Duration::from_secs(1),
            None,
        )
        .await
        else {
            panic!("configured indexer intro rejection should fail startup");
        };

        match err {
            ConnectError::StartupRejected { reason, .. } => {
                assert!(reason.contains("IndexerBlockExplorer intro mismatch"));
                assert!(reason.contains(&protocol_text(&PROTOCOL_VERSION)));
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
                assert!(reason.contains(&protocol_text(&PROTOCOL_VERSION)));
                assert!(reason.contains("DbGatewayIntroMismatchedProtocol"));
                assert!(reason.contains("--hogd-protocol"));
            }
            other => panic!("expected StartupRejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn configured_view_gateway_uses_protocol_override_for_startup_probe() {
        let (addr, _server) = start_protocol_checking_view_gateway("hogp/test-smoke00000").await;

        let client = connect_optional_view_gateway(
            Some(&addr.to_string()),
            Some(protocol_from_text("hogp/test-smoke00000").expect("bug: test token is 20 bytes")),
            Duration::from_secs(1),
            None,
        )
        .await
        .expect("matching protocol override should pass startup")
        .expect("configured ViewGateway should be connected");

        drop(client);
    }

    #[tokio::test]
    async fn configured_scheduler_gateway_uses_protocol_override_for_startup_probe() {
        let (addr, _server) =
            start_test_scheduler_gateway(IntroMode::Expect("hogp/test-smoke00000")).await;
        let protocol =
            protocol_from_text("hogp/test-smoke00000").expect("bug: test token is 20 bytes");

        let client = connect_optional_scheduler(&addr.to_string(), 1, 4, protocol, None)
            .await
            .expect("matching scheduler gateway protocol override should pass startup")
            .expect("configured scheduler gateway should be connected");

        drop(client);
    }

    #[tokio::test]
    async fn configured_indexer_and_db_gateway_use_protocol_override_for_startup_probe() {
        let (indexer_addr, _indexer_server) =
            start_test_indexer(IntroMode::Expect("hogp/test-smoke00000")).await;
        let (db_addr, _db_server) =
            start_test_db_gateway(IntroMode::Expect("hogp/test-smoke00000")).await;

        let indexer = connect_optional_indexer(
            Some(&indexer_addr.to_string()),
            4,
            Some(protocol_from_text("hogp/test-smoke00000").expect("bug: test token is 20 bytes")),
            Duration::from_secs(1),
            None,
        )
        .await
        .expect("matching indexer protocol override should pass startup")
        .expect("configured indexer should be connected");
        let db_gateway = connect_optional_db_gateway(
            Some(&db_addr.to_string()),
            1,
            Some(protocol_from_text("hogp/test-smoke00000").expect("bug: test token is 20 bytes")),
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
    async fn configured_view_gateway_intro_timeout_is_typed() {
        let (addr, _server) = start_slow_intro_view_gateway().await;

        let result = connect_optional_view_gateway(
            Some(&addr.to_string()),
            None,
            Duration::from_millis(10),
            None,
        )
        .await;

        assert!(matches!(
            result,
            Err(ConnectError::StartupProbeTimedOut {
                service: "view_gateway",
                ..
            })
        ));
    }

    #[test]
    fn optional_timeout_is_tolerated_only_at_process_startup() {
        let result =
            tolerate_optional_startup_timeout::<()>(Err(ConnectError::StartupProbeTimedOut {
                service: "indexer",
                addr: "127.0.0.1:1".into(),
                timeout: Duration::from_secs(1),
            }));

        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn configured_indexer_and_db_gateway_intro_timeouts_are_typed() {
        let (indexer_addr, _indexer_server) = start_test_indexer(IntroMode::Slow).await;
        let (db_addr, _db_server) = start_test_db_gateway(IntroMode::Slow).await;

        let indexer = connect_optional_indexer(
            Some(&indexer_addr.to_string()),
            4,
            None,
            Duration::from_millis(10),
            None,
        )
        .await;
        let db_gateway = connect_optional_db_gateway(
            Some(&db_addr.to_string()),
            1,
            None,
            Duration::from_millis(10),
            None,
        )
        .await;

        assert!(matches!(
            indexer,
            Err(ConnectError::StartupProbeTimedOut {
                service: "indexer",
                ..
            })
        ));
        assert!(matches!(
            db_gateway,
            Err(ConnectError::StartupProbeTimedOut {
                service: "db_gateway",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn configured_indexer_and_db_gateway_intro_rpc_errors_degrade_startup() {
        let (indexer_addr, _indexer_server) = start_test_indexer(IntroMode::RpcFail).await;
        let (db_addr, _db_server) = start_test_db_gateway(IntroMode::RpcFail).await;

        let indexer = connect_optional_indexer(
            Some(&indexer_addr.to_string()),
            4,
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
