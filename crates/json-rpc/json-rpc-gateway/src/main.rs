mod adaptive_batch;
mod backend;
mod config;
mod context;
mod error;
mod handlers;
mod health;
mod metrics;
mod middleware;
mod observability;
mod rpc_middleware;
mod storage_batcher;
mod submitter;
mod tip_coalescer;

use std::{future::Future, path::Path, sync::Arc, time::Duration};

use clap::Parser;
use tokio::io::AsyncReadExt;
use tracing_subscriber::EnvFilter;

use crate::backend::GatewayBackend;
use crate::config::{GatewayConfig, HealthBackend};
use crate::context::GatewayContext;
use groundhog_json_rpc_api::GroundhogApiServer;
use groundhog_json_rpc_server::{BuildInfo, GroundhogRpcServer, RpcServerConfig};

const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;
const BUILD_VERSION: groundhog_build_info::BuildVersion = groundhog_build_info::build_version!();

/// Groundhog JSON-RPC gateway.
#[derive(Parser)]
#[command(
    name = "hog-gateway",
    version = BUILD_VERSION.to_string(),
    about = "Groundhog JSON-RPC gateway"
)]
struct Cli {
    /// Path to TOML config file
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Path to backend runtime manifest JSON
    #[arg(long)]
    runtime_manifest: Option<std::path::PathBuf>,

    /// Listen address (overrides config file)
    #[arg(long)]
    listen: Option<String>,

    /// Private health/metrics listen address (overrides config file)
    #[arg(long)]
    observability_listen: Option<String>,

    /// Comma-separated backends required for readiness
    #[arg(long, value_delimiter = ',')]
    health_required_backends: Option<Vec<HealthBackend>>,

    /// Maximum allowed age of the last successful chain-tip probe
    #[arg(long)]
    health_chain_tip_max_age_ms: Option<u64>,

    /// Background readiness probe interval
    #[arg(long)]
    health_probe_interval_ms: Option<u64>,

    /// Maximum time to build one backend x23 connection pool
    #[arg(long)]
    backend_connect_timeout_ms: Option<u64>,

    /// Delay scheduler tip probes after building its x23 connection pool
    #[arg(long)]
    scheduler_reconnect_grace_ms: Option<u64>,

    /// Maximum total time to drain after a shutdown signal
    #[arg(long)]
    shutdown_grace_ms: Option<u64>,

    /// Scheduler (mempool) x23 RPC address (overrides config file)
    #[arg(long)]
    scheduler: Option<String>,

    /// Indexer x23 RPC address (overrides config file)
    #[arg(long)]
    indexer: Option<String>,

    /// `DbGateway` x23 RPC address (overrides config file)
    #[arg(long)]
    db_gateway: Option<String>,

    /// `ViewGateway` x23 RPC address (overrides config file)
    #[arg(long)]
    view_gateway: Option<String>,

    /// Override hogd protocol string for local cross-repo smoke tests
    #[arg(long, hide = true)]
    hogd_protocol: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- Observability ---
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    // --- Configuration ---
    let mut config = match &cli.config {
        Some(path) => {
            let contents = tokio::fs::read_to_string(path).await?;
            toml::from_str::<GatewayConfig>(&contents)?
        }
        None => GatewayConfig::default(),
    };

    if let Some(path) = &cli.runtime_manifest {
        let contents = read_runtime_manifest(path).await?;
        let manifest =
            groundhog_json_rpc_types::runtime_manifest::parse_runtime_manifest(&contents)?;
        config.apply_runtime_manifest(&manifest)?;
    }

    apply_cli_overrides(&mut config, cli);

    tracing::info!(
        listen = %config.listen_address,
        observability_listen = %config.observability_listen,
        health_required_backends = ?config.health_required_backends,
        backend_connect_timeout_ms = config.backend_connect_timeout_ms,
        scheduler_reconnect_grace_ms = config.scheduler_reconnect_grace_ms,
        shutdown_grace_ms = config.shutdown_grace_ms,
        submission_backend = ?config.submission_backend,
        scheduler = %config.scheduler_address,
        indexer = ?config.indexer_address,
        view_gateway = ?config.view_gateway_address,
        hogd_protocol_override = ?config.hogd_protocol_override,
        coordinator = ?config.coordinator_address,
        "loading configuration",
    );

    // --- Metrics registry ---
    let metrics = metrics::GatewayMetrics::new(BUILD_VERSION, option_env!("GIT_COMMIT"))?;
    metrics.set_rpc_connections_limit(config.max_connections);

