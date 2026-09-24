use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};

use groundhog_signing::{
    WalletCreateProofV2, wallet_create_proof_message, wallet_create_proof_message_v2,
};

#[test]
fn wallet_create_proof_v2_binds_client_nonce_and_wallet_address() {
    let proof = WalletCreateProofV2 {
        network_id: "devnet-preview",
        genesis_hash: SHA256Hash::from([0x11; 32]),
        faucet_wallet_address: ContractAddress::from([0x22; 32]),
        wallet_contract_hash: SHA256Hash::from([0x33; 32]),
        owner_public_key: Ed25519PublicKey::from([0x44; 32]),
        deploy_nonce: SHA256Hash::from([0x55; 32]),
        wallet_address: ContractAddress::from([0x66; 32]),
        client_nonce: &[0x77; 16],
    };

    let v2 = wallet_create_proof_message_v2(&proof).expect("bug: v2 proof should encode");
    let v1 = wallet_create_proof_message(
        SHA256Hash::from([0x11; 32]),
        ContractAddress::from([0x22; 32]),
        SHA256Hash::from([0x33; 32]),
        Ed25519PublicKey::from([0x44; 32]),
        SHA256Hash::from([0x55; 32]),
    )
    .expect("bug: v1 proof should encode");

    assert_ne!(v2, v1);
}
