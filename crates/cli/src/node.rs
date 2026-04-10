//! `groundhog node` — start the local dev node.

use crate::{CliError, Print};

#[cfg(feature = "dev-node")]
use crate::result::CommandResult;
#[cfg(feature = "dev-node")]
use groundhog_rpc_api_types::dev::DevAccount;
#[cfg(feature = "dev-node")]
use serde_json::json;

/// Start the dev node server.
///
/// Requires the `dev-node` feature.
#[cfg(feature = "dev-node")]
pub fn run(
    port: u16,
    account_count: usize,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    use groundhog_dev_node::{DevNodeConfig, start_server};
    use tracing_subscriber::EnvFilter;

    let default_filter = if print.is_json() {
        "groundhog_dev_node=warn"
    } else {
        "groundhog_dev_node=info"
    };

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .with_target(false)
        .without_time()
        .try_init();

    let config = DevNodeConfig {
        listen_address: format!("127.0.0.1:{port}"),
        account_count,
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;

    rt.block_on(async {
        let handle = start_server(config)
            .await
            .map_err(|e| CliError::DevNode(e.to_string()))?;

        if print.is_json() {
            print.json(&CommandResult::new(
                cmd,
                startup_payload(handle.addr, &handle.accounts),
            ))?;
        } else {
            // Print startup banner
            print.status("Groundhog Dev Node v0.1.0");
            print.status("Chain: groundhog-local (dev mode)");
            print.status("");
            print.status("Available Accounts");
            print.status("==================");
            for (i, account) in handle.accounts.iter().enumerate() {
                let addr = serde_json::to_string(&account.address)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string();
                print.status(format_args!(
                    "({i}) {addr}  ({} storage tokens)",
                    account.storage_tokens
                ));
            }
            print.status("");
            print.status("Private Keys");
            print.status("==================");
            for (i, account) in handle.accounts.iter().enumerate() {
                let key = format!("0x{}", hex::encode(&account.private_key.0));
                print.status(format_args!("({i}) {key}"));
            }
            print.status("");
            print.warn(
                "These keys are for LOCAL DEVELOPMENT ONLY. \
                 Do not use on a real network.",
            );
            print.status("");
            print.status(format_args!("Listening on http://{}", handle.addr));
        }

        // Block until Ctrl-C
        tokio::signal::ctrl_c()
            .await
            .map_err(|e| CliError::RuntimeInit(e.to_string()))?;

        let stats = handle.stats().await.ok();
        if !print.is_json() {
            print.status("");
            print.status("Shutting down...");
        }
        let _ = handle.stop();
        handle.stopped().await;
        if let Some(stats) = stats
            && !print.is_json()
        {
            print.status(format_args!(
                "Summary: {} blocks, {} committed transactions",
                stats.block_count, stats.tx_count
            ));
        }
        Ok(())
    })
}

#[cfg(not(feature = "dev-node"))]
pub fn run(
    _port: u16,
    _account_count: usize,
    _cmd: &'static str,
    _print: &Print,
) -> Result<(), CliError> {
    Err(CliError::Other(
        "`groundhog node` requires the dev-node feature.\n\n  \
         hint: install with `groundhogup` or build with:\n        \
         cargo install groundhog-cli --features dev-node"
            .into(),
    ))
}

#[cfg(feature = "dev-node")]
fn startup_payload(addr: std::net::SocketAddr, accounts: &[DevAccount]) -> serde_json::Value {
    json!({
        "chain": "groundhog-local",
        "mode": "dev",
        "rpc_url": format!("http://{addr}"),
        "accounts": accounts,
    })
}

#[cfg(all(test, feature = "dev-node"))]
mod tests {
    use super::startup_payload;
    use groundhog_rpc_api_types::dev::DevAccount;
    use groundhog_rpc_api_types::hex::{Hex0x, HexBytes};
    use groundhog_xdr::types::ContractAddress;

    #[test]
    fn startup_payload_includes_rpc_url_and_accounts() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let account = DevAccount {
            address: Hex0x(ContractAddress::from([7_u8; 32])),
            public_key: HexBytes::from(vec![8_u8; 32]),
            private_key: HexBytes::from(vec![9_u8; 32]),
            storage_tokens: 42,
        };

        let payload = startup_payload(addr, &[account]);
        assert_eq!(payload["rpc_url"], "http://127.0.0.1:9545");
        assert_eq!(
            payload["accounts"]
                .as_array()
                .expect("bug: accounts array")
                .len(),
            1
        );
    }
}
