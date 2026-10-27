//! Known-answer vectors for address derivation, signing, and
//! transaction hashing.
//!
//! The address constants are computed from the CXS preimage hogd specifies,
//! so they check this crate against the node rather than against itself. The
//! hash constants pin today's encoding against regression. A change that
//! breaks any of them alters on-chain identity or wire hashes, so treat it as
//! a consensus-affecting bug, not a test update.

use ed25519_dalek::{Verifier, VerifyingKey};
use thruput_signing::{
    BuildCallTransaction, DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount,
    SignTransactionParams, Signer, build_call_transaction, encode_signed_transaction_xdr,
    sign_transaction, transaction_hash,
};
use thruput_xdr::contract::ContractEntrypointID;
use thruput_xdr::types::{NetworkHash, SHA256Hash, Signature};

/// Fixed secret so the derived addresses below are stable.
const TEST_SECRET: [u8; 32] = [0x42; 32];

fn test_signer() -> LocalSigner {
    LocalSigner::from_secret_key(TEST_SECRET)
}

fn test_network_hash() -> NetworkHash {
    NetworkHash::new(SHA256Hash::new([0x5a; 32]))
}

#[test]
fn address_derivation_known_answers_at_two_nonces() {
    let signer = test_signer();

    let address_nonce0 =
        PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE).address();
    let address_nonce1 = PubkeyAccount::new(signer.public_key(), 1).address();

    // SHA256 of the literal CXS prefix 00 0d "PubkeyAccount", followed by the
    // 32-byte public key hash and the single-byte nonce (0 or 1). The key hash
    // is SHA256 of 00 09 "PublicKey", 00 (the Ed25519 scheme), and the key.
    // This is the derivation used by hogd/crypto/domain_separators.h.
    assert_eq!(
        hex::encode(address_nonce0.as_ref()),
        "a883232ca9d3061da35558a7f435575d33a13374525e2168df77f46988c01f62"
    );
    assert_eq!(
        hex::encode(address_nonce1.as_ref()),
        "3bbc9340a960e8cbb33ff18d046d2386c8f8ffeadb4beab916a4c2329a7ece0a"
    );
    assert_ne!(address_nonce0, address_nonce1);
}

#[test]
fn local_signer_signatures_verify_and_are_deterministic() {
    let signer = test_signer();
    let message = b"thruput signing roundtrip";

    let Signature::Ed25519 { ed25519: signature } =
        signer.sign(message).expect("bug: local signer should sign")
    else {
        panic!("bug: LocalSigner should produce an Ed25519 signature");
    };

    let verifying_key = VerifyingKey::from_bytes(&signer.verifying_key_bytes())
        .expect("bug: verifying key bytes should be a valid Ed25519 point");
    let signature_bytes: &[u8; 64] = signature
        .as_ref()
        .try_into()
        .expect("bug: Ed25519 signature should be 64 bytes");
    let dalek_signature = ed25519_dalek::Signature::from_bytes(signature_bytes);
    verifying_key
        .verify(message, &dalek_signature)
        .expect("bug: signature should verify against the signer's public key");
    verifying_key
        .verify(b"another message", &dalek_signature)
        .expect_err("bug: signature must not verify a different message");

    // Ed25519 signing is deterministic: same key + message = same bytes.
    let Signature::Ed25519 { ed25519: again } =
        signer.sign(message).expect("bug: local signer should sign")
    else {
        panic!("bug: LocalSigner should produce an Ed25519 signature");
    };
    assert_eq!(signature, again);
}

#[test]
fn transaction_hash_vector_matches_the_server_side_hash() {
    let signer = test_signer();
    let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
    let tx = build_call_transaction(BuildCallTransaction {
        invoked_address: account.address(),
        entrypoint: ContractEntrypointID::from(*b"getvalue"),
        calldata: vec![0x01, 0x02],
        gas_limit: 500_000,
        block_expiry_height: 2_000_000,
        nonce: 7,
        auth_claim: account.auth_claim(),
    });

    let tx_hash = transaction_hash(&tx).expect("bug: tx hash should encode");

    // Generated from the current implementation; pins the canonical
    // inner-transaction hash preimage.
    assert_eq!(
        hex::encode(tx_hash.as_ref()),
        "bb473d8d146ac91c288d1509bd40aaef0748094d7880ce4153ae47bf1e5d8b65"
    );

    // Cross-check against the gateway-side hash: the hash the server
    // derives from raw signed-transaction bytes must equal the hash the
    // signing crate computes before signing.
    let signed_tx = sign_transaction(
        tx,
        &signer,
        SignTransactionParams::default(),
        &test_network_hash(),
    )
    .expect("bug: transaction should sign");
    let signed_xdr =
        encode_signed_transaction_xdr(&signed_tx).expect("bug: signed tx should encode");
    let validated = thruput_json_rpc_server::tx::validate_raw_transaction_bytes(
        signed_xdr,
        &thruput_tx_validation::NetworkConfigParams::default(),
    )
    .expect("bug: signed tx should validate server-side");

    assert_eq!(validated.hash.0.0.as_ref(), tx_hash.as_ref());
    assert_eq!(*validated.block_expiry_height, 2_000_000);
}
