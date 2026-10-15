use thruput_encoding::encode;
use thruput_json_rpc_types::{NetworkHashText, runtime_manifest::BOOTSTRAP_ACCOUNT_ROLE_FAUCET};
use thruput_signing::LocalSigner;

use crate::{
    CliError, codec,
    config::{ResolvedProfile, SavedWalletBinding, resolve_default_key_for_profile},
    keys::load_local_signer,
    runtime_manifest::load_configured_runtime_manifest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalBootstrapRole {
    Faucet,
}

impl LocalBootstrapRole {
    fn manifest_role(self) -> &'static str {
        match self {
            Self::Faucet => BOOTSTRAP_ACCOUNT_ROLE_FAUCET,
        }
    }

    fn error_key_name(self) -> &'static str {
        self.manifest_role()
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedLocalBootstrapAccount {
    pub(crate) role: String,
    pub(crate) wallet_address: thruput_xdr::types::ContractAddress,
    /// Network this account signs for. Part of every signature preimage.
    pub(crate) network_hash: thruput_xdr::types::NetworkHash,
}

#[derive(Debug)]
pub(crate) struct ResolvedTransactionSender {
    pub(crate) native_account: Option<thruput_signing::PubkeyAccount>,
    pub(crate) key_name: String,
    pub(crate) wallet_name: Option<String>,
    pub(crate) signer: LocalSigner,
    pub(crate) wallet_address: thruput_xdr::types::ContractAddress,
    /// Optional network pin, validated against hosted RPC before signing.
    /// Hosted profiles may discover the identity from matching descriptor and RPC data.
    pub(crate) network_hash: Option<thruput_xdr::types::NetworkHash>,
}

impl ResolvedTransactionSender {
    pub(crate) fn wallet(
        &self,
        entrypoint: thruput_xdr::contract::ContractEntrypointID,
    ) -> thruput_client::Wallet<'_> {
        match &self.native_account {
            Some(account) => thruput_client::Wallet::Native(account),
            None => thruput_client::Wallet::Legacy {
                address: self.wallet_address,
                entrypoint,
            },
        }
    }
    pub(crate) fn signing_context(
        &self,
        profile: &ResolvedProfile,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<crate::network_descriptor::WalletSigningContext, CliError> {
        if self.native_account.is_some() {
            crate::network_descriptor::resolve_native_signing_context(
                profile,
                self.network_hash.as_ref(),
                runtime,
            )
        } else {
            crate::network_descriptor::resolve_wallet_signing_context(
                profile,
                &self.key_name,
                self.network_hash.clone(),
                runtime,
            )
        }
    }
}

pub(crate) struct ValidatedSavedWalletBinding {
    pub(crate) owner_public_key: Vec<u8>,
    pub(crate) address: thruput_xdr::types::ContractAddress,
}

pub(crate) fn resolve_transaction_sender(
    profile: &ResolvedProfile,
    explicit_wallet: Option<&str>,
) -> Result<ResolvedTransactionSender, CliError> {
    let sender = resolve_sender_account(profile, explicit_wallet)?;
    bind_sender_to_network(profile, sender)
}

/// Bind an already-resolved sender to the network it will sign for.
///
/// Split out so a command can reject bad input against the sender's own
/// address before resolving the network.
pub(crate) fn bind_sender_to_network(
    profile: &ResolvedProfile,
    sender: ResolvedSenderAccount,
) -> Result<ResolvedTransactionSender, CliError> {
    let network_hash = resolve_network_hash(profile, &sender.key_name)?;
    Ok(ResolvedTransactionSender {
        native_account: sender.native_account,
        key_name: sender.key_name,
        wallet_name: sender.wallet_name,
        signer: sender.signer,
        wallet_address: sender.wallet_address,
        network_hash,
    })
}

/// The key and wallet a profile would send from, without the network binding.
#[derive(Debug)]
pub(crate) struct ResolvedSenderAccount {
    pub(crate) native_account: Option<thruput_signing::PubkeyAccount>,
    pub(crate) key_name: String,
    pub(crate) wallet_name: Option<String>,
    pub(crate) signer: LocalSigner,
    pub(crate) wallet_address: thruput_xdr::types::ContractAddress,
}

/// Load the effective signer and validate its wallet.
/// Address-only callers should use [`resolve_address_target_or_default_wallet`].
///
/// Signing paths want [`resolve_transaction_sender`] instead, which also
/// resolves the network hash every signature is bound to.
pub(crate) fn resolve_sender_account(
    profile: &ResolvedProfile,
    explicit_wallet: Option<&str>,
) -> Result<ResolvedSenderAccount, CliError> {
    let key_name = resolve_default_key_for_profile(profile)?.ok_or(CliError::MissingSigningKey)?;
    let signer = load_local_signer(&key_name)?;
    let selected_wallet = explicit_wallet.or(profile.default_wallet.as_deref());
    let (wallet_name, wallet_address) =
        resolve_wallet_selection(profile, &key_name, &signer, selected_wallet)?;
    let native_account = match wallet_name.as_deref() {
        Some(name) => profile
            .wallets
            .get(name)
            .map(|wallet| native_account_for_binding(name, wallet))
            .transpose()?
            .flatten(),
        None => Some(default_native_account(&signer)),
    };
    Ok(ResolvedSenderAccount {
        native_account,
        key_name,
        wallet_name,
        signer,
        wallet_address,
    })
}

/// The network hash configured for signatures from this profile.
///
/// A runtime manifest is authoritative when configured, since it is written by
/// the node that owns the chain. Otherwise the profile's own `network_hash` is
/// the expected identity. Hosted profiles can omit this pin: signing discovers
/// the identity from the network descriptor and checks it against the live RPC.
pub(crate) fn resolve_network_hash(
    profile: &ResolvedProfile,
    key_name: &str,
) -> Result<Option<thruput_xdr::types::NetworkHash>, CliError> {
    if let Some(manifest) = load_configured_runtime_manifest(profile, key_name)? {
        return Ok(Some(manifest.network_hash.to_network_hash()));
    }
    let Some(configured) = profile.network_hash.as_deref() else {
        return if profile.faucet_url.is_some() {
            Ok(None)
        } else {
            Err(CliError::MissingNetworkHash)
        };
    };
    NetworkHashText::try_from(configured)
        .map(|hash| Some(hash.to_network_hash()))
        .map_err(|_| CliError::InvalidNetworkHash {
            value: configured.to_owned(),
        })
}

fn resolve_wallet_selection(
    profile: &ResolvedProfile,
    key_name: &str,
    signer: &LocalSigner,
    selected_wallet: Option<&str>,
) -> Result<(Option<String>, thruput_xdr::types::ContractAddress), CliError> {
    if let Some(wallet_name) = selected_wallet {
        return resolve_selected_wallet(
            profile,
            key_name,
            signer.verifying_key_bytes(),
            wallet_name,
        )
        .map(|address| (Some(wallet_name.to_owned()), address));
    }
    resolve_saved_wallet_for_public_key(profile, key_name, signer.verifying_key_bytes())
}

/// Validate a selected binding against the supplied key identity without loading a signer.
pub(crate) fn resolve_selected_wallet(
    profile: &ResolvedProfile,
    key_name: &str,
    public_key: [u8; 32],
    wallet_name: &str,
) -> Result<thruput_xdr::types::ContractAddress, CliError> {
    let wallet = profile
        .wallets
        .get(wallet_name)
        .ok_or_else(|| CliError::SavedWalletNotFound {
            wallet_name: wallet_name.to_owned(),
            profile: profile.name.clone(),
        })?;
    let validated = validate_saved_wallet_binding(wallet_name, wallet)?;
    if validated.owner_public_key.as_slice() != public_key.as_slice() {
        return Err(CliError::SavedWalletOwnerMismatch {
            wallet_name: wallet_name.to_owned(),
            key_name: key_name.to_owned(),
            configured: encode(&validated.owner_public_key),
            local: encode(&public_key),
        });
    }
    Ok(validated.address)
}

pub(crate) fn validate_saved_wallet_binding(
    wallet_name: &str,
    wallet: &SavedWalletBinding,
) -> Result<ValidatedSavedWalletBinding, CliError> {
    native_account_for_binding(wallet_name, wallet)?;
    Ok(ValidatedSavedWalletBinding {
        owner_public_key: parse_owner_public_key(wallet_name, &wallet.owner_public_key)?,
        address: codec::parse_contract_address(&wallet.address)?,
    })
}

fn native_account_for_binding(
    name: &str,
    wallet: &SavedWalletBinding,
) -> Result<Option<thruput_signing::PubkeyAccount>, CliError> {
    let invalid = |message: &str| CliError::InvalidWalletBinding {
        wallet_name: name.into(),
        message: message.into(),
    };
    match wallet.kind {
        crate::config::WalletKind::DevWallet => {
            if wallet.account_nonce.is_some() {
                return Err(invalid("account_nonce requires kind = native"));
            }
            Ok(None)
        }
        crate::config::WalletKind::Native => {
            let nonce = wallet
                .account_nonce
                .ok_or_else(|| invalid("native accounts require account_nonce"))?;
            let owner = thruput_signing::strkey::parse_public_key(&wallet.owner_public_key)
                .map_err(|_| invalid("native accounts require a valid owner_public_key"))?;
            let account = thruput_signing::PubkeyAccount::from_public_key_bytes(owner, nonce);
            if account.address() != codec::parse_contract_address(&wallet.address)? {
                return Err(invalid(
                    "address does not match the owner and account_nonce",
                ));
            }
            Ok(Some(account))
        }
    }
}

fn default_native_account(signer: &LocalSigner) -> thruput_signing::PubkeyAccount {
    thruput_signing::PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0)
}

