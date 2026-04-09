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
    BuildCallTransaction, BuildDeployTransaction, SignTransactionParams, build_call_transaction,
    build_deploy_transaction, sign_transaction,
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
    // output path. Replace or supplement them with runtime-generated
    // canonical vectors when those become available in a shared fixture
    // source.

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
    fn produces_expected_signed_xdr_and_hash() {
        let signer = test_signer();
        let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
        let tx = build_call_transaction(BuildCallTransaction {
            invoked_address: account.address().expect("bug: address should derive"),
            entrypoint: ContractEntrypointID::from([0u8; 8]),
            calldata: vec![],
            gas_limit: 1_000_000,
            block_expiry_height: 1_000_000,
            nonce: 1,
            auth_claims: vec![account.auth_claim()]
                .try_into()
                .expect("bug: test auth claims are within bounds"),
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
            "6e94695b6af0fbc13d96876f7fb8e940cd9db4d247e53614a45da252b3a7853a"
        );
        assert_eq!(decoded_signed_tx, signed_tx);
        assert_eq!(
            hex::encode(signed_tx_xdr),
            "bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001000000040000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a00000000000000000000000000000001000000000000000000000000706f58acd087550f315030db4beeb1527e86f0e12d9781957cdcc522f33e5d719b93c200381cff5bfb26a1ee8ce7874ffd485322cd5a0806605d2ffa51c8cf0f"
        );
    }
}
