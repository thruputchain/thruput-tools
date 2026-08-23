//! Canonical off-chain signing and transaction construction helpers for
//! Groundhog.

pub mod account;
pub mod build;
pub mod dev_account;
pub mod dev_wallet;
pub mod encode;
pub mod error;
pub mod hash;
pub mod indices;
pub mod local;
pub mod signer;

pub use account::{DEFAULT_PUBKEY_ACCOUNT_NONCE, PUBKEY_ACCOUNT_TAG_STR, PubkeyAccount};
pub use build::{
    BuildCallTransaction, BuildDeployTransaction, BuildDevWalletDoActionTransaction,
    DEFAULT_TX_GAS_LIMIT, SignTransactionParams, WALLET_DEPLOY_ENTRYPOINT, WalletCreateProofV2,
    build_call_transaction, build_deploy_transaction, build_dev_wallet_do_action_transaction,
    derive_deploy_address, dev_wallet_do_action_calldata, sign_and_encode_transaction_hex,
    sign_transaction, source_address_auth_claim, wallet_create_proof_message,
    wallet_create_proof_message_v2,
};
pub use dev_account::{
    DEFAULT_DEV_ACCOUNT_COUNT, DEFAULT_DEV_ACCOUNT_SEED, DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS,
    derive_default_dev_account_signer, derive_dev_account_signer,
};
pub use dev_wallet::{
    BuildSignedDevWalletHogTransfer, NATIVE_HOG_ADDRESS, NATIVE_HOG_DECIMALS, NATIVE_HOG_SCALE,
    NATIVE_HOG_TRANSFER_ENTRYPOINT, SignedDevWalletHogTransfer,
    build_signed_dev_wallet_hog_transfer, create_wallet_calldata, dev_wallet_contract,
    native_hog_transfer_calldata, wallet_contract_hash,
};
pub use encode::{encode_signed_transaction_xdr, encode_transaction_xdr};
pub use error::SigningError;
pub use hash::transaction_hash;
pub use indices::{AuthClaimIndex, PublicKeyId};
pub use local::LocalSigner;
pub use signer::Signer;

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_xdr::contract::ContractEntrypointID;
    use groundhog_xdr::transaction::SignedTransaction;
    use groundhog_xdr::types::{ContractAddress, NetworkHash, SHA256Hash};
    use sha2::{Digest, Sha256};

    // These pinned vectors currently come from the pre-refactor tx-building
    // output path. They are canonical XDR/hash vectors only; the signed
    // transaction is not auth-valid against empty hogd-dev genesis.
    // Replace or supplement them with runtime-generated canonical vectors when
    // those become available in a shared fixture source.

    fn test_signer() -> LocalSigner {
        let mut hasher = Sha256::new();
        hasher.update(b"test-wallet-seed");
        hasher.update(b":");
        hasher.update(b"test-account");
        let secret: [u8; 32] = hasher.finalize().into();
        LocalSigner::from_secret_key(secret)
    }

    // Non-zero, so a preimage that dropped the network hash would not still
    // produce this vector.
    fn test_network_hash() -> NetworkHash {
        NetworkHash::new(SHA256Hash::new([0x5a; 32]))
    }

    #[test]
    fn derives_expected_pubkey_account_address() {
        let signer = test_signer();
        let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);

        let address = account.address().expect("bug: address should derive");

        assert_eq!(
            hex::encode(address.as_ref()),
            "bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7"
        );
    }

    #[test]
    fn produces_expected_canonical_xdr_and_hash_vector() {
        let signer = test_signer();
        let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
        let tx = build_call_transaction(BuildCallTransaction {
            invoked_address: account.address().expect("bug: address should derive"),
            entrypoint: ContractEntrypointID::from([0u8; 8]),
            calldata: vec![],
            gas_limit: 1_000_000,
            block_expiry_height: 1_000_000,
            nonce: 1,
            auth_claim: account.auth_claim(),
        });

        let tx_hash = transaction_hash(&tx).expect("bug: tx hash should encode");
        let signed_tx = sign_transaction(
            tx,
            &signer,
            SignTransactionParams::default(),
            &test_network_hash(),
        )
        .expect("bug: transaction should sign");
        let signed_tx_xdr =
            encode_signed_transaction_xdr(&signed_tx).expect("bug: signed tx should encode");
        let decoded_signed_tx: SignedTransaction =
            xdrs::txs::decode(&signed_tx_xdr).expect("bug: signed tx should roundtrip through XDR");

        assert_eq!(
            hex::encode(tx_hash.as_ref()),
            "9dc7ad0fc4f74a43a62e047c0d6885c8e4df2d4840298ecd52cf157681157ed3"
        );
        assert_eq!(decoded_signed_tx, signed_tx);
        assert_eq!(
            hex::encode(signed_tx_xdr),
            "bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7000000000000000000000000000f42400000000000000000000f4240000000000000000100000000000000010000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a000000000000000000000001000000000000000057ca99d72f2f70aa272dc0053c44cbcfe01c2f9411ea13e10cf293cdf1b5c6cef17713c46502c7608512af519afe6a31736e2f4efa3eb6572b4de5df1cf5660f"
        );
    }

    #[test]
    fn signature_is_bound_to_one_network() {
        let signer = test_signer();
        let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
        let build = || {
            build_call_transaction(BuildCallTransaction {
                invoked_address: account.address().expect("bug: address should derive"),
                entrypoint: ContractEntrypointID::from([0u8; 8]),
                calldata: vec![],
                gas_limit: 1_000_000,
                block_expiry_height: 1_000_000,
                nonce: 1,
                auth_claim: account.auth_claim(),
            })
        };
        let other_network = NetworkHash::new(SHA256Hash::new([0xa5; 32]));

        let signed_here = sign_transaction(
            build(),
            &signer,
            SignTransactionParams::default(),
            &test_network_hash(),
        )
        .expect("bug: transaction should sign");
        let signed_elsewhere = sign_transaction(
            build(),
            &signer,
            SignTransactionParams::default(),
            &other_network,
        )
        .expect("bug: transaction should sign");

        // Same transaction, same signer, different network: the body and its
        // hash are identical and only the signature moves. A signer that reused
        // a stale network hash would produce the wrong one of these.
        assert_eq!(signed_here.tx, signed_elsewhere.tx);
        assert_eq!(
            transaction_hash(&signed_here.tx).expect("bug: tx hash should encode"),
            transaction_hash(&signed_elsewhere.tx).expect("bug: tx hash should encode")
        );
        assert_ne!(signed_here.signatures, signed_elsewhere.signatures);
    }

    #[test]
    fn builds_dev_wallet_do_action_transaction() {
        let wallet_address = ContractAddress::from([0x11; 32]);
        let target_address = ContractAddress::from([0x22; 32]);
        let wallet_entrypoint = ContractEntrypointID::from(*b"doAction");
        let target_entrypoint = ContractEntrypointID::from(*b"transfer");

        let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
            wallet_address,
            wallet_entrypoint,
            target_address,
            target_entrypoint,
            target_calldata: b"payload",
            gas_limit: 123,
            target_gas_limit: 124,
            block_expiry_height: 456,
            nonce: 789,
        })
        .expect("dev wallet transaction should build");

        assert_eq!(tx.invocation.invoked_address, wallet_address);
        assert_eq!(tx.invocation.entrypoint, wallet_entrypoint);
        assert_eq!(tx.invocation.gas_limit, 123);
        assert_eq!(*tx.uniquifier.block_expiry_height, 456);
        assert_eq!(tx.uniquifier.nonce, 789);
        assert!(!tx.invocation.calldata.0.is_empty());
    }
}
