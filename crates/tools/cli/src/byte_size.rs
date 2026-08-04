//! Shared parsing for byte-valued CLI arguments.

/// Parse a byte count written as decimal digits, `0x`-prefixed hex, or a
/// decimal value with a `K`/`M`/`G` unit suffix.
///
/// Suffixes are 1024-based and case-insensitive, and accept an optional `B` or
/// `iB` tail, so `2G`, `2g`, `2GB`, and `2GiB` all mean 2147483648. A bare `B`
/// suffix means bytes. Hex values do not take a suffix, and fractional values
/// such as `2.5G` are rejected.
pub fn parse_byte_size(value: &str) -> Result<u64, String> {
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
        .ok_or_else(|| format!("byte size {value:?} overflows a 64-bit byte count"))
}

fn invalid(value: &str) -> String {
    format!(
        "expected bytes as decimal, 0x hex, or a K/M/G suffixed value \
         (for example 2G, 512M, 0x8000), got {value:?}"
    )
}

#[cfg(test)]
mod tests {
    use super::parse_byte_size;

    #[test]
    fn accepts_decimal_hex_and_unit_suffixes() {
        for (input, expected) in [
            ("0", 0),
            ("2147483648", 2_147_483_648),
            ("0x80000000", 2_147_483_648),
            ("0x8000", 32_768),
            ("4096B", 4096),
            ("1024K", 1_048_576),
            ("512M", 536_870_912),
            ("2G", 2_147_483_648),
            ("2g", 2_147_483_648),
            ("2GB", 2_147_483_648),
            ("2gb", 2_147_483_648),
            ("2GiB", 2_147_483_648),
            ("2gib", 2_147_483_648),
            ("32k", 32_768),
            (" 32K ", 32_768),
        ] {
            assert_eq!(parse_byte_size(input), Ok(expected), "input {input:?}");
        }
    }

    #[test]
    fn rejects_malformed_values() {
        for bad in [
            "",
            "abc",
            "0x",
            "0xzz",
            "-16",
            "2.5G",
            "2T",
            "2Gi B",
            "2 G",
            "G",
            "K512",
            "0x8000K",
            "18446744073709551616",
        ] {
            assert!(
                parse_byte_size(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_suffixed_values_that_overflow() {
        assert!(parse_byte_size("18446744073709551615G").is_err());
    }
}
