use groundhog_json_rpc_types::{
    ContractAddress,
    runtime_manifest::{BootstrapAccount, RuntimeManifest},
};
use groundhog_signing::LocalSigner;

use crate::{
    CliError, codec,
    config::{ResolvedProfile, SavedWalletBinding},
    dev_keys,
    runtime_manifest::{SUPPORTED_DERIVATION_SCHEME, load_configured_runtime_manifest},
};

const LOCAL_BOOTSTRAP_FALLBACK_INDEX: usize = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalBootstrapRole {
    Faucet,
}

impl LocalBootstrapRole {
    fn manifest_role(self) -> &'static str {
        match self {
            Self::Faucet => "faucet",
        }
    }

    fn error_key_name(self) -> &'static str {
        self.manifest_role()
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedLocalBootstrapAccount {
    pub(crate) key_name: String,
    pub(crate) role: String,
    pub(crate) signer: LocalSigner,
    pub(crate) wallet_address: groundhog_xdr::types::ContractAddress,
}

/// Resolve the wallet address used as a transaction's invoked address.
///
/// Dev keys (`dev-N`) use wallet metadata advertised by the local dev backend.
/// Stored non-dev keys use saved wallet bindings created by `wallet create`.
pub(crate) fn resolve_wallet_address(
    profile: &ResolvedProfile,
    key_name: &str,
    signer: &LocalSigner,
) -> Result<groundhog_xdr::types::ContractAddress, CliError> {
    if let Some(index) = dev_keys::dev_key_index(key_name) {
        resolve_dev_wallet_address(profile, key_name, index, signer)
    } else if let Some(wallet) = profile.wallets.get(key_name) {
        resolve_saved_wallet_address(key_name, wallet, signer)
    } else {
        Err(CliError::SavedWalletMissing {
            key_name: key_name.to_string(),
            profile: profile.name.clone(),
        })
    }
}

fn resolve_saved_wallet_address(
    key_name: &str,
    wallet: &SavedWalletBinding,
    signer: &LocalSigner,
) -> Result<groundhog_xdr::types::ContractAddress, CliError> {
    let owner_public_key = parse_owner_public_key(key_name, &wallet.owner_public_key)?;
    let expected_public_key = signer.verifying_key_bytes();
    if owner_public_key.as_slice() != expected_public_key.as_slice() {
        return Err(CliError::SavedWalletOwnerMismatch {
            key_name: key_name.to_string(),
            configured: format_hex(&owner_public_key),
            local: format_hex(&expected_public_key),
        });
    }

    codec::parse_contract_address(&wallet.address)
}

fn parse_owner_public_key(key_name: &str, value: &str) -> Result<Vec<u8>, CliError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| {
            saved_wallet_owner_public_key_invalid(
                key_name,
                "must be a 0x-prefixed 64-character hex string",
            )
        })?;
    let bytes = hex::decode(hex).map_err(|error| {
        saved_wallet_owner_public_key_invalid(key_name, format!("not valid hex: {error}"))
    })?;
    if bytes.len() != 32 {
        return Err(saved_wallet_owner_public_key_invalid(
            key_name,
            "must be 32 bytes",
        ));
    }
    Ok(bytes)
}

fn saved_wallet_owner_public_key_invalid(key_name: &str, message: impl Into<String>) -> CliError {
    CliError::SavedWalletOwnerPublicKeyInvalid {
        key_name: key_name.to_string(),
        message: message.into(),
    }
}

fn resolve_dev_wallet_address(
    profile: &ResolvedProfile,
    key_name: &str,
    index: usize,
    signer: &LocalSigner,
) -> Result<groundhog_xdr::types::ContractAddress, CliError> {
    let account = resolve_dev_account(profile, key_name, index)?;

    let expected_public_key = signer.verifying_key_bytes();
    if account.public_key.as_slice() != expected_public_key.as_slice() {
        return Err(dev_account_error(
            key_name,
            format!(
                "local dev account metadata returned public key {} for dev-{index}, expected 0x{}",
                format_hex(&account.public_key),
                hex::encode(expected_public_key),
            ),
        ));
    }

    Ok(account.address)
}

