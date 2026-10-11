use thruput_cli::byte_size::parse_byte_size;

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
