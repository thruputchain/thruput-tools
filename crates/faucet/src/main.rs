use clap::Parser;
use groundhog_faucet::{
    FaucetConfig, airdrop::RpcAirdropSubmitter, config::FaucetConfigFile, serve,
    wallet_create::RpcWalletCreateSubmitter,
};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "groundhog-faucet",
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

/// Log filter from `RUST_LOG`, defaulting to `info` so operators get logs
/// without extra environment setup.
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
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
    let parts = file_config.into_parts()?;
    let wallet_create_submitter = RpcWalletCreateSubmitter::new(
        &parts.network_descriptor,
        parts.wallet_create,
        &parts.airdrop.faucet_secret_key,
    );
    let airdrop_submitter = RpcAirdropSubmitter::new(&parts.network_descriptor, &parts.airdrop)?;
    let mut config = FaucetConfig::new(
        parts.network_descriptor,
        Arc::new(airdrop_submitter),
        Arc::new(wallet_create_submitter),
    );
    if let Some(listen) = parts.listen_address {
        config.listen_address = listen;
    }
    if let Some(listen) = cli.listen {
        config.listen_address = listen;
    }
    config.cors_allowed_origins = parts.cors_allowed_origins;

    serve(config).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::env_filter;

    #[test]
    #[serial]
    fn env_filter_defaults_to_info_when_rust_log_unset() {
        unsafe { std::env::remove_var("RUST_LOG") };

        assert_eq!(env_filter().to_string(), "info");
    }

    #[test]
    #[serial]
    fn env_filter_honors_rust_log_when_set() {
        unsafe { std::env::set_var("RUST_LOG", "debug") };

        let filter = env_filter().to_string();

        unsafe { std::env::remove_var("RUST_LOG") };
        assert_eq!(filter, "debug");
    }
}
