//! Shared helpers for e2e tests.

use groundhog_rpc_api_types::dev::DevAccount;
use groundhog_rpc_api_types::transaction::TxHash;
use groundhog_signing::{
    BuildCallTransaction, LocalSigner, PubkeyAccount, SignTransactionParams,
    build_call_transaction, encode_signed_transaction_xdr, sign_transaction, transaction_hash,
    DEFAULT_PUBKEY_ACCOUNT_NONCE,
};
use groundhog_xdr::contract::ContractEntrypointID;

/// RPC URL, overridable via env var.
pub fn rpc_url() -> String {
    std::env::var("RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:9545".to_string())
}

/// Build and sign a no-op transaction from a dev account.
/// Returns (hex_encoded_tx, expected_tx_hash_hex).
pub fn build_test_tx(account: &DevAccount) -> (String, String) {
    let secret: [u8; 32] = account
        .private_key
        .0
        .as_slice()
        .try_into()
        .expect("bug: private key should be 32 bytes");
    let signer = LocalSigner::from_secret_key(secret);
    let pubkey_account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);

    let tx = build_call_transaction(BuildCallTransaction {
        invoked_address: pubkey_account
            .address()
            .expect("bug: address should derive"),
        entrypoint: ContractEntrypointID::from([0u8; 8]),
        calldata: vec![],
        gas_limit: 10_000_000,
        block_expiry_height: 1_000_000,
        nonce: 1,
        auth_claims: vec![pubkey_account.auth_claim()]
            .try_into()
            .expect("bug: auth claims should fit"),
    });

    let tx_hash = transaction_hash(&tx).expect("bug: tx hash should encode");
    let signed_tx = sign_transaction(tx, &signer, SignTransactionParams::default())
        .expect("bug: transaction should sign");
    let xdr_bytes =
        encode_signed_transaction_xdr(&signed_tx).expect("bug: signed tx should encode");

    let tx_hex = format!("0x{}", hex::encode(&xdr_bytes));
    let hash_hex = format!("0x{}", hex::encode(tx_hash.as_ref()));

    (tx_hex, hash_hex)
}
