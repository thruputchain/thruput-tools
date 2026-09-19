use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::atomic::{
    ConfigFileKind, insert_preserving_decor, transact_document, transact_document_with_peer,
};
use super::dirs::config_file_path;
use super::global::{LocalRuntimeState, load_default_key};
use super::read::{
    PersistedProfileState, load_local_runtime_state, load_persisted_config,
    resolve_persisted_profile,
};
use crate::CliError;

/// Project profile loaded from `groundhog.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectProfile {
    #[serde(default)]
    pub network: Option<Network>,
    #[serde(default)]
    pub rpc_url: Option<String>,
    /// Network the profile's endpoint serves, as 64 lowercase hex digits.
    ///
    /// Bound into every transaction signature, so a profile carrying the wrong
    /// one produces transactions the network rejects. One endpoint serves one
    /// network, so this is configured beside `rpc_url` rather than discovered.
    #[serde(default)]
    pub network_hash: Option<String>,
    #[serde(default)]
    pub faucet_url: Option<String>,
    #[serde(default)]
    pub default_key: Option<String>,
    #[serde(default)]
    pub default_wallet: Option<String>,
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
    #[serde(default)]
    pub owner_public_key: String,
}

/// Active project profile resolved from `groundhog.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub name: String,
    pub network: Option<Network>,
    pub rpc_url: Option<String>,
    /// Network the profile's endpoint serves, as 64 lowercase hex digits.
    pub network_hash: Option<String>,
    pub faucet_url: Option<String>,
    #[serde(skip)]
    #[schemars(skip)]
    pub rpc_url_cli_override: bool,
    pub default_key: Option<String>,
    #[serde(default)]
    pub default_wallet: Option<String>,
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

/// Returns the selected existing profile from explicit, environment, project,
/// or global selection.
pub fn active_profile_name(start: &Path, explicit: Option<&str>) -> Result<String, CliError> {
    let (profile, selection) = resolve_persisted_profile(start, explicit)?;
    debug_assert_eq!(profile.name, selection.profile);
    Ok(selection.profile)
}

fn profile_name_for_write(start: &Path, explicit: Option<&str>) -> Result<String, CliError> {
    if explicit.is_none() {
        return active_profile_name(start, None);
    }
    load_persisted_config(start)?
        .selection(explicit)?
        .map(|selection| selection.profile)
        .ok_or(CliError::NoActiveProfile)
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
    Ok(load_persisted_config(start)?.project.map(|layer| {
        layer
            .profiles
            .into_iter()
            .filter_map(|(name, state)| match state {
                PersistedProfileState::Valid(profile) => Some((name, profile)),
                PersistedProfileState::Invalid { .. } => None,
            })
            .collect()
    }))
}

/// Persist a contract alias under the selected project profile.
///
/// Creates `groundhog.toml` if it does not already exist.
/// Uses `toml_edit` for in-place edits that preserve user
/// comments and formatting.
/// Validate a `--alias` value while parsing arguments.
///
/// [`save_profile_contract_alias`] rejects an empty alias, but it only runs
/// after a deploy has been submitted: by then the contract is on chain and
/// only the local bookkeeping can fail. Rejecting it in the argument parser
/// fails before the transaction is spent. The value is trimmed so that what
/// the CLI reports matches the key actually written.
pub fn parse_contract_alias(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("contract alias cannot be empty".to_string());
    }
    Ok(trimmed.to_string())
}

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
            "contract address must be a C StrKey or 32-byte hex address".into(),
        ));
    }

    let profile_name = profile_name_for_write(project_root, explicit_profile)?;
    let path = profile_write_path(project_root)?;

    transact_profile_document(&path, |document| {
        let contracts = document
            .entry("profile")
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
            .ok_or_else(|| CliError::Other("contracts must be a table".into()))?;
        insert_preserving_decor(contracts, alias.trim(), toml_edit::value(address.trim()));
        Ok(())
    })?;
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
    save_profile_wallet_address_with_default(
        project_root,
        explicit_profile,
        name,
        address,
        owner_public_key,
        false,
    )
}

