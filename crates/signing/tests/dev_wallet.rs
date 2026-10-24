use std::path::Path;
use thruput_signing::SignTransactionParams;
use thruput_signing::SigningError;
use thruput_signing::transaction_hash;
use thruput_xdr::contract::Contract;
use thruput_xdr::contract::ContractEntrypointID;
use thruput_xdr::types::ContractAddress;
use thruput_xdr::types::Ed25519PublicKey;
use thruput_xdr::types::NetworkHash;
use thruput_xdr::types::SHA256Hash;

use thruput_signing::dev_wallet::*;
use thruput_signing::{DEFAULT_TX_GAS_LIMIT, LocalSigner};

#[test]
fn create_wallet_calldata_round_trips_expected_tuple() {
    let nonce = SHA256Hash::from([0x11; 32]);
    let contract_hash = SHA256Hash::from([0x22; 32]);
    let owner = Ed25519PublicKey::from([0x33; 32]);

    let encoded = create_wallet_calldata(&nonce, &contract_hash, owner)
        .expect("bug: create_wallet calldata should encode");
    let decoded: (SHA256Hash, SHA256Hash, Ed25519PublicKey) =
        xdrs::cxs::decode(&encoded).expect("bug: create_wallet calldata should decode");

    assert_eq!(decoded, (nonce, contract_hash, owner));
}

#[test]
fn signed_native_token_transfer_retains_exact_bytes_and_canonical_hash() {
    let signer = LocalSigner::from_secret_key([0x44; 32]);
    let transfer = build_signed_dev_wallet_native_token_transfer(
        &BuildSignedDevWalletNativeTokenTransfer {
            wallet_address: ContractAddress::from([0x11; 32]),
            wallet_entrypoint: ContractEntrypointID::from([0x22; 8]),
            recipient: ContractAddress::from([0x33; 32]),
            raw_amount: 10 * NATIVE_TOKEN_SCALE,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: 1_007,
            nonce: 99,
            sign_params: SignTransactionParams::default(),
            network_hash: NetworkHash::from(SHA256Hash::from([0x55; 32])),
        },
        &signer,
    )
    .expect("bug: native token transfer should build and sign");
    let decoded: thruput_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&transfer.bytes).expect("bug: signed transfer bytes should decode");

    assert_eq!(
        decoded.tx.invocation.invoked_address,
        ContractAddress::from([0x11; 32])
    );
    assert_eq!(
        transaction_hash(&decoded.tx).expect("bug: decoded transaction should hash"),
        transfer.tx_hash
    );
}

#[test]
fn native_token_transfer_calldata_encodes_recipient_and_raw_amount() {
    let recipient = ContractAddress::from([0x55; 32]);
    let calldata = native_token_transfer_calldata(&recipient, 123)
        .expect("bug: native token transfer calldata should encode");
    let decoded: (ContractAddress, u128) =
        xdrs::cxs::decode(&calldata).expect("bug: transfer calldata should decode");

    assert_eq!(decoded, (recipient, 123));
}

#[test]
fn dev_wallet_contract_uses_rv32_variant_for_rv32a_artifacts() {
    let contract = dev_wallet_contract(Path::new("dev_wallet.rv32a"), vec![1, 2, 3])
        .expect("bug: RV32 dev-wallet artifact should be accepted");

    assert!(matches!(contract, Contract::kRv32 { .. }));
}

fn assert_rejected_artifact(path: &Path) {
    let error = dev_wallet_contract(path, vec![1, 2, 3]).unwrap_err();

    assert!(matches!(
        error,
        SigningError::UnsupportedDevWalletArtifact(error_path) if error_path == path
    ));
}

#[test]
fn dev_wallet_contract_rejects_unknown_or_missing_artifact_extension() {
    assert_rejected_artifact(Path::new("contracts/dev_wallet.bin"));
    assert_rejected_artifact(Path::new("contracts/dev_wallet"));
}

#[cfg(unix)]
#[test]
fn dev_wallet_contract_rejects_non_utf8_artifact_extension() {
    use std::os::unix::ffi::OsStringExt;

    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(
        b"contracts/dev_wallet.\xff".to_vec(),
    ));
    assert_rejected_artifact(&path);
}
