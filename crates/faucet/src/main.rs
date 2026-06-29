use clap::Parser;
use groundhog_faucet::{FaucetConfig, serve};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "hog-faucet",
    version,
    about = "Groundhog Devnet faucet service"
)]
struct Cli {
    /// HTTP listen address.
    #[arg(long)]
    listen: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let mut config = FaucetConfig::default();
    if let Some(listen) = cli.listen {
        config.listen_address = listen;
    }

    serve(config).await?;
    Ok(())
}
