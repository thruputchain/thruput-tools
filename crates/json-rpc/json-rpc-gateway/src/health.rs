use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use groundhog_db_gateway::DbGatewayIntroOutcome;
use groundhog_indexer::IndexerBlockExplorerIntroOutcome;
use groundhog_json_rpc_types::{BlockHeight, block::ChainTip, hex::Hex0x};
use groundhog_scheduler::sched_coord::SchedulerGetTipResult;
use groundhog_scp::network::Protocol;
use groundhog_view::ViewGatewayIntroOutcome;
use groundhog_xdr::PROTOCOL_VERSION;
use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::RwLock;
use xdr_common::StringM;

use crate::{
    config::{GatewayConfig, HealthBackend},
    context::{
        GatewayContext, PendingSchedulerReconnect, connect_optional_db_gateway,
        connect_optional_indexer, connect_optional_scheduler, connect_optional_view_gateway,
        db_gateway_pool_size, scheduler_pool_size,
    },
    metrics::BackendRequestOutcome,
};

/// Cached health/readiness state shared by `/readyz` and `/health`.
#[derive(Debug, Clone)]
pub struct GatewayHealth {
    inner: Arc<RwLock<HealthSnapshot>>,
    max_chain_tip_age_ms: u64,
    max_snapshot_age_ms: u64,
    scheduler_required: bool,
    version: &'static str,
    commit_hash: Option<&'static str>,
}

impl GatewayHealth {
    /// Create cached readiness state initialized to `starting`.
    ///
    /// The snapshot remains `starting` until the background probe loop publishes
    /// its first result. Reads also enforce chain-tip and whole-snapshot
    /// freshness before returning the cached value.
    pub fn new(
        config: &GatewayConfig,
        version: &'static str,
        commit_hash: Option<&'static str>,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HealthSnapshot::starting(
                config,
                version,
                commit_hash,
            ))),
            max_chain_tip_age_ms: config.health_chain_tip_max_age_ms,
            max_snapshot_age_ms: config.health_probe_interval_ms.saturating_mul(3).max(1),
            scheduler_required: backend_required(config, HealthBackend::Scheduler),
            version,
            commit_hash,
        }
    }

    /// Return the latest cached health snapshot with freshness checks applied.
    pub async fn snapshot(&self) -> HealthSnapshot {
        let mut snapshot = self.inner.read().await.clone();
        snapshot.refresh(
            self.max_chain_tip_age_ms,
            self.max_snapshot_age_ms,
            self.scheduler_required,
        );
        snapshot
    }

    async fn update(&self, snapshot: HealthSnapshot) {
        *self.inner.write().await = snapshot;
    }

    #[cfg(test)]
    pub(crate) async fn update_for_test(&self, snapshot: HealthSnapshot) {
        self.update(snapshot).await;
    }
}

/// Top-level readiness status.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Starting,
    Ok,
    Degraded,
    Error,
}

impl HealthStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Error => "error",
        }
    }
}

pub(crate) type GatewayHealthSnapshot = HealthSnapshot;

/// JSON body returned by readiness endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    service: &'static str,
    process_name: &'static str,
    pid: u32,
    executable_path: String,
    status: HealthStatus,
    version: &'static str,
    commit_hash: Option<&'static str>,
    uptime_seconds: u64,
    last_updated_timestamp: String,
    snapshot_age_ms: u64,
    chain_tip: ChainTipHealth,
    backends: BTreeMap<&'static str, BackendHealth>,
    #[serde(skip)]
    pub(crate) snapshot_at: Instant,
}

impl HealthSnapshot {
    fn starting(
        config: &GatewayConfig,
        version: &'static str,
        commit_hash: Option<&'static str>,
    ) -> Self {
        let mut backends = BTreeMap::new();
        for backend in HealthBackend::ALL {
            let configured = backend_configured(config, backend);
            let required = backend_required(config, backend);
            let last_error = if required && !configured {
                Some("required backend is not configured".to_string())
            } else {
                None
            };
            backends.insert(
                backend.as_str(),
                BackendHealth {
                    configured,
                    required,
                    up: false,
                    last_success_timestamp: None,
                    last_error,
                },
            );
        }

        Self {
            service: "hog-gateway",
            process_name: "hog-gateway",
            pid: std::process::id(),
            executable_path: executable_path(),
            status: HealthStatus::Starting,
            version,
            commit_hash,
            uptime_seconds: 0,
            last_updated_timestamp: timestamp_now(),
            snapshot_age_ms: 0,
            chain_tip: ChainTipHealth {
                height: None,
                age_ms: None,
            },
            backends,
            snapshot_at: Instant::now(),
        }
    }

