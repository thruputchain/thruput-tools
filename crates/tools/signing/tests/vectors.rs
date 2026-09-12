//! Known-answer vectors for address derivation, signing, and
//! transaction hashing.
//!
//! Address constants come from the literal CXS preimage specified by hogd.
//! Historical wire/hash fixtures are kept separate from address derivation.
//! Changes to either affect on-chain identity or wire hashes.

use ed25519_dalek::{Verifier, VerifyingKey};
use groundhog_signing::{
    BuildCallTransaction, DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount,
    SignTransactionParams, Signer, build_call_transaction, encode_signed_transaction_xdr,
    sign_transaction, transaction_hash,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::{ContractAddress, NetworkHash, SHA256Hash, Signature};

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

    let address_nonce0 = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE)
        .address()
        .expect("bug: nonce-0 address should derive");
    let address_nonce1 = PubkeyAccount::new(signer.public_key(), 1)
        .address()
        .expect("bug: nonce-1 address should derive");

    // SHA256 of the literal CXS prefix 00 0d "PubkeyAccount", followed by
    // 00, the 32-byte public key, and the single-byte nonce (0 or 1).
    // This is the derivation used by hogd/crypto/domain_separators.h.
    assert_eq!(
        hex::encode(address_nonce0.as_ref()),
        "aea3c931b977bf8ef933cc134e372d0efbf4bac0e576cc117d52108c80ea3d78"
    );
    assert_eq!(
        hex::encode(address_nonce1.as_ref()),
        "564859b1c9b7afe948a57854d8c6b45425ca1225a2170799a5a03ecfd139daef"
    );
    assert_ne!(address_nonce0, address_nonce1);
}

#[test]
fn local_signer_signatures_verify_and_are_deterministic() {
    let signer = test_signer();
    let message = b"groundhog signing roundtrip";

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
        // Preserve the old wire-hash fixture; address derivation is tested above.
        invoked_address: ContractAddress::from(
            <[u8; 32]>::try_from(
                hex::decode("a240483ed9311b2394e4afc167e046fdbc83b3710ad9664287d50f083c27f677")
                    .expect("valid historical address hex"),
            )
            .expect("valid historical address hex"),
        ),
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
        "020639a7fc64b4275b97250ecf08cc6bd8c67f8739a46ce88bdfa8795a93acc6"
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
    let validated = groundhog_json_rpc_server::tx::validate_raw_transaction_bytes(
        signed_xdr,
        &groundhog_tx_validation::NetworkConfigParams::default(),
    )
    .expect("bug: signed tx should validate server-side");

    assert_eq!(validated.hash.0.0.as_ref(), tx_hash.as_ref());
    assert_eq!(*validated.block_expiry_height, 2_000_000);
}
