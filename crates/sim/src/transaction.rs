use thruput_signing::{SignTransactionParams, sign_and_encode_transaction_hex};
use thruput_xdr::contract::ContractEntrypointID;
use thruput_xdr::types::ContractAddress;

use crate::actor::ActorState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltSubmitTransaction {
    pub tx_hex: String,
    pub nonce: u64,
}

pub fn build_wallet_submit_smoke_transaction(
    actor: &ActorState,
) -> Result<BuiltSubmitTransaction, TransactionBuildError> {
    let calldata = xdrs::cxs::encode(&(actor.wallet_address,)).map_err(|error| {
        TransactionBuildError::BuildWalletAction {
            source: thruput_signing::SigningError::XdrEncode(error.to_string()),
        }
    })?;
    build_contract_call_transaction(
        actor,
        thruput_signing::NATIVE_HOG_ADDRESS,
        crate::native::BALANCE,
        &calldata,
    )
}

/// Build a signed native-wallet transaction that calls a deployed contract.
///
/// The actor nonce is read but not advanced here. The executor advances actor
/// state only after submit semantics determine whether the transaction should
/// count as submitted.
pub fn build_contract_call_transaction(
    actor: &ActorState,
    target_address: ContractAddress,
    target_entrypoint: ContractEntrypointID,
    target_calldata: &[u8],
) -> Result<BuiltSubmitTransaction, TransactionBuildError> {
    let nonce = actor.current_nonce()?;
    let tx = crate::native::execute(
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
    #[error("failed to build wallet action transaction: {source}")]
    BuildWalletAction {
        #[source]
        source: thruput_signing::SigningError,
    },
    #[error("failed to sign and encode simulation transaction: {source}")]
    SignAndEncode {
        #[source]
        source: thruput_signing::SigningError,
    },
}

#[cfg(test)]
mod tests {
    use thruput_json_rpc_types::hex::Hex0x;
    use thruput_signing::derive_dev_account_signer;
    use thruput_xdr::types::ContractAddress;

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
                network_hash: thruput_xdr::types::NetworkHash::new(
                    thruput_xdr::types::SHA256Hash::new([0; 32]),
                ),
            },
            NonceNamespace::new(1).expect("namespace"),
        );
        actor.use_native_wallet();
        assert!(build_wallet_submit_smoke_transaction(&actor).is_err());
        actor.native_chain_tip = Some(crate::actor::NativeChainTip {
            height: 20_000_000,
            observed_at: std::time::Instant::now(),
        });
        let smoke = build_wallet_submit_smoke_transaction(&actor).expect("native smoke");
        let call = build_contract_call_transaction(
            &actor,
            actor.wallet_address,
            crate::native::BALANCE,
            &[],
        )
        .expect("native call");
        for built in [smoke, call] {
            let bytes = hex::decode(built.tx_hex.trim_start_matches("0x")).expect("hex");
            let signed: thruput_xdr::transaction::SignedTransaction =
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
            assert!(build_wallet_submit_smoke_transaction(&actor).is_err());
        }
    }

    #[test]
    fn builds_wallet_submit_smoke_transaction_with_namespaced_nonce() {
        let mut actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x77; 32])).0,
                network_hash: thruput_xdr::types::NetworkHash::new(
                    thruput_xdr::types::SHA256Hash::new([0x5a; 32]),
                ),
            },
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );

        actor.use_native_wallet();
        actor.native_chain_tip = Some(crate::actor::NativeChainTip {
            height: 42,
            observed_at: std::time::Instant::now(),
        });
        let tx = build_wallet_submit_smoke_transaction(&actor).expect("transaction should build");

        assert_eq!(tx.nonce, 0x1234_5678_0000_0000);
        assert!(tx.tx_hex.starts_with("0x"));
        assert!(tx.tx_hex.len() > 2);
        assert_eq!(actor.next_nonce_sequence, 0);
    }

    #[test]
    fn wallet_submit_smoke_transaction_is_byte_equal_for_same_inputs() {
        let mut actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x77; 32])).0,
                network_hash: thruput_xdr::types::NetworkHash::new(
                    thruput_xdr::types::SHA256Hash::new([0x5a; 32]),
                ),
            },
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );

        actor.use_native_wallet();
        actor.native_chain_tip = Some(crate::actor::NativeChainTip {
            height: 42,
            observed_at: std::time::Instant::now(),
        });
        let first =
            build_wallet_submit_smoke_transaction(&actor).expect("first transaction should build");
        let second =
            build_wallet_submit_smoke_transaction(&actor).expect("second transaction should build");

        assert_eq!(first.tx_hex, second.tx_hex);
        assert_eq!(first.nonce, second.nonce);
    }

    #[test]
    fn builds_contract_call_transaction_against_target_contract() {
        let mut actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer: derive_dev_account_signer("seed", 0),
                wallet_address: Hex0x(ContractAddress::from([0x77; 32])).0,
                network_hash: thruput_xdr::types::NetworkHash::new(
                    thruput_xdr::types::SHA256Hash::new([0x5a; 32]),
                ),
            },
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );

        actor.use_native_wallet();
        actor.native_chain_tip = Some(crate::actor::NativeChainTip {
            height: 42,
            observed_at: std::time::Instant::now(),
        });
        let tx = build_contract_call_transaction(
            &actor,
            ContractAddress::from([0x88; 32]),
            thruput_xdr::contract::ContractEntrypointID::from([0x11; 8]),
            &[0x2a, 0x64],
        )
        .expect("transaction should build");

        assert_eq!(tx.nonce, 0x1234_5678_0000_0000);
        assert!(tx.tx_hex.starts_with("0x"));
        assert!(tx.tx_hex.len() > 2);
        assert_eq!(actor.next_nonce_sequence, 0);
    }
}
