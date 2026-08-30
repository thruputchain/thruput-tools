use std::{collections::BTreeMap, future::Future, path::PathBuf, sync::Arc, time::Duration};

use groundhog_build_info::BuildVersion;
use groundhog_db_gateway::DbGatewayIntroOutcome;
use groundhog_indexer::IndexerBlockExplorerIntroOutcome;
use groundhog_json_rpc_types::{
    BlockHeight, NetworkHashHex,
    block::{ChainTip, consensus_value_hash},
};
use groundhog_scheduler::{DbTip, sched_coord::SchedulerGetTipResult};
use groundhog_scp::network::ProtocolCommit as Protocol;
use groundhog_view::ViewGatewayIntroOutcome;
use groundhog_x23_transport::ConnectError;
use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{sync::RwLock, task::JoinHandle, time::Instant};

use crate::{
    airdrop::HostedAirdrop,
    config::{GatewayConfig, HealthBackend},
    context::{
        GatewayContext, QuarantinedScheduler, clear_scheduler_if_current,
        connect_optional_db_gateway, connect_optional_indexer, connect_optional_scheduler,
        connect_optional_view_gateway, db_gateway_pool_size, scheduler_pool_size,
    },
    metrics::{BackendReconnectOutcome, BackendRequestOutcome},
};

/// Cached health/readiness state shared by `/readyz` and `/health`.
#[derive(Debug, Clone)]
pub struct GatewayHealth {
    inner: Arc<RwLock<HealthSnapshot>>,
    #[cfg(test)]
    updated: Arc<tokio::sync::Notify>,
    max_chain_tip_age_ms: u64,
    max_snapshot_age_ms: u64,
    scheduler_required: bool,
    build_version: BuildVersion,
    commit_hash: Option<&'static str>,
    airdrop: Option<HostedAirdrop>,
}

impl GatewayHealth {
    /// Create cached readiness state initialized to `starting`.
    ///
    /// The snapshot remains `starting` until the background probe loop publishes
    /// its first result. Reads also enforce chain-tip and whole-snapshot
    /// freshness before returning the cached value.
    #[cfg(test)]
    pub fn new(
        config: &GatewayConfig,
        build_version: BuildVersion,
        commit_hash: Option<&'static str>,
    ) -> Self {
        Self::new_with_airdrop(config, build_version, commit_hash, None)
    }

