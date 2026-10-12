//! Shared helpers for the dev-wallet contract wire format.

use std::path::Path;

use sha2::{Digest, Sha256};
use thruput_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
use thruput_xdr::types::{ContractAddress, Ed25519PublicKey, NetworkHash, SHA256Hash};
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
#[derive(Clone, Debug)]
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
    args: &BuildSignedDevWalletHogTransfer,
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
