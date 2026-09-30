use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::ContractAddress;

use crate::CliError;

// Native HOG uses these protocol-defined values on every network.
pub(crate) const DECIMALS: u32 = groundhog_signing::NATIVE_HOG_DECIMALS;
pub(crate) const SCALE: u128 = groundhog_signing::NATIVE_HOG_SCALE;
pub(crate) const fn token_address() -> ContractAddress {
    groundhog_signing::NATIVE_HOG_ADDRESS
}

pub(crate) fn transfer_entrypoint() -> ContractEntrypointID {
    groundhog_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT
}

pub(crate) fn encode_transfer_calldata(
    recipient: &ContractAddress,
    raw_amount: u128,
) -> Result<Vec<u8>, CliError> {
    groundhog_signing::native_hog_transfer_calldata(recipient, raw_amount).map_err(|error| {
        CliError::Other(format!("failed to encode HOG transfer calldata: {error}"))
    })
}

pub(crate) fn parse_amount(input: &str) -> Result<u128, CliError> {
    groundhog_signing::parse_hog_amount(input).map_err(|error| CliError::InvalidHogAmount {
        input: input.trim().to_owned(),
        message: error.to_string(),
    })
}

pub(crate) fn format_amount(raw_amount: u128) -> String {
    let whole = raw_amount / SCALE;
    let fractional = raw_amount % SCALE;
    if fractional == 0 {
        return whole.to_string();
    }
    let fractional = format!("{fractional:0width$}", width = DECIMALS as usize)
        .trim_end_matches('0')
        .to_owned();
    format!("{whole}.{fractional}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_amounts() {
        for input in [
            "",
            " ",
            "0",
            "0.0000000",
            "+1",
            "-1",
            "1e2",
            ".1",
            "1.",
            "1.2.3",
            "1.00000001",
            "1. 0",
        ] {
            let error = parse_amount(input).expect_err("invalid amount should fail");
            assert_eq!(
                error.code(),
                "invalid_hog_amount",
                "unexpected error for {input:?}"
            );
        }
    }

    #[test]
    fn formats_raw_units_canonically() {
        assert_eq!(format_amount(SCALE), "1");
        assert_eq!(format_amount(15_000_000), "1.5");
        assert_eq!(format_amount(1), "0.0000001");
        assert_eq!(format_amount(105_000_000), "10.5");
    }

    #[test]
    fn protocol_constants_remain_stable() {
        assert_eq!(DECIMALS, 7);
        assert_eq!(SCALE, 10_000_000);
        assert_eq!(token_address(), groundhog_signing::NATIVE_HOG_ADDRESS);
        assert_eq!(
            transfer_entrypoint().as_ref(),
            &[0x63, 0x28, 0x5b, 0x35, 0x23, 0xf9, 0xdc, 0x3f]
        );
    }
}
