use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::dirs::config_file_path;
use super::global::{LocalRuntimeState, load_default_key, read_config_file};
use crate::CliError;

pub const DEFAULT_PROFILE: &str = "default";

/// Project profile loaded from `groundhog.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectProfile {
    #[serde(default)]
    pub network: Option<Network>,
    #[serde(default)]
    pub rpc_url: Option<String>,
    #[serde(default)]
    pub faucet_url: Option<String>,
    #[serde(default)]
    pub default_key: Option<String>,
    #[serde(default)]
    pub runtime_manifest: Option<PathBuf>,
    #[serde(default)]
    pub runtime_artifacts_dir: Option<PathBuf>,
    #[serde(default)]
    pub contracts: BTreeMap<String, String>,
    #[serde(default)]
    pub wallets: BTreeMap<String, SavedWalletBinding>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Local,
    Devnet,
    Mainnet,
}

/// Saved wallet binding for a local signing key.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SavedWalletBinding {
    /// Wallet contract address invoked by transactions for this key.
    pub address: String,
    /// Public key that owns the wallet contract.
    pub owner_public_key: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ProjectConfigFile {
    #[serde(default)]
    pub(crate) default_profile: Option<String>,
    #[serde(default)]
    pub(crate) profile: BTreeMap<String, ProjectProfile>,
}

/// Active project profile resolved from `groundhog.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub name: String,
    pub network: Option<Network>,
    pub rpc_url: Option<String>,
    pub faucet_url: Option<String>,
    #[serde(skip)]
    #[schemars(skip)]
    pub rpc_url_cli_override: bool,
    pub default_key: Option<String>,
    pub runtime_manifest: Option<PathBuf>,
    #[serde(skip)]
    #[schemars(skip)]
    pub runtime_manifest_cli_override: bool,
    pub runtime_artifacts_dir: Option<PathBuf>,
    pub contracts: BTreeMap<String, String>,
    #[serde(default)]
    pub wallets: BTreeMap<String, SavedWalletBinding>,
    pub source: Option<PathBuf>,
}

/// Explicit CLI profile overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileOverrides {
    pub rpc_url: Option<String>,
    pub runtime_manifest: Option<PathBuf>,
}

impl ResolvedProfile {
    /// Resolve a contract alias from the active profile, falling
    /// back to the original value when no alias exists.
    pub fn resolve_contract(&self, name_or_address: &str) -> String {
        self.contracts
            .get(name_or_address)
            .cloned()
            .unwrap_or_else(|| name_or_address.to_string())
    }

    /// Resolve a saved wallet name or direct address.
    pub fn resolve_wallet(&self, name_or_address: &str) -> Option<String> {
        if is_valid_address(name_or_address) {
            return Some(name_or_address.to_string());
        }
        self.wallets
            .get(name_or_address)
            .map(|wallet| wallet.address.clone())
    }
}

/// Returns the active profile name from an explicit selection,
/// `GROUNDHOG_PROFILE`, project default, global default, or the
/// built-in fallback.
pub fn active_profile_name(start: &Path, explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(name) = explicit.map(str::trim).filter(|name| !name.is_empty()) {
        return Ok(name.to_owned());
    }

    if let Some(name) = std::env::var("GROUNDHOG_PROFILE")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
    {
        return Ok(name);
    }

    if let Some(config) = load_project_config(start)?
        && let Some(name) = non_empty(config.default_profile)
    {
        return Ok(name);
    }

    let global = read_config_file()?;
    if let Some(name) = non_empty(global.default_profile) {
        return Ok(name);
    }

    Ok(DEFAULT_PROFILE.to_string())
}