fn resolve_saved_wallet_for_public_key(
    profile: &ResolvedProfile,
    key_name: &str,
    expected_public_key: [u8; 32],
) -> Result<(Option<String>, thruput_xdr::types::ContractAddress), CliError> {
    let mut matches = Vec::new();
    for (wallet_name, wallet) in &profile.wallets {
        match parse_owner_public_key(wallet_name, &wallet.owner_public_key) {
            Ok(owner_public_key)
                if owner_public_key.as_slice() == expected_public_key.as_slice() =>
            {
                matches.push((wallet_name, wallet));
            }
            // Neither another owner nor invalid metadata establishes a match.
            Ok(_) | Err(_) => {}
        }
    }
    let [(_, wallet)] = matches.as_slice() else {
        if matches.is_empty() {
            return Ok((
                None,
                thruput_signing::PubkeyAccount::from_public_key_bytes(expected_public_key, 0)
                    .address(),
            ));
        }
        return Err(CliError::SavedWalletAmbiguous {
            key_name: key_name.to_string(),
            profile: profile.name.clone(),
            wallet_names: matches.iter().map(|(name, _)| (*name).clone()).collect(),
        });
    };

    Ok((
        Some(matches[0].0.clone()),
        validate_saved_wallet_binding(matches[0].0, wallet)?.address,
    ))
}

