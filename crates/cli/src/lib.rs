//! Groundhog CLI — smart contract development tools.

pub mod auth;
pub mod build;
pub mod call;
pub mod checks;
pub mod config;
pub mod contract_spec;
pub mod deploy;
pub mod dev_keys;
pub mod error;
pub mod init;
pub mod keys;
pub mod new;
pub mod node;
pub mod print;
pub mod result;
pub mod scaffold;
pub mod status;
pub mod test_cmd;
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
