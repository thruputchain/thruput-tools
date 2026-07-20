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

use std::{path::Path, sync::Arc};

use clap::Parser;
use groundhog_json_rpc_types::runtime_manifest::RuntimeManifest;
use tokio::io::AsyncReadExt;
use tracing_subscriber::EnvFilter;

use crate::backend::GatewayBackend;
use crate::config::{GatewayConfig, HealthBackend};
use crate::context::GatewayContext;
use groundhog_json_rpc_api::GroundhogApiServer;
use groundhog_json_rpc_server::{BuildInfo, GroundhogRpcServer, RpcServerConfig};

const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Groundhog JSON-RPC gateway.
#[derive(Parser)]
#[command(name = "hog-gateway", version, about = "Groundhog JSON-RPC gateway")]
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

    /// Maximum time to rebuild one backend x23 connection pool
    #[arg(long)]
    backend_connect_timeout_ms: Option<u64>,

    /// Delay scheduler tip probes after rebuilding its x23 connection pool
    #[arg(long)]
    scheduler_reconnect_grace_ms: Option<u64>,

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
        let manifest = serde_json::from_slice::<RuntimeManifest>(&contents)?;
        config.apply_runtime_manifest(&manifest)?;
    }

    apply_cli_overrides(&mut config, cli);

    tracing::info!(
        listen = %config.listen_address,
        observability_listen = %config.observability_listen,
        health_required_backends = ?config.health_required_backends,
        backend_connect_timeout_ms = config.backend_connect_timeout_ms,
        scheduler_reconnect_grace_ms = config.scheduler_reconnect_grace_ms,
        submission_backend = ?config.submission_backend,
        scheduler = %config.scheduler_address,
        indexer = ?config.indexer_address,
        view_gateway = ?config.view_gateway_address,
        hogd_protocol_override = ?config.hogd_protocol_override,
        coordinator = ?config.coordinator_address,
        "loading configuration",
    );

    // --- Metrics registry ---
    let metrics =
        metrics::GatewayMetrics::new(env!("CARGO_PKG_VERSION"), option_env!("GIT_COMMIT"))?;
    metrics.set_rpc_connections_limit(config.max_connections);

    // --- Context (backend connections) ---
    let ctx = Arc::new(GatewayContext::new(config, metrics.clone()).await?);

    // --- Observability HTTP server ---
    let health = health::GatewayHealth::new(
        &ctx.config,
        env!("CARGO_PKG_VERSION"),
        option_env!("GIT_COMMIT"),
    );
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
        .with_max_events_per_page(ctx.config.max_events_per_page)
        .with_max_transactions_per_page(ctx.config.max_transactions_per_page)
        .with_max_cursor_bytes(ctx.config.max_cursor_bytes)
        .with_max_view_calldata_bytes(ctx.config.max_view_calldata_bytes)
        .with_view_gas_limit(ctx.config.view_gas_limit);
    let api_impl = GroundhogRpcServer::unified(
        GatewayBackend::new(ctx.clone()),
        rpc_config,
        BuildInfo::new(env!("CARGO_PKG_VERSION"), option_env!("GIT_COMMIT")),
    );
    let handle = server.start(api_impl.into_rpc());

    tracing::info!("hog-gateway listening");

    // Graceful shutdown on SIGINT/SIGTERM.
    tokio::signal::ctrl_c().await?;
    let _ = health_shutdown_tx.send(());
    let _ = observability_shutdown_tx.send(());
    handle.stop()?;
    handle.stopped().await;
    health_handle.await?;
    observability_server.handle.await??;

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