/// Persist a wallet binding and optionally select it as the profile default in
/// one atomic config update.
pub fn save_profile_wallet_address_with_default(
    project_root: &Path,
    explicit_profile: Option<&str>,
    name: &str,
    address: &str,
    owner_public_key: &str,
    make_default: bool,
) -> Result<PathBuf, CliError> {
    if name.trim().is_empty() {
        return Err(CliError::EmptyWalletName);
    }
    if address.trim().is_empty() {
        return Err(CliError::Other("wallet address cannot be empty".into()));
    }
    if !is_valid_address(address.trim()) {
        return Err(CliError::Other(
            "wallet address must be a C StrKey or 32-byte hex address".into(),
        ));
    }
    if groundhog_signing::strkey::parse_public_key(owner_public_key.trim()).is_err() {
        return Err(CliError::Other(
            "wallet owner public key must be a G StrKey or 32-byte hex public key".into(),
        ));
    }

    let profile_name = profile_name_for_write(project_root, explicit_profile)?;
    let path = profile_write_path(project_root)?;

    transact_profile_document(&path, |document| {
        let profile = document
            .entry("profile")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| CliError::Other("profile must be a table".into()))?
            .entry(&profile_name)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| CliError::Other("profile entry must be a table".into()))?;

        if make_default {
            insert_preserving_decor(profile, "default_wallet", toml_edit::value(name.trim()));
        }

        let wallet = profile
            .entry("wallets")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| CliError::Other("wallets must be a table".into()))?
            .entry(name.trim())
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| CliError::Other("wallet entry must be a table".into()))?;

        insert_preserving_decor(wallet, "address", toml_edit::value(address.trim()));
        insert_preserving_decor(
            wallet,
            "owner_public_key",
            toml_edit::value(owner_public_key.trim()),
        );
        Ok(())
    })?;
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

    let profile_name = profile_name_for_write(project_root, explicit_profile)?;
    let path = profile_write_path(project_root)?;
    transact_profile_document(&path, |document| {
        let profile = document
            .get_mut("profile")
            .and_then(toml_edit::Item::as_table_mut)
            .and_then(|profiles| profiles.get_mut(&profile_name))
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| CliError::Other(format!("wallet `{name}` is not saved")))?;
        let wallets = profile
            .get_mut("wallets")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| CliError::Other(format!("wallet `{name}` is not saved")))?;

        if wallets.remove(name).is_none() {
            return Err(CliError::Other(format!("wallet `{name}` is not saved")));
        }
        if profile
            .get("default_wallet")
            .and_then(toml_edit::Item::as_str)
            == Some(name)
        {
            profile.remove("default_wallet");
        }
        Ok(())
    })?;
    Ok(path)
}

/// A wallet removal bound to the config snapshot shown before confirmation.
#[derive(Debug)]
pub(crate) struct PreparedWalletRemoval {
    path: PathBuf,
    profile_name: String,
    wallet_name: String,
    expected_document: String,
    peer: Option<WalletRemovalPeer>,
    profile: ProjectProfile,
    inherited_profile: Option<ProjectProfile>,
}

#[derive(Debug)]
struct WalletRemovalPeer {
    path: PathBuf,
    expected_document: String,
}

impl PreparedWalletRemoval {
    pub(crate) fn profile_name(&self) -> &str {
        &self.profile_name
    }

    pub(crate) fn profile(&self) -> &ProjectProfile {
        &self.profile
    }

    pub(crate) fn inherited_profile(&self) -> Option<&ProjectProfile> {
        self.inherited_profile.as_ref()
    }

    pub(crate) fn writes_project_profile(&self) -> bool {
        self.peer.is_some()
    }
}

pub(crate) fn prepare_profile_wallet_removal(
    project_root: &Path,
    explicit_profile: Option<&str>,
    name: &str,
) -> Result<PreparedWalletRemoval, CliError> {
    let wallet_name = name.trim();
    if wallet_name.is_empty() {
        return Err(CliError::EmptyWalletName);
    }
    let profile_name = profile_name_for_write(project_root, explicit_profile)?;
    let path = profile_write_path(project_root)?;
    let document = super::atomic::load_document(&path)?;
    let profile = profile_from_document(&document, &profile_name)?.ok_or_else(|| {
        CliError::SavedWalletNotFound {
            wallet_name: wallet_name.to_owned(),
            profile: profile_name.clone(),
        }
    })?;
    if !profile.wallets.contains_key(wallet_name) {
        return Err(CliError::SavedWalletNotFound {
            wallet_name: wallet_name.to_owned(),
            profile: profile_name,
        });
    }
    let (peer, inherited_profile) = if path == config_file_path()? {
        (None, None)
    } else {
        let peer_path = config_file_path()?;
        let peer_document = super::atomic::load_document(&peer_path)?;
        let inherited_profile = profile_from_document(&peer_document, &profile_name)?;
        (
            Some(WalletRemovalPeer {
                path: peer_path,
                expected_document: peer_document.to_string(),
            }),
            inherited_profile,
        )
    };
    Ok(PreparedWalletRemoval {
        path,
        profile_name,
        wallet_name: wallet_name.to_owned(),
        expected_document: document.to_string(),
        peer,
        profile,
        inherited_profile,
    })
}

