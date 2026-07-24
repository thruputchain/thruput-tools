use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::ContractAddress;

use crate::CliError;

// TODO: Load local native HOG metadata from the runtime manifest. Hosted
// transaction construction already uses the Devnet /v1/network descriptor.
pub(crate) const DECIMALS: u32 = 7;
pub(crate) const SCALE: u128 = 10_000_000;
pub(crate) const TOKEN_ADDRESS: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000001";

pub(crate) fn transfer_entrypoint() -> ContractEntrypointID {
    let selector = contract_entrypoint_id(
        Some("ERC20"),
        "0.1.0",
        &NormalizedMethodSignature::new("transfer"),
    );
    let bytes: [u8; 8] = selector
        .as_ref()
        .try_into()
        .expect("bug: contract entrypoint selectors are always eight bytes");
    ContractEntrypointID::from(bytes)
}

pub(crate) fn encode_transfer_calldata(
    recipient: &ContractAddress,
    raw_amount: u128,
) -> Result<Vec<u8>, CliError> {
    xdrs::cxs::encode(&(recipient, raw_amount)).map_err(|error| {
        CliError::Other(format!("failed to encode HOG transfer calldata: {error}"))
    })
}