fn parse_owner_public_key(key_name: &str, value: &str) -> Result<Vec<u8>, CliError> {
    thruput_signing::strkey::parse_public_key(value)
        .map(|bytes| bytes.to_vec())
        .map_err(|error| saved_wallet_owner_public_key_invalid(key_name, error.to_string()))
}

fn saved_wallet_owner_public_key_invalid(key_name: &str, message: impl Into<String>) -> CliError {
    CliError::SavedWalletOwnerPublicKeyInvalid {
        key_name: key_name.to_string(),
        message: message.into(),
    }
}

/// Recognize base58 addresses and explicit typed/legacy prefixes for useful errors.
pub(crate) fn looks_like_address_literal(value: &str) -> bool {
    value.starts_with("0x")
        || value.starts_with("0X")
        || value.starts_with('C')
        || codec::parse_contract_address(value).is_ok()
}

/// Resolve a saved wallet, contract alias, direct address, or named key account.
pub(crate) fn resolve_address_target(
    profile: &ResolvedProfile,
    target: &str,
) -> Result<thruput_xdr::types::ContractAddress, CliError> {
    resolve_address_target_with_keys(profile, target, true)
}

fn resolve_address_target_with_keys(
    profile: &ResolvedProfile,
    target: &str,
    allow_key_names: bool,
) -> Result<thruput_xdr::types::ContractAddress, CliError> {
    if let Some(wallet) = profile.wallets.get(target) {
        native_account_for_binding(target, wallet)?;
        return parse_target_address(target, &wallet.address);
    }
    if let Some(address) = profile.contracts.get(target) {
        return parse_target_address(target, address);
    }
    let literal_error = if looks_like_address_literal(target)
        || (target.len() == 64 && target.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        match parse_target_address(target, target) {
            Ok(address) => return Ok(address),
            Err(error) => Some(error),
        }
    } else {
        None
    };
    if allow_key_names && let Some(public_key) = crate::keys::load_local_public_key(target)? {
        return resolve_saved_wallet_for_public_key(profile, target, public_key)
            .map(|(_, address)| address);
    }
    if let Some(error) = literal_error {
        return Err(error);
    }
    Err(CliError::UnknownAddressTarget {
        target: target.to_owned(),
    })
}

/// Resolve an explicit address target or the profile's effective wallet using public metadata.
pub(crate) fn resolve_address_target_or_default_wallet(
    profile: &ResolvedProfile,
    target: Option<&str>,
) -> Result<(String, thruput_xdr::types::ContractAddress), CliError> {
    if let Some(target) = target {
        return resolve_address_target(profile, target).map(|address| (target.to_owned(), address));
    }
    if let Some(wallet_name) = profile.default_wallet.as_deref() {
        // A configured wallet must retain its saved/alias/dev identity; a missing binding
        // must not silently become the same-named key's default native account.
        return resolve_address_target_with_keys(profile, wallet_name, false)
            .map(|address| (wallet_name.to_owned(), address));
    }

    let key_name = resolve_default_key_for_profile(profile)?.ok_or(CliError::MissingSigningKey)?;
    let public_key = crate::keys::load_local_public_key(&key_name)?
        .ok_or_else(|| CliError::KeyNotFound(key_name.clone()))?;
    let (wallet_name, address) =
        resolve_saved_wallet_for_public_key(profile, &key_name, public_key)?;
    Ok((wallet_name.unwrap_or(key_name), address))
}

