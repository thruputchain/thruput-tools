mod backend;
mod config;
mod context;
mod error;
mod handlers;
mod middleware;
mod submitter;
mod tip_coalescer;

use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::backend::GatewayBackend;
use crate::config::GatewayConfig;
use crate::context::GatewayContext;
use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_server::{GroundhogRpcServer, RpcServerConfig};

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

    tracing::info!(
        listen = %config.listen_address,
        submission_backend = ?config.submission_backend,
        scheduler = %config.scheduler_address,
        indexer = %config.indexer_address,
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

    let rpc_config =
        RpcServerConfig::default().with_max_event_height_range(ctx.config.max_event_height_range);
    let api_impl = GroundhogRpcServer::unified(GatewayBackend::new(ctx), rpc_config);
    let handle = server.start(api_impl.into_rpc());

    tracing::info!("groundhog-rpc gateway listening");

    // Graceful shutdown on SIGINT/SIGTERM.
    tokio::signal::ctrl_c().await?;
    handle.stop()?;
    handle.stopped().await;

    Ok(())
}