/// Find `groundhog.toml` by walking up from `start`.
pub fn find_project_config(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start
            .parent()
            .map_or_else(|| start.to_path_buf(), Path::to_path_buf)
    } else {
        start.to_path_buf()
    };

    loop {
        let candidate = dir.join("groundhog.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Load `groundhog.toml` if one exists.
pub fn load_project_profiles(
    start: &Path,
) -> Result<Option<BTreeMap<String, ProjectProfile>>, CliError> {
    Ok(load_project_config(start)?.map(|config| config.profile))
}

fn load_project_config(start: &Path) -> Result<Option<ProjectConfigFile>, CliError> {
    let Some(path) = find_project_config(start) else {
        return Ok(None);
    };

    let contents = std::fs::read_to_string(path)?;
    let config: ProjectConfigFile =
        toml::from_str(&contents).map_err(|e| CliError::Other(e.to_string()))?;
    Ok(Some(config))
}

/// Persist a contract alias under the selected project profile.
///
/// Creates `groundhog.toml` if it does not already exist.
/// Uses `toml_edit` for in-place edits that preserve user
/// comments and formatting.
pub fn save_profile_contract_alias(
    project_root: &Path,
    explicit_profile: Option<&str>,
    alias: &str,
    address: &str,
) -> Result<PathBuf, CliError> {
    if alias.trim().is_empty() {
        return Err(CliError::Other("contract alias cannot be empty".into()));
    }
    if address.trim().is_empty() {
        return Err(CliError::Other("contract address cannot be empty".into()));
    }
    if !is_valid_address(address.trim()) {
        return Err(CliError::Other(
            "contract address must be a 0x-prefixed \
             64-character hex string"
                .into(),
        ));
    }

    let profile_name = active_profile_name(project_root, explicit_profile)?;
    let path = profile_write_path(project_root)?;

    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    let mut doc: toml_edit::DocumentMut = contents
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| CliError::Other(e.to_string()))?;

    doc.entry("profile")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| CliError::Other("profile must be a table".into()))?
        .entry(&profile_name)
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| CliError::Other("profile entry must be a table".into()))?
        .entry("contracts")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| CliError::Other("contracts must be a table".into()))?
        .insert(alias.trim(), toml_edit::value(address.trim()));

    write_profile_config(&path, doc.to_string().as_bytes())?;
    Ok(path)
}

/// Persist a wallet address under the selected project profile.
///
/// Creates `groundhog.toml` if it does not already exist.
/// Uses `toml_edit` for in-place edits that preserve user
/// comments and formatting.
pub fn save_profile_wallet_address(
    project_root: &Path,
    explicit_profile: Option<&str>,
    name: &str,
    address: &str,
    owner_public_key: &str,
) -> Result<PathBuf, CliError> {
    if name.trim().is_empty() {
        return Err(CliError::EmptyWalletName);
    }
    if address.trim().is_empty() {
        return Err(CliError::Other("wallet address cannot be empty".into()));
    }
    if !is_valid_address(address.trim()) {
        return Err(CliError::Other(
            "wallet address must be a 0x-prefixed 64-character hex string".into(),
        ));
    }
    if !is_valid_address(owner_public_key.trim()) {
        return Err(CliError::Other(
            "wallet owner public key must be a 0x-prefixed 64-character hex string".into(),
        ));
    }

    let profile_name = active_profile_name(project_root, explicit_profile)?;
    let path = profile_write_path(project_root)?;

    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    let mut doc: toml_edit::DocumentMut = contents
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| CliError::Other(e.to_string()))?;

    let wallet = doc
        .entry("profile")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| CliError::Other("profile must be a table".into()))?
        .entry(&profile_name)
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| CliError::Other("profile entry must be a table".into()))?
        .entry("wallets")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| CliError::Other("wallets must be a table".into()))?
        .entry(name.trim())
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| CliError::Other("wallet entry must be a table".into()))?;

    wallet.insert("address", toml_edit::value(address.trim()));
    wallet.insert(
        "owner_public_key",
        toml_edit::value(owner_public_key.trim()),
    );

    write_profile_config(&path, doc.to_string().as_bytes())?;
    Ok(path)
}

