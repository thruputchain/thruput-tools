use thruput_signing::strkey::*;

fn corrupt_checksum(value: &mut String) {
    let replacement = if value.ends_with('A') { "B" } else { "A" };
    value.replace_range(value.len() - 1.., replacement);
}

#[test]
fn stellar_zero_vectors_round_trip_without_deriving_addresses() {
    let c = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";
    let g = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
    let s = "SAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSU2";
    assert_eq!(format_contract_address(&[0; 32].into()), c);
    assert_eq!(format_public_key(&[0; 32]), g);
    assert_eq!(format_secret_seed(&[0; 32]).as_str(), s);
    assert_eq!(parse_contract_address(c).unwrap(), [0; 32].into());
    assert_eq!(parse_public_key(g).unwrap(), [0; 32]);
    assert_eq!(*parse_secret_seed(s).unwrap(), [0; 32]);
    assert!(parse_contract_address(g).is_err());
    assert!(parse_contract_address(s).is_err());
    assert!(parse_public_key(c).is_err());
    assert!(parse_public_key(s).is_err());
    assert!(parse_secret_seed(g).is_err());
    assert!(parse_secret_seed(c).is_err());
}

#[test]
fn legacy_hex_and_bad_checksums_are_handled_without_echoing_input() {
    for prefix in ["", "0x", "0X"] {
        let value = format!("{prefix}{}", "ab".repeat(32));
        assert_eq!(parse_contract_address(&value).unwrap(), [0xab; 32].into());
        assert_eq!(parse_public_key(&value).unwrap(), [0xab; 32]);
        assert_eq!(*parse_secret_seed(&value).unwrap(), [0xab; 32]);
    }
    let mut bad = format_secret_seed(&[42; 32]);
    corrupt_checksum(&mut bad);
    let error = parse_secret_seed(&bad).unwrap_err();
    assert!(!format!("{error:?} {error}").contains(bad.as_str()));
    assert!(parse_secret_seed(&"ab".repeat(64)).is_err());
    let mut bad = format_contract_address(&[42; 32].into());
    corrupt_checksum(&mut bad);
    assert!(parse_contract_address(&bad).is_err());
    assert!(parse_public_key(&format_public_key(&[42; 32]).to_lowercase()).is_err());
}
