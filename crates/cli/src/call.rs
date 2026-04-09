use crate::{CliError, Print};

/// Call a method on a deployed contract.
pub fn run(
    _contract: &str,
    _method: &str,
    _args: &[String],
    _profile: &str,
    _cmd: &'static str,
    _print: &Print,
) -> Result<(), CliError> {
    Err(CliError::NotImplemented)
}
