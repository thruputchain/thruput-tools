//! Groundhog CLI — smart contract development tools.

pub mod abort_json;
pub(crate) mod account_resolution;
pub mod airdrop;
pub mod artifacts;
pub mod auth;
pub mod balance;
pub mod build;
pub mod byte_size;
pub mod call;
pub mod chain;
pub mod checks;
pub(crate) mod codec;
pub(crate) mod codegen;
pub mod codegen_cmd;
pub mod config;
pub mod config_cmd;
pub mod contract_spec;
pub mod deploy;
pub(crate) mod dev_detail;
pub mod dev_keys;
pub mod error;
pub(crate) mod event_spec;
pub mod events;
pub mod events_json;
pub(crate) mod faucet_client;
pub(crate) mod hog;
pub mod init;
pub mod keys;
pub(crate) mod keys_json;
pub(crate) mod network_descriptor;
pub mod new;
pub mod node;
pub(crate) mod pagination;
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
pub mod token;
pub mod token_json;
pub mod transfer;
pub mod transfer_json;
pub mod tx;
pub mod tx_json;
pub mod tx_utils;
pub mod view;
pub mod view_json;
pub mod wallet;
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
        // Tee stderr: stream it to the user as cargo produces it, and keep a
        // copy so failure hints can react to what cargo actually said (for
        // example a link error naming the Groundhog linker script). Piping
        // stderr would otherwise turn cargo's color off, so ask for it back
        // when our own stderr is a terminal and the user has not chosen.
        use std::io::{IsTerminal, Read, Write};
        if std::io::stderr().is_terminal() && std::env::var_os("CARGO_TERM_COLOR").is_none() {
            cmd.env("CARGO_TERM_COLOR", "always");
        }
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;
        let mut stderr_pipe = child
            .stderr
            .take()
            .expect("bug: child stderr is piped above");
        let mut captured = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let read = stderr_pipe.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            std::io::stderr().write_all(&chunk[..read])?;
            captured.extend_from_slice(&chunk[..read]);
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(CliError::CargoFailedWithOutput {
                code: status.code().unwrap_or(1),
                stdout: String::new(),
                stderr: String::from_utf8_lossy(&captured).into_owned(),
            });
        }
        Ok((String::new(), String::new()))
    }
}