/// Look up a local dev account address through the configured runtime manifest.
pub(crate) fn lookup_dev_account_address(
    profile: &ResolvedProfile,
    key_name: &str,
    index: usize,
) -> Result<ContractAddress, CliError> {
    Ok(resolve_dev_account(profile, key_name, index)?
        .address
        .into())
}

/// Resolve the local bootstrap account used for CLI-sponsored local flows.
///
/// Dedicated manifest roles are preferred when present. Until hog-devnode emits
/// those roles, local flows fall back to dev-0.
pub(crate) fn resolve_local_bootstrap_account(
    profile: &ResolvedProfile,
    role: LocalBootstrapRole,
) -> Result<ResolvedLocalBootstrapAccount, CliError> {
    let key_name = role.error_key_name();
    let Some(manifest) = load_configured_runtime_manifest(profile, key_name)? else {
        return Err(dev_account_error(
            key_name,
            "local bootstrap account resolution requires GROUNDHOG_RUNTIME_MANIFEST \
             or profile.<name>.runtime_manifest from `groundhog node`",
        ));
    };
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(dev_account_error(
            key_name,
            format!(
                "runtime manifest uses unsupported bootstrap account derivation scheme `{}`",
                manifest.bootstrap_account_derivation.scheme
            ),
        ));
    }

    let selected = manifest
        .bootstrap_accounts
        .iter()
        .find(|account| account.role == role.manifest_role())
        .or_else(|| {
            manifest.bootstrap_accounts.iter().find(|account| {
                usize::try_from(account.index).ok() == Some(LOCAL_BOOTSTRAP_FALLBACK_INDEX)
            })
        })
        .ok_or_else(|| {
            dev_account_error(
                key_name,
                "runtime manifest has no matching bootstrap account and no dev-0 fallback",
            )
        })?;

    let index = usize::try_from(selected.index).map_err(|_| {
        dev_account_error(
            key_name,
            format!(
                "runtime manifest bootstrap account index {} does not fit this platform",
                selected.index
            ),
        )
    })?;
    let signer = signer_for_manifest_account(&manifest, selected, key_name, index)?;
    Ok(ResolvedLocalBootstrapAccount {
        key_name: format!("dev-{index}"),
        role: selected.role.clone(),
        signer,
        wallet_address: selected.wallet_address.0,
    })
}

fn resolve_dev_account(
    profile: &ResolvedProfile,
    key_name: &str,
    index: usize,
) -> Result<ResolvedDevAccount, CliError> {
    if let Some(manifest) = load_configured_runtime_manifest(profile, key_name)? {
        return dev_account_from_manifest(&manifest, key_name, index);
    }
    Err(dev_account_error(
        key_name,
        "dev-N keys require GROUNDHOG_RUNTIME_MANIFEST or \
         profile.<name>.runtime_manifest from `groundhog node`",
    ))
}

struct ResolvedDevAccount {
    address: groundhog_xdr::types::ContractAddress,
    public_key: Vec<u8>,
}

fn dev_account_from_manifest(
    manifest: &RuntimeManifest,
    key_name: &str,
    index: usize,
) -> Result<ResolvedDevAccount, CliError> {
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(dev_account_error(
            key_name,
            format!(
                "runtime manifest uses unsupported bootstrap account derivation scheme `{}`",
                manifest.bootstrap_account_derivation.scheme
            ),
        ));
    }
    let Some(account) = manifest
        .bootstrap_accounts
        .iter()
        .find(|account| usize::try_from(account.index).ok() == Some(index))
    else {
        return Err(dev_account_error(
            key_name,
            format!(
                "runtime manifest returned {} bootstrap accounts, missing dev-{index}",
                manifest.bootstrap_accounts.len()
            ),
        ));
    };

    let signer = signer_for_manifest_account(manifest, account, key_name, index)?;

    Ok(ResolvedDevAccount {
        address: account.wallet_address.0,
        public_key: signer.verifying_key_bytes().to_vec(),
    })
}

