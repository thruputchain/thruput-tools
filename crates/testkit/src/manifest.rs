//! Runtime-manifest loading and explicit account derivation.

use std::path::Path;

use groundhog_json_rpc_types::runtime_manifest::{
    BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER, RuntimeManifest, RuntimeManifestParseError,
    parse_runtime_manifest,
};
use groundhog_signing::{
    DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount, Signer, derive_dev_account_signer,
};
use groundhog_xdr::types::{ContractAddress, NetworkHash};

use crate::error::{Result, TestkitError};

/// Derivation scheme this harness understands.
const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: usize = 1024 * 1024;

/// A signer paired with its nonce-zero native address.
#[derive(Debug)]
pub(crate) struct TestAccount {
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
    /// Deterministic empty native accounts for live and offline scenarios.
    Native {
        count: usize,
        seed: String,
        network_hash: Option<NetworkHash>,
    },
}

impl AccountSource {
    pub(crate) fn accounts(&self) -> Result<Vec<TestAccount>> {
        match self {
            Self::Manifest(manifest) => derive_manifest_accounts(manifest),
            Self::Native {
                count,
                seed,
                network_hash,
            } => Ok(derive_native_accounts(*count, seed, network_hash.as_ref())),
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

fn derive_manifest_accounts(manifest: &RuntimeManifest) -> Result<Vec<TestAccount>> {
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
            if PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()
                != account.wallet_address.0
            {
                return Err(TestkitError::Manifest {
                    field: "bootstrapAccounts.walletAddress",
                    detail: "address is not its nonce-zero native account".into(),
                });
            }
            Ok(TestAccount {
                index: account.index,
                signer,
                wallet_address: account.wallet_address.0,
                network_hash: Some(manifest.network_hash.to_network_hash()),
            })
        })
        .collect()
}

pub(crate) fn derive_native_accounts(
    count: usize,
    seed: &str,
    network_hash: Option<&NetworkHash>,
) -> Vec<TestAccount> {
    (0..count)
        .map(|index| {
            let signer = derive_dev_account_signer(&format!("groundhog-testkit:{seed}"), index);
            let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
            TestAccount {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                wallet_address: account.address(),
                signer,
                network_hash: network_hash.cloned(),
            }
        })
        .collect()
}

pub(crate) fn derive_faucet(manifest: &RuntimeManifest) -> Result<Option<TestAccount>> {
    let Some(account) = manifest.bootstrap_accounts.iter().find(|account| {
        account.role == groundhog_json_rpc_types::runtime_manifest::BOOTSTRAP_ACCOUNT_ROLE_FAUCET
    }) else {
        return Ok(None);
    };
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(TestkitError::UnsupportedDerivationScheme {
            scheme: manifest.bootstrap_account_derivation.scheme.clone(),
        });
    }
    let signer = derive_dev_account_signer(
        &manifest.bootstrap_account_derivation.seed,
        account.index as usize,
    );
    if account.public_key.as_ref() != signer.verifying_key_bytes().as_slice() {
        return Err(TestkitError::BootstrapPublicKeyMismatch {
            index: account.index,
        });
    }
    if PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()
        != account.wallet_address.0
    {
        return Err(TestkitError::Manifest {
            field: "bootstrapAccounts.walletAddress",
            detail: "address is not its nonce-zero native account".into(),
        });
    }
    Ok(Some(TestAccount {
        index: account.index,
        signer,
        wallet_address: account.wallet_address.0,
        network_hash: Some(manifest.network_hash.to_network_hash()),
    }))
}