/// Resolve a contract alias or a base58 / typed `StrKey` address.
pub(crate) fn resolve_contract_target(
    profile: &ResolvedProfile,
    target: &str,
) -> Result<thruput_xdr::types::ContractAddress, CliError> {
    if let Some(address) = profile.contracts.get(target) {
        return parse_target_address(target, address);
    }
    if looks_like_address_literal(target) {
        return parse_target_address(target, target);
    }
    Err(CliError::UnknownContractAlias {
        alias: target.to_owned(),
        profile: profile.name.clone(),
        config_source: profile.source.clone(),
        available_aliases: profile.contracts.keys().cloned().collect(),
    })
}

fn parse_target_address(
    target: &str,
    address: &str,
) -> Result<thruput_xdr::types::ContractAddress, CliError> {
    codec::parse_contract_address(address).map_err(|error| CliError::InvalidAddressTarget {
        target: target.to_owned(),
        message: error.to_string(),
    })
}

/// Resolve the local bootstrap account used for CLI-sponsored local flows.
///
pub(crate) fn resolve_local_bootstrap_account(
    profile: &ResolvedProfile,
    role: LocalBootstrapRole,
) -> Result<ResolvedLocalBootstrapAccount, CliError> {
    let key_name = role.error_key_name();
    let Some(manifest) = load_configured_runtime_manifest(profile, key_name)? else {
        return Err(local_runtime_error(
            key_name,
            "local bootstrap account resolution requires THRUPUT_RUNTIME_MANIFEST \
             or profile.<name>.runtime_manifest from `thruput node`",
        ));
    };
    if manifest.kind != "hogd-dev-runtime"
        || manifest.schema_version
            != thruput_json_rpc_types::runtime_manifest::RUNTIME_MANIFEST_SCHEMA_VERSION
    {
        return Err(local_runtime_error(
            key_name,
            "local funding requires a schema 5 local runtime; old state requires the previous runtime and tooling",
        ));
    }
    let mut accounts = manifest
        .bootstrap_accounts
        .iter()
        .filter(|account| account.role == role.manifest_role());
    let selected = accounts.next().ok_or_else(|| {
        local_runtime_error(
            key_name,
            "runtime manifest requires one account with role `faucet`",
        )
    })?;
    if accounts.next().is_some() {
        return Err(local_runtime_error(
            key_name,
            "runtime manifest contains multiple faucets",
        ));
    }
    let public_key: [u8; 32] = selected
        .public_key
        .as_ref()
        .try_into()
        .map_err(|_| local_runtime_error(key_name, "invalid faucet public key"))?;
    if thruput_signing::PubkeyAccount::from_public_key_bytes(public_key, 0).address()
        != selected.wallet_address.0
    {
        return Err(local_runtime_error(
            key_name,
            "faucet address does not match its nonce-zero native account",
        ));
    }
    Ok(ResolvedLocalBootstrapAccount {
        role: selected.role.clone(),
        wallet_address: selected.wallet_address.0,
        network_hash: manifest.network_hash.to_network_hash(),
    })
}

