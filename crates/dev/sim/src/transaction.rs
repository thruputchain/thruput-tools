use groundhog_json_rpc_types::runtime_manifest::{RuntimeManifest, RuntimeManifestEntrypointError};
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, SignTransactionParams,
    build_dev_wallet_do_action_transaction, sign_and_encode_transaction_hex,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::ContractAddress;

use crate::actor::{ActorState, WalletKind};

// TODO: Lift smoke gas and expiry into the simulation manifest once workload
// parameters are schema-backed.
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
    manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|source| TransactionBuildError::Entrypoint { source })
}

pub fn build_wallet_submit_smoke_transaction(
    actor: &ActorState,
    wallet_entrypoint: ContractEntrypointID,
) -> Result<BuiltSubmitTransaction, TransactionBuildError> {
    if actor.wallet_kind == WalletKind::NativePubkey {
        let calldata = xdrs::cxs::encode(&(actor.wallet_address,)).map_err(|error| {
            TransactionBuildError::BuildWalletAction {
                source: groundhog_signing::SigningError::XdrEncode(error.to_string()),
            }
        })?;
        return build_contract_call_transaction(
            actor,
            crate::native::EXECUTE,
            groundhog_signing::NATIVE_HOG_ADDRESS,
            crate::native::BALANCE,
            &calldata,
        );
    }
    // Legacy local smoke retains its admission-only self-call semantics.
    build_contract_call_transaction(
        actor,
        wallet_entrypoint,
        actor.wallet_address,
        wallet_entrypoint,
        &[],
    )
}

/// Build a signed dev-wallet transaction that calls a deployed contract.
///
/// The actor nonce is read but not advanced here. The executor advances actor
/// state only after submit semantics determine whether the transaction should
/// count as submitted.
pub fn build_contract_call_transaction(
    actor: &ActorState,
    wallet_entrypoint: ContractEntrypointID,
    target_address: ContractAddress,
    target_entrypoint: ContractEntrypointID,
    target_calldata: &[u8],
) -> Result<BuiltSubmitTransaction, TransactionBuildError> {
    let nonce = actor.current_nonce()?;
    let tx = if actor.wallet_kind == WalletKind::NativePubkey {
        crate::native::execute(
            actor,
            target_address,
            target_entrypoint,
            target_calldata,
            nonce,
            actor
                .native_chain_tip
                .ok_or(crate::actor::ActorError::NativeTipUnavailable)?
                .expiry()?,
        )
    } else {
        build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
            wallet_address: actor.wallet_address,
            wallet_entrypoint,
            target_address,
            target_entrypoint,
            target_calldata,
            gas_limit: DEV_WALLET_SMOKE_GAS_LIMIT,
            target_gas_limit: DEV_WALLET_SMOKE_GAS_LIMIT,
            block_expiry_height: DEV_WALLET_SMOKE_EXPIRY_HEIGHT,
            nonce,
        })
    }
    .map_err(|source| TransactionBuildError::BuildWalletAction { source })?;
    let tx_hex = sign_and_encode_transaction_hex(
        tx,
        &actor.signer,
        SignTransactionParams::default(),
        &actor.network_hash,
    )
    .map_err(|source| TransactionBuildError::SignAndEncode { source })?;

    Ok(BuiltSubmitTransaction { tx_hex, nonce })
}

