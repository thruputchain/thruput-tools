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

/// Project profile loaded from `thruput.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectProfile {
    #[serde(default)]
    pub network: Option<Network>,
    #[serde(default)]
    pub rpc_url: Option<String>,
    /// Optional signing-network pin, as base58 encoding of exactly 32 bytes.
    ///
    /// Bound into every transaction signature, so a profile carrying the wrong
    /// one produces transactions the network rejects. Without a pin, hosted
    /// profiles discover the identity from matching RPC and faucet metadata.
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
    /// Genesis parameter overrides for local nodes started under this profile.
    ///
    /// Values are integers or strings in `--network-config` notation
    /// (`"256K"`, `"0x8000"`); a flag for the same parameter replaces the value.
    #[serde(default, with = "network_config_toml")]
    pub network_config: BTreeMap<thruput_node_supervisor::ConfigParamName, u64>,
}

// Keys are protocol parameter names, so a typo fails when the profile loads
// rather than after hog-devnode starts.
mod network_config_toml {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
    use thruput_node_supervisor::ConfigParamName;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Integer(i64),
        Text(String),
    }

    pub(super) fn serialize<S: Serializer>(
        overrides: &BTreeMap<ConfigParamName, u64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // TOML integers stop at i64::MAX; text preserves the full override range.
        serializer.collect_map(
            overrides
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_string())),
        )
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<ConfigParamName, u64>, D::Error> {
        BTreeMap::<String, Value>::deserialize(deserializer)?
            .into_iter()
            .map(|(name, value)| {
                let value = match value {
                    Value::Integer(value) => value.to_string(),
                    Value::Text(value) => value,
                };
                crate::node::parse_network_config_override(&format!("{name}={value}"))
                    .map_err(|error| D::Error::custom(format!("network_config.{name}: {error}")))
            })
            .collect()
    }
}

/// Genesis overrides the selected profile sets, empty when no profile is selected.
pub fn profile_network_config(
    start: &Path,
    explicit: Option<&str>,
) -> Result<Vec<(thruput_node_supervisor::ConfigParamName, u64)>, CliError> {
    match resolve_persisted_profile(start, explicit) {
        Ok((profile, _)) => Ok(profile.values.network_config.into_iter().collect()),
        Err(CliError::NoActiveProfile) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Local,
    Futurenet,
    /// Compatibility name for the hosted network. New profiles use Futurenet.
    Devnet,
    Mainnet,
}

impl Network {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Futurenet => "futurenet",
            Self::Devnet => "devnet",
            Self::Mainnet => "mainnet",
        }
    }
}

/// Transaction format used by a saved wallet. Untyped historical bindings are legacy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WalletKind {
    /// Deployed developer wallet with the historical ABI.
    #[default]
    DevWallet,
    /// Pubkey account using the native `DefaultWallet`.
    Native,
}
impl WalletKind {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde skip predicate takes a reference"
    )]
    pub(crate) fn is_legacy(&self) -> bool {
        *self == Self::DevWallet
    }
}

/// Saved wallet binding for a local signing key.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SavedWalletBinding {
    /// Wallet contract address invoked by transactions for this key.
    pub address: String,
    /// Public key that owns the wallet contract.
    #[serde(default)]
    pub owner_public_key: String,
    /// Explicit account format; absent on historical developer wallets.
    #[serde(default, skip_serializing_if = "WalletKind::is_legacy")]
    pub kind: WalletKind,
    /// Native account derivation nonce, required for native bindings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_nonce: Option<u64>,
}

/// Active project profile resolved from `thruput.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedProfile {
    pub name: String,
    pub network: Option<Network>,
    pub rpc_url: Option<String>,
    /// Network the profile's endpoint serves, as base58 encoding of exactly 32 bytes.
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
    #[serde(with = "wallet_json")]
    #[schemars(with = "BTreeMap<String, wallet_json::Wallet>")]
    pub wallets: BTreeMap<String, SavedWalletBinding>,
    pub source: Option<PathBuf>,
}

// Resolved profiles appear in command reports. Persisted TOML bindings retain
// their numeric nonce; only the report projection uses decimal JSON strings.
mod wallet_json {
    use super::{SavedWalletBinding, WalletKind};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(remote = "SavedWalletBinding", rename_all = "camelCase")]
    #[schemars(rename = "JsonWalletBinding")]
    pub(super) struct Wallet {
        address: String,
        #[serde(default)]
        owner_public_key: String,
        #[serde(default, skip_serializing_if = "WalletKind::is_legacy")]
        kind: WalletKind,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "thruput_json_rpc_types::decimal::option"
        )]
        #[schemars(with = "Option<thruput_json_rpc_types::decimal::Decimal<u64>>")]
        account_nonce: Option<u64>,
    }

    struct WalletRef<'a>(&'a SavedWalletBinding);

    impl Serialize for WalletRef<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            Wallet::serialize(self.0, serializer)
        }
    }

    pub(super) fn serialize<S: serde::Serializer>(
        wallets: &BTreeMap<String, SavedWalletBinding>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_map(
            wallets
                .iter()
                .map(|(name, wallet)| (name, WalletRef(wallet))),
        )
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<String, SavedWalletBinding>, D::Error> {
        #[derive(Deserialize)]
        struct Owned(#[serde(with = "Wallet")] SavedWalletBinding);

        BTreeMap::<String, Owned>::deserialize(deserializer).map(|wallets| {
            wallets
                .into_iter()
                .map(|(name, wallet)| (name, wallet.0))
                .collect()
        })
    }
}

/// Explicit CLI profile overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileOverrides {
    pub rpc_url: Option<String>,
    pub runtime_manifest: Option<PathBuf>,
}