pub(crate) fn remove_prepared_profile_wallet(
    prepared: PreparedWalletRemoval,
) -> Result<PathBuf, CliError> {
    let PreparedWalletRemoval {
        path,
        profile_name,
        wallet_name,
        expected_document,
        peer,
        ..
    } = prepared;
    let remove = |document: &mut toml_edit::DocumentMut| {
        if document.to_string() != expected_document {
            return Err(CliError::ConfigChangedDuringConfirmation { path: path.clone() });
        }
        let profile = document
            .get_mut("profile")
            .and_then(toml_edit::Item::as_table_mut)
            .and_then(|profiles| profiles.get_mut(&profile_name))
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| CliError::SavedWalletNotFound {
                wallet_name: wallet_name.clone(),
                profile: profile_name.clone(),
            })?;
        let wallets = profile
            .get_mut("wallets")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| CliError::SavedWalletNotFound {
                wallet_name: wallet_name.clone(),
                profile: profile_name.clone(),
            })?;
        if wallets.remove(&wallet_name).is_none() {
            return Err(CliError::SavedWalletNotFound {
                wallet_name: wallet_name.clone(),
                profile: profile_name.clone(),
            });
        }
        if profile
            .get("default_wallet")
            .and_then(toml_edit::Item::as_str)
            == Some(wallet_name.as_str())
        {
            profile.remove("default_wallet");
        }
        Ok(())
    };
    if let Some(peer) = peer {
        transact_document_with_peer(
            &path,
            ConfigFileKind::Project,
            &peer.path,
            |document, peer_document| {
                if peer_document.to_string() != peer.expected_document {
                    return Err(CliError::ConfigChangedDuringConfirmation {
                        path: peer.path.clone(),
                    });
                }
                remove(document)
            },
            |_, _| Ok(()),
        )?;
    } else {
        transact_profile_document(&path, remove)?;
    }
    Ok(path)
}

fn profile_from_document(
    document: &toml_edit::DocumentMut,
    profile_name: &str,
) -> Result<Option<ProjectProfile>, CliError> {
    let root: toml::Table = toml::from_str(&document.to_string())
        .map_err(|error| CliError::Other(error.to_string()))?;
    root.get("profile")
        .and_then(toml::Value::as_table)
        .and_then(|profiles| profiles.get(profile_name))
        .cloned()
        .map(toml::Value::try_into)
        .transpose()
        .map_err(|error| CliError::ProfileConfigInvalid {
            profile: profile_name.to_owned(),
            message: error.to_string(),
        })
}

/// Persist the default wallet under the selected profile.
///
/// Creates the config file and profile when needed while preserving existing
/// comments and formatting.
pub fn save_profile_default_wallet(
    project_root: &Path,
    explicit_profile: Option<&str>,
    wallet: &str,
) -> Result<PathBuf, CliError> {
    let wallet = wallet.trim();
    if wallet.is_empty() {
        return Err(CliError::Other("default wallet cannot be empty".into()));
    }

    let profile_name = profile_name_for_write(project_root, explicit_profile)?;
    let path = profile_write_path(project_root)?;
    transact_profile_document(&path, |document| {
        let profile = document
            .entry("profile")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| CliError::Other("profile must be a table".into()))?
            .entry(&profile_name)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| CliError::Other("profile entry must be a table".into()))?;
        insert_preserving_decor(profile, "default_wallet", toml_edit::value(wallet));
        Ok(())
    })?;
    Ok(path)
}

/// Resolve the effective default key for the active profile.
///
/// Prefers `groundhog.toml` when the profile specifies
/// `default_key`, otherwise falls back to the global default
/// selected via `keys use`. Profiles targeting the active runtime
/// started by `groundhog node` use the lowest-index local-user signer when
/// neither selection is present.
pub fn resolve_default_key(
    start: &Path,
    explicit_profile: Option<&str>,
) -> Result<Option<String>, CliError> {
    let profile = resolve_profile(start, explicit_profile)?;
    resolve_default_key_for_profile(&profile)
}

/// Resolve the effective default key from an already resolved profile.
///
/// The implicit local-user signer is available only when the profile still
/// targets the active local runtime recorded by `groundhog node`. Explicit
/// project or CLI target overrides must select a key themselves.
pub fn resolve_default_key_for_profile(
    profile: &ResolvedProfile,
) -> Result<Option<String>, CliError> {
    if let Some(key) = &profile.default_key {
        return Ok(Some(key.clone()));
    }

    if let Some(key) = load_default_key()? {
        return Ok(Some(key));
    }

    if profile.network != Some(Network::Local) {
        return Ok(None);
    }

    let local_runtime_state = load_local_runtime_state()?;
    let uses_active_local_runtime = local_runtime_state.is_some_and(|state| {
        profile.rpc_url.as_deref() == Some(state.rpc_url.as_str())
            && profile.runtime_manifest.as_deref() == Some(state.runtime_manifest.as_path())
    });

    if !uses_active_local_runtime {
        return Ok(None);
    }

    let Some(manifest) =
        crate::runtime_manifest::load_configured_runtime_manifest(profile, "local_user")?
    else {
        return Ok(None);
    };
    Ok(crate::runtime_manifest::lowest_local_user_key(&manifest))
}

