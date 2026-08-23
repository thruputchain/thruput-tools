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
    let input = input.trim();
    if input.is_empty() {
        return Err(invalid_amount(input, "amount cannot be empty"));
    }
    if input.starts_with(['-', '+']) {
        return Err(invalid_amount(input, "amount must be unsigned"));
    }

    let mut parts = input.split('.');
    let whole = parts.next().unwrap_or_default();
    let fractional = parts.next();
    if parts.next().is_some() || whole.is_empty() || !whole.bytes().all(|c| c.is_ascii_digit()) {
        return Err(invalid_amount(input, "expected a decimal number"));
    }

    let whole = whole
        .parse::<u128>()
        .map_err(|_| invalid_amount(input, "amount is too large"))?;
    let fractional = fractional.map_or(Ok(0), |value| parse_fractional(input, value))?;
    let raw = whole
        .checked_mul(SCALE)
        .and_then(|value| value.checked_add(fractional))
        .ok_or_else(|| invalid_amount(input, "amount is too large"))?;
    if raw == 0 {
        return Err(invalid_amount(input, "amount must be greater than zero"));
    }
    Ok(raw)
}

fn parse_fractional(input: &str, fractional: &str) -> Result<u128, CliError> {
    if fractional.is_empty()
        || fractional.len() > DECIMALS as usize
        || !fractional.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(invalid_amount(
            input,
            "expected at most seven fractional digits",
        ));
    }

    let mut padded = fractional.to_owned();
    while padded.len() < DECIMALS as usize {
        padded.push('0');
    }
    padded
        .parse::<u128>()
        .map_err(|_| invalid_amount(input, "amount is too large"))
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

fn invalid_amount(input: &str, message: &str) -> CliError {
    CliError::InvalidHogAmount {
        input: input.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_amounts_to_raw_units() {
        assert_eq!(parse_amount("1").unwrap(), SCALE);
        assert_eq!(parse_amount("1.0").unwrap(), SCALE);
        assert_eq!(parse_amount("0.0000001").unwrap(), 1);
        assert_eq!(parse_amount("10.5000000").unwrap(), 105_000_000);
        assert_eq!(parse_amount(" \t1.5\n").unwrap(), 15_000_000);
    }

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
    fn checks_full_u128_conversion_range() {
        let whole = u128::MAX / SCALE;
        let fractional = u128::MAX % SCALE;
        let max = format!("{whole}.{fractional:07}");
        let addition_overflow = format!("{whole}.{:07}", fractional + 1);
        let multiplication_overflow = (whole + 1).to_string();

        assert_eq!(parse_amount(&max).unwrap(), u128::MAX);
        assert!(parse_amount(&addition_overflow).is_err());
        assert!(parse_amount(&multiplication_overflow).is_err());
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