fn signer_for_manifest_account(
    manifest: &RuntimeManifest,
    account: &BootstrapAccount,
    key_name: &str,
    index: usize,
) -> Result<LocalSigner, CliError> {
    let derived_secret =
        zeroize::Zeroizing::new(groundhog_signing::dev_account::derive_dev_account_secret(
            &manifest.bootstrap_account_derivation.seed,
            index,
        ));
    let signer = groundhog_signing::LocalSigner::from_secret_key(*derived_secret);
    let derived_public_key = signer.verifying_key_bytes();
    if account.public_key.0.as_ref() != derived_public_key.as_slice() {
        return Err(dev_account_error(
            key_name,
            format!(
                "runtime manifest bootstrap account dev-{index} public key {} does not match derivation seed",
                format_hex(account.public_key.0.as_ref()),
            ),
        ));
    }
    let _initial_storage_tokens = account.initial_storage_tokens.parse::<u64>().map_err(|error| {
        dev_account_error(
            key_name,
            format!(
                "runtime manifest bootstrap account dev-{index} initialStorageTokens is invalid: {error}"
            ),
        )
    })?;
    Ok(signer)
}

fn dev_account_error(key_name: &str, message: impl Into<String>) -> CliError {
    CliError::LocalDevAccountUnavailable {
        key_name: key_name.to_string(),
        message: message.into(),
    }
}

