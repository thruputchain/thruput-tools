use groundhog_xdr::types::ContractAddress;

use crate::CliError;

// TODO: Load native HOG metadata from the local runtime manifest and hosted
// Devnet /v1/network descriptor instead of hardcoding it here.
pub(crate) const DECIMALS: u32 = 7;
pub(crate) const SCALE: u128 = 10_000_000;
pub(crate) const TOKEN_ADDRESS: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000001";
pub(crate) const TRANSFER_ENTRYPOINT: [u8; 8] = 0x6328_5b35_23f9_dc3f_u64.to_be_bytes();

pub(crate) fn encode_transfer_calldata(
    recipient: &ContractAddress,
    raw_amount: u128,
) -> Result<Vec<u8>, CliError> {
    xdrs::cxs::encode(&(recipient, raw_amount)).map_err(|error| {
        CliError::Other(format!("failed to encode HOG transfer calldata: {error}"))
    })
}
