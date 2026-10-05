//! Runtime-manifest inputs and benchmark request construction.

use groundhog_json_rpc_types::hex::sha256_prefixed_hex;
use std::path::Path;

use groundhog_json_rpc_types::runtime_manifest::{
    BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER, BootstrapAccount, RuntimeManifest,
    RuntimeManifestParseError, parse_runtime_manifest,
};
use groundhog_signing::{
    LocalSigner, PubkeyAccount, SignTransactionParams, derive_dev_account_signer,
    sign_and_encode_transaction_hex, transaction_hash,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::{ContractAddress, NetworkHash};
use tokio::io::AsyncReadExt;

use crate::BenchError;
use groundhog_json_rpc_types::{hex::Hex0x, transaction::TxHash};

const HOG_BALANCE_ENTRYPOINT: ContractEntrypointID =
    ContractEntrypointID::new(0x41ac_95d6_e06e_2620_u64.to_be_bytes());

const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;

fn dev_signer_from_seed(seed: &str, index: usize) -> LocalSigner {
    derive_dev_account_signer(seed, index)
}

pub(super) struct BenchAccount {
    signer: LocalSigner,
    wallet_address: ContractAddress,
}

pub(super) struct LoadedRuntimeManifest {
    pub(super) manifest: RuntimeManifest,
    pub(super) sha256: String,
}

#[derive(Debug)]
pub(super) struct PreparedTransaction {
    pub(super) hex: String,
    pub(super) hash: TxHash,
}

/// Pre-generate a batch of signed transactions and their canonical hashes.
pub(super) fn generate_tx_batch(
    count: usize,
    accounts: &[BenchAccount],
    network_hash: &NetworkHash,
    nonce_start: u64,
) -> Result<Vec<PreparedTransaction>, BenchError> {
    (0..count)
        .map(|i| {
            let idx = i % accounts.len();
            let account = &accounts[idx];
            let native =
                PubkeyAccount::from_public_key_bytes(account.signer.verifying_key_bytes(), 0);
            let calldata = xdrs::cxs::encode(&(account.wallet_address,)).map_err(|error| {
                BenchError::WalletCalldata {
                    source: groundhog_signing::SigningError::XdrEncode(error.to_string()),
                }
            })?;
            let tx = groundhog_signing::default_wallet::build_default_wallet_call_transaction(
                groundhog_signing::default_wallet::BuildDefaultWalletCallTransaction {
                    account: &native,
                    target_address: groundhog_signing::NATIVE_HOG_ADDRESS,
                    target_entrypoint: HOG_BALANCE_ENTRYPOINT,
                    target_calldata: &calldata,
                    gas_limit: 20_000_000,
                    target_gas_limit: 10_000_000,
                    block_expiry_height: 10_000_000,
                    nonce: nonce_start.checked_add(i as u64).ok_or_else(|| {
                        BenchError::InvalidConfiguration("seed nonce range overflows u64".into())
                    })?,
                },
            )
            .map_err(|source| BenchError::WalletCalldata { source })?;

            let hash =
                TxHash(Hex0x(transaction_hash(&tx).map_err(|source| {
                    BenchError::SignAndEncodeTransaction { source }
                })?));
            let hex = sign_and_encode_transaction_hex(
                tx,
                &account.signer,
                SignTransactionParams::default(),
                network_hash,
            )
            .map_err(|source| BenchError::SignAndEncodeTransaction { source })?;
            Ok(PreparedTransaction { hex, hash })
        })
        .collect()
}

fn manifest_bench_accounts(manifest: &RuntimeManifest) -> Result<Vec<BenchAccount>, BenchError> {
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(BenchError::UnsupportedDerivationScheme {
            scheme: manifest.bootstrap_account_derivation.scheme.clone(),
        });
    }

    manifest
        .bootstrap_accounts
        .iter()
        .filter(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER)
        .map(|account| manifest_bench_account(manifest, account))
        .collect()
}

fn manifest_bench_account(
    manifest: &RuntimeManifest,
    account: &BootstrapAccount,
) -> Result<BenchAccount, BenchError> {
    let index =
        usize::try_from(account.index).map_err(|_| BenchError::BootstrapAccountIndexTooLarge {
            index: account.index,
        })?;
    let signer = dev_signer_from_seed(&manifest.bootstrap_account_derivation.seed, index);
    let expected_public_key = signer.verifying_key_bytes();
    if account.public_key.as_ref() != expected_public_key.as_slice() {
        return Err(BenchError::BootstrapPublicKeyMismatch {
            index: account.index,
        });
    }
    if PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()
        != account.wallet_address.0
    {
        return Err(BenchError::BootstrapAddressMismatch {
            index: account.index,
        });
    }
    Ok(BenchAccount {
        signer,
        wallet_address: account.wallet_address.0,
    })
}

