mod adaptive_batch;
mod backend;
mod config;
mod context;
mod error;
mod handlers;
mod middleware;
mod storage_batcher;
mod submitter;
mod tip_coalescer;

use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::backend::GatewayBackend;
use crate::config::GatewayConfig;
use crate::context::GatewayContext;
use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_server::{BuildInfo, GroundhogRpcServer, RpcServerConfig};

/// Groundhog JSON-RPC gateway.
#[derive(Parser)]
#[command(name = "groundhog-rpc", about = "Groundhog JSON-RPC gateway")]
struct Cli {
    /// Path to TOML config file
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Listen address (overrides config file)
    #[arg(long)]
    listen: Option<String>,

    /// Scheduler (mempool) x23 RPC address (overrides config file)
    #[arg(long)]
    scheduler: Option<String>,

    /// Indexer x23 RPC address (overrides config file)
    #[arg(long)]
    indexer: Option<String>,

    /// `ViewGateway` x23 RPC address (overrides config file)
    #[arg(long)]
    view_gateway: Option<String>,
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

    if let Some(listen) = cli.listen {
        config.listen_address = listen;
    }
    if let Some(scheduler) = cli.scheduler {
        config.scheduler_address = scheduler;
    }
    if let Some(indexer) = cli.indexer {
        config.indexer_address = indexer;
    }
    if let Some(view_gateway) = cli.view_gateway {
        config.view_gateway_address = Some(view_gateway);
    }

    tracing::info!(
        listen = %config.listen_address,
        submission_backend = ?config.submission_backend,
        scheduler = %config.scheduler_address,
        indexer = %config.indexer_address,
        view_gateway = ?config.view_gateway_address,
        coordinator = ?config.coordinator_address,
        "loading configuration",
    );

    // --- Context (backend connections) ---
    let ctx = Arc::new(GatewayContext::new(config).await?);

    // --- Server ---
    let server_cfg = jsonrpsee::server::ServerConfig::builder()
        .max_connections(ctx.config.max_connections)
        .build();
    let http_mw = middleware::http_middleware(&ctx.config);
    let server = jsonrpsee::server::Server::builder()
        .set_config(server_cfg)
        .set_http_middleware(http_mw)
        .build(&ctx.config.listen_address)
        .await?;

    let rpc_config = RpcServerConfig::default()
        .with_max_events_per_page(ctx.config.max_events_per_page)
        .with_max_cursor_bytes(ctx.config.max_cursor_bytes)
        .with_max_view_calldata_bytes(ctx.config.max_view_calldata_bytes)
        .with_view_gas_limit(ctx.config.view_gas_limit);
    let api_impl = GroundhogRpcServer::unified(
        GatewayBackend::new(ctx),
        rpc_config,
        BuildInfo::new(env!("CARGO_PKG_VERSION"), option_env!("GIT_COMMIT")),
    );
    let handle = server.start(api_impl.into_rpc());

    tracing::info!("groundhog-rpc gateway listening");

    // Graceful shutdown on SIGINT/SIGTERM.
    tokio::signal::ctrl_c().await?;
    handle.stop()?;
    handle.stopped().await;

    Ok(())
}
