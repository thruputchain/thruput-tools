use std::io::Write;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::dirs::config_file_path;
use super::project::ProjectProfile;
use crate::CliError;

const INITIAL_DEFAULT_PROFILE: &str = "local";
const LOCAL_RPC_URL: &str = "http://127.0.0.1:9545";

/// Executor credentials for the Groundhog test executor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutorCredentials {
    pub username: String,
    pub password: String,
}

/// Internal representation of `config.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct GlobalConfigFile {
    executor: Option<ExecutorCredentials>,
    default_key: Option<String>,
    #[serde(default)]
    pub(crate) default_profile: Option<String>,
    #[serde(default)]
    pub(crate) profile: BTreeMap<String, ProjectProfile>,
}

pub(crate) fn read_config_file() -> Result<GlobalConfigFile, CliError> {
    let path = config_file_path()?;
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let config: GlobalConfigFile =
                toml::from_str(&contents).map_err(|e| CliError::Other(e.to_string()))?;
            Ok(config)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GlobalConfigFile::default()),
        Err(e) => Err(e.into()),
    }
}

fn write_config_file(config: &GlobalConfigFile) -> Result<(), CliError> {
    let path = config_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string(config).map_err(|e| CliError::Other(e.to_string()))?;

    // Restrict permissions — file contains credentials.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);

    let mut file = opts.open(&path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// Save executor credentials to `config.toml`.
pub fn save_executor_credentials(creds: &ExecutorCredentials) -> Result<(), CliError> {
    let mut config = read_config_file()?;
    config.executor = Some(creds.clone());
    write_config_file(&config)
}

/// Load executor credentials from `config.toml`.
///
/// Returns `Ok(None)` if no credentials are stored
/// or the file does not exist.
pub fn load_executor_credentials() -> Result<Option<ExecutorCredentials>, CliError> {
    let config = read_config_file()?;
    Ok(config.executor)
}

/// Remove executor credentials from `config.toml`.
pub fn remove_executor_credentials() -> Result<(), CliError> {
    let mut config = read_config_file()?;
    config.executor = None;
    write_config_file(&config)
}

/// Save the default key name to `config.toml`.
pub fn save_default_key(name: &str) -> Result<(), CliError> {
    let mut config = read_config_file()?;
    config.default_key = Some(name.to_string());
    write_config_file(&config)
}

/// Load the default key name from `config.toml`.
pub fn load_default_key() -> Result<Option<String>, CliError> {
    let config = read_config_file()?;
    Ok(config.default_key)
}

/// Remove the default key selection from `config.toml`.
pub fn clear_default_key() -> Result<(), CliError> {
    let mut config = read_config_file()?;
    config.default_key = None;
    write_config_file(&config)
}

/// Initialize global profile defaults without overwriting existing user config.
pub fn initialize_global_profiles() -> Result<(), CliError> {
    let mut config = read_config_file()?;
    if config
        .default_profile
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        config.default_profile = Some(INITIAL_DEFAULT_PROFILE.to_string());
    }

    let local = config.profile.entry("local".to_string()).or_default();
    if local.rpc_url.is_none() {
        local.rpc_url = Some(LOCAL_RPC_URL.to_string());
    }
    config.profile.entry("devnet".to_string()).or_default();

    write_config_file(&config)
}
