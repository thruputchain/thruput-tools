use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::global::load_default_key;
use crate::CliError;

pub const DEFAULT_PROFILE: &str = "default";

/// Project profile loaded from `groundhog.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectProfile {
    #[serde(default)]
    pub rpc_url: Option<String>,
    #[serde(default)]
    pub default_key: Option<String>,
    #[serde(default)]
    pub runtime_manifest: Option<PathBuf>,
    #[serde(default)]
    pub contracts: BTreeMap<String, String>,
    #[serde(default)]
    pub wallets: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProjectConfigFile {
    #[serde(default)]
    profile: BTreeMap<String, ProjectProfile>,
}

/// Active project profile resolved from `groundhog.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub name: String,
    pub rpc_url: Option<String>,
    pub default_key: Option<String>,
    pub runtime_manifest: Option<PathBuf>,
    pub contracts: BTreeMap<String, String>,
    pub wallets: BTreeMap<String, String>,
    pub source: Option<PathBuf>,
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
        self.wallets.get(name_or_address).cloned()
    }
}

/// Returns the active profile name from an explicit selection
/// or `GROUNDHOG_PROFILE`.
pub fn active_profile_name(explicit: Option<&str>) -> String {
    explicit
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            std::env::var("GROUNDHOG_PROFILE")
                .ok()
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_PROFILE.to_string())
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
    let Some(path) = find_project_config(start) else {
        return Ok(None);
    };

    let contents = std::fs::read_to_string(path)?;
    let config: ProjectConfigFile =
        toml::from_str(&contents).map_err(|e| CliError::Other(e.to_string()))?;
    Ok(Some(config.profile))
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

    let profile_name = active_profile_name(explicit_profile);
    let path =
        find_project_config(project_root).unwrap_or_else(|| project_root.join("groundhog.toml"));

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

    atomic_write(&path, doc.to_string().as_bytes())?;
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
) -> Result<PathBuf, CliError> {
    if name.trim().is_empty() {
        return Err(CliError::Other("wallet name cannot be empty".into()));
    }
    if address.trim().is_empty() {
        return Err(CliError::Other("wallet address cannot be empty".into()));
    }
    if !is_valid_address(address.trim()) {
        return Err(CliError::Other(
            "wallet address must be a 0x-prefixed 64-character hex string".into(),
        ));
    }

    let profile_name = active_profile_name(explicit_profile);
    let path =
        find_project_config(project_root).unwrap_or_else(|| project_root.join("groundhog.toml"));

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
        .entry("wallets")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| CliError::Other("wallets must be a table".into()))?
        .insert(name.trim(), toml_edit::value(address.trim()));

    atomic_write(&path, doc.to_string().as_bytes())?;
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
    let name = active_profile_name(explicit);
    let source = find_project_config(start);

    let profile = match &source {
        Some(path) => {
            let contents = std::fs::read_to_string(path)?;
            let config: ProjectConfigFile =
                toml::from_str(&contents).map_err(|e| CliError::Other(e.to_string()))?;
            config
                .profile
                .get(&name)
                .cloned()
                .ok_or_else(|| CliError::ProfileNotFound(name.clone()))?
        }
        None if name == DEFAULT_PROFILE => ProjectProfile::default(),
        None => {
            return Err(CliError::NoProjectConfig(name));
        }
    };

    Ok(ResolvedProfile {
        name,
        rpc_url: profile.rpc_url,
        default_key: profile.default_key,
        runtime_manifest: profile.runtime_manifest,
        contracts: profile.contracts,
        wallets: profile.wallets,
        source,
    })
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
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, data)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}
