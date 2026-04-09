use std::path::PathBuf;

use crate::CliError;

/// Returns the user's home directory from `$HOME`.
pub fn home_dir() -> Result<PathBuf, CliError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| CliError::Other("HOME environment variable not set".into()))
}

/// Returns the Groundhog configuration directory.
///
/// Uses `$XDG_CONFIG_HOME/groundhog/` if set,
/// otherwise `~/.config/groundhog/`.
pub fn config_dir() -> Result<PathBuf, CliError> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        Ok(PathBuf::from(xdg).join("groundhog"))
    } else {
        Ok(home_dir()?.join(".config").join("groundhog"))
    }
}

/// Returns the Groundhog cache directory.
///
/// Uses `$XDG_CACHE_HOME/groundhog/` if set,
/// otherwise `~/.cache/groundhog/`.
pub fn cache_dir() -> Result<PathBuf, CliError> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        Ok(PathBuf::from(xdg).join("groundhog"))
    } else {
        Ok(home_dir()?.join(".cache").join("groundhog"))
    }
}

/// Returns the Groundhog key directory.
///
/// Uses `<config_dir>/keys/`.
pub fn keys_dir() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("keys"))
}

/// Returns the path to the global `config.toml`.
pub(super) fn config_file_path() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("config.toml"))
}
