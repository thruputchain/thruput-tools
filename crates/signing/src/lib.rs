//! Canonical off-chain signing and transaction construction helpers for
//! Groundhog.

pub mod account;
pub mod build;
pub mod encode;
pub mod error;
pub mod hash;
pub mod indices;
pub mod local;
pub mod signer;

pub use account::{DEFAULT_PUBKEY_ACCOUNT_NONCE, PUBKEY_ACCOUNT_TAG_STR, PubkeyAccount};
pub use build::{
    BuildCallTransaction, BuildDeployTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams,
    WALLET_DEPLOY_ENTRYPOINT, build_call_transaction, build_deploy_transaction,
    derive_deploy_address, sign_transaction, source_address_auth_claim,
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
    use sha2::{Digest, Sha256};

    // These pinned vectors currently come from the pre-refactor tx-builder
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
        let signed_tx = sign_transaction(tx, &signer, SignTransactionParams::default())
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
            "bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7000000000000000000000000000f42400000000000000000000f4240000000000000000100000000000000010000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a000000000000000000000001000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695031c9d3fe2554e5ce62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d"
        );
    }
}
