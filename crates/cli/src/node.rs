//! `groundhog node` — start the local dev node.

use crate::{CliError, Print};

/// Start the dev node server.
///
/// Requires the `dev-node` feature.
#[cfg(feature = "dev-node")]
pub fn run(
    port: u16,
    account_count: usize,
    _cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    use groundhog_dev_node::{DevNodeConfig, start_server};

    let config = DevNodeConfig {
        listen_address: format!("127.0.0.1:{port}"),
        account_count,
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::Other(e.to_string()))?;

    rt.block_on(async {
        let handle = start_server(config)
            .await
            .map_err(|e| CliError::DevNode(e.to_string()))?;

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

        // Block until Ctrl-C
        tokio::signal::ctrl_c()
            .await
            .map_err(|e| CliError::Other(e.to_string()))?;

        print.status("");
        print.status("Shutting down...");
        let _ = handle.stop();
        handle.stopped().await;
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