fn format_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn non_dev_key_uses_saved_wallet_address() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let wallet_address = groundhog_xdr::types::ContractAddress::from([0x80; 32]);
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                address: format_hex(wallet_address.as_ref()),
                owner_public_key: format_hex(&signer.verifying_key_bytes()),
            },
        );
        let profile = test_profile("alice", None, wallets);

        let resolved = resolve_wallet_address(&profile, "alice", &signer)
            .expect("bug: saved wallet should resolve");

        assert_eq!(resolved, wallet_address);
    }

    #[test]
    fn non_dev_key_requires_saved_wallet_address() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let profile = test_profile("alice", None, BTreeMap::default());

        let err = resolve_wallet_address(&profile, "alice", &signer)
            .expect_err("bug: missing wallet should fail");

        assert_eq!(err.code(), "saved_wallet_missing");
        assert!(err.to_string().contains("run `groundhog wallet create"));
        assert_eq!(
            err.details_json().expect("details expected")["profile"],
            "default"
        );
        assert!(
            err.hint()
                .expect("hint expected")
                .contains("groundhog wallet create")
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
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some(key_name.into()),
            runtime_manifest,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets,
            source: None,
        }
    }

    #[test]
    fn non_dev_key_uses_matching_saved_wallet_binding() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let wallet_address = groundhog_xdr::types::ContractAddress::from([0x41; 32]);
        let mut profile = test_profile_for_key("alice");
        profile.wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                address: format_hex(wallet_address.as_ref()),
                owner_public_key: format_hex(&signer.verifying_key_bytes()),
            },
        );

        let resolved = resolve_wallet_address(&profile, "alice", &signer)
            .expect("bug: matching saved wallet should resolve");

        assert_eq!(resolved, wallet_address);
    }

    #[test]
    fn non_dev_key_rejects_mismatched_saved_wallet_owner() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let other = groundhog_signing::derive_default_dev_account_signer(1);
        let wallet_address = groundhog_xdr::types::ContractAddress::from([0x41; 32]);
        let mut profile = test_profile_for_key("alice");
        profile.wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                address: format_hex(wallet_address.as_ref()),
                owner_public_key: format_hex(&other.verifying_key_bytes()),
            },
        );

        let error = resolve_wallet_address(&profile, "alice", &signer)
            .expect_err("bug: mismatched saved wallet owner should fail");

        assert_eq!(error.code(), "saved_wallet_owner_mismatch");
        assert!(
            error
                .to_string()
                .contains("saved wallet `alice` owner public key")
        );
        assert_eq!(
            error.details_json().expect("details expected")["keyName"],
            "alice"
        );
    }

    #[test]
    fn non_dev_key_rejects_invalid_saved_wallet_owner_public_key() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let wallet_address = groundhog_xdr::types::ContractAddress::from([0x41; 32]);
        let mut profile = test_profile_for_key("alice");
        profile.wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                address: format_hex(wallet_address.as_ref()),
                owner_public_key: "not-hex".into(),
            },
        );

        let error = resolve_wallet_address(&profile, "alice", &signer)
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
    fn non_dev_key_rejects_missing_saved_wallet_owner_public_key() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let wallet_address = groundhog_xdr::types::ContractAddress::from([0x41; 32]);
        let mut profile = test_profile_for_key("alice");
        profile.wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                address: format_hex(wallet_address.as_ref()),
                owner_public_key: String::new(),
            },
        );

        let error = resolve_wallet_address(&profile, "alice", &signer)
            .expect_err("bug: missing saved wallet owner should fail");

        assert_eq!(error.code(), "saved_wallet_owner_public_key_invalid");
        assert!(error.to_string().contains("owner_public_key is invalid"));
    }

    #[test]
    fn dev_key_manifest_resolution_does_not_require_rpc_url() {
        let temp = tempfile::TempDir::new().expect("bug: tempdir should be created");
        let manifest_path = temp.path().join("runtime-manifest.json");
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let wallet_address = groundhog_xdr::types::ContractAddress::from([0x71; 32]);
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "producer": {
                "binary": "hog-devnode",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000"
            },
            "bootstrapAccountDerivation": {
                "scheme": SUPPORTED_DERIVATION_SCHEME,
                "seed": groundhog_signing::DEFAULT_DEV_ACCOUNT_SEED
            },
            "bootstrapAccounts": [{
                "index": 0,
                "role": "local_user",
                "publicKey": format!("0x{}", hex::encode(signer.verifying_key_bytes())),
                "walletAddress": format!("0x{}", hex::encode(wallet_address.as_ref())),
                "initialStorageTokens": "1000000000"
            }],
            "bootstrap": {
                "devWallet": {
                    "contentHash": format!("0x{}", "51".repeat(32)),
                    "deployedCodeHash": format!("0x{}", "52".repeat(32)),
                    "artifactHint": "./dev-wallet.wasm",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "createWalletEntrypoint": "0x6372656174655761",
                    "doActionEntrypoint": "0x646f416374696f6e"
                },
                "storageCostModel": {
                    "hash": format!("0x{}", "53".repeat(32)),
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
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .expect("bug: manifest should write");
        let profile = ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".into()),
            runtime_manifest: Some(manifest_path),
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets: BTreeMap::default(),
            source: None,
        };

        let resolved = resolve_wallet_address(&profile, "dev-0", &signer)
            .expect("bug: manifest-backed dev key should not require rpc_url");

        assert_eq!(resolved, wallet_address);
    }

    #[test]
    fn dev_key_manifest_resolution_uses_profile_runtime_manifest() {
        let temp = tempfile::TempDir::new().expect("bug: tempdir should be created");
        let project = temp.path().join("counter");
        std::fs::create_dir(&project).expect("bug: project dir should be created");
        let config_path = project.join("groundhog.toml");
        std::fs::write(&config_path, "[profile.default]\ndefault_key = \"dev-0\"\n")
            .expect("bug: project config should write");
        let manifest_path = temp.path().join("runtime-manifest.json");
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let wallet_address = groundhog_xdr::types::ContractAddress::from([0x72; 32]);
        write_test_runtime_manifest(&manifest_path, &signer, wallet_address);
        let profile = ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".into()),
            runtime_manifest: Some(manifest_path),
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets: BTreeMap::default(),
            source: Some(config_path),
        };

        let resolved = resolve_wallet_address(&profile, "dev-0", &signer)
            .expect("bug: local-node state manifest should resolve dev key");

        assert_eq!(resolved, wallet_address);
    }

    #[test]
    fn local_bootstrap_account_prefers_manifest_role() {
        let temp = tempfile::TempDir::new().expect("bug: tempdir should be created");
        let manifest_path = temp.path().join("runtime-manifest.json");
        let dev0_wallet = groundhog_xdr::types::ContractAddress::from([0x70; 32]);
        let faucet_wallet = groundhog_xdr::types::ContractAddress::from([0x73; 32]);
        write_test_runtime_manifest_with_accounts(
            &manifest_path,
            &[(0, "local_user", dev0_wallet), (3, "faucet", faucet_wallet)],
        );
        let profile = profile_with_manifest(Some(manifest_path));

        let resolved = resolve_local_bootstrap_account(&profile, LocalBootstrapRole::Faucet)
            .expect("bug: faucet role should resolve");

        assert_eq!(resolved.key_name, "dev-3");
        assert_eq!(resolved.role, "faucet");
        assert_eq!(resolved.wallet_address, faucet_wallet);
        assert_eq!(
            resolved.signer.verifying_key_bytes(),
            groundhog_signing::derive_default_dev_account_signer(3).verifying_key_bytes(),
        );
    }

    #[test]
    fn local_bootstrap_account_falls_back_to_dev_zero() {
        let temp = tempfile::TempDir::new().expect("bug: tempdir should be created");
        let manifest_path = temp.path().join("runtime-manifest.json");
        let dev0_wallet = groundhog_xdr::types::ContractAddress::from([0x70; 32]);
        write_test_runtime_manifest_with_accounts(
            &manifest_path,
            &[(0, "local_user", dev0_wallet)],
        );
        let profile = profile_with_manifest(Some(manifest_path));

        let resolved = resolve_local_bootstrap_account(&profile, LocalBootstrapRole::Faucet)
            .expect("bug: faucet should fall back to dev-0");

        assert_eq!(resolved.key_name, "dev-0");
        assert_eq!(resolved.role, "local_user");
        assert_eq!(resolved.wallet_address, dev0_wallet);
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
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".into()),
            runtime_manifest: manifest_path,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets: BTreeMap::default(),
            source: None,
        }
    }

    fn test_profile_for_key(key_name: &str) -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: Some("http://127.0.0.1:1".into()),
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some(key_name.into()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets: BTreeMap::default(),
            source: None,
        }
    }

    fn write_test_runtime_manifest(
        manifest_path: &std::path::Path,
        signer: &LocalSigner,
        wallet_address: groundhog_xdr::types::ContractAddress,
    ) {
        let account = serde_json::json!({
            "index": 0,
            "role": "local_user",
            "publicKey": format!("0x{}", hex::encode(signer.verifying_key_bytes())),
            "walletAddress": format!("0x{}", hex::encode(wallet_address.as_ref())),
            "initialStorageTokens": "1000000000"
        });
        write_test_runtime_manifest_json(manifest_path, std::slice::from_ref(&account));
    }

    fn write_test_runtime_manifest_with_accounts(
        manifest_path: &std::path::Path,
        accounts: &[(usize, &str, groundhog_xdr::types::ContractAddress)],
    ) {
        let accounts: Vec<serde_json::Value> = accounts
            .iter()
            .map(|(index, role, wallet_address)| {
                let signer = groundhog_signing::derive_default_dev_account_signer(*index);
                serde_json::json!({
                    "index": index,
                    "role": role,
                    "publicKey": format!("0x{}", hex::encode(signer.verifying_key_bytes())),
                    "walletAddress": format!("0x{}", hex::encode(wallet_address.as_ref())),
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
            "schemaVersion": 2,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "producer": {
                "binary": "hog-devnode",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000"
            },
            "bootstrapAccountDerivation": {
                "scheme": SUPPORTED_DERIVATION_SCHEME,
                "seed": groundhog_signing::DEFAULT_DEV_ACCOUNT_SEED
            },
            "bootstrapAccounts": accounts,
            "bootstrap": {
                "devWallet": {
                    "contentHash": format!("0x{}", "51".repeat(32)),
                    "deployedCodeHash": format!("0x{}", "52".repeat(32)),
                    "artifactHint": "./dev-wallet.wasm",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "createWalletEntrypoint": "0x6372656174655761",
                    "doActionEntrypoint": "0x646f416374696f6e"
                },
                "storageCostModel": {
                    "hash": format!("0x{}", "53".repeat(32)),
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
