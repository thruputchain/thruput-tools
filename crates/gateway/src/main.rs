// TODO: remove once handlers are wired to the jsonrpsee server
#![allow(dead_code)]

mod api;
mod config;
mod context;
mod error;
mod handlers;
mod middleware;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::config::GatewayConfig;
use crate::context::GatewayContext;

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

    tracing::info!(
        listen = %config.listen_address,
        submission_backend = ?config.submission_backend,
        scheduler = %config.scheduler_address,
        indexer = %config.indexer_address,
        coordinator = ?config.coordinator_address,
        "loading configuration",
    );

    // --- Context (backend connections) ---
    let _ctx = GatewayContext::new(config)?;

    // --- Server ---
    // TODO: finish wiring the jsonrpsee server once the scaffolded backend
    // contracts in this repo solidify.
    //
    // Startup sequence:
    //
    // let middleware = middleware::middleware_stack(&ctx.config);
    //
    // let server = jsonrpsee::server::ServerBuilder::default()
    //     .set_http_middleware(middleware)
    //     .max_connections(ctx.config.max_in_flight_requests)
    //     .build(&ctx.config.listen_address)
    //     .await?;
    //
    // let api_impl = api::GroundhogApiImpl::new(ctx);
    // let handle = server.start(api_impl.into_rpc());
    //
    // tracing::info!("groundhog-rpc gateway listening");
    //
    // // Graceful shutdown on SIGINT/SIGTERM
    // tokio::signal::ctrl_c().await?;
    // handle.stop()?;
    // handle.stopped().await;

    tracing::info!("groundhog-rpc gateway starting");
    tracing::warn!("server scaffold only — backend wiring intentionally incomplete");

    Ok(())
}