/// Remove a persisted wallet address from the selected project profile.
pub fn remove_profile_wallet_address(
    project_root: &Path,
    explicit_profile: Option<&str>,
    name: &str,
) -> Result<PathBuf, CliError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CliError::EmptyWalletName);
    }

    let profile_name = active_profile_name(project_root, explicit_profile)?;
    let path = profile_write_path(project_root)?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = contents
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| CliError::Other(e.to_string()))?;

    let wallets = doc
        .get_mut("profile")
        .and_then(toml_edit::Item::as_table_mut)
        .and_then(|profiles| profiles.get_mut(&profile_name))
        .and_then(toml_edit::Item::as_table_mut)
        .and_then(|profile| profile.get_mut("wallets"))
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| CliError::Other(format!("wallet `{name}` is not saved")))?;

    if wallets.remove(name).is_none() {
        return Err(CliError::Other(format!("wallet `{name}` is not saved")));
    }

    write_profile_config(&path, doc.to_string().as_bytes())?;
    Ok(path)
}

/// Resolve the effective default key for the active profile.
///
/// Prefers `groundhog.toml` when the profile specifies
/// `default_key`, otherwise falls back to the global default
/// selected via `keys use`.
pub fn resolve_default_key(
    start: &Path,
    explicit_profile: Option<&str>,
) -> Result<Option<String>, CliError> {
    let profile = resolve_profile(start, explicit_profile)?;
    if profile.default_key.is_some() {
        Ok(profile.default_key)
    } else {
        load_default_key()
    }
}

/// Resolve the active project profile, defaulting to `default`
/// when no `groundhog.toml` exists.
pub fn resolve_profile(start: &Path, explicit: Option<&str>) -> Result<ResolvedProfile, CliError> {
    let name = active_profile_name(start, explicit)?;
    let project_source = find_project_config(start);
    let global_source = config_file_path()?;
    let project_config = load_project_config(start)?;
    let global_config = read_config_file()?;
    let local_runtime_state = global_config.local_runtime_state.clone();

    let global_profile = global_config
        .profile
        .get(&name)
        .cloned()
        .map(|mut profile| {
            normalize_runtime_manifest(&mut profile, &global_source);
            normalize_runtime_artifacts_dir(&mut profile, &global_source);
            profile
        });
    let project_profile = match (&project_config, &project_source) {
        (Some(config), Some(path)) => config.profile.get(&name).cloned().map(|mut profile| {
            normalize_runtime_artifacts_dir(&mut profile, path);
            profile
        }),
        _ => None,
    };

    let source = match (&project_profile, &global_profile, &project_source) {
        (Some(_), _, Some(path)) => Some(path.clone()),
        (None, Some(_), _) => Some(global_source),
        _ => None,
    };

    let mut profile = match (global_profile, project_profile, project_source.is_some()) {
        (Some(global), Some(project), _) => merge_profiles(global, project),
        (Some(global), None, _) => global,
        (None, Some(project), _) => project,
        (None, None, _) if name == DEFAULT_PROFILE => ProjectProfile::default(),
        (None, None, false) => return Err(CliError::NoProjectConfig(name)),
        (None, None, true) => return Err(CliError::ProfileNotFound(name)),
    };

    apply_local_runtime_state(&mut profile, local_runtime_state.as_ref());

    Ok(ResolvedProfile {
        name,
        network: profile.network,
        rpc_url: profile.rpc_url,
        faucet_url: profile.faucet_url,
        rpc_url_cli_override: false,
        default_key: profile.default_key,
        runtime_manifest: profile.runtime_manifest,
        runtime_manifest_cli_override: false,
        runtime_artifacts_dir: profile.runtime_artifacts_dir,
        contracts: profile.contracts,
        wallets: profile.wallets,
        source,
    })
}

