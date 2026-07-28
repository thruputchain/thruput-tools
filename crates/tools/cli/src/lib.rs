//! Groundhog CLI — smart contract development tools.

pub(crate) mod account_resolution;
pub mod airdrop;
pub mod artifacts;
pub mod auth;
pub mod balance;
pub mod build;
pub mod call;
pub mod checks;
pub(crate) mod codec;
pub(crate) mod codegen;
pub mod codegen_cmd;
pub mod config;
pub mod contract_spec;
pub mod deploy;
pub(crate) mod dev_detail;
pub mod dev_keys;
pub mod error;
pub mod events;
pub mod events_json;
pub(crate) mod faucet_client;
pub(crate) mod hog;
pub mod init;
pub mod keys;
pub(crate) mod network_descriptor;
pub mod new;
pub mod node;
pub mod print;
pub(crate) mod profile_backend;
pub mod result;
pub(crate) mod runtime_manifest;
pub mod scaffold;
pub mod status;
pub mod storage;
pub(crate) mod terminal;
pub mod test_cmd;
pub mod test_json;
pub mod transfer;
pub mod transfer_json;
pub mod tx;
pub mod tx_json;
pub mod tx_utils;
pub mod view;
pub mod view_json;
pub mod wallet;
pub mod wasm_instrument;
pub use error::CliError;
pub use print::Print;

/// Check that `cargo` is on PATH, returning a helpful error
/// if not found.
pub fn require_cargo() -> Result<(), CliError> {
    which::which("cargo").map_err(|_| CliError::CargoNotFound)?;
    Ok(())
}

/// Run a cargo subprocess, capturing output when JSON mode
/// is active and streaming to the terminal otherwise.
pub fn run_cargo(
    cmd: &mut std::process::Command,
    print: &Print,
) -> Result<(String, String), CliError> {
    print.trace_command(cmd);
    if print.is_json() {
        let output = cmd.output()?;
        if !output.status.success() {
            return Err(CliError::CargoFailedWithOutput {
                code: output.status.code().unwrap_or(1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok((
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    } else {
        let status = cmd.status()?;
        if !status.success() {
            return Err(CliError::CargoFailed(status.code().unwrap_or(1)));
        }
        Ok((String::new(), String::new()))
    }
}
