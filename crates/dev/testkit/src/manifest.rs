//! Runtime-manifest loading and dev-account derivation.

use std::path::Path;

use groundhog_json_rpc_types::runtime_manifest::{
    BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER, RuntimeManifest, RuntimeManifestParseError,
    parse_runtime_manifest,
};
use groundhog_signing::{
    DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount, Signer,
    derive_default_dev_account_signer, derive_dev_account_signer,
};
use groundhog_xdr::types::{ContractAddress, NetworkHash};

use crate::error::{Result, TestkitError};

/// Derivation scheme this harness understands.
const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: usize = 1024 * 1024;

/// A funded dev account: a signer paired with its on-chain wallet address.
#[derive(Debug)]
pub(crate) struct DevAccount {
    pub(crate) index: u32,
    pub(crate) signer: LocalSigner,
    pub(crate) wallet_address: ContractAddress,
    /// Network this account signs for. Part of every signature preimage.
    pub(crate) network_hash: Option<NetworkHash>,
}

/// Where a scenario's accounts come from.
///
/// Both variants re-derive on demand so a scenario can run its agents more than
/// once; derivation is deterministic, so each run gets the same signers.
#[derive(Debug)]
pub(crate) enum AccountSource {
    /// Accounts materialized by a live target and described by its manifest.
    Manifest(Box<RuntimeManifest>),
    /// Locally derived default dev accounts for offline scenarios.
    Offline { count: usize },
}

impl AccountSource {
    pub(crate) fn accounts(&self) -> Result<Vec<DevAccount>> {
        match self {
            Self::Manifest(manifest) => derive_manifest_accounts(manifest),
            Self::Offline { count } => derive_offline_accounts(*count),
        }
    }
}

/// Read and parse a runtime manifest from `path`.
pub(crate) fn load_runtime_manifest(path: &Path) -> Result<RuntimeManifest> {
    let display = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|source| TestkitError::ManifestLoad {
        path: display.clone(),
        detail: source.to_string(),
    })?;
    if bytes.len() > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(TestkitError::ManifestLoad {
            path: display,
            detail: format!("manifest exceeds {MAX_RUNTIME_MANIFEST_BYTES} bytes"),
        });
    }
    let manifest = parse_runtime_manifest(&bytes).map_err(|error| match error {
        RuntimeManifestParseError::Json(source) => TestkitError::ManifestLoad {
            path: display,
            detail: source.to_string(),
        },
        RuntimeManifestParseError::UnsupportedSchema { actual, .. } => {
            TestkitError::ManifestSchema { actual }
        }
    })?;
    Ok(manifest)
}

fn derive_manifest_accounts(manifest: &RuntimeManifest) -> Result<Vec<DevAccount>> {
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(TestkitError::UnsupportedDerivationScheme {
            scheme: manifest.bootstrap_account_derivation.scheme.clone(),
        });
    }
    if !manifest
        .bootstrap_accounts
        .iter()
        .any(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER)
    {
        return Err(TestkitError::NoBootstrapAccounts);
    }

    let seed = &manifest.bootstrap_account_derivation.seed;
    manifest
        .bootstrap_accounts
        .iter()
        .filter(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER)
        .map(|account| {
            let index = usize::try_from(account.index).unwrap_or(usize::MAX);
            let signer = derive_dev_account_signer(seed, index);
            if account.public_key.as_ref() != signer.verifying_key_bytes().as_slice() {
                return Err(TestkitError::BootstrapPublicKeyMismatch {
                    index: account.index,
                });
            }
            Ok(DevAccount {
                index: account.index,
                signer,
                wallet_address: account.wallet_address.0,
                network_hash: Some(manifest.network_hash.to_network_hash()),
            })
        })
        .collect()
}

fn derive_offline_accounts(count: usize) -> Result<Vec<DevAccount>> {
    (0..count)
        .map(|index| {
            let signer = derive_default_dev_account_signer(index);
            let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
            let wallet_address = account.address().map_err(TestkitError::signing)?;
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            Ok(DevAccount {
                index,
                signer,
                wallet_address,
                network_hash: None,
            })
        })
        .collect()
}
