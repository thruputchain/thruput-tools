mod api;
mod config;
mod context;
mod error;
mod handlers;
mod middleware;

use clap::Parser;
use tracing_subscriber::EnvFilter;

/// Groundhog JSON-RPC gateway.
#[derive(Parser)]
#[command(name = "groundhog-rpc", about = "Groundhog JSON-RPC gateway")]
struct Cli {
    /// Path to TOML config file
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Listen address
    #[arg(long, default_value = "0.0.0.0:8545")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let _cli = Cli::parse();

    // TODO: load config, create context, start server
    tracing::info!("groundhog-rpc gateway starting");

    Ok(())
}