    // --- Context (backend connections) ---
    let ctx = Arc::new(GatewayContext::new(config, metrics.clone()).await?);

    // --- Observability HTTP server ---
    let health = health::GatewayHealth::new(&ctx.config, BUILD_VERSION, option_env!("GIT_COMMIT"));
    let (health_shutdown_tx, health_shutdown_rx) = tokio::sync::oneshot::channel();
    let health_handle =
        health::spawn_health_probe_with_shutdown(ctx.clone(), health.clone(), health_shutdown_rx);
    let (observability_shutdown_tx, observability_shutdown_rx) = tokio::sync::oneshot::channel();
    let observability_server = observability::spawn_observability_server(
        &ctx.config.observability_listen,
        health,
        metrics.clone(),
        observability_shutdown_rx,
    )
    .await?;

    // --- Server ---
    let server_cfg = jsonrpsee::server::ServerConfig::builder()
        .max_connections(ctx.config.max_connections)
        .set_batch_request_config(jsonrpsee::server::BatchRequestConfig::Limit(1_000))
        .build();
    let http_mw = middleware::http_middleware(&ctx.config, metrics.clone());
    let rpc_metrics = metrics.clone();
    let rpc_mw = jsonrpsee::core::middleware::RpcServiceBuilder::new()
        .layer_fn(move |inner| rpc_middleware::RpcMetricsService::new(inner, rpc_metrics.clone()));
    let server = jsonrpsee::server::Server::builder()
        .set_config(server_cfg)
        .set_http_middleware(http_mw)
        .set_rpc_middleware(rpc_mw)
        .build(&ctx.config.listen_address)
        .await?;

    let rpc_config = RpcServerConfig::default()
        .with_request_timeout_ms(ctx.config.request_timeout_ms.max(3))
        .with_max_events_per_page(ctx.config.max_events_per_page)
        .with_max_transactions_per_page(ctx.config.max_transactions_per_page)
        .with_max_cursor_bytes(ctx.config.max_cursor_bytes)
        .with_max_view_calldata_bytes(ctx.config.max_view_calldata_bytes)
        .with_view_gas_limit(ctx.config.view_gas_limit);
    let api_impl = GroundhogRpcServer::unified(
        GatewayBackend::new(ctx.clone()),
        rpc_config,
        BuildInfo::from_build_version(BUILD_VERSION, option_env!("GIT_COMMIT")),
    );
    let handle = server.start(api_impl.into_rpc());

    tracing::info!("hog-gateway listening");

    shutdown_signal().await?;
    let _ = health_shutdown_tx.send(());
    let _ = observability_shutdown_tx.send(());
    handle.stop()?;
    let shutdown_result = drain_shutdown(
        handle.stopped(),
        health_handle,
        observability_server.handle,
        Duration::from_millis(ctx.config.shutdown_grace_ms.max(1)),
    )
    .await;
    ctx.abort_request_workers();
    shutdown_result?;

    Ok(())
}

