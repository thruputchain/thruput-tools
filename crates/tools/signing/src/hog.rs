//! Decimal amounts for the native HOG token.

use crate::{NATIVE_HOG_DECIMALS, NATIVE_HOG_SCALE};

/// Why a decimal HOG amount cannot be converted to positive raw units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseHogAmountError {
    /// The input contains only whitespace or is empty.
    #[error("amount cannot be empty")]
    Empty,
    /// A leading plus or minus sign was supplied.
    #[error("amount must be unsigned")]
    Signed,
    /// The whole part or decimal separator is malformed.
    #[error("expected a decimal number")]
    InvalidDecimal,
    /// The fractional part is empty, nonnumeric, or exceeds seven digits.
    #[error("expected at most seven fractional digits")]
    InvalidFraction,
    /// The raw amount does not fit in `u128`.
    #[error("amount is too large")]
    TooLarge,
    /// The amount is zero.
    #[error("amount must be greater than zero")]
    Zero,
}

/// Convert a positive decimal HOG amount to raw units without rounding.
///
/// Surrounding whitespace is ignored. Signs, exponent notation, and more than
/// seven fractional digits are rejected, as are zero and values exceeding `u128`.
///
/// # Errors
/// Returns the reason the input is invalid or outside the supported range.
pub fn parse_hog_amount(input: &str) -> Result<u128, ParseHogAmountError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ParseHogAmountError::Empty);
    }
    if input.starts_with(['-', '+']) {
        return Err(ParseHogAmountError::Signed);
    }

    let mut parts = input.split('.');
    let whole = parts.next().unwrap_or_default();
    let fractional = parts.next();
    if parts.next().is_some() || whole.is_empty() || !whole.bytes().all(|c| c.is_ascii_digit()) {
        return Err(ParseHogAmountError::InvalidDecimal);
    }

    let whole = whole
        .parse::<u128>()
        .map_err(|_| ParseHogAmountError::TooLarge)?;
    let fractional = fractional.map_or(Ok(0), parse_fractional)?;
    let raw = whole
        .checked_mul(NATIVE_HOG_SCALE)
        .and_then(|value| value.checked_add(fractional))
        .ok_or(ParseHogAmountError::TooLarge)?;
    if raw == 0 {
        return Err(ParseHogAmountError::Zero);
    }
    Ok(raw)
}

fn parse_fractional(fractional: &str) -> Result<u128, ParseHogAmountError> {
    if fractional.is_empty()
        || fractional.len() > NATIVE_HOG_DECIMALS as usize
        || !fractional.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(ParseHogAmountError::InvalidFraction);
    }

    let mut padded = fractional.to_owned();
    while padded.len() < NATIVE_HOG_DECIMALS as usize {
        padded.push('0');
    }
    padded
        .parse::<u128>()
        .map_err(|_| ParseHogAmountError::TooLarge)
}
