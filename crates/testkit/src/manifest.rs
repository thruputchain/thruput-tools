//! Runtime-manifest loading and explicit account derivation.

use std::path::Path;

use thruput_json_rpc_types::runtime_manifest::{
    BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER, RuntimeManifest, RuntimeManifestParseError,
    parse_runtime_manifest,
};
use thruput_signing::{
    DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount, Signer, derive_dev_account_signer,
};
use thruput_xdr::types::{ContractAddress, NetworkHash};

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
    let derivation = hosted_derivation(manifest)?;
    if !manifest
        .bootstrap_accounts
        .iter()
        .any(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER)
    {
        return Err(TestkitError::NoBootstrapAccounts);
    }

    let seed = &derivation.seed;
    manifest
        .bootstrap_accounts
        .iter()
        .filter(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER)
        .map(|account| {
            let account_index = hosted_index(account)?;
            let index = account_index as usize;
            let signer = derive_dev_account_signer(seed, index);
            if account.public_key.as_ref() != signer.verifying_key_bytes().as_slice() {
                return Err(TestkitError::BootstrapPublicKeyMismatch {
                    index: account_index,
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
                index: account_index,
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
            let signer = derive_dev_account_signer(&format!("thruput-testkit:{seed}"), index);
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
    if manifest.kind
        != thruput_json_rpc_types::runtime_manifest::HOSTED_DEVNET_RUNTIME_MANIFEST_KIND
    {
        return Ok(None);
    }
    let Some(account) = manifest.bootstrap_accounts.iter().find(|account| {
        account.role == thruput_json_rpc_types::runtime_manifest::BOOTSTRAP_ACCOUNT_ROLE_FAUCET
    }) else {
        return Ok(None);
    };
    let derivation = hosted_derivation(manifest)?;
    let account_index = hosted_index(account)?;
    let signer = derive_dev_account_signer(&derivation.seed, account_index as usize);
    if account.public_key.as_ref() != signer.verifying_key_bytes().as_slice() {
        return Err(TestkitError::BootstrapPublicKeyMismatch {
            index: account_index,
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
        index: account_index,
        signer,
        wallet_address: account.wallet_address.0,
        network_hash: Some(manifest.network_hash.to_network_hash()),
    }))
}

fn hosted_derivation(
    manifest: &RuntimeManifest,
) -> Result<&thruput_json_rpc_types::runtime_manifest::BootstrapAccountDerivation, TestkitError> {
    if manifest.kind
        != thruput_json_rpc_types::runtime_manifest::HOSTED_DEVNET_RUNTIME_MANIFEST_KIND
    {
        return Err(TestkitError::Manifest { field: "bootstrapAccounts", detail: "Local manifests contain public allocations only. Use scenario-owned accounts instead of bootstrap_accounts().".into() });
    }
    let derivation = manifest
        .bootstrap_account_derivation
        .as_ref()
        .ok_or_else(|| TestkitError::Manifest {
            field: "bootstrapAccountDerivation",
            detail: "hosted bootstrap derivation is missing".into(),
        })?;
    if derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(TestkitError::UnsupportedDerivationScheme {
            scheme: derivation.scheme.clone(),
        });
    }
    Ok(derivation)
}
fn hosted_index(
    account: &thruput_json_rpc_types::runtime_manifest::BootstrapAccount,
) -> Result<u32> {
    account.index.ok_or_else(|| TestkitError::Manifest {
        field: "bootstrapAccounts.index",
        detail: "hosted bootstrap index is missing".into(),
    })
}
