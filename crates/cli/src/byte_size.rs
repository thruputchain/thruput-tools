//! Shared parsing for byte-valued CLI arguments.

/// Parse a byte count written as decimal digits, `0x`-prefixed hex, or a
/// decimal value with a `K`/`M`/`G` unit suffix.
///
/// Suffixes are 1024-based and case-insensitive, and accept an optional `B` or
/// `iB` tail, so `2G`, `2g`, `2GB`, and `2GiB` all mean 2147483648. A bare `B`
/// suffix means bytes. Hex values do not take a suffix, and fractional values
/// such as `2.5G` are rejected.
pub fn parse_byte_size(value: &str) -> Result<u64, ByteSizeError> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).map_err(|_| invalid(value));
    }

    let digits_end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(digits_end);
    let parsed = digits.parse::<u64>().map_err(|_| invalid(value))?;

    let multiplier: u64 = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => return Err(invalid(value)),
    };

    parsed
        .checked_mul(multiplier)
        .ok_or_else(|| ByteSizeError::Overflow {
            value: value.to_owned(),
        })
}

/// Why a byte-valued argument was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ByteSizeError {
    /// The text is not a byte count in any notation `parse_byte_size` accepts.
    #[error(
        "expected bytes as decimal, 0x hex, or a K/M/G suffixed value \
         (for example 2G, 512M, 0x8000), got {value:?}"
    )]
    Malformed {
        /// The argument as written.
        value: String,
    },
    /// The digits parse, but the unit suffix scales them past a 64-bit count.
    #[error("byte size {value:?} overflows a 64-bit byte count")]
    Overflow {
        /// The argument as written.
        value: String,
    },
}

fn invalid(value: &str) -> ByteSizeError {
    ByteSizeError::Malformed {
        value: value.to_owned(),
    }
}
