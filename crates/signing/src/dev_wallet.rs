//! Shared helpers for the dev-wallet contract wire format.

use sha2::{Digest, Sha256};
use thruput_xdr::contract::{Contract, ContractEntrypointID};
use thruput_xdr::types::{ContractAddress, SHA256Hash};

use crate::SigningError;

/// Number of fractional decimal places used by the native token.
pub const NATIVE_TOKEN_DECIMALS: u32 = 7;

/// Number of raw units in one native token.
pub const NATIVE_TOKEN_SCALE: u128 = 10_000_000;

/// Canonical native-token ERC-20 `transfer` entrypoint used on every network.
pub const NATIVE_TOKEN_TRANSFER_ENTRYPOINT: ContractEntrypointID =
    ContractEntrypointID::new([0x63, 0x28, 0x5b, 0x35, 0x23, 0xf9, 0xdc, 0x3f]);

/// Encode native-token ERC-20 `transfer` calldata.
pub fn native_token_transfer_calldata(
    recipient: &ContractAddress,
    raw_amount: u128,
) -> Result<Vec<u8>, SigningError> {
    xdrs::cxs::encode(&(recipient, raw_amount))
        .map_err(|error| SigningError::XdrEncode(error.to_string()))
}

/// Compute the dev-wallet contract hash used for deterministic wallet creation.
pub fn wallet_contract_hash(contract: &Contract) -> Result<SHA256Hash, SigningError> {
    let contract_xdr =
        xdrs::txs::encode(contract).map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    let bytes: [u8; 32] = Sha256::digest(contract_xdr).into();
    Ok(SHA256Hash::from(bytes))
}