pub(super) fn transaction_inputs_for_method(
    method: &str,
    manifest: Option<&RuntimeManifest>,
    num_accounts: usize,
) -> Result<Vec<BenchAccount>, BenchError> {
    if method != "sendRawTransaction" {
        return Ok(Vec::new());
    }
    if num_accounts == 0 {
        return Err(BenchError::MissingAccounts);
    }

    let manifest = manifest.ok_or(BenchError::MissingRuntimeManifest)?;
    let mut accounts = manifest_bench_accounts(manifest)?;
    if accounts.len() < num_accounts {
        return Err(BenchError::NotEnoughAccounts {
            requested: num_accounts,
            available: accounts.len(),
        });
    }
    accounts.truncate(num_accounts);
    if accounts.is_empty() {
        return Err(BenchError::MissingAccounts);
    }
    Ok(accounts)
}

pub(super) async fn load_runtime_manifest(
    path: &Path,
) -> Result<LoadedRuntimeManifest, BenchError> {
    let path_display = path.display().to_string();
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|source| BenchError::ManifestRead {
            path: path_display.clone(),
            source,
        })?;
    let mut bytes = Vec::new();
    let mut reader = file.take(MAX_RUNTIME_MANIFEST_BYTES + 1);
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| BenchError::ManifestRead {
            path: path_display.clone(),
            source,
        })?;
    if bytes.len() as u64 > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(BenchError::ManifestTooLarge {
            path: path_display,
            max_bytes: MAX_RUNTIME_MANIFEST_BYTES,
        });
    }
    let manifest = parse_runtime_manifest(&bytes).map_err(|error| match error {
        RuntimeManifestParseError::Json(source) => BenchError::ManifestParse {
            path: path_display,
            source,
        },
        RuntimeManifestParseError::UnsupportedSchema { actual, .. } => {
            BenchError::UnsupportedManifestSchema { actual }
        }
    })?;
    Ok(LoadedRuntimeManifest {
        manifest,
        sha256: sha256_prefixed_hex(&bytes),
    })
}

/// Build a JSON-RPC request body.
pub(super) fn make_request(method: &str, input: &str, id: u64) -> serde_json::Value {
    match method {
        "sendRawTransaction" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "groundhog_sendRawTransaction",
            "params": [input]
        }),
        "chainTip" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "groundhog_chainTip",
            "params": []
        }),
        "getTransactionStatus" | "getTransactionReceipt" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": format!("groundhog_{method}"),
            "params": [if input.is_empty() { "0x0000000000000000000000000000000000000000000000000000000000000000" } else { input }]
        }),
        _ => unreachable!("bug: method validated before benchmark starts"),
    }
}