fn apply_cli_overrides(config: &mut GatewayConfig, cli: Cli) {
    if let Some(listen) = cli.listen {
        config.listen_address = listen;
    }
    if let Some(observability_listen) = cli.observability_listen {
        config.observability_listen = observability_listen;
    }
    if let Some(required_backends) = cli.health_required_backends {
        config.health_required_backends = required_backends;
    }
    if let Some(max_age_ms) = cli.health_chain_tip_max_age_ms {
        config.health_chain_tip_max_age_ms = max_age_ms;
    }
    if let Some(interval_ms) = cli.health_probe_interval_ms {
        config.health_probe_interval_ms = interval_ms;
    }
    if let Some(timeout_ms) = cli.backend_connect_timeout_ms {
        config.backend_connect_timeout_ms = timeout_ms;
    }
    if let Some(grace_ms) = cli.scheduler_reconnect_grace_ms {
        config.scheduler_reconnect_grace_ms = grace_ms;
    }
    if let Some(grace_ms) = cli.shutdown_grace_ms {
        config.shutdown_grace_ms = grace_ms;
    }
    if let Some(scheduler) = cli.scheduler {
        config.scheduler_address = scheduler;
    }
    if let Some(indexer) = cli.indexer {
        config.indexer_address = Some(indexer);
    }
    if let Some(db_gateway) = cli.db_gateway {
        config.db_gateway_address = Some(db_gateway);
    }
    if let Some(view_gateway) = cli.view_gateway {
        config.view_gateway_address = Some(view_gateway);
    }
    if let Some(hogd_protocol) = cli.hogd_protocol {
        config.hogd_protocol_override = Some(hogd_protocol);
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}

async fn drain_shutdown<R>(
    rpc_stopped: R,
    mut health_handle: tokio::task::JoinHandle<()>,
    mut observability_handle: tokio::task::JoinHandle<std::io::Result<()>>,
    grace: Duration,
) -> std::io::Result<()>
where
    R: Future<Output = ()>,
{
    let deadline = tokio::time::Instant::now()
        .checked_add(grace)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "shutdown grace exceeds the platform timer range",
            )
        })?;
    let mut rpc_stopped = std::pin::pin!(rpc_stopped);
    let mut rpc_done = false;
    let mut health_done = false;
    let mut observability_done = false;
    let mut first_error = None;

    while !(rpc_done && health_done && observability_done) {
        tokio::select! {
            biased;
            () = &mut rpc_stopped, if !rpc_done => rpc_done = true,
            result = &mut health_handle, if !health_done => {
                health_done = true;
                if let Err(error) = result {
                    first_error.get_or_insert_with(|| std::io::Error::other(error));
                }
            }
            result = &mut observability_handle, if !observability_done => {
                observability_done = true;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        first_error.get_or_insert(error);
                    }
                    Err(error) => {
                        first_error.get_or_insert_with(|| std::io::Error::other(error));
                    }
                }
            }
            () = tokio::time::sleep_until(deadline) => break,
        }
    }

    if !(rpc_done && health_done && observability_done) {
        tracing::warn!(
            grace = ?grace,
            rpc_pending = !rpc_done,
            health_pending = !health_done,
            observability_pending = !observability_done,
            "gateway shutdown grace expired; canceling owned tasks"
        );
        if !health_done {
            health_handle.abort();
        }
        if !observability_done {
            observability_handle.abort();
        }
    }

    first_error.map_or(Ok(()), Err)
}

async fn read_runtime_manifest(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let file = tokio::fs::File::open(path).await?;
    let mut contents = Vec::new();
    let mut reader = file.take(MAX_RUNTIME_MANIFEST_BYTES + 1);
    reader.read_to_end(&mut contents).await?;
    if contents.len() as u64 > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(format!(
            "runtime manifest {} exceeds {} bytes",
            path.display(),
            MAX_RUNTIME_MANIFEST_BYTES,
        )
        .into());
    }
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[tokio::test]
    async fn shutdown_joins_subsystems_that_finish_within_grace() {
        let health = tokio::spawn(async {});
        let observability = tokio::spawn(async { Ok(()) });

        drain_shutdown(async {}, health, observability, Duration::from_secs(1))
            .await
            .expect("bug: graceful shutdown should succeed");
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_aborts_owned_tasks_at_one_total_deadline() {
        let (health_dropped_tx, health_dropped_rx) = tokio::sync::oneshot::channel();
        let health = tokio::spawn(async move {
            let _notice = NotifyOnDrop(Some(health_dropped_tx));
            std::future::pending::<()>().await;
        });
        let (observability_dropped_tx, observability_dropped_rx) = tokio::sync::oneshot::channel();
        let observability = tokio::spawn(async move {
            let _notice = NotifyOnDrop(Some(observability_dropped_tx));
            std::future::pending::<std::io::Result<()>>().await
        });
        tokio::task::yield_now().await;

        let started_at = tokio::time::Instant::now();
        drain_shutdown(
            async {
                tokio::time::sleep(Duration::from_secs(7)).await;
            },
            health,
            observability,
            Duration::from_secs(10),
        )
        .await
        .expect("bug: hard shutdown should not manufacture an error");
        assert_eq!(started_at.elapsed(), Duration::from_secs(10));
        health_dropped_rx
            .await
            .expect("bug: health task should be canceled and dropped");
        observability_dropped_rx
            .await
            .expect("bug: observability task should be canceled and dropped");
    }

    #[tokio::test]
    async fn shutdown_rejects_an_unrepresentable_deadline() {
        let health = tokio::spawn(async {});
        let observability = tokio::spawn(async { Ok(()) });

        let error = drain_shutdown(async {}, health, observability, Duration::MAX)
            .await
            .expect_err("bug: an unrepresentable shutdown grace should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
