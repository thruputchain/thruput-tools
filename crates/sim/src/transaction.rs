use groundhog_rpc_api_types::runtime_manifest::RuntimeManifest;
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, SignTransactionParams,
    build_dev_wallet_do_action_transaction, encode_signed_transaction_xdr, sign_transaction,
};
use groundhog_xdr::contract::ContractEntrypointID;

use crate::actor::ActorState;

const DEV_WALLET_SMOKE_GAS_LIMIT: u64 = 10_000_000;
const DEV_WALLET_SMOKE_EXPIRY_HEIGHT: i64 = 10_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltSubmitTransaction {
    pub tx_hex: String,
    pub nonce: u64,
}

pub fn runtime_dev_wallet_action_entrypoint(
    manifest: &RuntimeManifest,
) -> Result<ContractEntrypointID, TransactionBuildError> {
    let value = manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint
        .strip_prefix("0x")
        .unwrap_or(&manifest.bootstrap.dev_wallet.do_action_entrypoint);
    let bytes =
        hex::decode(value).map_err(|source| TransactionBuildError::EntrypointHex { source })?;
    let len = bytes.len();
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| TransactionBuildError::EntrypointLength { len })?;
    Ok(ContractEntrypointID::from(bytes))
}

pub fn build_wallet_submit_smoke_transaction(
    actor: &mut ActorState,
    wallet_entrypoint: ContractEntrypointID,
) -> Result<BuiltSubmitTransaction, TransactionBuildError> {
    let nonce = actor.next_nonce()?;
    let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
        wallet_address: actor.wallet_address,
        wallet_entrypoint,
        target_address: actor.wallet_address,
        target_entrypoint: wallet_entrypoint,
        target_calldata: &[],
        gas_limit: DEV_WALLET_SMOKE_GAS_LIMIT,
        block_expiry_height: DEV_WALLET_SMOKE_EXPIRY_HEIGHT,
        nonce,
    })
    .map_err(|source| TransactionBuildError::WalletCalldata {
        reason: source.to_string(),
    })?;

    let signed = sign_transaction(tx, &actor.signer, SignTransactionParams::default()).map_err(
        |source| TransactionBuildError::SignTransaction {
            reason: source.to_string(),
        },
    )?;
    let xdr = encode_signed_transaction_xdr(&signed).map_err(|source| {
        TransactionBuildError::EncodeTransaction {
            reason: source.to_string(),
        }
    })?;

    Ok(BuiltSubmitTransaction {
        tx_hex: format!("0x{}", hex::encode(xdr)),
        nonce,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum TransactionBuildError {
    #[error(transparent)]
    Actor(#[from] crate::actor::ActorError),
    #[error("runtime manifest devWallet.doActionEntrypoint is not valid hex: {source}")]
    EntrypointHex {
        #[source]
        source: hex::FromHexError,
    },
    #[error("runtime manifest devWallet.doActionEntrypoint must be 8 bytes, got {len}")]
    EntrypointLength { len: usize },
    #[error("failed to encode wallet action calldata: {reason}")]
    WalletCalldata { reason: String },
    #[error("failed to sign simulation transaction: {reason}")]
    SignTransaction { reason: String },
    #[error("failed to encode signed simulation transaction: {reason}")]
    EncodeTransaction { reason: String },
}

#[cfg(test)]
mod tests {
    use groundhog_rpc_api_types::{hex::Hex0x, runtime_manifest::RuntimeManifest};
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::ContractAddress;
    use serde_json::json;

    use super::*;
    use crate::{actor::ActorState, nonce::NonceNamespace, runtime::SimAccount};

    #[test]
    fn parses_runtime_dev_wallet_entrypoint() {
        let entrypoint = runtime_dev_wallet_action_entrypoint(&runtime_manifest())
            .expect("entrypoint should parse");

        assert_eq!(entrypoint.as_ref(), b"doAction");
    }

    #[test]
    fn builds_wallet_submit_smoke_transaction_with_namespaced_nonce() {
        let mut actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x77; 32])).0,
            },
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );
        let entrypoint = runtime_dev_wallet_action_entrypoint(&runtime_manifest())
            .expect("entrypoint should parse");

        let tx = build_wallet_submit_smoke_transaction(&mut actor, entrypoint)
            .expect("transaction should build");

        assert_eq!(tx.nonce, 0x1234_5678_0000_0000);
        assert!(tx.tx_hex.starts_with("0x"));
        assert!(tx.tx_hex.len() > 2);
        assert_eq!(actor.next_nonce_sequence, 1);
    }

    fn runtime_manifest() -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x11),
            "producer": {
                "binary": "hogd-dev",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
                "indexer": "127.0.0.1:1001"
            },
            "bootstrapAccountDerivation": {
                "scheme": "groundhog-bootstrap-account-v1",
                "seed": "seed"
            },
            "bootstrapAccounts": [],
            "bootstrap": {
                "devWallet": {
                    "contentHash": prefixed_hex(0x33),
                    "deployedCodeHash": prefixed_hex(0x44),
                    "artifactHint": "./dev-wallet.wasm",
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