    /// Current readiness status after applying cached-snapshot freshness rules.
    pub fn status(&self) -> HealthStatus {
        self.status
    }

    pub(crate) fn backend_up(&self, backend: HealthBackend) -> bool {
        self.backends
            .get(backend.as_str())
            .is_some_and(|health| health.up)
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn chain_tip_height(&self) -> Option<f64> {
        let height = self.chain_tip.height.as_ref()?;
        Some(serde_json::to_value(height).ok()?.as_i64()? as f64)
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn chain_tip_age_seconds(&self) -> Option<f64> {
        Some(self.chain_tip.age_ms? as f64 / 1000.0)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        status: HealthStatus,
        chain_tip: Option<(i64, u64)>,
        backend_ups: &[(HealthBackend, bool)],
    ) -> Self {
        let mut backends = BTreeMap::new();
        for backend in HealthBackend::ALL {
            let up = backend_ups
                .iter()
                .find(|(candidate, _)| *candidate == backend)
                .is_some_and(|(_, up)| *up);
            backends.insert(
                backend.as_str(),
                BackendHealth {
                    configured: true,
                    required: true,
                    up,
                    last_success_timestamp: None,
                    last_error: None,
                },
            );
        }

        let (height, age_ms) = chain_tip.map_or((None, None), |(height, age_ms)| {
            (Some(BlockHeight::from(height)), Some(age_ms))
        });

        Self {
            service: "hog-gateway",
            process_name: "hog-gateway",
            pid: std::process::id(),
            executable_path: executable_path(),
            status,
            version: "test",
            commit_hash: None,
            uptime_seconds: 0,
            last_updated_timestamp: timestamp_now(),
            snapshot_age_ms: 0,
            chain_tip: ChainTipHealth { height, age_ms },
            backends,
            snapshot_at: Instant::now(),
        }
    }

    fn refresh(
        &mut self,
        max_chain_tip_age_ms: u64,
        max_snapshot_age_ms: u64,
        scheduler_required: bool,
    ) {
        let elapsed_ms = u64::try_from(self.snapshot_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.snapshot_age_ms = elapsed_ms;
        if elapsed_ms > max_snapshot_age_ms {
            self.status = HealthStatus::Error;
        }
        if let Some(age_ms) = self.chain_tip.age_ms.as_mut() {
            *age_ms = age_ms.saturating_add(elapsed_ms);
            if scheduler_required && *age_ms > max_chain_tip_age_ms {
                self.status = HealthStatus::Error;
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChainTipHealth {
    height: Option<BlockHeight>,
    age_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct BackendHealth {
    configured: bool,
    required: bool,
    up: bool,
    last_success_timestamp: Option<String>,
    last_error: Option<String>,
}

/// Spawn the background readiness probe loop with an explicit shutdown signal.
pub fn spawn_health_probe_with_shutdown(
    ctx: Arc<GatewayContext>,
    health: GatewayHealth,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    let version = health.version;
    let commit_hash = health.commit_hash;
    tokio::spawn(async move {
        let started_at = Instant::now();
        let mut probe_state = ProbeState::default();
        let mut last_status = None;
        loop {
            let snapshot = tokio::select! {
                snapshot = probe_once(&ctx, started_at, version, commit_hash, &mut probe_state) => snapshot,
                _ = &mut shutdown => break,
            };
            let status = snapshot.status();
            if last_status == Some(status) {
                tracing::debug!(
                    status = status.as_str(),
                    "gateway readiness probe completed"
                );
            } else {
                tracing::info!(
                    status = status.as_str(),
                    previous_status = last_status.map(HealthStatus::as_str),
                    "gateway readiness status changed",
                );
                last_status = Some(status);
            }
            health.update(snapshot).await;
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(ctx.config.health_probe_interval_ms.max(1))) => {}
                _ = &mut shutdown => break,
            }
        }
    })
}

#[derive(Default)]
struct ProbeState {
    last_tip: Option<(BlockHeight, Instant)>,
    last_success: BTreeMap<HealthBackend, String>,
}

fn backend_probe_timeout(config: &GatewayConfig) -> Duration {
    Duration::from_millis(
        config
            .request_timeout_ms
            .min(config.health_probe_interval_ms)
            .max(1),
    )
}

fn backend_reconnect_timeout(config: &GatewayConfig) -> Duration {
    Duration::from_millis(config.backend_connect_timeout_ms.max(1))
}

fn scheduler_reconnect_grace(config: &GatewayConfig) -> Duration {
    Duration::from_millis(config.scheduler_reconnect_grace_ms)
}

async fn probe_once(
    ctx: &GatewayContext,
    started_at: Instant,
    version: &'static str,
    commit_hash: Option<&'static str>,
    state: &mut ProbeState,
) -> HealthSnapshot {
    let config = &ctx.config;
    let now = timestamp_now();
    let mut backends = BTreeMap::new();
    let probes = probe_backends(ctx).await;

    let scheduler = probes.scheduler;
    let indexer = probes.indexer;
    let db_gateway = probes.db_gateway;
    let view_gateway = probes.view_gateway;

    if let Some(tip) = scheduler.tip.as_ref() {
        state.last_tip = Some((tip.height.clone(), Instant::now()));
    }
    let chain_tip = state.last_tip.as_ref().map_or(
        ChainTipHealth {
            height: None,
            age_ms: None,
        },
        |(height, seen_at)| ChainTipHealth {
            height: Some(height.clone()),
            age_ms: Some(u64::try_from(seen_at.elapsed().as_millis()).unwrap_or(u64::MAX)),
        },
    );
    backends.insert(
        HealthBackend::Scheduler.as_str(),
        backend_health(
            config,
            HealthBackend::Scheduler,
            scheduler.up,
            scheduler.error,
            &now,
            &mut state.last_success,
        ),
    );

    backends.insert(
        HealthBackend::Indexer.as_str(),
        backend_health(
            config,
            HealthBackend::Indexer,
            indexer.up,
            indexer.error,
            &now,
            &mut state.last_success,
        ),
    );

    backends.insert(
        HealthBackend::DbGateway.as_str(),
        backend_health(
            config,
            HealthBackend::DbGateway,
            db_gateway.up,
            db_gateway.error,
            &now,
            &mut state.last_success,
        ),
    );

    backends.insert(
        HealthBackend::ViewGateway.as_str(),
        backend_health(
            config,
            HealthBackend::ViewGateway,
            view_gateway.up,
            view_gateway.error,
            &now,
            &mut state.last_success,
        ),
    );

    let status = readiness_status(config, &backends, &chain_tip);
    HealthSnapshot {
        service: "hog-gateway",
        process_name: "hog-gateway",
        pid: std::process::id(),
        executable_path: executable_path(),
        status,
        version,
        commit_hash,
        uptime_seconds: started_at.elapsed().as_secs(),
        last_updated_timestamp: now,
        snapshot_age_ms: 0,
        chain_tip,
        backends,
        snapshot_at: Instant::now(),
    }
}

struct BackendProbeResults {
    scheduler: SchedulerProbeResult,
    indexer: ProbeResult,
    db_gateway: ProbeResult,
    view_gateway: ProbeResult,
}

async fn probe_backends(ctx: &GatewayContext) -> BackendProbeResults {
    let config = &ctx.config;
    let probe_timeout = backend_probe_timeout(config);
    let reconnect_timeout = backend_reconnect_timeout(config);
    let protocol_version = config
        .hogd_protocol_override
        .as_deref()
        .unwrap_or(PROTOCOL_VERSION);
    let protocol: Protocol = StringM(protocol_version.as_bytes().to_vec());

    let (scheduler, indexer, db_gateway, view_gateway) = tokio::join!(
        probe_scheduler(ctx, probe_timeout, reconnect_timeout),
        probe_indexer(
            ctx,
            protocol_version,
            &protocol,
            probe_timeout,
            reconnect_timeout,
        ),
        probe_db_gateway(
            ctx,
            protocol_version,
            &protocol,
            probe_timeout,
            reconnect_timeout,
        ),
        probe_view_gateway(
            ctx,
            protocol_version,
            &protocol,
            probe_timeout,
            reconnect_timeout,
        ),
    );
    BackendProbeResults {
        scheduler,
        indexer,
        db_gateway,
        view_gateway,
    }
}

struct ProbeResult {
    up: bool,
    error: Option<String>,
}

struct SchedulerProbeResult {
    up: bool,
    tip: Option<ChainTip>,
    error: Option<String>,
}

async fn probe_scheduler(
    ctx: &GatewayContext,
    probe_timeout: Duration,
    reconnect_timeout: Duration,
) -> SchedulerProbeResult {
    // Bind the clone separately so the read guard is dropped before a
    // reconnect path tries to acquire the write lock. A `match` scrutinee
    // temporary lives through the selected arm and would self-deadlock here.
    let current_client = { ctx.scheduler.read().await.clone() };
    let Some(client) = current_client else {
        if let Some(result) = advance_pending_scheduler(ctx, probe_timeout).await {
            return result;
        }
        match tokio::time::timeout(
            reconnect_timeout,
            connect_optional_scheduler(
                &ctx.config.scheduler_address,
                scheduler_pool_size(&ctx.config),
            ),
        )
        .await
        {
            Err(_) => {
                return SchedulerProbeResult {
                    up: false,
                    tip: None,
                    error: Some("scheduler reconnect timed out".to_string()),
                };
            }
            Ok(Ok(Some(client))) => {
                let grace = scheduler_reconnect_grace(&ctx.config);
                *ctx.pending_scheduler.write().await = Some(PendingSchedulerReconnect {
                    client,
                    probe_not_before: Instant::now() + grace,
                });
                return SchedulerProbeResult {
                    up: false,
                    tip: None,
                    error: Some("scheduler connected; awaiting readiness grace".to_string()),
                };
            }
            Ok(Ok(None)) => {
                return SchedulerProbeResult {
                    up: false,
                    tip: None,
                    error: Some("scheduler not connected".to_string()),
                };
            }
            Ok(Err(err)) => {
                return SchedulerProbeResult {
                    up: false,
                    tip: None,
                    error: Some(bounded_error(err.to_string())),
                };
            }
        }
    };

    let started_at = Instant::now();
    let rpc_result = tokio::time::timeout(probe_timeout, client.schedcoord_get_tip()).await;
    let outcome = match &rpc_result {
        Err(_) => BackendRequestOutcome::Timeout,
        Ok(Ok(SchedulerGetTipResult::SchedulerGetTipOK { .. })) => BackendRequestOutcome::Ok,
        Ok(Ok(_)) => BackendRequestOutcome::Rejected,
        Ok(Err(_)) => BackendRequestOutcome::Error,
    };
    ctx.metrics.observe_backend_request(
        HealthBackend::Scheduler,
        "schedcoord_get_tip",
        outcome,
        started_at.elapsed(),
    );

    match rpc_result {
        Ok(Ok(SchedulerGetTipResult::SchedulerGetTipOK { db_tip })) => SchedulerProbeResult {
            up: true,
            tip: Some(ChainTip {
                height: db_tip.height,
                block_hash: Hex0x(db_tip.block_hash.into_inner()),
            }),
            error: None,
        },
        Ok(Ok(_)) => SchedulerProbeResult {
            up: false,
            tip: None,
            error: Some("scheduler returned no committed blocks".to_string()),
        },
        Ok(Err(err)) => {
            *ctx.scheduler.write().await = None;
            SchedulerProbeResult {
                up: false,
                tip: None,
                error: Some(bounded_error(err.to_string())),
            }
        }
        Err(_) => {
            *ctx.scheduler.write().await = None;
            SchedulerProbeResult {
                up: false,
                tip: None,
                error: Some("scheduler probe timed out".to_string()),
            }
        }
    }
}

async fn advance_pending_scheduler(
    ctx: &GatewayContext,
    probe_timeout: Duration,
) -> Option<SchedulerProbeResult> {
    let pending = { ctx.pending_scheduler.read().await.clone() }?;
    let null_result =
        tokio::time::timeout(probe_timeout, pending.client.schedulercoord_null()).await;
    if !matches!(null_result, Ok(Ok(()))) {
        *ctx.pending_scheduler.write().await = None;
        return Some(SchedulerProbeResult {
            up: false,
            tip: None,
            error: Some("pending scheduler connection unavailable".to_string()),
        });
    }
    if Instant::now() < pending.probe_not_before {
        return Some(SchedulerProbeResult {
            up: false,
            tip: None,
            error: Some("scheduler reconnect readiness grace active".to_string()),
        });
    }

    *ctx.pending_scheduler.write().await = None;
    *ctx.scheduler.write().await = Some(pending.client);
    Some(SchedulerProbeResult {
        up: false,
        tip: None,
        error: Some("scheduler reconnected; awaiting tip probe".to_string()),
    })
}

async fn probe_indexer(
    ctx: &GatewayContext,
    protocol_version: &str,
    protocol: &Protocol,
    probe_timeout: Duration,
    reconnect_timeout: Duration,
) -> ProbeResult {
    let current_client = { ctx.indexer.read().await.clone() };
    let Some(client) = current_client else {
        match connect_optional_indexer(
            ctx.config.indexer_address.as_deref(),
            Some(protocol_version),
            reconnect_timeout,
            Some(&ctx.metrics),
        )
        .await
        {
            Ok(Some(client)) => {
                *ctx.indexer.write().await = Some(client.clone());
                return ok_probe();
            }
            Ok(None) => {
                return ProbeResult {
                    up: false,
                    error: Some("indexer not connected".to_string()),
                };
            }
            Err(err) => return failed_probe(err.to_string()),
        }
    };
    let started_at = Instant::now();
    let rpc_result =
        tokio::time::timeout(probe_timeout, client.indexer_block_explorer_intro(protocol)).await;
    let outcome = match &rpc_result {
        Err(_) => BackendRequestOutcome::Timeout,
        Ok(Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)) => BackendRequestOutcome::Ok,
        Ok(Ok(_)) => BackendRequestOutcome::Rejected,
        Ok(Err(_)) => BackendRequestOutcome::Error,
    };
    ctx.metrics.observe_backend_request(
        HealthBackend::Indexer,
        "indexer_block_explorer_intro",
        outcome,
        started_at.elapsed(),
    );
    match rpc_result {
        Ok(Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)) => ok_probe(),
        Ok(Ok(outcome)) => failed_probe(format!("indexer intro rejected: {outcome}")),
        Ok(Err(err)) => {
            *ctx.indexer.write().await = None;
            failed_probe(format!("indexer intro failed: {err}"))
        }
        Err(_) => {
            *ctx.indexer.write().await = None;
            failed_probe("indexer probe timed out")
        }
    }
}

async fn probe_db_gateway(
    ctx: &GatewayContext,
    protocol_version: &str,
    protocol: &Protocol,
    probe_timeout: Duration,
    reconnect_timeout: Duration,
) -> ProbeResult {
    let current_client = { ctx.db_gateway.read().await.clone() };
    let Some(client) = current_client else {
        match connect_optional_db_gateway(
            ctx.config.db_gateway_address.as_deref(),
            db_gateway_pool_size(&ctx.config),
            Some(protocol_version),
            reconnect_timeout,
            Some(&ctx.metrics),
        )
        .await
        {
            Ok(Some(client)) => {
                *ctx.db_gateway.write().await = Some(client.clone());
                return ok_probe();
            }
            Ok(None) => {
                return ProbeResult {
                    up: false,
                    error: Some("db_gateway not connected".to_string()),
                };
            }
            Err(err) => return failed_probe(err.to_string()),
        }
    };
    let started_at = Instant::now();
    let rpc_result = tokio::time::timeout(probe_timeout, client.dbgateway_intro(protocol)).await;
    let outcome = match &rpc_result {
        Err(_) => BackendRequestOutcome::Timeout,
        Ok(Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)) => BackendRequestOutcome::Ok,
        Ok(Ok(_)) => BackendRequestOutcome::Rejected,
        Ok(Err(_)) => BackendRequestOutcome::Error,
    };
    ctx.metrics.observe_backend_request(
        HealthBackend::DbGateway,
        "dbgateway_intro",
        outcome,
        started_at.elapsed(),
    );
    match rpc_result {
        Ok(Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)) => ok_probe(),
        Ok(Ok(outcome)) => failed_probe(format!("db_gateway intro rejected: {outcome}")),
        Ok(Err(err)) => {
            *ctx.db_gateway.write().await = None;
            failed_probe(format!("db_gateway intro failed: {err}"))
        }
        Err(_) => {
            *ctx.db_gateway.write().await = None;
            failed_probe("db_gateway probe timed out")
        }
    }
}

async fn probe_view_gateway(
    ctx: &GatewayContext,
    protocol_version: &str,
    protocol: &Protocol,
    probe_timeout: Duration,
    reconnect_timeout: Duration,
) -> ProbeResult {
    let current_client = { ctx.view_gateway.read().await.clone() };
    let Some(client) = current_client else {
        match connect_optional_view_gateway(
            ctx.config.view_gateway_address.as_deref(),
            Some(protocol_version),
            reconnect_timeout,
            Some(&ctx.metrics),
        )
        .await
        {
            Ok(Some(client)) => {
                *ctx.view_gateway.write().await = Some(client.clone());
                return ok_probe();
            }
            Ok(None) => {
                return ProbeResult {
                    up: false,
                    error: Some("view_gateway not connected".to_string()),
                };
            }
            Err(err) => return failed_probe(err.to_string()),
        }
    };
    let started_at = Instant::now();
    let rpc_result = tokio::time::timeout(probe_timeout, client.viewgateway_intro(protocol)).await;
    let outcome = match &rpc_result {
        Err(_) => BackendRequestOutcome::Timeout,
        Ok(Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)) => BackendRequestOutcome::Ok,
        Ok(Ok(_)) => BackendRequestOutcome::Rejected,
        Ok(Err(_)) => BackendRequestOutcome::Error,
    };
    ctx.metrics.observe_backend_request(
        HealthBackend::ViewGateway,
        "viewgateway_intro",
        outcome,
        started_at.elapsed(),
    );
    match rpc_result {
        Ok(Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)) => ok_probe(),
        Ok(Ok(outcome)) => failed_probe(format!("view_gateway intro rejected: {outcome}")),
        Ok(Err(err)) => {
            *ctx.view_gateway.write().await = None;
            failed_probe(format!("view_gateway intro failed: {err}"))
        }
        Err(_) => {
            *ctx.view_gateway.write().await = None;
            failed_probe("view_gateway probe timed out")
        }
    }
}

