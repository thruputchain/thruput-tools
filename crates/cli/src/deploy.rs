use crate::{CliError, Print};

/// Deploy a contract to the network.
pub fn run(
    _wasm: &Option<String>,
    _profile: &str,
    _cmd: &'static str,
    _print: &Print,
) -> Result<(), CliError> {
    Err(CliError::NotImplemented)
}
