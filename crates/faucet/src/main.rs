use clap::Parser;
use groundhog_faucet::{
    FaucetConfig, airdrop::RpcAirdropSubmitter, config::FaucetConfigFile, serve,
    wallet_create::RpcWalletCreateSubmitter,
};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "hog-faucet",
    version,
    about = "Groundhog Devnet faucet service"
)]
struct Cli {
    /// Path to faucet TOML config.
    #[arg(long)]
    config: std::path::PathBuf,

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
    let config_toml = tokio::fs::read_to_string(&cli.config)
        .await
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed to read faucet config `{}`: {error}",
                    cli.config.display()
                ),
            )
        })?;
    let file_config = toml::from_str::<FaucetConfigFile>(&config_toml)?;
    let (listen_address, network_descriptor, airdrop_config, wallet_create_config) =
        file_config.into_parts()?;
    let faucet_secret_key = airdrop_config.faucet_secret_key;
    let airdrop_submitter = RpcAirdropSubmitter::new(&network_descriptor, airdrop_config)?;
    let wallet_create_submitter =
        RpcWalletCreateSubmitter::new(&network_descriptor, wallet_create_config, faucet_secret_key);
    let mut config = FaucetConfig::new(
        network_descriptor,
        Arc::new(airdrop_submitter),
        Arc::new(wallet_create_submitter),
    );
    if let Some(listen) = listen_address {
        config.listen_address = listen;
    }
    if let Some(listen) = cli.listen {
        config.listen_address = listen;
    }

    serve(config).await?;
    Ok(())
}