/// Resolve the active profile and apply transient local-node state.
pub fn resolve_profile(start: &Path, explicit: Option<&str>) -> Result<ResolvedProfile, CliError> {
    let (persisted, _) = resolve_persisted_profile(start, explicit)?;
    let source = if persisted.project {
        find_project_config(start)
    } else if persisted.global {
        Some(config_file_path()?)
    } else {
        None
    };
    let name = persisted.name;
    let mut profile = persisted.values;
    if profile.network == Some(Network::Local) {
        apply_local_runtime_state(&mut profile, load_local_runtime_state()?.as_ref());
    }

    Ok(ResolvedProfile {
        name,
        network: profile.network,
        rpc_url: profile.rpc_url,
        network_hash: profile.network_hash,
        faucet_url: profile.faucet_url,
        rpc_url_cli_override: false,
        default_key: profile.default_key,
        default_wallet: profile.default_wallet,
        runtime_manifest: profile.runtime_manifest,
        runtime_manifest_cli_override: false,
        runtime_artifacts_dir: profile.runtime_artifacts_dir,
        contracts: profile.contracts,
        wallets: profile.wallets,
        source,
    })
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

fn profile_write_path(start: &Path) -> Result<PathBuf, CliError> {
    Ok(find_project_config(start).unwrap_or(config_file_path()?))
}

fn transact_profile_document(
    path: &Path,
    mutate: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), CliError>,
) -> Result<bool, CliError> {
    let kind = if path == config_file_path()? {
        ConfigFileKind::Global
    } else {
        ConfigFileKind::Project
    };
    transact_document(path, kind, mutate, |_| Ok(()))
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
    groundhog_signing::strkey::parse_contract_address(addr).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{
        PreparedWalletRemoval, ProjectProfile, WalletRemovalPeer, remove_prepared_profile_wallet,
    };

    #[test]
    fn confirmed_wallet_removal_rejects_changed_config() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("groundhog.toml");
        let original = "[profile.local.wallets.alice]\naddress = '0x00'\n";
        std::fs::write(&path, original).unwrap();
        let prepared = PreparedWalletRemoval {
            path: path.clone(),
            profile_name: "local".into(),
            wallet_name: "alice".into(),
            expected_document: original
                .parse::<toml_edit::DocumentMut>()
                .unwrap()
                .to_string(),
            peer: None,
            profile: ProjectProfile::default(),
            inherited_profile: None,
        };
        let changed = "[profile.local.wallets.alice]\naddress = '0x01'\n";
        std::fs::write(&path, changed).unwrap();

        let error = remove_prepared_profile_wallet(prepared).unwrap_err();

        assert_eq!(error.code(), "config_changed_during_confirmation");
        assert_eq!(std::fs::read_to_string(path).unwrap(), changed);
    }

    #[test]
    fn confirmed_wallet_removal_rejects_changed_inherited_config() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("groundhog.toml");
        let peer_path = temp.path().join("config.toml");
        let original = "[profile.local.wallets.alice]\naddress = '0x00'\n";
        let inherited = "[profile.local]\ndefault_wallet = 'global'\n";
        std::fs::write(&path, original).unwrap();
        std::fs::write(&peer_path, inherited).unwrap();
        let prepared = PreparedWalletRemoval {
            path: path.clone(),
            profile_name: "local".into(),
            wallet_name: "alice".into(),
            expected_document: original
                .parse::<toml_edit::DocumentMut>()
                .unwrap()
                .to_string(),
            peer: Some(WalletRemovalPeer {
                path: peer_path.clone(),
                expected_document: inherited
                    .parse::<toml_edit::DocumentMut>()
                    .unwrap()
                    .to_string(),
            }),
            profile: ProjectProfile::default(),
            inherited_profile: Some(ProjectProfile::default()),
        };
        let changed = "[profile.local]\ndefault_wallet = 'other'\n";
        std::fs::write(&peer_path, changed).unwrap();

        let error = remove_prepared_profile_wallet(prepared).unwrap_err();

        assert_eq!(error.code(), "config_changed_during_confirmation");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(std::fs::read_to_string(peer_path).unwrap(), changed);
    }
}