impl ResolvedProfile {
    /// Require an endpoint after applying persisted settings, local runtime state,
    /// and CLI overrides. An absent local endpoint is a node-startup problem,
    /// not a request to manually persist the dynamically published RPC URL.
    pub(crate) fn rpc_endpoint(&self) -> Result<&str, CliError> {
        self.rpc_url.as_deref().ok_or_else(|| {
            if self.network == Some(Network::Local) {
                CliError::LocalNodeUnavailable {
                    profile: self.name.clone(),
                }
            } else {
                CliError::MissingRpcUrl
            }
        })
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

/// Find `thruput.toml` by walking up from `start`.
pub fn find_project_config(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start
            .parent()
            .map_or_else(|| start.to_path_buf(), Path::to_path_buf)
    } else {
        start.to_path_buf()
    };

    loop {
        let candidate = dir.join("thruput.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Load `thruput.toml` if one exists.
pub fn load_project_profiles(
    start: &Path,
) -> Result<Option<BTreeMap<String, ProjectProfile>>, CliError> {
    Ok(load_persisted_config(start)?.project.map(|layer| {
        layer
            .profiles
            .into_iter()
            .filter_map(|(name, state)| match state {
                PersistedProfileState::Valid(profile) => Some((name, *profile)),
                PersistedProfileState::Invalid { .. } => None,
            })
            .collect()
    }))
}

/// Persist a contract alias under the selected project profile.
///
/// Creates `thruput.toml` if it does not already exist.
/// Uses `toml_edit` for in-place edits that preserve user
/// comments and formatting.
/// Validate a `--alias` value while parsing arguments.
///
/// [`save_profile_contract_alias`] rejects an empty alias, but it only runs
/// after a deploy has been submitted: by then the contract is on chain and
/// only the local bookkeeping can fail. Rejecting it in the argument parser
/// fails before the transaction is spent. The value is trimmed so that what
/// the CLI reports matches the key actually written.
pub fn parse_contract_alias(value: &str) -> Result<String, EmptyContractAlias> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(EmptyContractAlias);
    }
    Ok(trimmed.to_string())
}

/// A `--alias` value that is blank once trimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("contract alias cannot be empty")]
pub struct EmptyContractAlias;

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
    let address =
        thruput_signing::strkey::parse_contract_address(address.trim()).map_err(|_| {
            CliError::Other(
                "contract address must be a C StrKey or base58 encoding of exactly 32 bytes".into(),
            )
        })?;
    let address = crate::codec::format_contract_address(&address);

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
/// Creates `thruput.toml` if it does not already exist.
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
    let address =
        thruput_signing::strkey::parse_contract_address(address.trim()).map_err(|_| {
            CliError::Other(
                "wallet address must be a C StrKey or base58 encoding of exactly 32 bytes".into(),
            )
        })?;
    let address = crate::codec::format_contract_address(&address);
    let owner_public_key = thruput_signing::strkey::parse_public_key(owner_public_key.trim())
        .map_err(|_| {
            CliError::Other(
                "wallet owner public key must be a G StrKey or base58 encoding of exactly 32 bytes"
                    .into(),
            )
        })?;
    let owner_public_key = thruput_encoding::encode(&owner_public_key);

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

/// Save a native account without replacing an existing identity. Returns whether inserted.
pub(crate) fn save_native_wallet(
    start: &Path,
    profile_name: &str,
    name: &str,
    key_name: &str,
    binding: &SavedWalletBinding,
) -> Result<bool, CliError> {
    crate::account_resolution::validate_saved_wallet_binding(name, binding)?;
    let path = profile_write_path(start)?;
    let global_path = config_file_path()?;
    let mut inserted = false;
    let mut save = |document: &mut toml_edit::DocumentMut, global: &toml_edit::DocumentMut| {
        let local = profile_from_document(document, profile_name)?.unwrap_or_default();
        let inherited = if path == global_path {
            ProjectProfile::default()
        } else {
            profile_from_document(global, profile_name)?.unwrap_or_default()
        };
        if let Some(existing) = local
            .wallets
            .get(name)
            .or_else(|| inherited.wallets.get(name))
        {
            let validated =
                crate::account_resolution::validate_saved_wallet_binding(name, existing)?;
            if existing.kind == binding.kind
                && existing.account_nonce == binding.account_nonce
                && validated.address == crate::codec::parse_contract_address(&binding.address)?
                && validated.owner_public_key
                    == thruput_signing::strkey::parse_public_key(&binding.owner_public_key)
                        .map_err(|e| CliError::Other(e.to_string()))?
            {
                return Ok(());
            }
            return Err(CliError::WalletBindingConflict {
                wallet_name: name.into(),
            });
        }
        let effective_key = local
            .default_key
            .as_deref()
            .or(inherited.default_key.as_deref())
            .or_else(|| global.get("default_key").and_then(toml_edit::Item::as_str));
        // Preserve the effective merged identity when adding another wallet for its owner.
        let mut existing_wallets = inherited.wallets.clone();
        existing_wallets.extend(local.wallets.clone());
        let owner_matches: Vec<_> = existing_wallets
            .iter()
            .filter(|(_, wallet)| {
                thruput_signing::strkey::parse_public_key(&wallet.owner_public_key).ok()
                    == thruput_signing::strkey::parse_public_key(&binding.owner_public_key).ok()
            })
            .map(|(name, _)| name.as_str())
            .collect();
        let default_to_save = if local.default_wallet.is_none()
            && inherited.default_wallet.is_none()
            && effective_key == Some(key_name)
        {
            match owner_matches.as_slice() {
                [] => Some(name),
                [existing] => Some(*existing),
                _ => None,
            }
        } else {
            None
        };
        let profile = document
            .entry("profile")
            .or_insert(toml_edit::table())
            .as_table_mut()
            .ok_or_else(|| CliError::Other("profile must be a table".into()))?
            .entry(profile_name)
            .or_insert(toml_edit::table())
            .as_table_mut()
            .ok_or_else(|| CliError::Other("profile entry must be a table".into()))?;
        if let Some(default) = default_to_save {
            profile.insert("default_wallet", toml_edit::value(default));
        }
        let wallets = profile
            .entry("wallets")
            .or_insert(toml_edit::table())
            .as_table_mut()
            .ok_or_else(|| CliError::Other("wallets must be a table".into()))?;
        wallets.insert(name, toml_edit::Item::Table(native_binding_table(binding)?));
        inserted = true;
        Ok(())
    };
    if path == global_path {
        transact_profile_document(&path, |doc| {
            let global = doc.clone();
            save(doc, &global)
        })?;
    } else {
        transact_document_with_peer(
            &path,
            ConfigFileKind::Project,
            &global_path,
            save,
            |_, _| Ok(()),
        )?;
    }
    Ok(inserted)
}

fn native_binding_table(binding: &SavedWalletBinding) -> Result<toml_edit::Table, CliError> {
    let mut wallet = toml_edit::Table::new();
    wallet.insert("address", toml_edit::value(&binding.address));
    wallet.insert(
        "owner_public_key",
        toml_edit::value(&binding.owner_public_key),
    );
    wallet.insert("kind", toml_edit::value("native"));
    wallet.insert(
        "account_nonce",
        toml_edit::value(
            i64::try_from(binding.account_nonce.unwrap_or(0))
                .map_err(|_| CliError::Other("account nonce exceeds TOML integer range".into()))?,
        ),
    );
    Ok(wallet)
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
/// Prefers `thruput.toml` when the profile specifies
/// `default_key`, otherwise falls back to the global default
/// selected via `keys use`. No selection leaves the default key unset.
pub fn resolve_default_key(
    start: &Path,
    explicit_profile: Option<&str>,
) -> Result<Option<String>, CliError> {
    let profile = resolve_profile(start, explicit_profile)?;
    resolve_default_key_for_profile(&profile)
}

/// Resolve the effective default key from an already resolved profile.
///
/// Project or profile selection takes precedence over the stored global default.
pub fn resolve_default_key_for_profile(
    profile: &ResolvedProfile,
) -> Result<Option<String>, CliError> {
    match &profile.default_key {
        Some(key) => Ok(Some(key.clone())),
        None => load_default_key(),
    }
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
    thruput_signing::strkey::parse_contract_address(addr).is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn resolved_wallet_json_preserves_wide_nonces_without_changing_toml() {
        let wire = serde_json::json!({
            "name": "test", "contracts": {}, "wallets": {
                "native": {"address": "address", "ownerPublicKey": "owner",
                    "kind": "native", "accountNonce": "9007199254740993"}
            }
        });
        let profile: super::ResolvedProfile = serde_json::from_value(wire.clone()).unwrap();
        let wallet = &profile.wallets["native"];
        assert_eq!(wallet.account_nonce, Some(9_007_199_254_740_993));
        assert!(
            toml::to_string(wallet)
                .unwrap()
                .contains("account_nonce = 9007199254740993")
        );
        let output = serde_json::to_value(profile).unwrap();
        assert_eq!(output["wallets"], wire["wallets"]);

        let mut invalid = wire;
        invalid["wallets"]["native"]["accountNonce"] = serde_json::json!(42);
        assert!(serde_json::from_value::<super::ResolvedProfile>(invalid).is_err());
    }

    use super::{
        PreparedWalletRemoval, ProjectProfile, WalletRemovalPeer, remove_prepared_profile_wallet,
    };

    #[test]
    fn confirmed_wallet_removal_rejects_changed_config() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("thruput.toml");
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
        let path = temp.path().join("thruput.toml");
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