    pub(crate) fn new_with_airdrop(
        config: &GatewayConfig,
        build_version: BuildVersion,
        commit_hash: Option<&'static str>,
        airdrop: Option<HostedAirdrop>,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HealthSnapshot::starting(
                config,
                build_version,
                commit_hash,
            ))),
            #[cfg(test)]
            updated: Arc::new(tokio::sync::Notify::new()),
            max_chain_tip_age_ms: config.health_chain_tip_max_age_ms,
            max_snapshot_age_ms: config.health_probe_interval_ms.saturating_mul(3).max(1),
            scheduler_required: backend_required(config, HealthBackend::Scheduler),
            build_version,
            commit_hash,
            airdrop,
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
        {
            *self.inner.write().await = snapshot;
        }
        #[cfg(test)]
        self.updated.notify_one();
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
    #[serde(skip_serializing_if = "Option::is_none")]
    platform_version: Option<&'static str>,
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
        build_version: BuildVersion,
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
                    scheduler_identity: SchedulerIdentityStatus::Unverified,
                },
            );
        }
        if config.faucet_url.is_some() {
            backends.insert(
                "faucet",
                BackendHealth {
                    configured: true,
                    required: true,
                    up: false,
                    last_success_timestamp: None,
                    last_error: None,
                    scheduler_identity: SchedulerIdentityStatus::Unverified,
                },
            );
        }

        Self {
            service: "hog-gateway",
            process_name: "hog-gateway",
            pid: std::process::id(),
            executable_path: executable_path(),
            status: HealthStatus::Starting,
            version: build_version.crate_version(),
            platform_version: build_version.platform_version(),
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

    pub(crate) fn named_backend_up(&self, backend: &'static str) -> Option<bool> {
        self.backends.get(backend).map(|health| health.up)
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
                    scheduler_identity: SchedulerIdentityStatus::Unverified,
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
            platform_version: None,
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
    #[serde(skip)]
    scheduler_identity: SchedulerIdentityStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SchedulerIdentityStatus {
    #[default]
    Unverified,
    Validated,
    Mismatch,
}

/// Spawn the background readiness probe loop with an explicit shutdown signal.
pub fn spawn_health_probe_with_shutdown(
    ctx: Arc<GatewayContext>,
    health: GatewayHealth,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    let build_version = health.build_version;
    let commit_hash = health.commit_hash;
    let airdrop = health.airdrop.clone();
    tokio::spawn(async move {
        let started_at = Instant::now();
        let mut probe_state = ProbeState::new(&ctx.config);
        let mut reconnects = ReconnectAttempts::default();
        let mut last_status = None;
        loop {
            let snapshot = tokio::select! {
                snapshot = probe_once(&ctx, airdrop.as_ref(), started_at, build_version, commit_hash, &mut probe_state) => snapshot,
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
            maintain_reconnects(&ctx, &mut reconnects).await;
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(ctx.config.health_probe_interval_ms.max(1))) => {}
                _ = &mut shutdown => break,
            }
        }
        reconnects.shutdown().await;
    })
}

#[derive(Default)]
struct ProbeState {
    last_tip: Option<(BlockHeight, Instant)>,
    last_success: BTreeMap<HealthBackend, String>,
    scheduler: SchedulerProbeState,
    timeouts: ProbeTimeouts,
    last_faucet_success: Option<String>,
}

impl ProbeState {
    fn new(config: &GatewayConfig) -> Self {
        Self {
            scheduler: SchedulerProbeState::with_expected_network_hash(config.network_hash.clone()),
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct SchedulerProbeState {
    timeout_streak: u8,
    pending: Option<PendingSchedulerProbe>,
    expected_network_hash: Option<NetworkHashHex>,
    identity_status: SchedulerIdentityStatus,
}

impl SchedulerProbeState {
    fn with_expected_network_hash(expected_network_hash: Option<NetworkHashHex>) -> Self {
        Self {
            timeout_streak: 0,
            pending: None,
            expected_network_hash,
            identity_status: SchedulerIdentityStatus::Unverified,
        }
    }
}

struct PendingSchedulerProbe {
    handle: JoinHandle<Result<SchedulerGetTipResult, rpc_driver::RpcError>>,
    started_at: Instant,
    generation: u64,
}

impl Drop for SchedulerProbeState {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.as_ref() {
            pending.handle.abort();
        }
    }
}

#[derive(Default)]
struct ProbeTimeouts {
    indexer: u8,
    db_gateway: u8,
    view_gateway: u8,
}

#[derive(Default)]
struct ReconnectAttempts {
    scheduler: Option<JoinHandle<()>>,
    indexer: Option<JoinHandle<()>>,
    db_gateway: Option<JoinHandle<()>>,
    view_gateway: Option<JoinHandle<()>>,
}

impl ReconnectAttempts {
    async fn reap_finished(&mut self) {
        reap_finished(&mut self.scheduler, HealthBackend::Scheduler).await;
        reap_finished(&mut self.indexer, HealthBackend::Indexer).await;
        reap_finished(&mut self.db_gateway, HealthBackend::DbGateway).await;
        reap_finished(&mut self.view_gateway, HealthBackend::ViewGateway).await;
    }

    async fn shutdown(&mut self) {
        for attempt in [
            &mut self.scheduler,
            &mut self.indexer,
            &mut self.db_gateway,
            &mut self.view_gateway,
        ] {
            if let Some(handle) = attempt.take() {
                handle.abort();
                let _ = handle.await;
            }
        }
    }
}

impl Drop for ReconnectAttempts {
    fn drop(&mut self) {
        for handle in [
            self.scheduler.as_ref(),
            self.indexer.as_ref(),
            self.db_gateway.as_ref(),
            self.view_gateway.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            handle.abort();
        }
    }
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

fn reconnect_error_outcome(error: &ConnectError) -> BackendReconnectOutcome {
    if matches!(error, ConnectError::StartupProbeTimedOut { .. }) {
        BackendReconnectOutcome::Timeout
    } else {
        BackendReconnectOutcome::Error
    }
}

async fn reap_finished(attempt: &mut Option<JoinHandle<()>>, backend: HealthBackend) {
    if !attempt.as_ref().is_some_and(JoinHandle::is_finished) {
        return;
    }
    let handle = attempt
        .take()
        .expect("finished reconnect attempt should still be present");
    if let Err(err) = handle.await {
        tracing::warn!(backend = %backend, error = %err, "backend reconnect task failed");
    }
}

fn spawn_once<F, Fut>(attempt: &mut Option<JoinHandle<()>>, make_future: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    if attempt.is_none() {
        *attempt = Some(tokio::spawn(make_future()));
    }
}

#[allow(clippy::too_many_lines)]
async fn maintain_reconnects(ctx: &Arc<GatewayContext>, attempts: &mut ReconnectAttempts) {
    attempts.reap_finished().await;
    let reconnect_timeout = backend_reconnect_timeout(&ctx.config);

    let scheduler_absent = ctx.scheduler.read().await.is_none();
    let scheduler_quarantine_empty = ctx.scheduler_quarantine.read().await.is_none();
    if scheduler_absent && scheduler_quarantine_empty {
        let ctx = ctx.clone();
        spawn_once(&mut attempts.scheduler, move || async move {
            let result = tokio::time::timeout(
                reconnect_timeout,
                connect_optional_scheduler(
                    &ctx.config.scheduler_address,
                    scheduler_pool_size(&ctx.config),
                ),
            )
            .await;
            match result {
                Ok(Ok(Some(client))) => {
                    if ctx.scheduler.read().await.is_none() {
                        let mut quarantine = ctx.scheduler_quarantine.write().await;
                        if quarantine.is_none() {
                            *quarantine = Some(QuarantinedScheduler {
                                client,
                                ready_at: Instant::now() + scheduler_reconnect_grace(&ctx.config),
                            });
                            ctx.metrics.observe_backend_reconnect(
                                HealthBackend::Scheduler,
                                BackendReconnectOutcome::Success,
                            );
                            tracing::info!(
                                backend = "scheduler",
                                "backend connection re-established; readiness grace started"
                            );
                        }
                    }
                }
                Ok(Ok(None)) => ctx.metrics.observe_backend_reconnect(
                    HealthBackend::Scheduler,
                    BackendReconnectOutcome::Error,
                ),
                Ok(Err(err)) => {
                    ctx.metrics.observe_backend_reconnect(
                        HealthBackend::Scheduler,
                        BackendReconnectOutcome::Error,
                    );
                    tracing::warn!(backend = "scheduler", error = %err, "backend reconnect failed");
                }
                Err(_) => {
                    ctx.metrics.observe_backend_reconnect(
                        HealthBackend::Scheduler,
                        BackendReconnectOutcome::Timeout,
                    );
                    tracing::warn!(
                        backend = "scheduler",
                        timeout = ?reconnect_timeout,
                        "backend reconnect timed out"
                    );
                }
            }
        });
    }

    if ctx.config.indexer_address.is_some() && ctx.indexer.read().await.is_none() {
        let ctx = ctx.clone();
        spawn_once(&mut attempts.indexer, move || async move {
            let result = connect_optional_indexer(
                ctx.config.indexer_address.as_deref(),
                Some(ctx.protocol),
                reconnect_timeout,
                Some(&ctx.metrics),
            )
            .await;
            match result {
                Ok(Some(client)) => {
                    let mut slot = ctx.indexer.write().await;
                    if slot.is_none() {
                        *slot = Some(client);
                        ctx.metrics.observe_backend_reconnect(
                            HealthBackend::Indexer,
                            BackendReconnectOutcome::Success,
                        );
                        tracing::info!(backend = "indexer", "backend reconnect completed");
                    }
                }
                Ok(None) => ctx.metrics.observe_backend_reconnect(
                    HealthBackend::Indexer,
                    BackendReconnectOutcome::Error,
                ),
                Err(err) => {
                    ctx.metrics.observe_backend_reconnect(
                        HealthBackend::Indexer,
                        reconnect_error_outcome(&err),
                    );
                    tracing::warn!(backend = "indexer", error = %err, "backend reconnect failed");
                }
            }
        });
    }

    if ctx.config.db_gateway_address.is_some() && ctx.db_gateway.read().await.is_none() {
        let ctx = ctx.clone();
        spawn_once(&mut attempts.db_gateway, move || async move {
            let result = connect_optional_db_gateway(
                ctx.config.db_gateway_address.as_deref(),
                db_gateway_pool_size(&ctx.config),
                Some(ctx.protocol),
                reconnect_timeout,
                Some(&ctx.metrics),
            )
            .await;
            match result {
                Ok(Some(client)) => {
                    let mut slot = ctx.db_gateway.write().await;
                    if slot.is_none() {
                        *slot = Some(client);
                        ctx.metrics.observe_backend_reconnect(
                            HealthBackend::DbGateway,
                            BackendReconnectOutcome::Success,
                        );
                        tracing::info!(backend = "db_gateway", "backend reconnect completed");
                    }
                }
                Ok(None) => ctx.metrics.observe_backend_reconnect(
                    HealthBackend::DbGateway,
                    BackendReconnectOutcome::Error,
                ),
                Err(err) => {
                    ctx.metrics.observe_backend_reconnect(
                        HealthBackend::DbGateway,
                        reconnect_error_outcome(&err),
                    );
                    tracing::warn!(backend = "db_gateway", error = %err, "backend reconnect failed");
                }
            }
        });
    }

    if ctx.config.view_gateway_address.is_some() && ctx.view_gateway.read().await.is_none() {
        let ctx = ctx.clone();
        spawn_once(&mut attempts.view_gateway, move || async move {
            let result = connect_optional_view_gateway(
                ctx.config.view_gateway_address.as_deref(),
                Some(ctx.protocol),
                reconnect_timeout,
                Some(&ctx.metrics),
            )
            .await;
            match result {
                Ok(Some(client)) => {
                    let mut slot = ctx.view_gateway.write().await;
                    if slot.is_none() {
                        *slot = Some(client);
                        ctx.metrics.observe_backend_reconnect(
                            HealthBackend::ViewGateway,
                            BackendReconnectOutcome::Success,
                        );
                        tracing::info!(backend = "view_gateway", "backend reconnect completed");
                    }
                }
                Ok(None) => ctx.metrics.observe_backend_reconnect(
                    HealthBackend::ViewGateway,
                    BackendReconnectOutcome::Error,
                ),
                Err(err) => {
                    ctx.metrics.observe_backend_reconnect(
                        HealthBackend::ViewGateway,
                        reconnect_error_outcome(&err),
                    );
                    tracing::warn!(backend = "view_gateway", error = %err, "backend reconnect failed");
                }
            }
        });
    }
}

#[allow(clippy::too_many_lines)]
async fn probe_once(
    ctx: &GatewayContext,
    airdrop: Option<&HostedAirdrop>,
    started_at: Instant,
    build_version: BuildVersion,
    commit_hash: Option<&'static str>,
    state: &mut ProbeState,
) -> HealthSnapshot {
    let config = &ctx.config;
    let mut backends = BTreeMap::new();
    let probes = Box::pin(probe_backends(
        ctx,
        airdrop,
        &mut state.scheduler,
        &mut state.timeouts,
    ))
    .await;
    let now = timestamp_now();

    let scheduler = probes.scheduler.result;
    let indexer = probes.indexer.result;
    let db_gateway = probes.db_gateway.result;
    let view_gateway = probes.view_gateway.result;
    let faucet = probes.faucet.result;

    if let Some(tip) = scheduler.tip.as_ref() {
        state.last_tip = Some((
            tip.height.clone(),
            scheduler
                .tip_observed_at
                .expect("successful scheduler tip should record its observation time"),
        ));
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
    let mut scheduler_health = backend_health(
        config,
        HealthBackend::Scheduler,
        scheduler.up,
        scheduler.error,
        &probes.scheduler.completed_at,
        &mut state.last_success,
    );
    scheduler_health.scheduler_identity = scheduler.identity_status;
    backends.insert(HealthBackend::Scheduler.as_str(), scheduler_health);

    backends.insert(
        HealthBackend::Indexer.as_str(),
        backend_health(
            config,
            HealthBackend::Indexer,
            indexer.up,
            indexer.error,
            &probes.indexer.completed_at,
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
            &probes.db_gateway.completed_at,
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
            &probes.view_gateway.completed_at,
            &mut state.last_success,
        ),
    );

    if config.faucet_url.is_some() {
        if faucet.up {
            state.last_faucet_success = Some(probes.faucet.completed_at.clone());
        }
        backends.insert(
            "faucet",
            BackendHealth {
                configured: true,
                required: true,
                up: faucet.up,
                last_success_timestamp: state.last_faucet_success.clone(),
                last_error: faucet.error,
                scheduler_identity: SchedulerIdentityStatus::Unverified,
            },
        );
    }

    let status = readiness_status(config, &backends, &chain_tip);
    HealthSnapshot {
        service: "hog-gateway",
        process_name: "hog-gateway",
        pid: std::process::id(),
        executable_path: executable_path(),
        status,
        version: build_version.crate_version(),
        platform_version: build_version.platform_version(),
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
    scheduler: TimedProbe<SchedulerProbeResult>,
    indexer: TimedProbe<ProbeResult>,
    db_gateway: TimedProbe<ProbeResult>,
    view_gateway: TimedProbe<ProbeResult>,
    faucet: TimedProbe<ProbeResult>,
}

struct TimedProbe<T> {
    result: T,
    completed_at: String,
}

async fn timestamp_probe<T>(future: impl Future<Output = T>) -> TimedProbe<T> {
    TimedProbe {
        result: future.await,
        completed_at: timestamp_now(),
    }
}

async fn probe_backends(
    ctx: &GatewayContext,
    airdrop: Option<&HostedAirdrop>,
    scheduler_state: &mut SchedulerProbeState,
    timeouts: &mut ProbeTimeouts,
) -> BackendProbeResults {
    let config = &ctx.config;
    let probe_timeout = backend_probe_timeout(config);
    let protocol = ctx.protocol;

    let ProbeTimeouts {
        indexer: indexer_timeouts,
        db_gateway: db_gateway_timeouts,
        view_gateway: view_gateway_timeouts,
    } = timeouts;
    let (scheduler, indexer, db_gateway, view_gateway, faucet) = tokio::join!(
        timestamp_probe(probe_scheduler(ctx, probe_timeout, scheduler_state)),
        timestamp_probe(probe_indexer(
            ctx,
            &protocol,
            probe_timeout,
            indexer_timeouts,
        )),
        timestamp_probe(probe_db_gateway(
            ctx,
            &protocol,
            probe_timeout,
            db_gateway_timeouts,
        )),
        timestamp_probe(probe_view_gateway(
            ctx,
            &protocol,
            probe_timeout,
            view_gateway_timeouts,
        )),
        timestamp_probe(probe_faucet(airdrop, probe_timeout)),
    );
    BackendProbeResults {
        scheduler,
        indexer,
        db_gateway,
        view_gateway,
        faucet,
    }
}

async fn probe_faucet(airdrop: Option<&HostedAirdrop>, timeout: Duration) -> ProbeResult {
    let Some(airdrop) = airdrop else {
        return failed_probe("faucet not configured");
    };
    let result = airdrop.probe(timeout).await;
    ProbeResult {
        up: result.up,
        error: result.error.map(bounded_error),
    }
}

struct ProbeResult {
    up: bool,
    error: Option<String>,
}

struct SchedulerProbeResult {
    up: bool,
    tip: Option<ChainTip>,
    tip_observed_at: Option<Instant>,
    error: Option<String>,
    identity_status: SchedulerIdentityStatus,
}

fn repeated_probe_timeout(streak: &mut u8) -> bool {
    *streak = streak.saturating_add(1);
    if *streak < 2 {
        return false;
    }
    *streak = 0;
    true
}

#[allow(clippy::too_many_lines)]
async fn probe_scheduler(
    ctx: &GatewayContext,
    probe_timeout: Duration,
    state: &mut SchedulerProbeState,
) -> SchedulerProbeResult {
    // Bind the clone separately so the read guard is dropped before a
    // reconnect path tries to acquire the write lock. A `match` scrutinee
    // temporary lives through the selected arm and would self-deadlock here.
    let current_client = { ctx.scheduler.read().await.clone() };
    let Some(client) = current_client else {
        state.timeout_streak = 0;
        if let Some(pending) = state.pending.take() {
            pending.handle.abort();
        }
        if let Some(result) = probe_quarantined_scheduler(ctx, probe_timeout, state).await {
            return result;
        }
        return SchedulerProbeResult {
            up: false,
            tip: None,
            tip_observed_at: None,
            error: Some("scheduler not connected".to_string()),
            identity_status: state.identity_status,
        };
    };
    let generation = client.generation();
    if state
        .pending
        .as_ref()
        .is_some_and(|pending| pending.generation != generation)
    {
        if let Some(pending) = state.pending.take() {
            pending.handle.abort();
        }
        state.timeout_streak = 0;
    }

    let (rpc_result, started_at, record_outcome) = if let Some(mut pending) = state.pending.take() {
        if let Ok(result) = tokio::time::timeout(probe_timeout, &mut pending.handle).await {
            (
                result.unwrap_or(Err(rpc_driver::RpcError::SystemError)),
                pending.started_at,
                false,
            )
        } else {
            clear_scheduler_if_current(&ctx.scheduler, pending.generation).await;
            pending.handle.abort();
            let _ = pending.handle.await;
            state.timeout_streak = 0;
            return SchedulerProbeResult {
                up: false,
                tip: None,
                tip_observed_at: None,
                error: Some("scheduler probe repeatedly timed out; reconnecting".to_string()),
                identity_status: state.identity_status,
            };
        }
    } else {
        let started_at = Instant::now();
        let probe_client = client.probe_client().clone();
        let mut handle = tokio::spawn(async move { probe_client.schedcoord_get_tip().await });
        if let Ok(result) = tokio::time::timeout(probe_timeout, &mut handle).await {
            (
                result.unwrap_or(Err(rpc_driver::RpcError::SystemError)),
                started_at,
                true,
            )
        } else {
            ctx.metrics.observe_backend_request(
                HealthBackend::Scheduler,
                "schedcoord_get_tip",
                BackendRequestOutcome::Timeout,
                started_at.elapsed(),
            );
            state.timeout_streak = 1;
            state.pending = Some(PendingSchedulerProbe {
                handle,
                started_at,
                generation,
            });
            return SchedulerProbeResult {
                up: false,
                tip: None,
                tip_observed_at: None,
                error: Some("scheduler probe timed out".to_string()),
                identity_status: state.identity_status,
            };
        }
    };
    let outcome = match &rpc_result {
        Ok(SchedulerGetTipResult::SchedulerGetTipOK { .. }) => BackendRequestOutcome::Ok,
        Ok(_) => BackendRequestOutcome::Rejected,
        Err(_) => BackendRequestOutcome::Error,
    };
    if record_outcome {
        ctx.metrics.observe_backend_request(
            HealthBackend::Scheduler,
            "schedcoord_get_tip",
            outcome,
            started_at.elapsed(),
        );
    }

    match rpc_result {
        Ok(SchedulerGetTipResult::SchedulerGetTipOK { db_tip }) => {
            state.timeout_streak = 0;
            let result = successful_scheduler_probe(&db_tip, state);
            if result.identity_status == SchedulerIdentityStatus::Mismatch {
                clear_scheduler_if_current(&ctx.scheduler, generation).await;
            }
            result
        }
        Ok(SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock) => {
            state.timeout_streak = 0;
            SchedulerProbeResult {
                up: false,
                tip: None,
                tip_observed_at: None,
                error: Some("scheduler returned no committed blocks".to_string()),
                identity_status: state.identity_status,
            }
        }
        Ok(other) => {
            state.timeout_streak = 0;
            SchedulerProbeResult {
                up: false,
                tip: None,
                tip_observed_at: None,
                error: Some(format!("unexpected scheduler tip result: {other:?}")),
                identity_status: state.identity_status,
            }
        }
        Err(err) => {
            state.timeout_streak = 0;
            clear_scheduler_if_current(&ctx.scheduler, generation).await;
            SchedulerProbeResult {
                up: false,
                tip: None,
                tip_observed_at: None,
                error: Some(bounded_error(err.to_string())),
                identity_status: state.identity_status,
            }
        }
    }
}

fn successful_scheduler_probe(
    db_tip: &DbTip,
    state: &mut SchedulerProbeState,
) -> SchedulerProbeResult {
    let observed = canonical_network_hash(
        db_tip
            .value
            .proposal_header
            .network_hash
            .clone()
            .into_inner()
            .as_ref(),
    );
    if let Some(expected) = state.expected_network_hash.as_ref() {
        if expected != &observed {
            state.identity_status = SchedulerIdentityStatus::Mismatch;
            tracing::error!(
                expected_network_hash = %expected,
                observed_network_hash = %observed,
                "scheduler network identity mismatch"
            );
            return SchedulerProbeResult {
                up: false,
                tip: None,
                tip_observed_at: None,
                error: Some(format!(
                    "scheduler network hash mismatch: expected {expected}, observed {observed}"
                )),
                identity_status: SchedulerIdentityStatus::Mismatch,
            };
        }
    } else {
        state.expected_network_hash = Some(observed);
    }
    state.identity_status = SchedulerIdentityStatus::Validated;

    match consensus_value_hash(&db_tip.value) {
        Ok(block_hash) => SchedulerProbeResult {
            up: true,
            tip: Some(ChainTip {
                height: db_tip.value.proposal_header.block_height.clone(),
                block_hash,
            }),
            tip_observed_at: Some(Instant::now()),
            error: None,
            identity_status: SchedulerIdentityStatus::Validated,
        },
        Err(error) => SchedulerProbeResult {
            up: false,
            tip: None,
            tip_observed_at: None,
            error: Some(format!(
                "scheduler returned an unencodable consensus value: {error}"
            )),
            identity_status: SchedulerIdentityStatus::Validated,
        },
    }
}

fn canonical_network_hash(bytes: &[u8]) -> NetworkHashHex {
    let mut hash = [0; 32];
    hash.copy_from_slice(bytes);
    NetworkHashHex::from(hash)
}

async fn probe_quarantined_scheduler(
    ctx: &GatewayContext,
    probe_timeout: Duration,
    state: &mut SchedulerProbeState,
) -> Option<SchedulerProbeResult> {
    let quarantined = { ctx.scheduler_quarantine.read().await.clone() }?;
    let started_at = Instant::now();
    let null_result = tokio::time::timeout(
        probe_timeout,
        quarantined.client.probe_client().schedulercoord_null(),
    )
    .await;
    let outcome = match &null_result {
        Err(_) => BackendRequestOutcome::Timeout,
        Ok(Ok(())) => BackendRequestOutcome::Ok,
        Ok(Err(_)) => BackendRequestOutcome::Error,
    };
    ctx.metrics.observe_backend_request(
        HealthBackend::Scheduler,
        "schedulercoord_null",
        outcome,
        started_at.elapsed(),
    );
    if !matches!(null_result, Ok(Ok(()))) {
        *ctx.scheduler_quarantine.write().await = None;
        return Some(SchedulerProbeResult {
            up: false,
            tip: None,
            tip_observed_at: None,
            error: Some("quarantined scheduler connection unavailable".to_string()),
            identity_status: state.identity_status,
        });
    }
    if Instant::now() < quarantined.ready_at {
        return Some(SchedulerProbeResult {
            up: false,
            tip: None,
            tip_observed_at: None,
            error: Some("scheduler reconnect readiness grace active".to_string()),
            identity_status: state.identity_status,
        });
    }

    probe_quarantined_scheduler_tip(ctx, quarantined, probe_timeout, state).await
}

async fn probe_quarantined_scheduler_tip(
    ctx: &GatewayContext,
    quarantined: QuarantinedScheduler,
    probe_timeout: Duration,
    state: &mut SchedulerProbeState,
) -> Option<SchedulerProbeResult> {
    let generation = quarantined.client.generation();
    let started_at = Instant::now();
    let rpc_result = tokio::time::timeout(
        probe_timeout,
        quarantined.client.probe_client().schedcoord_get_tip(),
    )
    .await;
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

    let db_tip = match rpc_result {
        Err(_) => {
            clear_quarantined_scheduler_if_current(ctx, generation).await;
            return Some(scheduler_probe_failure(
                state,
                "scheduler quarantine tip probe timed out; reconnecting",
            ));
        }
        Ok(Err(error)) => {
            clear_quarantined_scheduler_if_current(ctx, generation).await;
            return Some(scheduler_probe_failure(
                state,
                format!("scheduler quarantine tip probe failed: {error}"),
            ));
        }
        Ok(Ok(SchedulerGetTipResult::SchedulerGetTipOK { db_tip })) => db_tip,
        Ok(Ok(SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock)) => {
            clear_quarantined_scheduler_if_current(ctx, generation).await;
            return Some(scheduler_probe_failure(
                state,
                "scheduler quarantine returned no committed blocks",
            ));
        }
        Ok(Ok(other)) => {
            clear_quarantined_scheduler_if_current(ctx, generation).await;
            return Some(scheduler_probe_failure(
                state,
                format!("unexpected scheduler quarantine tip result: {other:?}"),
            ));
        }
    };

    let mut quarantine_slot = ctx.scheduler_quarantine.write().await;
    if quarantine_slot
        .as_ref()
        .is_none_or(|candidate| candidate.client.generation() != generation)
    {
        return Some(scheduler_probe_failure(
            state,
            "scheduler quarantine changed during identity validation",
        ));
    }

    let mut scheduler = ctx.scheduler.write().await;
    if scheduler.is_some() {
        *quarantine_slot = None;
        return Some(scheduler_probe_failure(
            state,
            "scheduler became active during quarantine validation",
        ));
    }

    let result = successful_scheduler_probe(&db_tip, state);
    if !result.up {
        *quarantine_slot = None;
        return Some(result);
    }

    let Some(candidate) = quarantine_slot.take().map(|candidate| candidate.client) else {
        return Some(scheduler_probe_failure(
            state,
            "validated scheduler quarantine disappeared before publication",
        ));
    };
    *scheduler = Some(candidate);
    tracing::info!(
        backend = "scheduler",
        "backend identity validated and connection published"
    );
    Some(result)
}

async fn clear_quarantined_scheduler_if_current(ctx: &GatewayContext, generation: u64) {
    let mut quarantine = ctx.scheduler_quarantine.write().await;
    if quarantine
        .as_ref()
        .is_some_and(|candidate| candidate.client.generation() == generation)
    {
        *quarantine = None;
    }
}

fn scheduler_probe_failure(
    state: &SchedulerProbeState,
    message: impl Into<String>,
) -> SchedulerProbeResult {
    SchedulerProbeResult {
        up: false,
        tip: None,
        tip_observed_at: None,
        error: Some(bounded_error(message.into())),
        identity_status: state.identity_status,
    }
}

async fn probe_indexer(
    ctx: &GatewayContext,
    protocol: &Protocol,
    probe_timeout: Duration,
    timeout_streak: &mut u8,
) -> ProbeResult {
    let current_client = { ctx.indexer.read().await.clone() };
    let Some(client) = current_client else {
        *timeout_streak = 0;
        return failed_probe("indexer not connected");
    };
    let started_at = Instant::now();
    let rpc_result = tokio::time::timeout(
        probe_timeout,
        client.probe_client().indexer_block_explorer_intro(protocol),
    )
    .await;
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
        Ok(Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)) => {
            *timeout_streak = 0;
            ok_probe()
        }
        Ok(Ok(outcome)) => {
            *timeout_streak = 0;
            failed_probe(format!("indexer intro rejected: {outcome}"))
        }
        Ok(Err(err)) => {
            *timeout_streak = 0;
            *ctx.indexer.write().await = None;
            failed_probe(format!("indexer intro failed: {err}"))
        }
        Err(_) => {
            let reconnecting = repeated_probe_timeout(timeout_streak);
            if reconnecting {
                *ctx.indexer.write().await = None;
            }
            failed_probe(if reconnecting {
                "indexer probe repeatedly timed out; reconnecting"
            } else {
                "indexer probe timed out"
            })
        }
    }
}

async fn probe_db_gateway(
    ctx: &GatewayContext,
    protocol: &Protocol,
    probe_timeout: Duration,
    timeout_streak: &mut u8,
) -> ProbeResult {
    let current_client = { ctx.db_gateway.read().await.clone() };
    let Some(client) = current_client else {
        *timeout_streak = 0;
        return failed_probe("db_gateway not connected");
    };
    let started_at = Instant::now();
    let rpc_result = tokio::time::timeout(
        probe_timeout,
        client.probe_client().dbgateway_intro(protocol),
    )
    .await;
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
        Ok(Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)) => {
            *timeout_streak = 0;
            ok_probe()
        }
        Ok(Ok(outcome)) => {
            *timeout_streak = 0;
            failed_probe(format!("db_gateway intro rejected: {outcome}"))
        }
        Ok(Err(err)) => {
            *timeout_streak = 0;
            *ctx.db_gateway.write().await = None;
            failed_probe(format!("db_gateway intro failed: {err}"))
        }
        Err(_) => {
            let reconnecting = repeated_probe_timeout(timeout_streak);
            if reconnecting {
                *ctx.db_gateway.write().await = None;
            }
            failed_probe(if reconnecting {
                "db_gateway probe repeatedly timed out; reconnecting"
            } else {
                "db_gateway probe timed out"
            })
        }
    }
}

async fn probe_view_gateway(
    ctx: &GatewayContext,
    protocol: &Protocol,
    probe_timeout: Duration,
    timeout_streak: &mut u8,
) -> ProbeResult {
    let current_client = { ctx.view_gateway.read().await.clone() };
    let Some(client) = current_client else {
        *timeout_streak = 0;
        return failed_probe("view_gateway not connected");
    };
    let started_at = Instant::now();
    let rpc_result = tokio::time::timeout(
        probe_timeout,
        client.probe_client().viewgateway_intro(protocol),
    )
    .await;
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
        Ok(Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)) => {
            *timeout_streak = 0;
            ok_probe()
        }
        Ok(Ok(outcome)) => {
            *timeout_streak = 0;
            failed_probe(format!("view_gateway intro rejected: {outcome}"))
        }
        Ok(Err(err)) => {
            *timeout_streak = 0;
            *ctx.view_gateway.write().await = None;
            failed_probe(format!("view_gateway intro failed: {err}"))
        }
        Err(_) => {
            let reconnecting = repeated_probe_timeout(timeout_streak);
            if reconnecting {
                *ctx.view_gateway.write().await = None;
            }
            failed_probe(if reconnecting {
                "view_gateway probe repeatedly timed out; reconnecting"
            } else {
                "view_gateway probe timed out"
            })
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
        scheduler_identity: SchedulerIdentityStatus::Unverified,
    }
}

fn readiness_status(
    config: &GatewayConfig,
    backends: &BTreeMap<&'static str, BackendHealth>,
    chain_tip: &ChainTipHealth,
) -> HealthStatus {
    if backends
        .get(HealthBackend::Scheduler.as_str())
        .is_some_and(|health| health.scheduler_identity == SchedulerIdentityStatus::Mismatch)
    {
        return HealthStatus::Error;
    }
    for backend in HealthBackend::ALL {
        if !backends.contains_key(backend.as_str()) {
            return HealthStatus::Error;
        }
    }
    if backends
        .values()
        .any(|health| health.required && (!health.configured || !health.up))
    {
        return HealthStatus::Error;
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
    use groundhog_db_gateway::{
        AddressAndKeyBatch, DbGateway_DbGateway1, DbGateway_DbGateway1_Dispatcher,
        DbGatewayItemResult, DbGatewayPayload, DbGatewayResult,
    };
    use groundhog_indexer::{
        AddressTransactionQuery, BlockLookupResult, BlockTransactionQuery, EventPageResult,
        EventQuery, IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, TransactionPageResult,
        TransactionQuery, TransactionRefPageResult, TxInfoOutcome, TxInfoResult,
    };
    use groundhog_json_rpc_server::{ChainPlane, ReadPlane, RpcRequestContext, ViewPlane};
    use groundhog_json_rpc_types::{
        ContractAddress as RpcContractAddress,
        hex::{Hex0x, HexBytes},
        transaction::{TransactionStatus, TxHash},
        view::{RootInvocationOutput, ViewRequest},
    };
    use groundhog_scheduler::{
        ConsensusValue, CoordinatorRegistrationOutcome, DbSchedResult, DbTip,
        ParallelSignedTransactionBatch, Proposal, ProposalTxsHash, RPCError,
        SchedulerCommitOutcome, SchedulerGetTipResult, SchedulerProposalResult,
        SchedulerValidationResult,
        sched_coord::{SchedulerCoord_SchedulerCoord1, SchedulerCoord_SchedulerCoord1_Dispatcher},
    };
    use groundhog_view::{
        ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Dispatcher, ViewGatewayPayload,
        ViewGatewayResult,
    };
    use groundhog_xdr::PROTOCOL_VERSION;
    use groundhog_xdr::transaction::{
        OptSignedTransactionBatch, TransactionHash, TransactionHashBatch,
    };
    use groundhog_xdr::{
        auth_claim::{AuthClaim, AuthClaim_source_address_claim},
        contract::{ContractEntrypointID, ContractType, DeployedContractHash},
        genesis::GenesisConfiguration,
        header::{ProposalContext, ProposalHeader},
        invoke_flag::InvokeFlag,
        network_config::NetworkConfig,
        transaction::{Invocation, InvocationFrameOutput},
        types::{
            ConfigHash, ConsensusValueHash, ContractAddress, EventsHash, ExtensionPoint,
            NetworkHash, Protocol as ConsensusProtocol, SHA256Hash, StorageShortHash, TimePoint,
            TxsHash,
        },
        validator::ValidatorID,
        view::TransactionViewQuery,
    };
    use rpc_driver::{ProgramError, RpcServer, ServiceMap};
    use std::{
        net::SocketAddr,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    use xdr_common::{BytesM, VecM};

    use crate::{
        backend::GatewayBackend,
        error::GatewayError,
        metrics::GatewayMetrics,
        observability::spawn_observability_server,
        tip_coalescer::{SchedulerTipSink, TipSink},
    };

    use super::*;

    struct SlowIntroIndexer {
        intros: Arc<AtomicUsize>,
        release: Arc<tokio::sync::Notify>,
    }

    #[derive(Clone, Default)]
    struct HandlerGate {
        calls: Arc<AtomicUsize>,
        release: Arc<tokio::sync::Notify>,
    }

    impl HandlerGate {
        async fn wait(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
        }
    }

    struct SaturatedIndexer(HandlerGate);
    struct SaturatedDbGateway(HandlerGate);
    struct SaturatedViewGateway(HandlerGate);

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for SaturatedIndexer {
        async fn indexer_block_explorer_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
            Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
        }

        async fn get_transaction_info(
            &self,
            _tx_hash: TransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            self.0.wait().await;
            Err(ProgramError::msg("released indexer handler"))
        }

        async fn get_block_by_height(
            &self,
            _height: groundhog_xdr::types::BlockHeight,
        ) -> Result<BlockLookupResult, ProgramError> {
            panic!("probe isolation test should only issue transaction lookups")
        }

        async fn get_events(&self, _query: EventQuery) -> Result<EventPageResult, ProgramError> {
            panic!("probe isolation test should only issue transaction lookups")
        }

        async fn get_transactions(
            &self,
            _query: TransactionQuery,
        ) -> Result<TransactionPageResult, ProgramError> {
            panic!("probe isolation test should only issue transaction lookups")
        }

        async fn get_transactions_by_address(
            &self,
            _query: AddressTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("probe isolation test should only issue transaction lookups")
        }

        async fn get_transactions_by_block(
            &self,
            _query: BlockTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("probe isolation test should only issue transaction lookups")
        }
    }

    #[async_trait]
    impl DbGateway_DbGateway1 for SaturatedDbGateway {
        async fn dbgateway_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<DbGatewayIntroOutcome, ProgramError> {
            Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)
        }

        async fn dbgateway_get_items(
            &self,
            _batch: AddressAndKeyBatch,
        ) -> Result<DbGatewayResult, ProgramError> {
            self.0.wait().await;
            Err(ProgramError::msg("released db_gateway handler"))
        }
    }

    #[async_trait]
    impl ViewGateway_ViewGateway1 for SaturatedViewGateway {
        async fn viewgateway_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<ViewGatewayIntroOutcome, ProgramError> {
            Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)
        }

        async fn viewgateway_view(
            &self,
            _query: TransactionViewQuery,
        ) -> Result<ViewGatewayResult, ProgramError> {
            self.0.wait().await;
            Err(ProgramError::msg("released view_gateway handler"))
        }
    }

    #[derive(Clone, Default)]
    struct SchedulerCallCounts {
        null: Arc<AtomicUsize>,
        tip: Arc<AtomicUsize>,
    }

    #[derive(Clone, Copy)]
    enum TipBehavior {
        Ready,
        Pending,
    }

    #[derive(Clone)]
    enum NullBehavior {
        Ready,
        Wait(Arc<tokio::sync::Notify>),
    }

    struct TestScheduler {
        calls: SchedulerCallCounts,
        tip_behavior: TipBehavior,
        null_behavior: NullBehavior,
    }

    fn test_db_tip(network_hash: [u8; 32]) -> DbTip {
        DbTip {
            id: 1,
            storage_long_hash: [0u8; 2048].into(),
            default_wallet: DeployedContractHash {
                hash: SHA256Hash::from([0; 32]),
                contract_size: 0,
                contract_type: ContractType::kNative,
            },
            network_config: NetworkConfig {
                ext: ExtensionPoint::_0,
                max_sha256_buffers: 0,
                max_cross_invoke_depth: 0,
            },
            value: ConsensusValue {
                protocol: ConsensusProtocol::new(0),
                ext: ExtensionPoint::_0,
                prev_hash: ConsensusValueHash::new(SHA256Hash::from([0x42; 32])),
                proposal_header: ProposalHeader {
                    ext: ExtensionPoint::_0,
                    network_hash: NetworkHash::new(SHA256Hash::from(network_hash)),
                    config_hash: ConfigHash::new(SHA256Hash::from([0; 32])),
                    proposer: ValidatorID::new([0x40; 32]),
                    block_timepoint: TimePoint::new(1),
                    block_height: 1.into(),
                    txs_hash: TxsHash::new(SHA256Hash::from([0x43; 32])),
                    storage_hash: StorageShortHash::new(SHA256Hash::from([0x44; 32])),
                    events_hash: EventsHash::new(SHA256Hash::from([0x45; 32])),
                },
                proposal_txs_hash: ProposalTxsHash::new(SHA256Hash::from([0x46; 32])),
                upgrades: VecM::default(),
            },
        }
    }

    #[test]
    fn scheduler_network_hash_matching_pin_is_healthy() {
        let expected = NetworkHashHex::from([0x41; 32]);
        let mut state = SchedulerProbeState::with_expected_network_hash(Some(expected.clone()));

        let result = successful_scheduler_probe(&test_db_tip([0x41; 32]), &mut state);

        assert!(result.up);
        assert!(result.error.is_none());
        assert_eq!(result.identity_status, SchedulerIdentityStatus::Validated);
        assert_eq!(state.expected_network_hash, Some(expected));
    }

    #[test]
    fn optional_scheduler_network_hash_mismatch_is_readiness_error() {
        let expected = NetworkHashHex::from([0x42; 32]);
        let mut state = SchedulerProbeState::with_expected_network_hash(Some(expected.clone()));

        let result = successful_scheduler_probe(&test_db_tip([0x41; 32]), &mut state);

        assert!(!result.up);
        assert!(result.tip.is_none());
        assert_eq!(result.identity_status, SchedulerIdentityStatus::Mismatch);
        assert_eq!(
            result.error.as_deref(),
            Some(
                "scheduler network hash mismatch: expected \
4242424242424242424242424242424242424242424242424242424242424242, observed \
4141414141414141414141414141414141414141414141414141414141414141"
            )
        );
        assert_eq!(state.expected_network_hash, Some(expected));

        let config = GatewayConfig {
            health_required_backends: Vec::new(),
            indexer_address: None,
            ..GatewayConfig::default()
        };
        let mut backends = BTreeMap::new();
        let mut last_success = BTreeMap::new();
        for backend in HealthBackend::ALL {
            backends.insert(
                backend.as_str(),
                backend_health(
                    &config,
                    backend,
                    backend != HealthBackend::Scheduler,
                    result.error.clone(),
                    "2026-01-01T00:00:00Z",
                    &mut last_success,
                ),
            );
        }
        backends
            .get_mut(HealthBackend::Scheduler.as_str())
            .expect("scheduler health should be present")
            .scheduler_identity = result.identity_status;
        assert_eq!(
            readiness_status(
                &config,
                &backends,
                &ChainTipHealth {
                    height: Some(1.into()),
                    age_ms: Some(0),
                },
            ),
            HealthStatus::Error
        );
    }

    #[test]
    fn scheduler_first_tip_pins_identity_across_later_probes() {
        let pinned = NetworkHashHex::from([0x41; 32]);
        let mut state = SchedulerProbeState::default();

        let first = successful_scheduler_probe(&test_db_tip([0x41; 32]), &mut state);
        let mismatch = successful_scheduler_probe(&test_db_tip([0x42; 32]), &mut state);

        assert!(first.up);
        assert!(!mismatch.up);
        assert_eq!(mismatch.identity_status, SchedulerIdentityStatus::Mismatch);
        assert_eq!(state.expected_network_hash, Some(pinned));
    }

    #[async_trait]
    impl SchedulerCoord_SchedulerCoord1 for TestScheduler {
        async fn schedulercoord_null(&self) -> Result<(), ProgramError> {
            self.calls.null.fetch_add(1, Ordering::SeqCst);
            if let NullBehavior::Wait(gate) = &self.null_behavior {
                gate.notified().await;
            }
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
            _validator: ValidatorID,
            _batch: ParallelSignedTransactionBatch,
        ) -> Result<(), ProgramError> {
            panic!("health probe test should not submit")
        }

        async fn schedulercoord_propose_block(
            &self,
            _block: groundhog_scheduler::BlockID,
            _context: ProposalContext,
        ) -> Result<SchedulerProposalResult, ProgramError> {
            panic!("health probe test should not propose")
        }

        async fn schedulercoord_validate_block(
            &self,
            _block: groundhog_scheduler::BlockID,
            _proposal: Proposal,
        ) -> Result<SchedulerValidationResult, ProgramError> {
            panic!("health probe test should not validate")
        }

        async fn schedulercoord_commit_block(
            &self,
            _block: groundhog_scheduler::BlockID,
            _value: ConsensusValue,
        ) -> Result<SchedulerCommitOutcome, ProgramError> {
            panic!("health probe test should not commit")
        }

        async fn schedcoord_get_tip(&self) -> Result<SchedulerGetTipResult, ProgramError> {
            self.calls.tip.fetch_add(1, Ordering::SeqCst);
            match self.tip_behavior {
                TipBehavior::Pending => std::future::pending().await,
                TipBehavior::Ready => Ok(SchedulerGetTipResult::SchedulerGetTipOK {
                    db_tip: test_db_tip([0x41; 32]),
                }),
            }
        }

        async fn schedcoord_load_genesis_configuration(
            &self,
            _genesis: GenesisConfiguration,
        ) -> Result<DbSchedResult, ProgramError> {
            panic!("health probe test should not load genesis")
        }

        async fn schedulercoord_commit_genesis_ceremony(
            &self,
            _request: groundhog_scheduler::GenesisCeremonyRequest,
        ) -> Result<groundhog_scheduler::GenesisCeremonyResult, ProgramError> {
            panic!("health probe test should not commit a genesis ceremony")
        }

        async fn schedcoord_lookup_transactions(
            &self,
            hashes: TransactionHashBatch,
        ) -> Result<OptSignedTransactionBatch, ProgramError> {
            Ok(VecM(vec![None; hashes.0.len()]))
        }
    }

    struct LifecycleIndexer;

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for LifecycleIndexer {
        async fn indexer_block_explorer_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
            Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
        }

        async fn get_transaction_info(
            &self,
            _tx_hash: TransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            Ok(TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0,
            })
        }

        async fn get_block_by_height(
            &self,
            _height: groundhog_xdr::types::BlockHeight,
        ) -> Result<BlockLookupResult, ProgramError> {
            panic!("lifecycle test should not query blocks")
        }

        async fn get_events(&self, _query: EventQuery) -> Result<EventPageResult, ProgramError> {
            panic!("lifecycle test should not query events")
        }

        async fn get_transactions(
            &self,
            _query: TransactionQuery,
        ) -> Result<TransactionPageResult, ProgramError> {
            panic!("lifecycle test should not query transactions")
        }

        async fn get_transactions_by_address(
            &self,
            _query: AddressTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("lifecycle test should not query transactions by address")
        }

        async fn get_transactions_by_block(
            &self,
            _query: BlockTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("lifecycle test should not query transactions by block")
        }
    }

    struct LifecycleDbGateway;

    #[async_trait]
    impl DbGateway_DbGateway1 for LifecycleDbGateway {
        async fn dbgateway_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<DbGatewayIntroOutcome, ProgramError> {
            Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)
        }

        async fn dbgateway_get_items(
            &self,
            batch: AddressAndKeyBatch,
        ) -> Result<DbGatewayResult, ProgramError> {
            Ok(DbGatewayResult::kRPCNone {
                payload: DbGatewayPayload {
                    block_height: 42.into(),
                    items: VecM(
                        batch
                            .0
                            .iter()
                            .map(|_| DbGatewayItemResult::Default {
                                error: RPCError::kRPCNotFound.0,
                            })
                            .collect(),
                    ),
                },
            })
        }
    }

    struct LifecycleViewGateway;

    #[async_trait]
    impl ViewGateway_ViewGateway1 for LifecycleViewGateway {
        async fn viewgateway_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<ViewGatewayIntroOutcome, ProgramError> {
            Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)
        }

        async fn viewgateway_view(
            &self,
            _query: TransactionViewQuery,
        ) -> Result<ViewGatewayResult, ProgramError> {
            Ok(ViewGatewayResult::ViewGatewayOk {
                payload: ViewGatewayPayload {
                    height: 42.into(),
                    output: InvocationFrameOutput::Return {
                        return_buffer: BytesM(vec![1, 2, 3]),
                    },
                    events: VecM(vec![]),
                },
            })
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct LifecycleAddresses {
        scheduler: SocketAddr,
        indexer: SocketAddr,
        db_gateway: SocketAddr,
        view_gateway: SocketAddr,
    }

    struct LifecycleBackendCluster {
        addresses: LifecycleAddresses,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
        completed: Option<tokio::sync::oneshot::Receiver<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl LifecycleBackendCluster {
        #[allow(clippy::too_many_lines)]
        async fn start(addresses: Option<LifecycleAddresses>) -> Self {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
            let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
            let thread = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("lifecycle fixture runtime should build");
                runtime.block_on(async move {
                    let ephemeral = SocketAddr::from(([127, 0, 0, 1], 0));
                    let scheduler_listener = TcpListener::bind(
                        addresses.map_or(ephemeral, |addresses| addresses.scheduler),
                    )
                    .await
                    .expect("lifecycle scheduler should bind");
                    let indexer_listener = TcpListener::bind(
                        addresses.map_or(ephemeral, |addresses| addresses.indexer),
                    )
                    .await
                    .expect("lifecycle indexer should bind");
                    let db_gateway_listener = TcpListener::bind(
                        addresses.map_or(ephemeral, |addresses| addresses.db_gateway),
                    )
                    .await
                    .expect("lifecycle db gateway should bind");
                    let view_gateway_listener = TcpListener::bind(
                        addresses.map_or(ephemeral, |addresses| addresses.view_gateway),
                    )
                    .await
                    .expect("lifecycle view gateway should bind");

                    let addresses = LifecycleAddresses {
                        scheduler: scheduler_listener
                            .local_addr()
                            .expect("lifecycle scheduler should have an address"),
                        indexer: indexer_listener
                            .local_addr()
                            .expect("lifecycle indexer should have an address"),
                        db_gateway: db_gateway_listener
                            .local_addr()
                            .expect("lifecycle db gateway should have an address"),
                        view_gateway: view_gateway_listener
                            .local_addr()
                            .expect("lifecycle view gateway should have an address"),
                    };

                    let mut scheduler_services = ServiceMap::new();
                    scheduler_services.add_service(Box::new(
                        SchedulerCoord_SchedulerCoord1_Dispatcher::new(Box::new(TestScheduler {
                            calls: SchedulerCallCounts::default(),
                            tip_behavior: TipBehavior::Ready,
                            null_behavior: NullBehavior::Ready,
                        })),
                    ));
                    let mut indexer_services = ServiceMap::new();
                    indexer_services.add_service(Box::new(
                        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(
                            LifecycleIndexer,
                        )),
                    ));
                    let mut db_gateway_services = ServiceMap::new();
                    db_gateway_services.add_service(Box::new(
                        DbGateway_DbGateway1_Dispatcher::new(Box::new(LifecycleDbGateway)),
                    ));
                    let mut view_gateway_services = ServiceMap::new();
                    view_gateway_services.add_service(Box::new(
                        ViewGateway_ViewGateway1_Dispatcher::new(Box::new(LifecycleViewGateway)),
                    ));

                    let mut handles = vec![
                        RpcServer::new(scheduler_listener, scheduler_services)
                            .start()
                            .await,
                        RpcServer::new(indexer_listener, indexer_services)
                            .start()
                            .await,
                        RpcServer::new(db_gateway_listener, db_gateway_services)
                            .start()
                            .await,
                        RpcServer::new(view_gateway_listener, view_gateway_services)
                            .start()
                            .await,
                    ];
                    ready_tx
                        .send(addresses)
                        .expect("lifecycle fixture owner should await startup");

                    let _ = shutdown_rx.await;
                    for handle in &handles {
                        handle.abort();
                    }
                    for handle in handles.drain(..) {
                        let _ = handle.await;
                    }
                });
                drop(runtime);
                let _ = completed_tx.send(());
            });
            let addresses = tokio::time::timeout(Duration::from_secs(5), ready_rx)
                .await
                .expect("lifecycle fixture startup should not hang")
                .expect("lifecycle fixture thread should report startup");
            Self {
                addresses,
                shutdown: Some(shutdown_tx),
                completed: Some(completed_rx),
                thread: Some(thread),
            }
        }

        async fn stop(mut self) {
            let _ = self
                .shutdown
                .take()
                .expect("lifecycle fixture should have a shutdown sender")
                .send(());
            let completed = self
                .completed
                .take()
                .expect("lifecycle fixture should have a completion receiver");
            tokio::time::timeout(Duration::from_secs(5), completed)
                .await
                .expect("lifecycle fixture shutdown should not hang")
                .expect("lifecycle fixture thread should signal completion");
            let thread = self
                .thread
                .take()
                .expect("lifecycle fixture should have a thread");
            thread
                .join()
                .expect("completed lifecycle fixture thread should not panic");
        }
    }

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for SlowIntroIndexer {
        async fn indexer_block_explorer_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
            self.intros.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
            Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
        }

        async fn get_transaction_info(
            &self,
            _tx_hash: TransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }

        async fn get_block_by_height(
            &self,
            _height: groundhog_xdr::types::BlockHeight,
        ) -> Result<BlockLookupResult, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }

        async fn get_events(&self, _query: EventQuery) -> Result<EventPageResult, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }

        async fn get_transactions(
            &self,
            _query: TransactionQuery,
        ) -> Result<TransactionPageResult, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }

        async fn get_transactions_by_address(
            &self,
            _query: AddressTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }

        async fn get_transactions_by_block(
            &self,
            _query: BlockTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            panic!("reconnect test should not issue indexer data RPCs")
        }
    }

    async fn start_slow_intro_indexer() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        Arc<AtomicUsize>,
        Arc<tokio::sync::Notify>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test indexer should bind");
        let addr = listener
            .local_addr()
            .expect("test indexer should have a local address");
        let mut services = ServiceMap::new();
        let intros = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        services.add_service(Box::new(
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(
                SlowIntroIndexer {
                    intros: intros.clone(),
                    release: release.clone(),
                },
            )),
        ));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle, intros, release)
    }

    async fn start_saturated_indexer() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        HandlerGate,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test indexer should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test indexer should have a local address");
        let gate = HandlerGate::default();
        let mut services = ServiceMap::new();
        services.add_service(Box::new(
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(SaturatedIndexer(
                gate.clone(),
            ))),
        ));
        let handle = RpcServer::new(listener, services).start().await;
        (addr, handle, gate)
    }

    async fn start_saturated_db_gateway() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        HandlerGate,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test db_gateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test db_gateway should have a local address");
        let gate = HandlerGate::default();
        let mut services = ServiceMap::new();
        services.add_service(Box::new(DbGateway_DbGateway1_Dispatcher::new(Box::new(
            SaturatedDbGateway(gate.clone()),
        ))));
        let handle = RpcServer::new(listener, services).start().await;
        (addr, handle, gate)
    }

    async fn start_saturated_view_gateway() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        HandlerGate,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test view_gateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test view_gateway should have a local address");
        let gate = HandlerGate::default();
        let mut services = ServiceMap::new();
        services.add_service(Box::new(ViewGateway_ViewGateway1_Dispatcher::new(
            Box::new(SaturatedViewGateway(gate.clone())),
        )));
        let handle = RpcServer::new(listener, services).start().await;
        (addr, handle, gate)
    }

    fn test_view_query() -> TransactionViewQuery {
        TransactionViewQuery {
            invocation: Invocation {
                invoked_address: ContractAddress::from([7; 32]),
                entrypoint: ContractEntrypointID::from([8; 8]),
                gas_limit: 1,
                calldata: BytesM(vec![]),
            },
            parent_address: None,
            auth_claim: AuthClaim::kSourceAddressAuthData {
                source_address_claim: AuthClaim_source_address_claim {
                    allowed_keys: 0,
                    required_weight: 0,
                },
            },
            forwarded_tokens: VecM(vec![]),
            invoke_flags: InvokeFlag::None.0,
        }
    }

    async fn start_test_scheduler(
        tip_behavior: TipBehavior,
        null_behavior: NullBehavior,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        SchedulerCallCounts,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test scheduler should bind");
        let addr = listener
            .local_addr()
            .expect("test scheduler should have a local address");
        let mut services = ServiceMap::new();
        let calls = SchedulerCallCounts::default();
        services.add_service(Box::new(SchedulerCoord_SchedulerCoord1_Dispatcher::new(
            Box::new(TestScheduler {
                calls: calls.clone(),
                tip_behavior,
                null_behavior,
            }),
        )));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle, calls)
    }

    async fn http_status(addr: std::net::SocketAddr, path: &str) -> u16 {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut stream = TcpStream::connect(addr)
                .await
                .expect("observability request should connect");
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .expect("observability request should write");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .await
                .expect("observability response should read");
            let response = String::from_utf8(response).expect("response should be UTF-8");
            response
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|status| status.parse().ok())
                .expect("response should contain an HTTP status")
        })
        .await
        .expect("observability request should not hang")
    }

    async fn wait_for_health_snapshot(
        health: &GatewayHealth,
        mut predicate: impl FnMut(&HealthSnapshot) -> bool,
    ) -> HealthSnapshot {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = health.updated.notified();
                let snapshot = health.snapshot().await;
                if predicate(&snapshot) {
                    return snapshot;
                }
                notified.await;
            }
        })
        .await
        .expect("health should reach the expected state")
    }

    async fn wait_for_lifecycle_clients_cleared(health: &GatewayHealth, ctx: &GatewayContext) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = health.updated.notified();
                let cleared = ctx.scheduler.read().await.is_none()
                    && ctx.scheduler_quarantine.read().await.is_none()
                    && ctx.indexer.read().await.is_none()
                    && ctx.db_gateway.read().await.is_none()
                    && ctx.view_gateway.read().await.is_none();
                if cleared {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("health loop should clear unavailable clients");
    }

    async fn wait_for_failed_lifecycle_reconnects(
        metrics: &GatewayMetrics,
        health: &GatewayHealth,
    ) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = health.snapshot().await;
                let body = metrics
                    .gather(&snapshot)
                    .expect("lifecycle reconnect metrics should encode");
                let all_failed = HealthBackend::ALL.into_iter().all(|backend| {
                    let prefix = format!(
                        "groundhog_gateway_backend_reconnects_total{{backend=\"{}\",outcome=\"error\"}} ",
                        backend.as_str()
                    );
                    body.lines()
                        .find_map(|line| line.strip_prefix(&prefix))
                        .and_then(|value| value.parse::<f64>().ok())
                        .is_some_and(|value| value >= 1.0)
                });
                if all_failed {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("every backend should complete a failed reconnect while ports are down");
    }

    fn lifecycle_tx_hash() -> TxHash {
        TxHash(Hex0x(SHA256Hash::from([0x55; 32])))
    }

    fn lifecycle_contract_address() -> RpcContractAddress {
        RpcContractAddress::from(ContractAddress::from([0x66; 32]))
    }

    fn lifecycle_view_request() -> ViewRequest {
        ViewRequest {
            contract: lifecycle_contract_address(),
            entrypoint: HexBytes(vec![0x77; 8]),
            calldata: HexBytes(vec![0x88]),
            caller: None,
        }
    }

    async fn assert_lifecycle_data_plane_available(backend: &GatewayBackend) {
        let tip = backend
            .chain_tip(RpcRequestContext::unbounded())
            .await
            .expect("chain tip should be available");
        assert_eq!(*tip.height, 1);

        let status = backend
            .transaction_status(RpcRequestContext::unbounded(), lifecycle_tx_hash())
            .await
            .expect("transaction status should be available");
        assert!(matches!(status, TransactionStatus::Unknown));

        let storage = backend
            .get_storage_at(
                RpcRequestContext::unbounded(),
                lifecycle_contract_address(),
                [0x99; 32],
            )
            .await
            .expect("storage read should be available");
        assert_eq!(*storage.block_height, 42);
        assert!(storage.item.is_none());

        let view = backend
            .view(
                RpcRequestContext::unbounded(),
                lifecycle_view_request(),
                1_000,
            )
            .await
            .expect("view should be available");
        assert_eq!(*view.height, 42);
        assert!(matches!(
            view.output,
            RootInvocationOutput::Return { return_data } if return_data.0 == vec![1, 2, 3]
        ));
    }

    async fn assert_lifecycle_data_plane_unavailable(backend: &GatewayBackend) {
        assert!(
            backend
                .chain_tip(RpcRequestContext::unbounded())
                .await
                .is_err(),
            "chain tip should fail closed"
        );
        assert!(
            backend
                .transaction_status(RpcRequestContext::unbounded(), lifecycle_tx_hash())
                .await
                .is_err(),
            "transaction status should fail closed"
        );
        assert!(
            backend
                .get_storage_at(
                    RpcRequestContext::unbounded(),
                    lifecycle_contract_address(),
                    [0x99; 32],
                )
                .await
                .is_err(),
            "storage read should fail closed"
        );
        assert!(
            backend
                .view(
                    RpcRequestContext::unbounded(),
                    lifecycle_view_request(),
                    1_000,
                )
                .await
                .is_err(),
            "view should fail closed"
        );
    }

    #[test]
    fn required_missing_backend_is_error() {
        let config = GatewayConfig {
            health_required_backends: vec![HealthBackend::DbGateway],
            db_gateway_address: None,
            ..GatewayConfig::default()
        };
        let snapshot = HealthSnapshot::starting(&config, BuildVersion::new("test", None), None);

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
                    scheduler_identity: SchedulerIdentityStatus::Unverified,
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
    fn required_faucet_down_is_readiness_error() {
        let config = GatewayConfig::default();
        let mut backends = BTreeMap::new();
        for backend in HealthBackend::ALL {
            backends.insert(
                backend.as_str(),
                BackendHealth {
                    configured: true,
                    required: backend_required(&config, backend),
                    up: true,
                    last_success_timestamp: None,
                    last_error: None,
                    scheduler_identity: SchedulerIdentityStatus::Unverified,
                },
            );
        }
        backends.insert(
            "faucet",
            BackendHealth {
                configured: true,
                required: true,
                up: false,
                last_success_timestamp: None,
                last_error: Some("unavailable".into()),
                scheduler_identity: SchedulerIdentityStatus::Unverified,
            },
        );

        let status = readiness_status(
            &config,
            &backends,
            &ChainTipHealth {
                height: Some(BlockHeight::from(1i64)),
                age_ms: Some(0),
            },
        );

        assert_eq!(status, HealthStatus::Error);
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

    #[test]
    fn optional_reconnect_deadline_is_classified_as_timeout() {
        let error = ConnectError::StartupProbeTimedOut {
            service: "indexer",
            addr: "127.0.0.1:1".into(),
            timeout: Duration::from_secs(1),
        };

        assert_eq!(
            reconnect_error_outcome(&error),
            BackendReconnectOutcome::Timeout
        );
    }

    #[tokio::test]
    async fn indexer_reconnect_does_not_block_health_probe() {
        let (indexer_addr, _server, intros, release) = start_slow_intro_indexer().await;
        let config = Arc::new(GatewayConfig {
            indexer_address: Some(indexer_addr.to_string()),
            db_gateway_address: None,
            view_gateway_address: None,
            health_required_backends: Vec::new(),
            request_timeout_ms: 1_000,
            health_probe_interval_ms: 10,
            backend_connect_timeout_ms: 500,
            ..GatewayConfig::default()
        });
        let metrics = GatewayMetrics::for_tests();
        let ctx = Arc::new(GatewayContext::assemble(
            PROTOCOL_VERSION,
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config,
            metrics,
        ));
        let protocol = PROTOCOL_VERSION;
        let mut reconnects = ReconnectAttempts::default();
        maintain_reconnects(&ctx, &mut reconnects).await;
        maintain_reconnects(&ctx, &mut reconnects).await;

        let mut timeout_streak = 0;
        let result = tokio::time::timeout(
            Duration::from_millis(10),
            probe_indexer(
                &ctx,
                &protocol,
                Duration::from_millis(10),
                &mut timeout_streak,
            ),
        )
        .await
        .expect("health probe should not await the reconnect task");

        assert!(!result.up);
        assert!(ctx.indexer.read().await.is_none());
        assert!(reconnects.indexer.is_some());
        tokio::time::timeout(Duration::from_secs(1), async {
            while intros.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("indexer reconnect should reach the intro probe");
        assert_eq!(intros.load(Ordering::SeqCst), 1);
        release.notify_one();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                reconnects.reap_finished().await;
                if ctx.indexer.read().await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("indexer reconnect should complete independently");
        assert_eq!(intros.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn slow_optional_reconnect_keeps_degraded_readiness_fresh() {
        let (scheduler_addr, _scheduler_server, _) =
            start_test_scheduler(TipBehavior::Ready, NullBehavior::Ready).await;
        let scheduler = connect_optional_scheduler(&scheduler_addr.to_string(), 1)
            .await
            .expect("scheduler connection should succeed")
            .expect("scheduler should be connected");
        let (indexer_addr, _indexer_server, intros, release) = start_slow_intro_indexer().await;
        let config = Arc::new(GatewayConfig {
            scheduler_address: scheduler_addr.to_string(),
            indexer_address: Some(indexer_addr.to_string()),
            db_gateway_address: None,
            view_gateway_address: None,
            health_required_backends: vec![HealthBackend::Scheduler],
            request_timeout_ms: 20,
            health_probe_interval_ms: 20,
            backend_connect_timeout_ms: 500,
            health_chain_tip_max_age_ms: 1_000,
            ..GatewayConfig::default()
        });
        let metrics = GatewayMetrics::for_tests();
        let ctx = Arc::new(GatewayContext::assemble(
            PROTOCOL_VERSION,
            Arc::new(RwLock::new(Some(scheduler))),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config.clone(),
            metrics.clone(),
        ));
        let health = GatewayHealth::new(&config, BuildVersion::new("test", None), None);
        let (health_shutdown_tx, health_shutdown_rx) = tokio::sync::oneshot::channel();
        let health_handle =
            spawn_health_probe_with_shutdown(ctx.clone(), health.clone(), health_shutdown_rx);
        let (observability_shutdown_tx, observability_shutdown_rx) =
            tokio::sync::oneshot::channel();
        let observability = spawn_observability_server(
            "127.0.0.1:0",
            health.clone(),
            metrics,
            observability_shutdown_rx,
        )
        .await
        .expect("observability server should start");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if health.snapshot().await.status() == HealthStatus::Degraded
                    && intros.load(Ordering::SeqCst) == 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("health loop should publish degraded state and start reconnecting");
        let first_snapshot_at = health.snapshot().await.snapshot_at;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if health.snapshot().await.snapshot_at > first_snapshot_at {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("health loop should publish while reconnect remains blocked");

        let snapshot = health.snapshot().await;
        assert_eq!(snapshot.status(), HealthStatus::Degraded);
        assert_eq!(http_status(observability.local_addr, "/readyz").await, 200);
        assert_eq!(http_status(observability.local_addr, "/health").await, 200);
        assert_eq!(intros.load(Ordering::SeqCst), 1);

        health_shutdown_tx.send(()).unwrap();
        observability_shutdown_tx.send(()).unwrap();
        health_handle.await.unwrap();
        observability.handle.await.unwrap().unwrap();
        assert!(ctx.indexer.read().await.is_none());
        release.notify_one();
    }

    #[tokio::test]
    async fn initial_scheduler_connection_enters_quarantine() {
        let (scheduler_addr, _server, calls) =
            start_test_scheduler(TipBehavior::Pending, NullBehavior::Ready).await;
        let config = GatewayConfig {
            scheduler_address: scheduler_addr.to_string(),
            indexer_address: None,
            db_gateway_address: None,
            view_gateway_address: None,
            scheduler_reconnect_grace_ms: 25,
            ..GatewayConfig::default()
        };
        let metrics = GatewayMetrics::for_tests();

        let ctx = GatewayContext::new(config, metrics)
            .await
            .expect("scheduler startup connection should succeed");

        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.scheduler_quarantine.read().await.is_some());
        assert_eq!(calls.null.load(Ordering::SeqCst), 0);
        assert_eq!(calls.tip.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn scheduler_request_deadline_fails_closed_before_worker_cancellation() {
        let (scheduler_addr, _server, calls) =
            start_test_scheduler(TipBehavior::Pending, NullBehavior::Ready).await;
        let client = connect_optional_scheduler(&scheduler_addr.to_string(), 1)
            .await
            .expect("scheduler connection should succeed")
            .expect("scheduler should be connected");
        let metrics = GatewayMetrics::for_tests();
        let scheduler = Arc::new(RwLock::new(Some(client)));
        let sink =
            SchedulerTipSink::new(scheduler.clone(), metrics.clone(), Duration::from_secs(1));

        assert!(matches!(
            sink.get_tip(std::time::Instant::now() + Duration::from_millis(25))
                .await,
            Err(GatewayError::BackendTimeout)
        ));
        assert!(scheduler.read().await.is_none());
        assert_eq!(calls.tip.load(Ordering::SeqCst), 1);

        let snapshot = HealthSnapshot::for_test(HealthStatus::Error, None, &[]);
        let body = metrics.gather(&snapshot).expect("metrics should encode");
        assert!(body.contains(
            "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedcoord_get_tip\",outcome=\"timeout\"} 1"
        ));
    }

    #[tokio::test]
    async fn old_scheduler_timeout_does_not_clear_new_generation() {
        let (scheduler_addr, _server, calls) =
            start_test_scheduler(TipBehavior::Pending, NullBehavior::Ready).await;
        let old_client = connect_optional_scheduler(&scheduler_addr.to_string(), 1)
            .await
            .expect("old scheduler connection should succeed")
            .expect("old scheduler should be connected");
        let new_client = connect_optional_scheduler(&scheduler_addr.to_string(), 1)
            .await
            .expect("new scheduler connection should succeed")
            .expect("new scheduler should be connected");
        let new_generation = new_client.generation();
        let scheduler = Arc::new(RwLock::new(Some(old_client)));
        let sink = Arc::new(SchedulerTipSink::new(
            scheduler.clone(),
            GatewayMetrics::for_tests(),
            Duration::from_millis(25),
        ));
        let call = tokio::spawn(async move {
            sink.get_tip(std::time::Instant::now() + Duration::from_secs(1))
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while calls.tip.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old scheduler call should reach the backend");
        *scheduler.write().await = Some(new_client);

        assert!(matches!(
            call.await.unwrap(),
            Err(GatewayError::BackendTimeout)
        ));
        assert_eq!(
            scheduler
                .read()
                .await
                .as_ref()
                .expect("new scheduler generation should remain installed")
                .generation(),
            new_generation
        );
    }

    #[tokio::test]
    async fn scheduler_quarantine_tip_timeout_never_publishes_and_retries() {
        let (scheduler_addr, _server, calls) =
            start_test_scheduler(TipBehavior::Pending, NullBehavior::Ready).await;
        let config = Arc::new(GatewayConfig {
            scheduler_address: scheduler_addr.to_string(),
            indexer_address: None,
            db_gateway_address: None,
            view_gateway_address: None,
            scheduler_reconnect_grace_ms: 30_000,
            ..GatewayConfig::default()
        });
        let metrics = GatewayMetrics::for_tests();
        let ctx = Arc::new(GatewayContext::assemble(
            PROTOCOL_VERSION,
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config,
            metrics,
        ));
        let mut reconnects = ReconnectAttempts::default();
        maintain_reconnects(&ctx, &mut reconnects).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                reconnects.reap_finished().await;
                if ctx.scheduler_quarantine.read().await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("scheduler reconnect should enter quarantine");
        let mut scheduler_state = SchedulerProbeState::default();

        let grace_result =
            probe_scheduler(&ctx, Duration::from_millis(25), &mut scheduler_state).await;
        assert!(!grace_result.up);
        assert_eq!(
            grace_result.error.as_deref(),
            Some("scheduler reconnect readiness grace active")
        );
        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.scheduler_quarantine.read().await.is_some());
        assert_eq!(calls.null.load(Ordering::SeqCst), 1);
        assert_eq!(calls.tip.load(Ordering::SeqCst), 0);

        let before_deadline =
            probe_scheduler(&ctx, Duration::from_millis(25), &mut scheduler_state).await;
        assert!(!before_deadline.up);
        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.scheduler_quarantine.read().await.is_some());
        assert_eq!(calls.null.load(Ordering::SeqCst), 2);
        assert_eq!(calls.tip.load(Ordering::SeqCst), 0);

        ctx.scheduler_quarantine
            .write()
            .await
            .as_mut()
            .expect("scheduler should remain quarantined")
            .ready_at = Instant::now();
        let timeout_result =
            probe_scheduler(&ctx, Duration::from_millis(25), &mut scheduler_state).await;
        assert!(!timeout_result.up);
        assert_eq!(
            timeout_result.error.as_deref(),
            Some("scheduler quarantine tip probe timed out; reconnecting")
        );
        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.scheduler_quarantine.read().await.is_none());
        assert_eq!(calls.null.load(Ordering::SeqCst), 3);
        assert_eq!(calls.tip.load(Ordering::SeqCst), 1);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                maintain_reconnects(&ctx, &mut reconnects).await;
                if ctx.scheduler_quarantine.read().await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("scheduler reconnect should retry after quarantine timeout");
        assert!(ctx.scheduler.read().await.is_none());

        let snapshot = HealthSnapshot::for_test(HealthStatus::Error, None, &[]);
        let body = ctx
            .metrics
            .gather(&snapshot)
            .expect("metrics should encode");
        assert!(body.contains(
            "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedulercoord_null\",outcome=\"ok\"} 3"
        ));
        assert!(body.contains(
            "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedcoord_get_tip\",outcome=\"timeout\"} 1"
        ));
    }

    #[tokio::test]
    async fn quarantined_scheduler_identity_mismatch_is_never_published() {
        let (scheduler_addr, _server, _calls) =
            start_test_scheduler(TipBehavior::Ready, NullBehavior::Ready).await;
        let config = Arc::new(GatewayConfig {
            scheduler_address: scheduler_addr.to_string(),
            network_hash: Some(NetworkHashHex::from([0x42; 32])),
            indexer_address: None,
            db_gateway_address: None,
            view_gateway_address: None,
            scheduler_reconnect_grace_ms: 0,
            ..GatewayConfig::default()
        });
        let ctx = Arc::new(GatewayContext::assemble(
            PROTOCOL_VERSION,
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config.clone(),
            GatewayMetrics::for_tests(),
        ));
        let mut reconnects = ReconnectAttempts::default();
        maintain_reconnects(&ctx, &mut reconnects).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                reconnects.reap_finished().await;
                if ctx.scheduler_quarantine.read().await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("scheduler reconnect should enter quarantine");
        let mut scheduler_state =
            SchedulerProbeState::with_expected_network_hash(config.network_hash.clone());

        let result = probe_scheduler(&ctx, Duration::from_secs(1), &mut scheduler_state).await;

        assert!(!result.up);
        assert_eq!(result.identity_status, SchedulerIdentityStatus::Mismatch);
        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.scheduler_quarantine.read().await.is_none());
        assert_eq!(
            scheduler_state.expected_network_hash,
            Some(NetworkHashHex::from([0x42; 32]))
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                maintain_reconnects(&ctx, &mut reconnects).await;
                if ctx.scheduler_quarantine.read().await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("identity mismatch should permit a new quarantined reconnect attempt");
        assert!(ctx.scheduler.read().await.is_none());
    }

    #[tokio::test]
    async fn active_scheduler_identity_mismatch_is_evicted() {
        let (scheduler_addr, _server, _calls) =
            start_test_scheduler(TipBehavior::Ready, NullBehavior::Ready).await;
        let config = Arc::new(GatewayConfig {
            scheduler_address: scheduler_addr.to_string(),
            network_hash: Some(NetworkHashHex::from([0x42; 32])),
            indexer_address: None,
            db_gateway_address: None,
            view_gateway_address: None,
            ..GatewayConfig::default()
        });
        let client = connect_optional_scheduler(&scheduler_addr.to_string(), 1)
            .await
            .expect("scheduler connection should succeed")
            .expect("scheduler should be connected");
        let ctx = GatewayContext::assemble(
            PROTOCOL_VERSION,
            Arc::new(RwLock::new(Some(client))),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config.clone(),
            GatewayMetrics::for_tests(),
        );
        let mut scheduler_state =
            SchedulerProbeState::with_expected_network_hash(config.network_hash.clone());

        let result = probe_scheduler(&ctx, Duration::from_secs(1), &mut scheduler_state).await;

        assert!(!result.up);
        assert_eq!(result.identity_status, SchedulerIdentityStatus::Mismatch);
        assert!(ctx.scheduler.read().await.is_none());
    }

    #[tokio::test]
    async fn scheduler_probe_lane_remains_available_during_handler_saturation() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let (scheduler_addr, _server, calls) =
            start_test_scheduler(TipBehavior::Ready, NullBehavior::Wait(gate.clone())).await;
        let config = Arc::new(GatewayConfig {
            scheduler_address: scheduler_addr.to_string(),
            indexer_address: None,
            db_gateway_address: None,
            view_gateway_address: None,
            ..GatewayConfig::default()
        });
        let metrics = GatewayMetrics::for_tests();
        let client = connect_optional_scheduler(&scheduler_addr.to_string(), 1)
            .await
            .expect("scheduler connection should succeed")
            .expect("scheduler should be connected");
        let ctx = GatewayContext::assemble(
            PROTOCOL_VERSION,
            Arc::new(RwLock::new(Some(client.clone()))),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config,
            metrics,
        );
        let blocking_call = tokio::spawn(async move { client.schedulercoord_null().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while calls.null.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("saturating scheduler call should reach the backend");

        let mut scheduler_state = SchedulerProbeState::default();
        let probe = probe_scheduler(&ctx, Duration::from_millis(25), &mut scheduler_state).await;
        assert!(probe.up);
        assert!(ctx.scheduler.read().await.is_some());
        assert_eq!(calls.tip.load(Ordering::SeqCst), 1);

        gate.notify_one();
        tokio::time::timeout(Duration::from_secs(1), blocking_call)
            .await
            .expect("saturating scheduler call should finish")
            .unwrap()
            .expect("scheduler null call should succeed");
        assert_eq!(scheduler_state.timeout_streak, 0);
        assert!(ctx.scheduler.read().await.is_some());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn optional_backend_probe_lanes_remain_available_during_handler_saturation() {
        let (indexer_addr, _indexer_server, indexer_gate) = start_saturated_indexer().await;
        let (db_gateway_addr, _db_gateway_server, db_gateway_gate) =
            start_saturated_db_gateway().await;
        let (view_gateway_addr, _view_gateway_server, view_gateway_gate) =
            start_saturated_view_gateway().await;
        let startup_timeout = Duration::from_secs(1);
        let indexer =
            connect_optional_indexer(Some(&indexer_addr.to_string()), None, startup_timeout, None)
                .await
                .expect("bug: indexer connection should succeed")
                .expect("bug: indexer should be connected");
        let db_gateway = connect_optional_db_gateway(
            Some(&db_gateway_addr.to_string()),
            1,
            None,
            startup_timeout,
            None,
        )
        .await
        .expect("bug: db_gateway connection should succeed")
        .expect("bug: db_gateway should be connected");
        let view_gateway = connect_optional_view_gateway(
            Some(&view_gateway_addr.to_string()),
            None,
            startup_timeout,
            None,
        )
        .await
        .expect("bug: view_gateway connection should succeed")
        .expect("bug: view_gateway should be connected");
        let config = Arc::new(GatewayConfig {
            indexer_address: Some(indexer_addr.to_string()),
            db_gateway_address: Some(db_gateway_addr.to_string()),
            view_gateway_address: Some(view_gateway_addr.to_string()),
            ..GatewayConfig::default()
        });
        let ctx = GatewayContext::assemble(
            PROTOCOL_VERSION,
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(Some(indexer.clone()))),
            Arc::new(RwLock::new(Some(view_gateway.clone()))),
            Arc::new(RwLock::new(Some(db_gateway.clone()))),
            config,
            GatewayMetrics::for_tests(),
        );

        let tx_hash = TransactionHash::from(SHA256Hash::from([0; 32]));
        let indexer_calls: Vec<_> = (0..4)
            .map(|_| {
                let client = indexer.clone();
                tokio::spawn(async move { client.get_transaction_info(&tx_hash).await })
            })
            .collect();
        let db_gateway_call = tokio::spawn(async move {
            let batch: AddressAndKeyBatch = VecM(vec![]);
            db_gateway.dbgateway_get_items(&batch).await
        });
        let query = test_view_query();
        let view_gateway_calls: Vec<_> = (0..4)
            .map(|_| {
                let client = view_gateway.clone();
                let query = query.clone();
                tokio::spawn(async move { client.viewgateway_view(&query).await })
            })
            .collect();
        tokio::time::timeout(Duration::from_secs(1), async {
            while indexer_gate.calls.load(Ordering::SeqCst) < 4
                || db_gateway_gate.calls.load(Ordering::SeqCst) < 1
                || view_gateway_gate.calls.load(Ordering::SeqCst) < 4
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bug: handler pools should saturate");

        let protocol = PROTOCOL_VERSION;
        let mut indexer_timeouts = 0;
        let mut db_gateway_timeouts = 0;
        let mut view_gateway_timeouts = 0;
        let (indexer_probe, db_gateway_probe, view_gateway_probe) = tokio::join!(
            probe_indexer(
                &ctx,
                &protocol,
                Duration::from_millis(25),
                &mut indexer_timeouts,
            ),
            probe_db_gateway(
                &ctx,
                &protocol,
                Duration::from_millis(25),
                &mut db_gateway_timeouts,
            ),
            probe_view_gateway(
                &ctx,
                &protocol,
                Duration::from_millis(25),
                &mut view_gateway_timeouts,
            ),
        );

        assert!(indexer_probe.up);
        assert!(db_gateway_probe.up);
        assert!(view_gateway_probe.up);
        assert!(ctx.indexer.read().await.is_some());
        assert!(ctx.db_gateway.read().await.is_some());
        assert!(ctx.view_gateway.read().await.is_some());
        indexer_gate.release.notify_waiters();
        db_gateway_gate.release.notify_waiters();
        view_gateway_gate.release.notify_waiters();
        for call in indexer_calls {
            let _ = call.await;
        }
        for call in view_gateway_calls {
            let _ = call.await;
        }
        let _ = db_gateway_call.await;
    }

    #[tokio::test]
    async fn reconnect_attempts_are_single_and_cancel_on_shutdown() {
        struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let starts = Arc::new(AtomicUsize::new(0));
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let mut reconnects = ReconnectAttempts::default();
        let starts_for_task = starts.clone();
        spawn_once(&mut reconnects.indexer, move || async move {
            starts_for_task.fetch_add(1, Ordering::SeqCst);
            let _notify = NotifyOnDrop(Some(dropped_tx));
            std::future::pending::<()>().await;
        });
        spawn_once(&mut reconnects.indexer, || async {
            panic!("a second reconnect attempt must not start");
        });
        tokio::task::yield_now().await;

        assert_eq!(starts.load(Ordering::SeqCst), 1);
        reconnects.shutdown().await;
        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("shutdown should cancel the reconnect future")
            .unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn four_backend_restart_recovers_without_rebuilding_gateway() {
        let cluster = LifecycleBackendCluster::start(None).await;
        let addresses = cluster.addresses;
        let config = GatewayConfig {
            scheduler_address: addresses.scheduler.to_string(),
            indexer_address: Some(addresses.indexer.to_string()),
            db_gateway_address: Some(addresses.db_gateway.to_string()),
            view_gateway_address: Some(addresses.view_gateway.to_string()),
            health_required_backends: HealthBackend::ALL.to_vec(),
            request_timeout_ms: 100,
            health_probe_interval_ms: 500,
            health_chain_tip_max_age_ms: 60_000,
            backend_connect_timeout_ms: 1_000,
            scheduler_reconnect_grace_ms: 60_000,
            ..GatewayConfig::default()
        };
        let metrics = GatewayMetrics::for_tests();
        let health = GatewayHealth::new(&config, BuildVersion::new("test", None), None);
        let ctx = Arc::new(
            GatewayContext::new(config, metrics.clone())
                .await
                .expect("gateway should connect to the lifecycle fixtures"),
        );
        let backend = GatewayBackend::new(ctx.clone());

        assert!(ctx.scheduler.read().await.is_none());
        ctx.scheduler_quarantine
            .write()
            .await
            .as_mut()
            .expect("initial scheduler should be quarantined")
            .ready_at = Instant::now();
        let (health_shutdown_tx, health_shutdown_rx) = tokio::sync::oneshot::channel();
        let health_handle =
            spawn_health_probe_with_shutdown(ctx.clone(), health.clone(), health_shutdown_rx);

        let (observability_shutdown_tx, observability_shutdown_rx) =
            tokio::sync::oneshot::channel();
        let observability = spawn_observability_server(
            "127.0.0.1:0",
            health.clone(),
            metrics.clone(),
            observability_shutdown_rx,
        )
        .await
        .expect("observability server should start");
        wait_for_health_snapshot(&health, |snapshot| snapshot.status() == HealthStatus::Ok).await;
        assert_eq!(http_status(observability.local_addr, "/livez").await, 200);
        assert_eq!(http_status(observability.local_addr, "/readyz").await, 200);
        assert_eq!(http_status(observability.local_addr, "/health").await, 200);
        assert_lifecycle_data_plane_available(&backend).await;

        cluster.stop().await;
        wait_for_health_snapshot(&health, |snapshot| snapshot.status() == HealthStatus::Error)
            .await;
        wait_for_lifecycle_clients_cleared(&health, &ctx).await;
        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.scheduler_quarantine.read().await.is_none());
        assert!(ctx.indexer.read().await.is_none());
        assert!(ctx.db_gateway.read().await.is_none());
        assert!(ctx.view_gateway.read().await.is_none());
        assert_eq!(health.snapshot().await.status(), HealthStatus::Error);
        wait_for_failed_lifecycle_reconnects(&metrics, &health).await;
        assert_lifecycle_data_plane_unavailable(&backend).await;
        assert_eq!(http_status(observability.local_addr, "/livez").await, 200);
        assert_eq!(http_status(observability.local_addr, "/readyz").await, 503);
        assert_eq!(http_status(observability.local_addr, "/health").await, 503);

        let restarted_cluster = LifecycleBackendCluster::start(Some(addresses)).await;
        assert_eq!(restarted_cluster.addresses, addresses);
        let quarantine_snapshot = wait_for_health_snapshot(&health, |snapshot| {
            snapshot.status() == HealthStatus::Error
                && !snapshot.backend_up(HealthBackend::Scheduler)
                && snapshot.backend_up(HealthBackend::Indexer)
                && snapshot.backend_up(HealthBackend::DbGateway)
                && snapshot.backend_up(HealthBackend::ViewGateway)
        })
        .await;
        assert!(ctx.scheduler.read().await.is_none());
        assert!(ctx.scheduler_quarantine.read().await.is_some());
        assert!(ctx.indexer.read().await.is_some());
        assert!(ctx.db_gateway.read().await.is_some());
        assert!(ctx.view_gateway.read().await.is_some());

        assert_eq!(quarantine_snapshot.status(), HealthStatus::Error);
        assert!(!quarantine_snapshot.backend_up(HealthBackend::Scheduler));
        assert!(quarantine_snapshot.backend_up(HealthBackend::Indexer));
        assert!(quarantine_snapshot.backend_up(HealthBackend::DbGateway));
        assert!(quarantine_snapshot.backend_up(HealthBackend::ViewGateway));
        assert!(ctx.scheduler.read().await.is_none());
        assert!(
            ctx.scheduler_quarantine
                .read()
                .await
                .as_ref()
                .expect("scheduler should remain quarantined")
                .ready_at
                > Instant::now()
        );
        assert_eq!(http_status(observability.local_addr, "/readyz").await, 503);
        assert_eq!(http_status(observability.local_addr, "/health").await, 503);

        let receipt = backend
            .transaction_receipt(RpcRequestContext::unbounded(), lifecycle_tx_hash())
            .await
            .expect("indexer-backed receipt lookup should recover during scheduler quarantine");
        assert!(receipt.is_none());
        let storage = backend
            .get_storage_at(
                RpcRequestContext::unbounded(),
                lifecycle_contract_address(),
                [0x99; 32],
            )
            .await
            .expect("db gateway should recover during scheduler quarantine");
        assert_eq!(*storage.block_height, 42);
        let view = backend
            .view(
                RpcRequestContext::unbounded(),
                lifecycle_view_request(),
                1_000,
            )
            .await
            .expect("view gateway should recover during scheduler quarantine");
        assert_eq!(*view.height, 42);
        assert!(
            backend
                .chain_tip(RpcRequestContext::unbounded())
                .await
                .is_err(),
            "scheduler-backed calls should remain unavailable during quarantine"
        );

        ctx.scheduler_quarantine
            .write()
            .await
            .as_mut()
            .expect("scheduler should still be quarantined before promotion")
            .ready_at = Instant::now();
        wait_for_health_snapshot(&health, |snapshot| snapshot.status() == HealthStatus::Ok).await;
        assert!(ctx.scheduler.read().await.is_some());
        assert!(ctx.scheduler_quarantine.read().await.is_none());
        assert!(ctx.indexer.read().await.is_some());
        assert!(ctx.db_gateway.read().await.is_some());
        assert!(ctx.view_gateway.read().await.is_some());
        assert_eq!(health.snapshot().await.status(), HealthStatus::Ok);
        assert_eq!(http_status(observability.local_addr, "/livez").await, 200);
        assert_eq!(http_status(observability.local_addr, "/readyz").await, 200);
        assert_eq!(http_status(observability.local_addr, "/health").await, 200);
        assert_lifecycle_data_plane_available(&backend).await;

        health_shutdown_tx
            .send(())
            .expect("health shutdown receiver should remain open");
        observability_shutdown_tx
            .send(())
            .expect("observability shutdown receiver should remain open");
        tokio::time::timeout(Duration::from_secs(5), health_handle)
            .await
            .expect("health loop shutdown should not hang")
            .expect("health loop task should join");
        tokio::time::timeout(Duration::from_secs(5), observability.handle)
            .await
            .expect("observability shutdown should not hang")
            .expect("observability task should join")
            .expect("observability server should stop cleanly");
        restarted_cluster.stop().await;
    }

    #[tokio::test]
    async fn stale_snapshot_is_error_even_when_backends_are_healthy() {
        let config = GatewayConfig {
            health_probe_interval_ms: 10,
            ..GatewayConfig::default()
        };
        let health = GatewayHealth::new(&config, BuildVersion::new("test", None), None);
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
