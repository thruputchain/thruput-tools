use clap::Parser;
use groundhog_faucet::{FaucetConfig, serve};
use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
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

    /// Path to public network descriptor JSON served by /v1/network.
    #[arg(long)]
    network_descriptor: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let descriptor_json = tokio::fs::read(&cli.network_descriptor)
        .await
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed to read network descriptor `{}`: {error}",
                    cli.network_descriptor.display()
                ),
            )
        })?;
    let network_descriptor = serde_json::from_slice::<NetworkDescriptor>(&descriptor_json)?;
    let mut config = FaucetConfig::new(network_descriptor);
    if let Some(listen) = cli.listen {
        config.listen_address = listen;
    }

    serve(config).await?;
    Ok(())
}