fn ok_probe() -> ProbeResult {
    ProbeResult {
        up: true,
        error: None,
    }
}

fn failed_probe(message: impl Into<String>) -> ProbeResult {
    ProbeResult {
        up: false,
        error: Some(bounded_error(message.into())),
    }
}

fn backend_health(
    config: &GatewayConfig,
    backend: HealthBackend,
    up: bool,
    probe_error: Option<String>,
    timestamp: &str,
    last_success: &mut BTreeMap<HealthBackend, String>,
) -> BackendHealth {
    let configured = backend_configured(config, backend);
    let required = backend_required(config, backend);
    let last_error = if required && !configured {
        Some("required backend is not configured".to_string())
    } else if configured && !up {
        probe_error
    } else {
        None
    };
    if configured && up {
        last_success.insert(backend, timestamp.to_string());
    }

    BackendHealth {
        configured,
        required,
        up: configured && up,
        last_success_timestamp: last_success.get(&backend).cloned(),
        last_error,
    }
}

fn readiness_status(
    config: &GatewayConfig,
    backends: &BTreeMap<&'static str, BackendHealth>,
    chain_tip: &ChainTipHealth,
) -> HealthStatus {
    for backend in HealthBackend::ALL {
        let Some(health) = backends.get(backend.as_str()) else {
            return HealthStatus::Error;
        };
        if health.required && (!health.configured || !health.up) {
            return HealthStatus::Error;
        }
    }

    if backend_required(config, HealthBackend::Scheduler)
        && chain_tip
            .age_ms
            .is_none_or(|age| age > config.health_chain_tip_max_age_ms)
    {
        return HealthStatus::Error;
    }

    if backends
        .values()
        .any(|health| health.configured && !health.required && !health.up)
    {
        return HealthStatus::Degraded;
    }

    HealthStatus::Ok
}

