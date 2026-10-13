use thruput_signing::{NATIVE_HOG_SCALE as SCALE, ParseHogAmountError, parse_hog_amount};

#[test]
fn parses_valid_amounts_to_raw_units() {
    assert_eq!(parse_hog_amount("1").unwrap(), SCALE);
    assert_eq!(parse_hog_amount("1.0").unwrap(), SCALE);
    assert_eq!(parse_hog_amount("0.0000001").unwrap(), 1);
    assert_eq!(parse_hog_amount("10.5000000").unwrap(), 105_000_000);
    assert_eq!(parse_hog_amount(" \t1.5\n").unwrap(), 15_000_000);
}

#[test]
fn checks_full_u128_conversion_range() {
    let whole = u128::MAX / SCALE;
    let fractional = u128::MAX % SCALE;
    let max = format!("{whole}.{fractional:07}");
    let addition_overflow = format!("{whole}.{:07}", fractional + 1);
    let multiplication_overflow = (whole + 1).to_string();

    assert_eq!(parse_hog_amount(&max).unwrap(), u128::MAX);
    assert!(parse_hog_amount(&addition_overflow).is_err());
    assert!(parse_hog_amount(&multiplication_overflow).is_err());
}

#[test]
fn rejects_invalid_amounts_with_typed_errors() {
    use ParseHogAmountError::{Empty, InvalidDecimal, InvalidFraction, Signed, TooLarge, Zero};
    for (input, expected) in [
        ("", Empty),
        (" ", Empty),
        ("+1", Signed),
        ("-1", Signed),
        ("0", Zero),
        ("0.0000000", Zero),
        ("1e2", InvalidDecimal),
        (".1", InvalidDecimal),
        ("1.2.3", InvalidDecimal),
        ("1.", InvalidFraction),
        ("1.00000001", InvalidFraction),
        ("1. 0", InvalidFraction),
        ("１", InvalidDecimal),
        ("340282366920938463463374607431768211456", TooLarge),
    ] {
        assert_eq!(parse_hog_amount(input), Err(expected), "input: {input:?}");
    }
}