fn local_runtime_error(key_name: &str, message: impl Into<String>) -> CliError {
    CliError::LocalRuntimeUnavailable {
        key_name: key_name.to_string(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::config::SavedWalletBinding;

    use super::*;

    #[test]
    fn saved_strkey_wallet_selects_the_matching_signer_and_rejects_another() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let other = thruput_signing::derive_default_dev_account_signer(1);
        let address = thruput_xdr::types::ContractAddress::from([42; 32]);
        let binding = SavedWalletBinding {
            kind: crate::config::WalletKind::DevWallet,
            account_nonce: None,
            address: thruput_signing::strkey::format_contract_address(&address),
            owner_public_key: thruput_signing::strkey::format_public_key(
                &signer.verifying_key_bytes(),
            ),
        };
        let profile = test_profile("alice", None, BTreeMap::from([("wallet".into(), binding)]));
        assert_eq!(
            resolve_wallet_selection(&profile, "alice", &signer, Some("wallet"))
                .unwrap()
                .1,
            address
        );
        assert_eq!(
            resolve_wallet_selection(&profile, "alice", &signer, None)
                .unwrap()
                .1,
            address
        );
        assert!(resolve_wallet_selection(&profile, "bob", &other, Some("wallet")).is_err());
    }

    #[test]
    fn strkey_targets_and_saved_wallet_owner_are_distinct() {
        let profile = test_profile("alice", None, BTreeMap::new());
        let address = thruput_signing::strkey::format_contract_address(&[42; 32].into());
        let public = thruput_signing::strkey::format_public_key(&[42; 32]);
        assert_eq!(
            resolve_address_target(&profile, &address).unwrap(),
            [42; 32].into()
        );
        assert_eq!(
            resolve_contract_target(&profile, &address).unwrap(),
            [42; 32].into()
        );
        assert!(resolve_contract_target(&profile, &public).is_err());
        assert_eq!(
            parse_owner_public_key("alice", &public).unwrap(),
            vec![42; 32]
        );
        assert!(parse_owner_public_key("alice", &address).is_err());
    }

    #[test]
    fn address_target_prefers_saved_wallet_over_contract_alias() {
        let wallet_address = thruput_xdr::types::ContractAddress::from([0x11; 32]);
        let contract_address = thruput_xdr::types::ContractAddress::from([0x22; 32]);
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(wallet_address.as_ref()),
                owner_public_key: encode(&[0x33; 32]),
            },
        );
        let mut profile = test_profile("alice", None, wallets);
        profile
            .contracts
            .insert("alice".into(), encode(contract_address.as_ref()));

        let resolved = resolve_address_target(&profile, "alice")
            .expect("bug: saved wallet target should resolve");

        assert_eq!(resolved, wallet_address);
    }

    #[test]
    fn address_target_resolves_contract_alias() {
        let expected = thruput_xdr::types::ContractAddress::from([0x44; 32]);
        let mut profile = test_profile("alice", None, BTreeMap::new());
        profile
            .contracts
            .insert("counter".into(), encode(expected.as_ref()));

        let resolved = resolve_address_target(&profile, "counter")
            .expect("bug: contract alias should resolve");

        assert_eq!(resolved, expected);
    }

    #[test]
    fn address_target_accepts_base58_and_preserves_case() {
        let target = encode(&[0xab; 32]);

        let resolved =
            resolve_address_target(&test_profile("alice", None, BTreeMap::new()), &target)
                .expect("bug: base58 direct address should resolve");

        assert_eq!(codec::format_contract_address(&resolved), target);
    }

    #[test]
    fn address_target_rejects_malformed_prefixed_address() {
        let error =
            resolve_address_target(&test_profile("alice", None, BTreeMap::new()), "0xdeadbeef")
                .expect_err("bug: malformed direct address should fail");

        assert_eq!(error.code(), "invalid_address_target");
    }

    #[test]
    fn address_target_rejects_unknown_name() {
        let error =
            resolve_address_target(&test_profile("alice", None, BTreeMap::new()), "missing")
                .expect_err("bug: unknown target should fail");

        assert_eq!(error.code(), "unknown_address_target");
    }

    #[test]
    fn contract_target_reports_unknown_alias_with_profile_context() {
        let source = std::path::PathBuf::from("/tmp/project/thruput.toml");
        let mut profile = test_profile("alice", None, BTreeMap::new());
        profile.name = "local".into();
        profile.source = Some(source.clone());
        profile
            .contracts
            .insert("zebra".into(), encode(&[0x11; 32]));
        profile
            .contracts
            .insert("alpha".into(), encode(&[0x22; 32]));

        let error = resolve_contract_target(&profile, "counter")
            .expect_err("bug: unknown contract alias should fail");

        assert_eq!(error.code(), "unknown_contract_alias");
        assert_eq!(
            error.details_json().expect("details expected"),
            serde_json::json!({
                "alias": "counter",
                "profile": "local",
                "configSource": source,
                "availableAliases": ["alpha", "zebra"],
            })
        );
        assert!(
            error
                .hint()
                .expect("hint expected")
                .contains("thruput config get local --sources")
        );
    }

    #[test]
    fn contract_target_preserves_direct_address_validation() {
        let profile = test_profile("alice", None, BTreeMap::new());

        let error = resolve_contract_target(&profile, "0xdeadbeef")
            .expect_err("bug: malformed direct address should fail");

        assert_eq!(error.code(), "invalid_address_target");
        assert_eq!(
            error.details_json().expect("details expected")["target"],
            "0xdeadbeef"
        );
    }

    #[test]
    fn contract_target_does_not_resolve_saved_wallet_names() {
        let address = encode(&[0x33; 32]);
        let wallets = BTreeMap::from([(
            "counter".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address,
                owner_public_key: encode(&[0x44; 32]),
            },
        )]);
        let profile = test_profile("alice", None, wallets);

        let error = resolve_contract_target(&profile, "counter")
            .expect_err("bug: wallet names should not resolve as contract aliases");

        assert_eq!(error.code(), "unknown_contract_alias");
    }

    #[test]
    fn non_dev_key_uses_saved_wallet_address() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let wallet_address = thruput_xdr::types::ContractAddress::from([0x80; 32]);
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(wallet_address.as_ref()),
                owner_public_key: encode(&signer.verifying_key_bytes()),
            },
        );
        let profile = test_profile("alice", None, wallets);

        let resolved = resolve_wallet_selection(&profile, "alice", &signer, None)
            .expect("bug: saved wallet should resolve")
            .1;

        assert_eq!(resolved, wallet_address);
    }

    #[test]
    fn non_dev_key_uses_unique_owner_matched_wallet_with_different_name() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let wallet_address = thruput_xdr::types::ContractAddress::from([0x81; 32]);
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "dev-default".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(wallet_address.as_ref()),
                owner_public_key: encode(&signer.verifying_key_bytes()),
            },
        );
        let profile = test_profile("default", None, wallets);

        let resolved = resolve_wallet_selection(&profile, "default", &signer, None)
            .expect("bug: unique owner-matched wallet should resolve")
            .1;

        assert_eq!(resolved, wallet_address);
    }

    #[test]
    fn non_dev_key_uses_owner_match_despite_unrelated_malformed_wallet() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let wallet_address = thruput_xdr::types::ContractAddress::from([0x84; 32]);
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "broken".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(&[0x85; 32]),
                owner_public_key: "not-hex".into(),
            },
        );
        wallets.insert(
            "trading".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(wallet_address.as_ref()),
                owner_public_key: encode(&signer.verifying_key_bytes()),
            },
        );
        let profile = test_profile("alice", None, wallets);

        let resolved = resolve_wallet_selection(&profile, "alice", &signer, None)
            .expect("bug: unrelated malformed wallet should not hide owner match")
            .1;

        assert_eq!(resolved, wallet_address);
    }

    #[test]
    fn non_dev_key_rejects_multiple_owner_matched_wallets() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let owner_public_key = encode(&signer.verifying_key_bytes());
        let mut wallets = BTreeMap::new();
        for (name, byte) in [("primary", 0x82), ("trading", 0x83)] {
            wallets.insert(
                name.into(),
                SavedWalletBinding {
                    kind: crate::config::WalletKind::DevWallet,
                    account_nonce: None,
                    address: encode(&[byte; 32]),
                    owner_public_key: owner_public_key.clone(),
                },
            );
        }
        let profile = test_profile("default", None, wallets);

        let error = resolve_wallet_selection(&profile, "default", &signer, None)
            .expect_err("bug: multiple owner-matched wallets should be ambiguous");

        assert_eq!(error.code(), "saved_wallet_ambiguous");
        assert_eq!(
            error.details_json().expect("details expected")["walletNames"],
            serde_json::json!(["primary", "trading"])
        );
    }

    #[test]
    fn selected_wallet_disambiguates_owner_matched_wallets() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let owner_public_key = encode(&signer.verifying_key_bytes());
        let mut wallets = BTreeMap::new();
        for (name, byte) in [("primary", 0x82), ("trading", 0x83)] {
            wallets.insert(
                name.into(),
                SavedWalletBinding {
                    kind: crate::config::WalletKind::DevWallet,
                    account_nonce: None,
                    address: encode(&[byte; 32]),
                    owner_public_key: owner_public_key.clone(),
                },
            );
        }
        let profile = test_profile("default", None, wallets);

        let (wallet_name, address) =
            resolve_wallet_selection(&profile, "default", &signer, Some("trading"))
                .expect("bug: selected owner-matched wallet should resolve");

        assert_eq!(wallet_name.as_deref(), Some("trading"));
        assert_eq!(address.as_ref(), &[0x83; 32]);
    }

    #[test]
    fn profile_default_wallet_disambiguates_owner_matched_wallets() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let owner_public_key = encode(&signer.verifying_key_bytes());
        let mut wallets = BTreeMap::new();
        for (name, byte) in [("primary", 0x82), ("trading", 0x83)] {
            wallets.insert(
                name.into(),
                SavedWalletBinding {
                    kind: crate::config::WalletKind::DevWallet,
                    account_nonce: None,
                    address: encode(&[byte; 32]),
                    owner_public_key: owner_public_key.clone(),
                },
            );
        }
        let mut profile = test_profile("default", None, wallets);
        profile.default_wallet = Some("primary".into());

        let address = resolve_wallet_selection(
            &profile,
            "default",
            &signer,
            profile.default_wallet.as_deref(),
        )
        .expect("bug: profile default wallet should resolve ambiguity")
        .1;

        assert_eq!(address.as_ref(), &[0x82; 32]);
    }

    #[test]
    fn selected_wallet_must_belong_to_effective_key() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let other = thruput_signing::derive_default_dev_account_signer(1);
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "trading".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(&[0x83; 32]),
                owner_public_key: encode(&other.verifying_key_bytes()),
            },
        );
        let profile = test_profile("default", None, wallets);

        let error = resolve_wallet_selection(&profile, "default", &signer, Some("trading"))
            .expect_err("bug: mismatched selected wallet should fail");

        assert_eq!(error.code(), "saved_wallet_owner_mismatch");
        assert_eq!(
            error.details_json().expect("details expected")["walletName"],
            "trading"
        );
    }

    #[test]
    fn selected_wallet_must_exist() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let profile = test_profile("default", None, BTreeMap::new());

        let error = resolve_wallet_selection(&profile, "default", &signer, Some("missing"))
            .expect_err("bug: unknown selected wallet should fail");

        assert_eq!(error.code(), "saved_wallet_not_found");
    }

    #[test]
    fn ordinary_key_without_saved_wallet_uses_native_nonce_zero() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let profile = test_profile("alice", None, BTreeMap::default());
        let (name, address) = resolve_wallet_selection(&profile, "alice", &signer, None).unwrap();
        assert!(name.is_none());
        assert_eq!(address, default_native_account(&signer).address());
    }

    #[test]
    fn rpc_only_signing_still_requires_a_network_hash() {
        let profile = test_profile("alice", None, BTreeMap::new());
        assert!(matches!(
            resolve_network_hash(&profile, "alice"),
            Err(CliError::MissingNetworkHash)
        ));
    }

    #[test]
    fn hosted_signing_does_not_ignore_an_invalid_network_pin() {
        let mut profile = test_profile("alice", None, BTreeMap::new());
        profile.faucet_url = Some("http://127.0.0.1:2".into());
        profile.network_hash = Some("invalid".into());
        assert!(matches!(
            resolve_network_hash(&profile, "alice"),
            Err(CliError::InvalidNetworkHash { .. })
        ));
    }

    #[test]
    fn network_hash_accepts_canonical_base58() {
        let mut profile = test_profile("alice", None, BTreeMap::new());
        profile.network_hash = Some(NetworkHashText::from([0xab; 32]).to_string());

        let resolved = resolve_network_hash(&profile, "alice").unwrap();

        assert_eq!(
            resolved,
            Some(NetworkHashText::from([0xab; 32]).to_network_hash())
        );
    }

    #[test]
    fn native_bindings_require_a_matching_nonce_owner_and_address() {
        let signer = LocalSigner::from_secret_key([42; 32]);
        let account =
            thruput_signing::PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 17);
        let binding = SavedWalletBinding {
            address: crate::codec::format_contract_address(&account.address()),
            owner_public_key: encode(&signer.verifying_key_bytes()),
            kind: crate::config::WalletKind::Native,
            account_nonce: Some(17),
        };
        assert_eq!(
            validate_saved_wallet_binding("alice", &binding)
                .unwrap()
                .address,
            account.address()
        );
        let mut invalid = binding.clone();
        invalid.account_nonce = None;
        assert!(validate_saved_wallet_binding("alice", &invalid).is_err());
        invalid.account_nonce = Some(0);
        assert!(validate_saved_wallet_binding("alice", &invalid).is_err());
        let profile = test_profile(
            "alice",
            None,
            BTreeMap::from([("alice".into(), invalid.clone())]),
        );
        assert!(
            resolve_selected_wallet(&profile, "alice", signer.verifying_key_bytes(), "alice")
                .is_err()
        );
        assert!(
            resolve_saved_wallet_for_public_key(&profile, "alice", signer.verifying_key_bytes())
                .is_err()
        );
        assert!(resolve_address_target(&profile, "alice").is_err());
        invalid.kind = crate::config::WalletKind::DevWallet;
        assert!(validate_saved_wallet_binding("alice", &invalid).is_err());
        let untyped = format!(
            "address = '{}'\nowner_public_key = '{}'\n",
            binding.address, binding.owner_public_key
        );
        let legacy: SavedWalletBinding = toml::from_str(&untyped).unwrap();
        assert_eq!(legacy.kind, crate::config::WalletKind::DevWallet);
        assert!(legacy.account_nonce.is_none());
        assert!(
            toml::from_str::<SavedWalletBinding>(&format!("{untyped}kind = 'unknown'\n")).is_err()
        );
    }

    fn test_profile(
        key_name: &str,
        runtime_manifest: Option<std::path::PathBuf>,
        wallets: BTreeMap<String, SavedWalletBinding>,
    ) -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: Some("http://127.0.0.1:1".into()),
            network_hash: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some(key_name.into()),
            default_wallet: None,
            runtime_manifest,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets,
            source: None,
        }
    }

    #[test]
    fn non_dev_key_ignores_wallet_owned_by_another_key() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let other = thruput_signing::derive_default_dev_account_signer(1);
        let wallet_address = thruput_xdr::types::ContractAddress::from([0x41; 32]);
        let mut profile = test_profile("alice", None, BTreeMap::new());
        profile.wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(wallet_address.as_ref()),
                owner_public_key: encode(&other.verifying_key_bytes()),
            },
        );

        let (name, address) = resolve_wallet_selection(&profile, "alice", &signer, None).unwrap();
        assert!(name.is_none());
        assert_eq!(address, default_native_account(&signer).address());
        assert_ne!(address, wallet_address);
    }

    #[test]
    fn selected_wallet_rejects_invalid_owner_public_key() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let wallet_address = thruput_xdr::types::ContractAddress::from([0x41; 32]);
        let mut profile = test_profile("alice", None, BTreeMap::new());
        profile.wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(wallet_address.as_ref()),
                owner_public_key: "not-hex".into(),
            },
        );

        let error = resolve_wallet_selection(&profile, "alice", &signer, Some("alice"))
            .expect_err("bug: invalid saved wallet owner should fail");

        assert_eq!(error.code(), "saved_wallet_owner_public_key_invalid");
        assert!(error.to_string().contains("owner_public_key is invalid"));
        assert!(
            error
                .hint()
                .expect("hint expected")
                .contains("owner_public_key")
        );
    }

    #[test]
    fn automatic_selection_ignores_malformed_owner_public_key() {
        let signer = thruput_signing::derive_default_dev_account_signer(0);
        let wallet_address = thruput_xdr::types::ContractAddress::from([0x41; 32]);
        let mut profile = test_profile("alice", None, BTreeMap::new());
        profile.wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: encode(wallet_address.as_ref()),
                owner_public_key: String::new(),
            },
        );

        let (name, address) = resolve_wallet_selection(&profile, "alice", &signer, None).unwrap();
        assert!(name.is_none());
        assert_eq!(address, default_native_account(&signer).address());
    }

    #[test]
    fn local_bootstrap_account_resolves_public_faucet() {
        let temp = tempfile::TempDir::new().expect("bug: tempdir should be created");
        let manifest_path = temp.path().join("runtime-manifest.json");
        let faucet_wallet =
            default_native_account(&thruput_signing::derive_default_dev_account_signer(0))
                .address();
        write_test_runtime_manifest_with_accounts(&manifest_path, &[(0, "faucet", faucet_wallet)]);
        let profile = profile_with_manifest(Some(manifest_path));

        let resolved = resolve_local_bootstrap_account(&profile, LocalBootstrapRole::Faucet)
            .expect("bug: faucet role should resolve");

        assert_eq!(resolved.role, "faucet");
        assert_eq!(resolved.wallet_address, faucet_wallet);
    }

    #[test]
    fn local_bootstrap_account_rejects_dev_zero_without_faucet_role() {
        let temp = tempfile::TempDir::new().expect("bug: tempdir should be created");
        let manifest_path = temp.path().join("runtime-manifest.json");
        let dev0_wallet = thruput_xdr::types::ContractAddress::from([0x70; 32]);
        write_test_runtime_manifest_with_accounts(
            &manifest_path,
            &[(0, "local_user", dev0_wallet)],
        );
        let profile = profile_with_manifest(Some(manifest_path));

        let error = resolve_local_bootstrap_account(&profile, LocalBootstrapRole::Faucet)
            .expect_err("dev-0 without faucet role should fail");

        assert!(error.to_string().contains("role `faucet`"));
    }

    #[test]
    fn local_bootstrap_account_requires_manifest() {
        let profile = profile_with_manifest(None);

        let err = resolve_local_bootstrap_account(&profile, LocalBootstrapRole::Faucet)
            .expect_err("bug: missing manifest should fail");

        assert!(err.to_string().contains("local bootstrap account"));
    }

    fn profile_with_manifest(manifest_path: Option<std::path::PathBuf>) -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: None,
            network_hash: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".into()),
            default_wallet: None,
            runtime_manifest: manifest_path,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets: BTreeMap::default(),
            source: None,
        }
    }

    fn write_test_runtime_manifest_with_accounts(
        manifest_path: &std::path::Path,
        accounts: &[(usize, &str, thruput_xdr::types::ContractAddress)],
    ) {
        let accounts: Vec<serde_json::Value> = accounts
            .iter()
            .map(|(index, role, wallet_address)| {
                let signer = thruput_signing::derive_default_dev_account_signer(*index);
                serde_json::json!({
                    "role": role,
                    "publicKey": thruput_json_rpc_types::Ed25519PublicKey::from(thruput_xdr::types::Ed25519PublicKey::from(signer.verifying_key_bytes())),
                    "walletAddress": thruput_json_rpc_types::ContractAddress::from(*wallet_address),
                    "initialStorageTokens": "1000000000"
                })
            })
            .collect();
        write_test_runtime_manifest_json(manifest_path, &accounts);
    }

    fn write_test_runtime_manifest_json(
        manifest_path: &std::path::Path,
        accounts: &[serde_json::Value],
    ) {
        let manifest = serde_json::json!({
            "schemaVersion": 5,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "1".repeat(32),
            "protocol": "hogp/test00000000000",
            "genesisHash": NetworkHashText::from([0x42; 32]),
            "producer": {
                "binary": "hog-devnode",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000"
            },
            "bootstrapAccounts": accounts,
            "bootstrap": {
                "storageCostModel": {
                    "hash": NetworkHashText::from([0x53; 32]),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234"
                }
            }
        });
        std::fs::write(
            manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("bug: manifest serializes"),
        )
        .expect("bug: manifest should write");
    }
}
