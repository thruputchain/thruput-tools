use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value, value};

use super::atomic::{ConfigFileKind, insert_preserving_decor, transact_document};
use super::dirs::config_file_path;
use super::project::ProjectProfile;
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

pub(crate) fn load_global_profile(name: &str) -> Result<Option<ProjectProfile>, CliError> {
    Ok(read_config_file()?.profile.remove(name))
}

fn transact_config_file(
    mutate: impl FnOnce(&mut DocumentMut, &GlobalConfigFile) -> Result<(), CliError>,
) -> Result<bool, CliError> {
    let path = config_file_path()?;
    transact_document(
        &path,
        ConfigFileKind::Global,
        |document| {
            let config = parse_config_document(document)?;
            mutate(document, &config)
        },
        |document| parse_config_document(document).map(|_| ()),
    )
}

fn parse_config_document(document: &DocumentMut) -> Result<GlobalConfigFile, CliError> {
    toml::from_str(&document.to_string()).map_err(|error| CliError::Other(error.to_string()))
}

fn root_table<'a>(
    document: &'a mut DocumentMut,
    key: &str,
    error: &'static str,
) -> Result<&'a mut Item, CliError> {
    let item = document
        .entry(key)
        .or_insert_with(|| Item::Table(Table::new()));
    if item.as_table_like().is_none() {
        return Err(CliError::Other(error.into()));
    }
    Ok(item)
}

fn child_table<'a>(
    parent: &'a mut Item,
    key: &str,
    error: &'static str,
) -> Result<&'a mut Item, CliError> {
    let default = if parent.as_inline_table().is_some() {
        Item::Value(Value::InlineTable(InlineTable::new()))
    } else {
        Item::Table(Table::new())
    };
    let child = parent
        .as_table_like_mut()
        .ok_or_else(|| CliError::Other(error.into()))?
        .entry(key)
        .or_insert(default);
    if child.as_table_like().is_none() {
        return Err(CliError::Other(error.into()));
    }
    Ok(child)
}

/// Save mock-executor FFI download credentials to `config.toml`.
pub fn save_executor_credentials(creds: &ExecutorCredentials) -> Result<(), CliError> {
    transact_config_file(|document, _| {
        let executor = root_table(document, "executor", "executor must be a table")?
            .as_table_like_mut()
            .ok_or_else(|| CliError::Other("executor must be a table".into()))?;
        insert_preserving_decor(executor, "username", value(creds.username.as_str()));
        insert_preserving_decor(executor, "password", value(creds.password.as_str()));
        Ok(())
    })?;
    Ok(())
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
    transact_config_file(|document, _| {
        insert_preserving_decor(document.as_table_mut(), "default_key", value(name));
        Ok(())
    })?;
    Ok(())
}

/// Load the default key name from `config.toml`.
pub fn load_default_key() -> Result<Option<String>, CliError> {
    let config = read_config_file()?;
    Ok(config.default_key)
}

/// Remove the default key selection from `config.toml`.
pub fn clear_default_key() -> Result<(), CliError> {
    transact_config_file(|document, _| {
        document.remove("default_key");
        Ok(())
    })?;
    Ok(())
}

/// Initialize global profile defaults without overwriting existing user config.
pub fn initialize_global_profiles() -> Result<(), CliError> {
    transact_config_file(|document, config| {
        if config
            .default_profile
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            insert_preserving_decor(
                document.as_table_mut(),
                "default_profile",
                value(INITIAL_DEFAULT_PROFILE),
            );
        }

        let profiles = root_table(document, "profile", "profile must be a table")?;
        {
            let local_config = config.profile.get("local");
            let local = child_table(profiles, "local", "profile entry must be a table")?;
            if local_config.is_none_or(|profile| profile.network.is_none()) {
                insert_preserving_decor(
                    local
                        .as_table_like_mut()
                        .ok_or_else(|| CliError::Other("profile entry must be a table".into()))?,
                    "network",
                    value("local"),
                );
            }
            if local_config.is_none_or(|profile| !profile.contracts.contains_key("HOG")) {
                insert_preserving_decor(
                    child_table(local, "contracts", "contracts must be a table")?
                        .as_table_like_mut()
                        .ok_or_else(|| CliError::Other("contracts must be a table".into()))?,
                    "HOG",
                    value(HOG_CONTRACT_ADDRESS),
                );
            }
        }
        {
            let devnet_config = config.profile.get("devnet");
            let devnet = child_table(profiles, "devnet", "profile entry must be a table")?;
            let devnet_table = devnet
                .as_table_like_mut()
                .ok_or_else(|| CliError::Other("profile entry must be a table".into()))?;
            if devnet_config.is_none_or(|profile| profile.network.is_none()) {
                insert_preserving_decor(devnet_table, "network", value("devnet"));
            }
            if devnet_config.is_none_or(|profile| profile.rpc_url.is_none()) {
                insert_preserving_decor(devnet_table, "rpc_url", value(DEVNET_RPC_URL));
            }
            if devnet_config.is_none_or(|profile| profile.faucet_url.is_none()) {
                insert_preserving_decor(devnet_table, "faucet_url", value(DEVNET_FAUCET_URL));
            }
            if devnet_config.is_none_or(|profile| !profile.contracts.contains_key("HOG")) {
                insert_preserving_decor(
                    child_table(devnet, "contracts", "contracts must be a table")?
                        .as_table_like_mut()
                        .ok_or_else(|| CliError::Other("contracts must be a table".into()))?,
                    "HOG",
                    value(HOG_CONTRACT_ADDRESS),
                );
            }
        }
        Ok(())
    })?;
    Ok(())
}

pub fn save_local_runtime_state(state: &LocalRuntimeState) -> Result<PathBuf, CliError> {
    validate_local_runtime_state(state)?;
    let runtime_manifest = state
        .runtime_manifest
        .to_str()
        .ok_or_else(|| CliError::Other("runtime_manifest must be valid UTF-8".into()))?;
    let runtime_artifacts_dir = state
        .runtime_artifacts_dir
        .as_deref()
        .map(|path| {
            path.to_str()
                .ok_or_else(|| CliError::Other("runtime_artifacts_dir must be valid UTF-8".into()))
        })
        .transpose()?;
    let path = config_file_path()?;
    transact_config_file(|document, _| {
        let runtime = root_table(
            document,
            "local_runtime_state",
            "local_runtime_state must be a table",
        )?
        .as_table_like_mut()
        .ok_or_else(|| CliError::Other("local_runtime_state must be a table".into()))?;
        insert_preserving_decor(runtime, "node_id", value(state.node_id.as_str()));
        insert_preserving_decor(runtime, "rpc_url", value(state.rpc_url.as_str()));
        insert_preserving_decor(runtime, "runtime_manifest", value(runtime_manifest));
        if let Some(artifacts_dir) = runtime_artifacts_dir {
            insert_preserving_decor(runtime, "runtime_artifacts_dir", value(artifacts_dir));
        } else {
            runtime.remove("runtime_artifacts_dir");
        }
        Ok(())
    })?;
    Ok(path)
}

pub fn clear_local_runtime_state_if_matches(
    state: &LocalRuntimeState,
) -> Result<PathBuf, CliError> {
    let path = config_file_path()?;
    transact_config_file(|document, config| {
        if config.local_runtime_state.as_ref() == Some(state) {
            document.remove("local_runtime_state");
        }
        Ok(())
    })?;
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