fn merge_profiles(mut global: ProjectProfile, project: ProjectProfile) -> ProjectProfile {
    if project.network.is_some() {
        global.network = project.network;
    }
    if project.rpc_url.is_some() {
        global.rpc_url = project.rpc_url;
    }
    if project.faucet_url.is_some() {
        global.faucet_url = project.faucet_url;
    }
    if project.default_key.is_some() {
        global.default_key = project.default_key;
    }
    if project.runtime_manifest.is_some() {
        global.runtime_manifest = project.runtime_manifest;
    }
    if project.runtime_artifacts_dir.is_some() {
        global.runtime_artifacts_dir = project.runtime_artifacts_dir;
    }
    global.contracts.extend(project.contracts);
    global.wallets.extend(project.wallets);
    global
}

fn apply_local_runtime_state(profile: &mut ProjectProfile, state: Option<&LocalRuntimeState>) {
    if profile.network != Some(Network::Local) {
        return;
    }
    let Some(state) = state else {
        return;
    };
    if profile.rpc_url.is_none() {
        profile.rpc_url = Some(state.rpc_url.clone());
    }
    if profile.runtime_manifest.is_none() {
        profile.runtime_manifest = Some(state.runtime_manifest.clone());
    }
    if profile.runtime_artifacts_dir.is_none() {
        profile
            .runtime_artifacts_dir
            .clone_from(&state.runtime_artifacts_dir);
    }
}

fn normalize_runtime_manifest(profile: &mut ProjectProfile, config_path: &Path) {
    let Some(path) = profile.runtime_manifest.as_ref() else {
        return;
    };
    if path.is_absolute() {
        return;
    }
    if let Some(parent) = config_path.parent() {
        profile.runtime_manifest = Some(parent.join(path));
    }
}

fn normalize_runtime_artifacts_dir(profile: &mut ProjectProfile, config_path: &Path) {
    let Some(path) = profile.runtime_artifacts_dir.as_ref() else {
        return;
    };
    if path.is_absolute() {
        return;
    }
    if let Some(parent) = config_path.parent() {
        profile.runtime_artifacts_dir = Some(parent.join(path));
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn profile_write_path(start: &Path) -> Result<PathBuf, CliError> {
    Ok(find_project_config(start).unwrap_or(config_file_path()?))
}

fn write_profile_config(path: &Path, data: &[u8]) -> Result<(), CliError> {
    if path == config_file_path()? {
        atomic_write_private(path, data)
    } else {
        atomic_write(path, data)
    }
}

/// Resolve a profile and apply explicit CLI target overrides.
pub fn resolve_profile_with_overrides(
    start: &Path,
    explicit: Option<&str>,
    overrides: &ProfileOverrides,
) -> Result<ResolvedProfile, CliError> {
    let mut profile = resolve_profile(start, explicit)?;
    apply_profile_overrides(start, &mut profile, overrides)?;
    Ok(profile)
}

/// Apply explicit CLI target overrides to an already resolved profile.
pub fn apply_profile_overrides(
    start: &Path,
    profile: &mut ResolvedProfile,
    overrides: &ProfileOverrides,
) -> Result<(), CliError> {
    if let Some(rpc_url) = overrides.rpc_url.as_ref() {
        let rpc_url = rpc_url.trim();
        if rpc_url.is_empty() {
            return Err(CliError::Other("--rpc-url cannot be empty".into()));
        }
        if !rpc_url.starts_with("http://") && !rpc_url.starts_with("https://") {
            return Err(CliError::Other(
                "--rpc-url must start with http:// or https://".into(),
            ));
        }
        profile.rpc_url = Some(rpc_url.to_owned());
        profile.rpc_url_cli_override = true;
    }

    if let Some(path) = overrides.runtime_manifest.as_ref() {
        profile.runtime_manifest = Some(if path.is_absolute() {
            path.clone()
        } else {
            start.join(path)
        });
        profile.runtime_manifest_cli_override = true;
    }

    Ok(())
}

fn is_valid_address(addr: &str) -> bool {
    let Some(hex) = addr.strip_prefix("0x") else {
        return false;
    };
    hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Write `data` to `path` atomically by writing to a
/// temporary file in the same directory, then renaming.
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, data)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

fn atomic_write_private(path: &Path, data: &[u8]) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    let result = (|| -> Result<(), CliError> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);

        let mut file = options.open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
