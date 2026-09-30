use account::DEFAULT_PUBKEY_ACCOUNT_NONCE;
use account::PubkeyAccount;
use build::BuildCallTransaction;
use build::BuildDevWalletDoActionTransaction;
use build::SignTransactionParams;
use build::build_call_transaction;
use build::build_dev_wallet_do_action_transaction;
use build::sign_transaction;
use encode::encode_signed_transaction_xdr;
use hash::transaction_hash;
use local::LocalSigner;

use groundhog_signing::*;
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

    let address = account.address();

    assert_eq!(
        hex::encode(address.as_ref()),
        "8b1568c894aeb3a704ec0dcd27fe18a4d40193ffa187edadc181b1660286b066"
    );
}

#[test]
fn produces_expected_canonical_xdr_and_hash_vector() {
    let signer = test_signer();
    let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
    let tx = build_call_transaction(BuildCallTransaction {
        invoked_address: account.address(),
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
        "847d86e85273a697f265224e249d9fe417634e0f8d02452d2e616fc6b885408f"
    );
    assert_eq!(decoded_signed_tx, signed_tx);
    assert_eq!(
        hex::encode(signed_tx_xdr),
        "8b1568c894aeb3a704ec0dcd27fe18a4d40193ffa187edadc181b1660286b066000000000000000000000000000f42400000000000000000000f4240000000000000000100000000000000010000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a0000000000000000000000010000000000000000ae806c98773715b9c7cf95d3cf60f53f89469140d4382eab35d17bbb755f8cfb0c6f677584482db6256f5cd1728e27e430c106f862c8e11b6a4a6c3bec7c6306"
    );
}

#[test]
fn signature_is_bound_to_one_network() {
    let signer = test_signer();
    let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
    let build = || {
        build_call_transaction(BuildCallTransaction {
            invoked_address: account.address(),
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
