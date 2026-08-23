//! Shared helpers for the dev-wallet contract wire format.

use std::path::Path;

use groundhog_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, NetworkHash, SHA256Hash};
use sha2::{Digest, Sha256};
use xdr_common::BytesM;

use crate::{
    BuildDevWalletDoActionTransaction, SignTransactionParams, Signer, SigningError,
    build_dev_wallet_do_action_transaction, encode_signed_transaction_xdr, sign_transaction,
    transaction_hash,
};

/// Number of fractional decimal places used by native HOG.
pub const NATIVE_HOG_DECIMALS: u32 = 7;

/// Number of raw units in one native HOG.
pub const NATIVE_HOG_SCALE: u128 = 10_000_000;

/// Canonical native-HOG contract address used on every network.
pub const NATIVE_HOG_ADDRESS: ContractAddress = ContractAddress::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
]);

/// Canonical native-HOG ERC-20 `transfer` entrypoint used on every network.
pub const NATIVE_HOG_TRANSFER_ENTRYPOINT: ContractEntrypointID =
    ContractEntrypointID::new([0x63, 0x28, 0x5b, 0x35, 0x23, 0xf9, 0xdc, 0x3f]);

/// Inputs for constructing and signing a native-HOG transfer through a dev wallet.
#[derive(Clone, Copy, Debug)]
pub struct BuildSignedDevWalletHogTransfer {
    /// Dev-wallet contract that owns the HOG being transferred.
    pub wallet_address: ContractAddress,
    /// Runtime-advertised dev-wallet `doAction` entrypoint.
    pub wallet_entrypoint: ContractEntrypointID,
    /// Address receiving native HOG.
    pub recipient: ContractAddress,
    /// Native amount in indivisible units.
    pub raw_amount: u128,
    /// Gas limit for both the wallet and inner transfer invocations.
    pub gas_limit: u64,
    /// Last block height at which the transaction may be included.
    pub block_expiry_height: i64,
    /// Transaction uniquifier nonce.
    pub nonce: u64,
    /// Signature annotation parameters.
    pub sign_params: SignTransactionParams,
    /// Network whose domain separator is bound into the signature.
    pub network_hash: NetworkHash,
}

/// Exact signed transaction bytes and their canonical inner-transaction hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedDevWalletHogTransfer {
    /// Canonical signed-transaction XDR bytes.
    pub bytes: Vec<u8>,
    /// SHA-256 hash of the unsigned inner transaction.
    pub tx_hash: SHA256Hash,
}

/// Encode native-HOG ERC-20 `transfer` calldata.
pub fn native_hog_transfer_calldata(
    recipient: &ContractAddress,
    raw_amount: u128,
) -> Result<Vec<u8>, SigningError> {
    xdrs::cxs::encode(&(recipient, raw_amount))
        .map_err(|error| SigningError::XdrEncode(error.to_string()))
}

/// Construct, sign, and encode one native-HOG transfer through a dev wallet.
pub fn build_signed_dev_wallet_hog_transfer(
    args: BuildSignedDevWalletHogTransfer,
    signer: &impl Signer,
) -> Result<SignedDevWalletHogTransfer, SigningError> {
    let transfer_calldata = native_hog_transfer_calldata(&args.recipient, args.raw_amount)?;
    let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
        wallet_address: args.wallet_address,
        wallet_entrypoint: args.wallet_entrypoint,
        target_address: NATIVE_HOG_ADDRESS,
        target_entrypoint: NATIVE_HOG_TRANSFER_ENTRYPOINT,
        target_calldata: &transfer_calldata,
        gas_limit: args.gas_limit,
        target_gas_limit: args.gas_limit,
        block_expiry_height: args.block_expiry_height,
        nonce: args.nonce,
    })?;
    let tx_hash = transaction_hash(&tx)?;
    let signed_tx = sign_transaction(tx, signer, args.sign_params, &args.network_hash)?;
    let bytes = encode_signed_transaction_xdr(&signed_tx)?;
    Ok(SignedDevWalletHogTransfer { bytes, tx_hash })
}

/// Encode the dev-wallet `create_wallet` calldata tuple.
pub fn create_wallet_calldata(
    deploy_nonce: &SHA256Hash,
    contract_hash: &SHA256Hash,
    owner: Ed25519PublicKey,
) -> Result<Vec<u8>, SigningError> {
    xdrs::cxs::encode(&(deploy_nonce, contract_hash, &owner))
        .map_err(|error| SigningError::XdrEncode(error.to_string()))
}

/// Build a dev-wallet contract value from an artifact path and bytes.
pub fn dev_wallet_contract(
    artifact_path: &Path,
    artifact_bytes: Vec<u8>,
) -> Result<Contract, SigningError> {
    match artifact_path
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rv32a") => Ok(Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(artifact_bytes),
            },
        }),
        _ => Err(SigningError::UnsupportedDevWalletArtifact(
            artifact_path.to_path_buf(),
        )),
    }
}

/// Compute the dev-wallet contract hash used for deterministic wallet creation.
pub fn wallet_contract_hash(contract: &Contract) -> Result<SHA256Hash, SigningError> {
    let contract_xdr =
        xdrs::txs::encode(contract).map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    let bytes: [u8; 32] = Sha256::digest(contract_xdr).into();
    Ok(SHA256Hash::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_TX_GAS_LIMIT, LocalSigner};

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
    fn signed_hog_transfer_retains_exact_bytes_and_canonical_hash() {
        let signer = LocalSigner::from_secret_key([0x44; 32]);
        let transfer = build_signed_dev_wallet_hog_transfer(
            BuildSignedDevWalletHogTransfer {
                wallet_address: ContractAddress::from([0x11; 32]),
                wallet_entrypoint: ContractEntrypointID::from([0x22; 8]),
                recipient: ContractAddress::from([0x33; 32]),
                raw_amount: 10 * NATIVE_HOG_SCALE,
                gas_limit: DEFAULT_TX_GAS_LIMIT,
                block_expiry_height: 1_007,
                nonce: 99,
                sign_params: SignTransactionParams::default(),
                network_hash: NetworkHash::from([0x55; 32]),
            },
            &signer,
        )
        .expect("bug: native HOG transfer should build and sign");
        let decoded: groundhog_xdr::transaction::SignedTransaction =
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
    fn native_hog_transfer_calldata_encodes_recipient_and_raw_amount() {
        let recipient = ContractAddress::from([0x55; 32]);
        let calldata = native_hog_transfer_calldata(&recipient, 123)
            .expect("bug: native HOG transfer calldata should encode");
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
}