fn backend_configured(config: &GatewayConfig, backend: HealthBackend) -> bool {
    match backend {
        HealthBackend::Scheduler => true,
        HealthBackend::Indexer => config.indexer_address.is_some(),
        HealthBackend::DbGateway => config.db_gateway_address.is_some(),
        HealthBackend::ViewGateway => config.view_gateway_address.is_some(),
    }
}

fn backend_required(config: &GatewayConfig, backend: HealthBackend) -> bool {
    config.health_required_backends.contains(&backend)
}

fn timestamp_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn executable_path() -> String {
    std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("hog-gateway"))
        .display()
        .to_string()
}

fn bounded_error(message: String) -> String {
    const MAX_LEN: usize = 240;
    if message.len() <= MAX_LEN {
        return message;
    }
    let boundary = message
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx <= MAX_LEN)
        .last()
        .unwrap_or(0);
    message[..boundary].to_string()
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use groundhog_indexer::{
        AddressTransactionQuery, EventPage as IdxEventPage, EventQuery,
        IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher,
        TransactionPage as IdxTransactionPage, TransactionQuery, TransactionRefPage, TxInfoResult,
    };
    use groundhog_scheduler::{
        CoordinatorRegistrationOutcome, DbSchedResult, DbTip, Nomination,
        ParallelSignedTransactionBatch, SchedulerCommitOutcome, SchedulerGetTipResult,
        SchedulerNominationResult, SchedulerValidationResult,
        sched_coord::{SchedulerCoord_SchedulerCoord1, SchedulerCoord_SchedulerCoord1_Dispatcher},
    };
    use groundhog_xdr::genesis::GenesisConfiguration;
    use groundhog_xdr::transaction::{
        OptSignedTransactionBatch, SignedTransactionHash, SignedTransactionHashBatch,
    };
    use rpc_driver::{ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;

    use crate::metrics::GatewayMetrics;

    use super::*;

    struct SlowIntroIndexer;

    struct StuckTipScheduler;

    #[async_trait]
    impl SchedulerCoord_SchedulerCoord1 for StuckTipScheduler {
        async fn schedulercoord_null(&self) -> Result<(), ProgramError> {
            Ok(())
        }

        async fn schedulercoord_intro_coord(
            &self,
            _card: groundhog_scheduler::CoordCard,
        ) -> Result<CoordinatorRegistrationOutcome, ProgramError> {
            panic!("health probe test should not call intro_coord")
        }

        async fn schedulercoord_submit_unvetted(
            &self,
            _batch: ParallelSignedTransactionBatch,
        ) -> Result<(), ProgramError> {
            panic!("health probe test should not submit")
        }

        async fn schedulercoord_submit_vetted(
            &self,
            _validator: groundhog_coordinator::discovery::ValidatorID,
            _batch: ParallelSignedTransactionBatch,
        ) -> Result<(), ProgramError> {
            panic!("health probe test should not submit")
        }

        async fn schedulercoord_nominate(
            &self,
            _tip: DbTip,
        ) -> Result<SchedulerNominationResult, ProgramError> {
            panic!("health probe test should not nominate")
        }

        async fn schedulercoord_validate(
            &self,
            _tip: DbTip,
            _nomination: Nomination,
        ) -> Result<SchedulerValidationResult, ProgramError> {
            panic!("health probe test should not validate")
        }

        async fn schedulercoord_commit_block(
            &self,
            _block: groundhog_scheduler::BlockID,
        ) -> Result<SchedulerCommitOutcome, ProgramError> {
            panic!("health probe test should not commit")
        }

        async fn schedcoord_get_tip(&self) -> Result<SchedulerGetTipResult, ProgramError> {
            std::future::pending().await
        }

        async fn schedcoord_load_genesis_configuration(
            &self,
            _genesis: GenesisConfiguration,
        ) -> Result<DbSchedResult, ProgramError> {
            panic!("health probe test should not load genesis")
        }

        async fn schedcoord_lookup_transactions(
            &self,
            _hashes: SignedTransactionHashBatch,
        ) -> Result<OptSignedTransactionBatch, ProgramError> {
            panic!("health probe test should not look up transactions")
        }
    }

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for SlowIntroIndexer {
        async fn indexer_block_explorer_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
        }

        async fn get_transaction_info(
            &self,
            _tx_hash: SignedTransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }

        async fn get_events(&self, _query: EventQuery) -> Result<IdxEventPage, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }

        async fn get_transactions(
            &self,
            _query: TransactionQuery,
        ) -> Result<IdxTransactionPage, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }

        async fn get_transactions_by_address(
            &self,
            _query: AddressTransactionQuery,
        ) -> Result<TransactionRefPage, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }
    }

    async fn start_slow_intro_indexer() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test indexer should bind");
        let addr = listener
            .local_addr()
            .expect("test indexer should have a local address");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(SlowIntroIndexer)),
        ));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    async fn start_stuck_tip_scheduler() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test scheduler should bind");
        let addr = listener
            .local_addr()
            .expect("test scheduler should have a local address");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(SchedulerCoord_SchedulerCoord1_Dispatcher::new(
            Box::new(StuckTipScheduler),
        )));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    #[test]
    fn required_missing_backend_is_error() {
        let config = GatewayConfig {
            health_required_backends: vec![HealthBackend::DbGateway],
            db_gateway_address: None,
            ..GatewayConfig::default()
        };
        let snapshot = HealthSnapshot::starting(&config, "test", None);

        assert_eq!(snapshot.status(), HealthStatus::Starting);
        let db = snapshot
            .backends
            .get("db_gateway")
            .expect("db_gateway should be present");
        assert!(db.required);
        assert!(!db.configured);
        assert_eq!(
            db.last_error.as_deref(),
            Some("required backend is not configured")
        );
    }

    #[test]
    fn optional_configured_backend_down_is_degraded() {
        let config = GatewayConfig {
            db_gateway_address: Some("127.0.0.1:1".to_string()),
            health_required_backends: vec![HealthBackend::Scheduler, HealthBackend::Indexer],
            ..GatewayConfig::default()
        };
        let mut backends = BTreeMap::new();
        for backend in HealthBackend::ALL {
            backends.insert(
                backend.as_str(),
                BackendHealth {
                    configured: backend != HealthBackend::ViewGateway,
                    required: backend_required(&config, backend),
                    up: backend != HealthBackend::DbGateway
                        && backend != HealthBackend::ViewGateway,
                    last_success_timestamp: None,
                    last_error: None,
                },
            );
        }

        let status = readiness_status(
            &config,
            &backends,
            &ChainTipHealth {
                height: Some(BlockHeight::from(1i64)),
                age_ms: Some(0),
            },
        );

        assert_eq!(status, HealthStatus::Degraded);
    }

    #[test]
    fn backend_reconnect_timeout_is_independent_of_probe_interval() {
        let config = GatewayConfig {
            request_timeout_ms: 60_000,
            health_probe_interval_ms: 5_000,
            backend_connect_timeout_ms: 30_000,
            scheduler_reconnect_grace_ms: 20_000,
            ..GatewayConfig::default()
        };

        assert_eq!(backend_probe_timeout(&config), Duration::from_secs(5));
        assert_eq!(backend_reconnect_timeout(&config), Duration::from_secs(30));
        assert_eq!(scheduler_reconnect_grace(&config), Duration::from_secs(20));
    }

    #[tokio::test]
    async fn indexer_reconnect_can_outlive_one_probe_interval() {
        let (indexer_addr, _server) = start_slow_intro_indexer().await;
        let config = Arc::new(GatewayConfig {
            indexer_address: Some(indexer_addr.to_string()),
            db_gateway_address: None,
            view_gateway_address: None,
            request_timeout_ms: 1_000,
            health_probe_interval_ms: 10,
            backend_connect_timeout_ms: 500,
            ..GatewayConfig::default()
        });
        let metrics = GatewayMetrics::new("test", None).expect("test metrics should initialize");
        let ctx = GatewayContext::assemble(
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config,
            metrics,
        );
        let protocol: Protocol = StringM(PROTOCOL_VERSION.as_bytes().to_vec());

        let result = probe_indexer(
            &ctx,
            PROTOCOL_VERSION,
            &protocol,
            Duration::from_millis(10),
            Duration::from_millis(500),
        )
        .await;

        assert!(result.up, "reconnect should use the longer connect budget");
        assert!(ctx.indexer.read().await.is_some());
    }

    #[tokio::test]
    async fn scheduler_reconnect_grace_prevents_early_tip_probe() {
        let (scheduler_addr, _server) = start_stuck_tip_scheduler().await;
        let config = Arc::new(GatewayConfig {
            scheduler_address: scheduler_addr.to_string(),
            indexer_address: None,
            db_gateway_address: None,
            view_gateway_address: None,
            scheduler_reconnect_grace_ms: 25,
            ..GatewayConfig::default()
        });
        let metrics = GatewayMetrics::new("test", None).expect("test metrics should initialize");
        let ctx = GatewayContext::assemble(
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config,
            metrics,
        );

        let reconnect_result = tokio::time::timeout(
            Duration::from_secs(1),
            probe_scheduler(&ctx, Duration::from_millis(25), Duration::from_millis(500)),
        )
        .await
        .expect("scheduler probe should respect its timeout");

        assert!(!reconnect_result.up);
        assert_eq!(
            reconnect_result.error.as_deref(),
            Some("scheduler connected; awaiting readiness grace")
        );
        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.pending_scheduler.read().await.is_some());

        let grace_result =
            probe_scheduler(&ctx, Duration::from_millis(25), Duration::from_millis(500)).await;
        assert!(!grace_result.up);
        assert_eq!(
            grace_result.error.as_deref(),
            Some("scheduler reconnect readiness grace active")
        );
        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.pending_scheduler.read().await.is_some());

        tokio::time::sleep(Duration::from_millis(30)).await;
        let promote_result =
            probe_scheduler(&ctx, Duration::from_millis(25), Duration::from_millis(500)).await;
        assert!(!promote_result.up);
        assert_eq!(
            promote_result.error.as_deref(),
            Some("scheduler reconnected; awaiting tip probe")
        );
        assert!(ctx.scheduler.read().await.is_some());
        assert!(ctx.pending_scheduler.read().await.is_none());

        let probe_result = tokio::time::timeout(
            Duration::from_secs(1),
            probe_scheduler(&ctx, Duration::from_millis(25), Duration::from_millis(500)),
        )
        .await
        .expect("post-grace scheduler probe should respect its timeout");
        assert!(!probe_result.up);
        assert_eq!(
            probe_result.error.as_deref(),
            Some("scheduler probe timed out")
        );
        assert!(ctx.scheduler.read().await.is_none());
    }

    #[tokio::test]
    async fn stale_snapshot_is_error_even_when_backends_are_healthy() {
        let config = GatewayConfig {
            health_probe_interval_ms: 10,
            ..GatewayConfig::default()
        };
        let health = GatewayHealth::new(&config, "test", None);
        let mut snapshot = HealthSnapshot::for_test(
            HealthStatus::Ok,
            Some((1, 0)),
            &[
                (HealthBackend::Scheduler, true),
                (HealthBackend::Indexer, true),
            ],
        );
        snapshot.snapshot_at = Instant::now()
            .checked_sub(Duration::from_millis(31))
            .expect("test duration should be in range");

        health.update(snapshot).await;
        let stale = health.snapshot().await;

        assert_eq!(stale.status(), HealthStatus::Error);
        assert!(stale.snapshot_age_ms >= 31);
    }
}
