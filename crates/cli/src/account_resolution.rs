use groundhog_rpc_api_types::{ContractAddress, runtime_manifest::RuntimeManifest};
use groundhog_signing::{DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount, Signer};

use crate::{
    CliError,
    config::ResolvedProfile,
    dev_keys,
    runtime_manifest::{SUPPORTED_DERIVATION_SCHEME, load_configured_runtime_manifest},
};

/// Resolve the wallet address used as a transaction's invoked address.
///
/// Stored keys keep the legacy pubkey-derived account path. Dev keys (`dev-N`)
/// must use wallet metadata advertised by the local dev backend and must not
/// fall back to local address derivation when that metadata is unavailable.
pub(crate) fn resolve_wallet_address(
    profile: &ResolvedProfile,
    key_name: &str,
    signer: &LocalSigner,
    _rt: &tokio::runtime::Runtime,
) -> Result<groundhog_xdr::types::ContractAddress, CliError> {
    if let Some(index) = dev_keys::dev_key_index(key_name) {
        resolve_dev_wallet_address(profile, key_name, index, signer)
    } else {
        let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
        account
            .address()
            .map_err(|error| CliError::WalletAddressDerivationFailed {
                key_name: key_name.to_owned(),
                message: error.to_string(),
            })
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
        "dev-N keys require GROUNDHOG_RUNTIME_MANIFEST or profile.<name>.runtime_manifest",
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

    let derived_secret =
        zeroize::Zeroizing::new(groundhog_signing::dev_account::derive_dev_account_secret(
            &manifest.bootstrap_account_derivation.seed,
            index,
        ));
    let derived = groundhog_signing::LocalSigner::from_secret_key(*derived_secret);
    let derived_public_key = derived.verifying_key_bytes();
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

    Ok(ResolvedDevAccount {
        address: account.wallet_address.0,
        public_key: account.public_key.0.as_ref().to_vec(),
    })
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
    fn non_dev_key_uses_pubkey_derived_wallet_address() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let rt = tokio::runtime::Runtime::new().expect("bug: runtime should start");
        let profile = ResolvedProfile {
            name: "default".into(),
            rpc_url: Some("http://127.0.0.1:1".into()),
            default_key: Some("alice".into()),
            runtime_manifest: None,
            contracts: BTreeMap::default(),
            source: None,
        };
        let resolved = resolve_wallet_address(&profile, "alice", &signer, &rt)
            .expect("bug: non-dev key should not call RPC");
        let expected = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE)
            .address()
            .expect("bug: pubkey account address should derive");

        assert_eq!(resolved, expected);
    }

    #[test]
    fn dev_key_manifest_resolution_does_not_require_rpc_url() {
        let temp = tempfile::TempDir::new().expect("bug: tempdir should be created");
        let manifest_path = temp.path().join("runtime-manifest.json");
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let wallet_address = groundhog_xdr::types::ContractAddress::from([0x71; 32]);
        let manifest = serde_json::json!({
            "schemaVersion": 1,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "producer": {
                "binary": "hogd-dev",
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
        let rt = tokio::runtime::Runtime::new().expect("bug: runtime should start");
        let profile = ResolvedProfile {
            name: "default".into(),
            rpc_url: None,
            default_key: Some("dev-0".into()),
            runtime_manifest: Some(manifest_path),
            contracts: BTreeMap::default(),
            source: None,
        };

        let resolved = resolve_wallet_address(&profile, "dev-0", &signer, &rt)
            .expect("bug: manifest-backed dev key should not require rpc_url");

        assert_eq!(resolved, wallet_address);
    }
}
