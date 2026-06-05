use std::path::Path;

use groundhog_json_rpc_types::runtime_manifest::{
    BootstrapAccount, RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeManifest,
};
use groundhog_signing::{LocalSigner, derive_dev_account_signer};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct SimAccount {
    pub index: u32,
    pub signer: LocalSigner,
    pub wallet_address: groundhog_xdr::types::ContractAddress,
}

#[derive(Debug, Clone)]
pub struct LoadedRuntimeManifest {
    pub manifest: RuntimeManifest,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

pub async fn load_runtime_manifest(
    path: &Path,
) -> Result<LoadedRuntimeManifest, RuntimeManifestError> {
    let path_display = path.display().to_string();
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|source| RuntimeManifestError::Read {
            path: path_display.clone(),
            source,
        })?;
    let mut bytes = Vec::new();
    let mut reader = file.take(MAX_RUNTIME_MANIFEST_BYTES + 1);
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| RuntimeManifestError::Read {
            path: path_display.clone(),
            source,
        })?;
    if bytes.len() as u64 > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(RuntimeManifestError::TooLarge {
            path: path_display,
            max_bytes: MAX_RUNTIME_MANIFEST_BYTES,
        });
    }
    let manifest: RuntimeManifest =
        serde_json::from_slice(&bytes).map_err(|source| RuntimeManifestError::Parse {
            path: path_display,
            source,
        })?;
    if manifest.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
        return Err(RuntimeManifestError::UnsupportedSchema {
            actual: manifest.schema_version,
        });
    }

    let sha256 = format!("0x{}", hex::encode(Sha256::digest(&bytes)));
    Ok(LoadedRuntimeManifest {
        manifest,
        bytes,
        sha256,
    })
}

pub fn derive_accounts(
    manifest: &RuntimeManifest,
) -> Result<Vec<SimAccount>, RuntimeManifestError> {
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(RuntimeManifestError::UnsupportedDerivationScheme {
            scheme: manifest.bootstrap_account_derivation.scheme.clone(),
        });
    }
    if manifest.bootstrap_accounts.is_empty() {
        return Err(RuntimeManifestError::MissingBootstrapAccounts);
    }

    manifest
        .bootstrap_accounts
        .iter()
        .map(|account| derive_account(manifest, account))
        .collect()
}

fn derive_account(
    manifest: &RuntimeManifest,
    account: &BootstrapAccount,
) -> Result<SimAccount, RuntimeManifestError> {
    let index = usize::try_from(account.index).map_err(|_| {
        RuntimeManifestError::BootstrapAccountIndexTooLarge {
            index: account.index,
        }
    })?;
    let signer = derive_dev_account_signer(&manifest.bootstrap_account_derivation.seed, index);
    let expected_public_key = signer.verifying_key_bytes();
    if account.public_key.as_ref() != expected_public_key.as_slice() {
        return Err(RuntimeManifestError::BootstrapPublicKeyMismatch {
            index: account.index,
        });
    }

    Ok(SimAccount {
        index: account.index,
        signer,
        wallet_address: account.wallet_address.0,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeManifestError {
    #[error("failed to read runtime manifest {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime manifest {path} exceeds {max_bytes} bytes")]
    TooLarge { path: String, max_bytes: u64 },
    #[error("failed to parse runtime manifest {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported runtime manifest schemaVersion {actual}")]
    UnsupportedSchema { actual: u32 },
    #[error("unsupported bootstrap account derivation scheme `{scheme}`")]
    UnsupportedDerivationScheme { scheme: String },
    #[error("runtime manifest does not advertise bootstrap accounts")]
    MissingBootstrapAccounts,
    #[error("runtime manifest bootstrap account index {index} does not fit this platform")]
    BootstrapAccountIndexTooLarge { index: u32 },
    #[error("runtime manifest bootstrap account {index} public key does not match derivation seed")]
    BootstrapPublicKeyMismatch { index: u32 },
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::hex::Hex0x;
    use groundhog_json_rpc_types::runtime_manifest::RuntimeManifest;
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::Ed25519PublicKey;
    use serde_json::json;

    use super::*;

    #[test]
    fn derive_accounts_validates_manifest_public_keys() {
        let manifest = runtime_manifest(false);

        let accounts = derive_accounts(&manifest).expect("matching public key should derive");

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].index, 0);
    }

    #[test]
    fn derive_accounts_rejects_public_key_mismatch() {
        let manifest = runtime_manifest(true);

        let err = derive_accounts(&manifest).expect_err("mismatch should fail");

        assert!(matches!(
            err,
            RuntimeManifestError::BootstrapPublicKeyMismatch { index: 0 }
        ));
    }

    fn runtime_manifest(mismatch: bool) -> RuntimeManifest {
        let signer = derive_dev_account_signer("seed", 0);
        let public_key = if mismatch {
            Hex0x(Ed25519PublicKey::from([0xff; 32]))
        } else {
            Hex0x(Ed25519PublicKey::from(signer.verifying_key_bytes()))
        };
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x11),
            "producer": {
                "binary": "hog-devnode",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
                "indexer": "127.0.0.1:1001"
            },
            "bootstrapAccountDerivation": {
                "scheme": SUPPORTED_DERIVATION_SCHEME,
                "seed": "seed"
            },
            "bootstrapAccounts": [{
                "index": 0,
                "role": "local_user",
                "publicKey": public_key,
                "walletAddress": prefixed_hex(0x22),
                "initialStorageTokens": "1000000000"
            }],
            "bootstrap": {
                "devWallet": {
                    "contentHash": prefixed_hex(0x33),
                    "deployedCodeHash": prefixed_hex(0x44),
                    "artifactHint": "dev-wallet.wasm",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "doActionEntrypoint": "0x646f416374696f6e"
                },
                "storageCostModel": {
                    "hash": prefixed_hex(0x55),
                    "baseDataItemCost": "1",
                    "bytestringCostPerByte": "1",
                    "baseContractDeploymentCost": "1",
                    "contractDeploymentCostPerByte": "1",
                    "authPerKeyCost": "1",
                    "rcqCostPerEntry": "1",
                    "replayCacheCostPerEntry": "1",
                    "balanceCost": "1"
                }
            }
        }))
        .expect("runtime manifest fixture should parse")
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
