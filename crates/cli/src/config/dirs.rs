use std::path::PathBuf;

use crate::CliError;

/// Returns the user's home directory from `$HOME`.
pub fn home_dir() -> Result<PathBuf, CliError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| CliError::Other("HOME environment variable not set".into()))
}

/// Returns the Thruput configuration directory.
///
/// Uses `$XDG_CONFIG_HOME/thruput/` if set,
/// otherwise `~/.config/thruput/`.
pub fn config_dir() -> Result<PathBuf, CliError> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        Ok(PathBuf::from(xdg).join("thruput"))
    } else {
        Ok(home_dir()?.join(".config").join("thruput"))
    }
}

/// Returns the Thruput cache directory.
///
/// Uses `$XDG_CACHE_HOME/thruput/` if set,
/// otherwise `~/.cache/thruput/`.
pub fn cache_dir() -> Result<PathBuf, CliError> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        Ok(PathBuf::from(xdg).join("thruput"))
    } else {
        Ok(home_dir()?.join(".cache").join("thruput"))
    }
}

/// Returns the Thruput key directory.
///
/// Uses `<config_dir>/keys/`.
pub fn keys_dir() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("keys"))
}

/// Returns the path to the global `config.toml`.
pub(super) fn config_file_path() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("config.toml"))
}

/// Resolve the user's cargo config path.
pub(crate) fn cargo_config_path() -> Result<PathBuf, CliError> {
    let cargo_home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|_| home_dir().map(|h| h.join(".cargo")))?;
    Ok(cargo_home.join("config.toml"))
}
