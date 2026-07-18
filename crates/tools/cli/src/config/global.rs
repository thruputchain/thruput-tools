use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::atomic::atomic_write_private;
use super::dirs::config_file_path;
use std::path::PathBuf;

use super::project::{Network, ProjectProfile};
use crate::CliError;

const INITIAL_DEFAULT_PROFILE: &str = "local";
const DEVNET_RPC_URL: &str = "http://44.236.180.135:8545";
const DEVNET_FAUCET_URL: &str = "http://44.236.180.135:8085";
const HOG_CONTRACT_ADDRESS: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000001";

/// Credentials used to download the mock-executor FFI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutorCredentials {
    #[serde(default)]
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalRuntimeState {
    pub node_id: String,
    pub rpc_url: String,
    pub runtime_manifest: PathBuf,
    #[serde(default)]
    pub runtime_artifacts_dir: Option<PathBuf>,
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
    #[serde(default)]
    pub(crate) local_runtime_state: Option<LocalRuntimeState>,
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
    atomic_write_private(&path, contents.as_bytes())
}

/// Save mock-executor FFI download credentials to `config.toml`.
pub fn save_executor_credentials(creds: &ExecutorCredentials) -> Result<(), CliError> {
    let mut config = read_config_file()?;
    config.executor = Some(creds.clone());
    write_config_file(&config)
}

/// Load mock-executor FFI download credentials from `config.toml`.
///
/// Returns `Ok(None)` if no credentials are stored
/// or the file does not exist.
pub fn load_executor_credentials() -> Result<Option<ExecutorCredentials>, CliError> {
    let config = read_config_file()?;
    Ok(config.executor)
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
    if local.network.is_none() {
        local.network = Some(Network::Local);
    }
    local
        .contracts
        .entry("HOG".to_string())
        .or_insert_with(|| HOG_CONTRACT_ADDRESS.to_string());

    let devnet = config.profile.entry("devnet".to_string()).or_default();
    if devnet.network.is_none() {
        devnet.network = Some(Network::Devnet);
    }
    if devnet.rpc_url.is_none() {
        devnet.rpc_url = Some(DEVNET_RPC_URL.to_string());
    }
    if devnet.faucet_url.is_none() {
        devnet.faucet_url = Some(DEVNET_FAUCET_URL.to_string());
    }
    devnet
        .contracts
        .entry("HOG".to_string())
        .or_insert_with(|| HOG_CONTRACT_ADDRESS.to_string());

    write_config_file(&config)
}

pub fn save_local_runtime_state(state: &LocalRuntimeState) -> Result<PathBuf, CliError> {
    validate_local_runtime_state(state)?;
    let mut config = read_config_file()?;
    config.local_runtime_state = Some(state.clone());
    write_config_file(&config)?;
    config_file_path()
}

pub fn clear_local_runtime_state_if_matches(
    state: &LocalRuntimeState,
) -> Result<PathBuf, CliError> {
    let path = config_file_path()?;
    let mut config = match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str::<GlobalConfigFile>(&contents)
            .map_err(|e| CliError::Other(e.to_string()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(path),
        Err(e) => return Err(e.into()),
    };

    if config.local_runtime_state.as_ref() == Some(state) {
        config.local_runtime_state = None;
        write_config_file(&config)?;
    }

    Ok(path)
}

fn validate_local_runtime_state(state: &LocalRuntimeState) -> Result<(), CliError> {
    let rpc_url = state.rpc_url.trim();
    if rpc_url.is_empty() {
        return Err(CliError::Other("rpc_url cannot be empty".into()));
    }
    if !rpc_url.starts_with("http://") && !rpc_url.starts_with("https://") {
        return Err(CliError::Other(
            "rpc_url must start with http:// or https://".into(),
        ));
    }
    if state.node_id.trim().is_empty() {
        return Err(CliError::Other("node_id cannot be empty".into()));
    }
    if !state.runtime_manifest.is_absolute() {
        return Err(CliError::Other(
            "runtime_manifest written by groundhog node must be absolute".into(),
        ));
    }
    if state
        .runtime_artifacts_dir
        .as_deref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(CliError::Other(
            "runtime_artifacts_dir written by groundhog node must be absolute".into(),
        ));
    }
    Ok(())
}