pub(super) fn validate_method(method: &str) -> Result<(), BenchError> {
    match method {
        "sendRawTransaction" | "chainTip" | "getTransactionStatus" | "getTransactionReceipt" => {
            Ok(())
        }
        method => Err(BenchError::UnknownMethod {
            method: method.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::{hex::Hex0x, runtime_manifest::RUNTIME_MANIFEST_SCHEMA_VERSION};
    use groundhog_xdr::types::{Ed25519PublicKey as XdrEd25519PublicKey, SHA256Hash};
    use serde_json::json;

    use super::*;

    #[test]
    fn read_only_manifest_does_not_validate_bootstrap_accounts() {
        let mut manifest = manifest();
        manifest.bootstrap_account_derivation.scheme = "future-scheme".into();
        manifest.bootstrap_accounts.clear();

        let accounts = transaction_inputs_for_method("chainTip", Some(&manifest), 0)
            .expect("bug: read-only manifest should not validate accounts");

        assert!(accounts.is_empty());
    }

    #[test]
    fn send_raw_transaction_rejects_unsupported_manifest_derivation_scheme() {
        let mut manifest = manifest();
        manifest.bootstrap_account_derivation.scheme = "future-scheme".into();

        let err = match transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 1) {
            Ok(_) => panic!("bug: unsupported derivation scheme should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            err,
            BenchError::UnsupportedDerivationScheme { .. }
        ));
    }

    #[test]
    fn send_raw_transaction_requires_runtime_manifest() {
        let err = match transaction_inputs_for_method("sendRawTransaction", None, 1) {
            Ok(_) => panic!("bug: submit benchmark without runtime metadata should fail"),
            Err(error) => error,
        };

        assert!(matches!(err, BenchError::MissingRuntimeManifest));
    }

    #[test]
    fn send_raw_transaction_rejects_zero_accounts_before_manifest_validation() {
        let mut manifest = manifest();
        manifest.bootstrap_account_derivation.scheme = "future-scheme".into();

        let err = match transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 0) {
            Ok(_) => panic!("bug: zero-account submit benchmark should fail"),
            Err(error) => error,
        };

        assert!(matches!(err, BenchError::MissingAccounts));
    }

    #[test]
    fn send_raw_transaction_uses_native_execute_entrypoint() {
        let accounts =
            transaction_inputs_for_method("sendRawTransaction", Some(&manifest()), 1).unwrap();
        let network = manifest().network_hash.to_network_hash();
        let transactions = generate_tx_batch(1, &accounts, &network, 42).unwrap();
        let bytes = hex::decode(transactions[0].hex.trim_start_matches("0x")).unwrap();
        let signed: groundhog_xdr::transaction::SignedTransaction =
            xdrs::txs::decode(&bytes).unwrap();
        assert_eq!(
            signed.tx.invocation.entrypoint,
            groundhog_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT
        );
        assert_eq!(
            signed.tx.invocation.invoked_address,
            accounts[0].wallet_address
        );
        assert_eq!(signed.tx.uniquifier.nonce, 42);
    }

    #[test]
    fn send_raw_transaction_uses_the_requested_subset_of_manifest_accounts() {
        let accounts = transaction_inputs_for_method(
            "sendRawTransaction",
            Some(&manifest_with_accounts(3)),
            2,
        )
        .expect("bug: manifest has the requested number of accounts");

        assert_eq!(accounts.len(), 2);
    }

    #[test]
    fn send_raw_transaction_excludes_faucet_and_preserves_derivation_indices() {
        let mut manifest = manifest_with_accounts(3);
        manifest.bootstrap_accounts[0].role = "faucet".into();

        let accounts = transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 2)
            .expect("bug: two local-user accounts should be available");

        assert_eq!(accounts.len(), 2);
        assert_eq!(
            accounts[0].signer.verifying_key_bytes(),
            derive_dev_account_signer(&manifest.bootstrap_account_derivation.seed, 1)
                .verifying_key_bytes()
        );
    }

    #[test]
    fn send_raw_transaction_rejects_more_accounts_than_the_manifest_contains() {
        let err = match transaction_inputs_for_method(
            "sendRawTransaction",
            Some(&manifest_with_accounts(2)),
            3,
        ) {
            Ok(_) => panic!("bug: account underfill should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            err,
            BenchError::NotEnoughAccounts {
                requested: 3,
                available: 2
            }
        ));
    }

    #[test]
    fn send_raw_transaction_rejects_public_key_mismatch() {
        let mut manifest = manifest();
        manifest.bootstrap_accounts[0].public_key = Hex0x(XdrEd25519PublicKey::from([0x99; 32]));

        let err = match transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 1) {
            Ok(_) => panic!("bug: public key mismatch should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            err,
            BenchError::BootstrapPublicKeyMismatch { index: 0 }
        ));
    }

    fn manifest() -> RuntimeManifest {
        let seed = "groundhog-dev-accounts";
        let signer = derive_dev_account_signer(seed, 0);
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&signer.verifying_key_bytes());
        serde_json::from_value(json!({
            "schemaVersion": RUNTIME_MANIFEST_SCHEMA_VERSION,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test00000000000",
            "producer": {
                "binary": "hogd-dev",
                "version": "0.1.0",
                "commitHash": "abcdef0",
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
            },
            "bootstrapAccountDerivation": {
                "scheme": SUPPORTED_DERIVATION_SCHEME,
                "seed": seed,
            },
            "bootstrapAccounts": [{
                "index": 0,
                "role": "local_user",
                "publicKey": Hex0x(XdrEd25519PublicKey::from(public_key)),
                "walletAddress": Hex0x(PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()),
                "initialStorageTokens": "1000000000",
            }],
            "bootstrap": {
                "storageCostModel": {
                    "hash": hash(0x53),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234",
                },
            },
        }))
        .expect("bug: manifest JSON should parse")
    }

    fn manifest_with_accounts(count: usize) -> RuntimeManifest {
        let mut manifest = manifest();
        for index in 1..count {
            let signer =
                derive_dev_account_signer(&manifest.bootstrap_account_derivation.seed, index);
            let mut public_key = [0u8; 32];
            public_key.copy_from_slice(&signer.verifying_key_bytes());
            let mut account = manifest.bootstrap_accounts[0].clone();
            account.index = u32::try_from(index).expect("bug: test account index should fit");
            account.wallet_address = Hex0x(
                PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address(),
            );
            account.public_key = Hex0x(XdrEd25519PublicKey::from(public_key));
            manifest.bootstrap_accounts.push(account);
        }
        manifest
    }

    fn hash(byte: u8) -> Hex0x<SHA256Hash> {
        Hex0x(SHA256Hash::from([byte; 32]))
    }
}
