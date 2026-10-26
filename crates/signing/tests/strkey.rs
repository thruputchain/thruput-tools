use thruput_signing::strkey::*;

fn corrupt_checksum(value: &mut String) {
    let replacement = if value.ends_with('A') { "B" } else { "A" };
    value.replace_range(value.len() - 1.., replacement);
}

#[test]
fn typed_zero_vectors_round_trip_without_deriving_addresses() {
    let z = "ZAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAZ4L";
    let c = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";
    let g = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
    let s = "SAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSU2";
    assert_eq!(format_contract_address(&[0; 32].into()), z);
    assert_eq!(format_public_key(&[0; 32]), g);
    assert_eq!(format_secret_seed(&[0; 32]).as_str(), s);
    assert_eq!(parse_contract_address(z).unwrap(), [0; 32].into());
    assert!(parse_contract_address(c).is_err());
    assert_eq!(parse_public_key(g).unwrap(), [0; 32]);
    assert_eq!(*parse_secret_seed(s).unwrap(), [0; 32]);
    assert!(parse_contract_address(g).is_err());
    assert!(parse_contract_address(s).is_err());
    assert!(parse_public_key(c).is_err());
    assert!(parse_public_key(z).is_err());
    assert!(parse_public_key(s).is_err());
    assert!(parse_secret_seed(g).is_err());
    assert!(parse_secret_seed(c).is_err());
    assert!(parse_secret_seed(z).is_err());
}

#[test]
fn base58_round_trips_and_legacy_hex_is_rejected() {
    for bytes in [[0; 32], [0xab; 32], [255; 32]] {
        let value = thruput_encoding::encode(&bytes);
        assert_eq!(parse_contract_address(&value).unwrap(), bytes.into());
        assert_eq!(parse_public_key(&value).unwrap(), bytes);
        assert_eq!(*parse_secret_seed(&value).unwrap(), bytes);
    }
    for prefix in ["", "0x", "0X"] {
        let value = format!("{prefix}{}", "ab".repeat(32));
        assert!(parse_contract_address(&value).is_err());
        assert!(parse_public_key(&value).is_err());
        assert!(parse_secret_seed(&value).is_err());
    }
}

#[test]
fn bad_checksums_are_handled_without_echoing_input() {
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