#[derive(Debug, thiserror::Error)]
pub enum TransactionBuildError {
    #[error(transparent)]
    Actor(#[from] crate::actor::ActorError),
    #[error("failed to parse runtime manifest devWallet.doActionEntrypoint: {source}")]
    Entrypoint {
        #[source]
        source: RuntimeManifestEntrypointError,
    },
    #[error("failed to build wallet action transaction: {source}")]
    BuildWalletAction {
        #[source]
        source: groundhog_signing::SigningError,
    },
    #[error("failed to sign and encode simulation transaction: {source}")]
    SignAndEncode {
        #[source]
        source: groundhog_signing::SigningError,
    },
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::{hex::Hex0x, runtime_manifest::RuntimeManifest};
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::ContractAddress;
    use serde_json::json;

    use super::*;
    use crate::{actor::ActorState, nonce::NonceNamespace, runtime::SimAccount};

    #[test]
    fn native_transactions_use_live_expiry_and_reject_missing_stale_or_invalid_tip() {
        let mut actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("expiry-test", 0),
                wallet_address: ContractAddress::from([0; 32]),
                network_hash: groundhog_xdr::types::NetworkHash::new(
                    groundhog_xdr::types::SHA256Hash::new([0; 32]),
                ),
            },
            NonceNamespace::new(1).expect("namespace"),
        );
        actor.use_native_wallet();
        assert!(build_wallet_submit_smoke_transaction(&actor, crate::native::EXECUTE).is_err());
        actor.native_chain_tip = Some(crate::actor::NativeChainTip {
            height: 20_000_000,
            observed_at: std::time::Instant::now(),
        });
        let smoke = build_wallet_submit_smoke_transaction(&actor, crate::native::EXECUTE)
            .expect("native smoke");
        let call = build_contract_call_transaction(
            &actor,
            crate::native::EXECUTE,
            actor.wallet_address,
            crate::native::BALANCE,
            &[],
        )
        .expect("native call");
        for built in [smoke, call] {
            let bytes = hex::decode(built.tx_hex.trim_start_matches("0x")).expect("hex");
            let signed: groundhog_xdr::transaction::SignedTransaction =
                xdrs::txs::decode(&bytes).expect("signed tx");
            assert_eq!(signed.tx.uniquifier.block_expiry_height, 20_001_000.into());
        }
        for (height, age) in [
            (20_000_000, crate::actor::NATIVE_TIP_MAX_AGE),
            (i64::MAX, std::time::Duration::ZERO),
            (-1, std::time::Duration::ZERO),
        ] {
            actor.native_chain_tip = Some(crate::actor::NativeChainTip {
                height,
                observed_at: std::time::Instant::now()
                    .checked_sub(age)
                    .expect("test clock age"),
            });
            assert!(build_wallet_submit_smoke_transaction(&actor, crate::native::EXECUTE).is_err());
        }
    }

    #[test]
    fn parses_runtime_dev_wallet_entrypoint() {
        let entrypoint = runtime_dev_wallet_action_entrypoint(&runtime_manifest())
            .expect("entrypoint should parse");

        assert_eq!(entrypoint.as_ref(), b"doAction");
    }

    #[test]
    fn builds_wallet_submit_smoke_transaction_with_namespaced_nonce() {
        let actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x77; 32])).0,
                network_hash: groundhog_xdr::types::NetworkHash::new(
                    groundhog_xdr::types::SHA256Hash::new([0x5a; 32]),
                ),
            },
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );
        let entrypoint = runtime_dev_wallet_action_entrypoint(&runtime_manifest())
            .expect("entrypoint should parse");

        let tx = build_wallet_submit_smoke_transaction(&actor, entrypoint)
            .expect("transaction should build");

        assert_eq!(tx.nonce, 0x1234_5678_0000_0000);
        assert!(tx.tx_hex.starts_with("0x"));
        assert!(tx.tx_hex.len() > 2);
        assert_eq!(actor.next_nonce_sequence, 0);
    }

    #[test]
    fn wallet_submit_smoke_transaction_is_byte_equal_for_same_inputs() {
        let actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x77; 32])).0,
                network_hash: groundhog_xdr::types::NetworkHash::new(
                    groundhog_xdr::types::SHA256Hash::new([0x5a; 32]),
                ),
            },
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );
        let entrypoint = runtime_dev_wallet_action_entrypoint(&runtime_manifest())
            .expect("entrypoint should parse");

        let first = build_wallet_submit_smoke_transaction(&actor, entrypoint)
            .expect("first transaction should build");
        let second = build_wallet_submit_smoke_transaction(&actor, entrypoint)
            .expect("second transaction should build");

        assert_eq!(first.tx_hex, second.tx_hex);
        assert_eq!(first.nonce, second.nonce);
    }

    #[test]
    fn builds_contract_call_transaction_against_target_contract() {
        let actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x77; 32])).0,
                network_hash: groundhog_xdr::types::NetworkHash::new(
                    groundhog_xdr::types::SHA256Hash::new([0x5a; 32]),
                ),
            },
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );
        let wallet_entrypoint = runtime_dev_wallet_action_entrypoint(&runtime_manifest())
            .expect("entrypoint should parse");

        let tx = build_contract_call_transaction(
            &actor,
            wallet_entrypoint,
            ContractAddress::from([0x88; 32]),
            groundhog_xdr::contract::ContractEntrypointID::from([0x11; 8]),
            &[0x2a, 0x64],
        )
        .expect("transaction should build");

        assert_eq!(tx.nonce, 0x1234_5678_0000_0000);
        assert!(tx.tx_hex.starts_with("0x"));
        assert!(tx.tx_hex.len() > 2);
        assert_eq!(actor.next_nonce_sequence, 0);
    }

    fn runtime_manifest() -> RuntimeManifest {
        serde_json::from_value(json!({
            "schemaVersion": 2,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test00000000000",
            "producer": {
                "binary": "hog-devnode",
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
                    "artifactHint": "./dev_wallet.rv32a",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "createWalletEntrypoint": "0x6372656174655761",
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
