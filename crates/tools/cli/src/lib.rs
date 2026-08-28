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
pub(crate) mod http_client;
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

const BUILD_VERSION: groundhog_build_info::BuildVersion = groundhog_build_info::build_version!();

/// Discover the validated release identity for the `groundhog` client binary.
pub fn bundle_identity() -> groundhog_bundle_identity::BundleIdentity {
    groundhog_bundle_identity::BundleIdentity::discover(
        groundhog_bundle_identity::BundleSubject::client("groundhog", "groundhog-rpc", "cli"),
        BUILD_VERSION,
    )
}

/// Check that `cargo` is on PATH, returning a helpful error
/// if not found.
pub fn require_cargo() -> Result<(), CliError> {
    which::which("cargo").map_err(|_| CliError::CargoNotFound)?;
    Ok(())
}

/// Ask cargo for color back when its output is piped through us.
///
/// Piping stderr is what lets cargo's output be relayed and kept at the same
/// time, and it is also what makes cargo drop color, so restore it when our
/// own stderr is a terminal and the user has not already chosen.
pub(crate) fn keep_cargo_color(cmd: &mut std::process::Command) {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() && std::env::var_os("CARGO_TERM_COLOR").is_none() {
        cmd.env("CARGO_TERM_COLOR", "always");
    }
}

/// Stop and reap a cargo subprocess we can no longer follow.
///
/// Dropping a `Child` neither kills nor waits for it, so returning early from
/// a post-spawn failure would leave cargo running unattended — still working
/// the registry, still holding the package-cache lock — after the CLI has
/// reported failure and exited. The next build would then wait on a lock held
/// by a process nobody can see, which is the hang relaying cargo's output is
/// meant to make impossible.
///
/// Signals cargo itself, not the tree beneath it. Cargo runs in the CLI's own
/// process group so that a terminal's Ctrl-C reaches it — the way a user ends
/// a wait this code deliberately does not bound. Isolating cargo in a group
/// of its own would let its children be killed with it, but that interrupt
/// would no longer arrive, which is the worse trade: cargo owns the locks
/// that matter and killing it releases them, while a compiler left over from
/// an interrupted build finishes on its own.
pub(crate) fn stop_cargo(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Keep a copy of a cargo subprocess's stderr, relaying it to the user as it
/// arrives when `relay` is set.
///
/// Reading to end is also what keeps cargo running — a pipe nobody drains
/// stops it mid-write — so this must run before the child is waited on. The
/// copy is what lets a failure report what cargo actually said.
pub(crate) fn capture_cargo_stderr(
    mut pipe: impl std::io::Read,
    relay: bool,
) -> std::io::Result<Vec<u8>> {
    use std::io::Write;
    let mut captured = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = pipe.read(&mut chunk)?;
        if read == 0 {
            return Ok(captured);
        }
        if relay {
            std::io::stderr().write_all(&chunk[..read])?;
        }
        captured.extend_from_slice(&chunk[..read]);
    }
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
        keep_cargo_color(cmd);
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;
        let stderr_pipe = child
            .stderr
            .take()
            .expect("bug: child stderr is piped above");
        let captured = match capture_cargo_stderr(stderr_pipe, true) {
            Ok(captured) => captured,
            Err(error) => {
                stop_cargo(&mut child);
                return Err(error.into());
            }
        };
        let status = match child.wait() {
            Ok(status) => status,
            Err(error) => {
                stop_cargo(&mut child);
                return Err(error.into());
            }
        };
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
