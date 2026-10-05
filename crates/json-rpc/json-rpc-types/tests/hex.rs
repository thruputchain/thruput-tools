use groundhog_json_rpc_types::hex::*;

#[test]
fn hex_serialization_matches_reference_across_chunk_boundaries() {
    for len in (0..=257).chain([511, 512, 513, 2048, 65536]) {
        let bytes: Vec<u8> = (0..=255).cycle().take(len).collect();
        let expected = format!("\"0x{}\"", hex::encode(&bytes));
        assert_eq!(
            serde_json::to_string(&HexBytes(bytes.clone())).unwrap(),
            expected
        );
        assert_eq!(serde_json::to_string(&Hex0x(bytes)).unwrap(), expected);
    }
    let bytes = std::array::from_fn(|i| u8::try_from(i * 8).unwrap());
    assert_eq!(
        serde_json::to_string(&Topic(bytes)).unwrap(),
        format!("\"0x{}\"", hex::encode(bytes))
    );
}

#[test]
fn hex_bytes_roundtrip() {
    let original = HexBytes(vec![0xca, 0xfe, 0xba, 0xbe]);
    let json = serde_json::to_string(&original).unwrap();
    assert_eq!(json, r#""0xcafebabe""#);

    let decoded: HexBytes = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn hex_bytes_empty() {
    let original = HexBytes(vec![]);
    let json = serde_json::to_string(&original).unwrap();
    assert_eq!(json, r#""0x""#);

    let decoded: HexBytes = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn hex_bytes_accepts_no_prefix() {
    let decoded: HexBytes = serde_json::from_str(r#""cafebabe""#).unwrap();
    assert_eq!(decoded.0, vec![0xca, 0xfe, 0xba, 0xbe]);
}

#[test]
fn topic_roundtrip() {
    let mut bytes = [0u8; 32];
    bytes[0] = 0xde;
    bytes[1] = 0xad;
    bytes[31] = 0xff;
    let original = Topic(bytes);

    let json = serde_json::to_string(&original).unwrap();
    assert!(json.starts_with(r#""0xdead"#));
    assert!(json.ends_with(r#"ff""#));
    assert_eq!(json.len(), 2 + 64 + 2); // quotes + 0x + 64 hex chars

    let decoded: Topic = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn topic_wrong_length_fails() {
    let result: Result<Topic, _> = serde_json::from_str(r#""0xcafe""#);
    assert!(result.is_err());
}

#[test]
fn hex0x_sha256_roundtrip() {
    use groundhog_xdr::types::SHA256Hash;

    let mut bytes = [0u8; 32];
    bytes[0] = 0xab;
    bytes[31] = 0xcd;
    let original: Hex0x<SHA256Hash> = Hex0x(SHA256Hash::from(bytes));

    let json = serde_json::to_string(&original).unwrap();
    assert!(json.starts_with(r#""0xab"#));
    assert!(json.ends_with(r#"cd""#));
    assert_eq!(json.len(), 2 + 64 + 2); // quotes + 0x + 64 hex chars

    let decoded: Hex0x<SHA256Hash> = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn hex0x_accepts_no_prefix() {
    use groundhog_xdr::types::SHA256Hash;

    let hex_str = format!(r#""{}""#, "ab".to_owned() + &"00".repeat(30) + "cd");
    let decoded: Hex0x<SHA256Hash> = serde_json::from_str(&hex_str).unwrap();
    assert_eq!(decoded.0.as_ref()[0], 0xab);
    assert_eq!(decoded.0.as_ref()[31], 0xcd);
}

#[test]
fn hex0x_wrong_length_fails() {
    use groundhog_xdr::types::SHA256Hash;

    let result: Result<Hex0x<SHA256Hash>, _> = serde_json::from_str(r#""0xcafe""#);
    assert!(result.is_err());
}

#[test]
fn prefixed_hex_matches_serde_projection() {
    let bytes: Vec<u8> = (0..=255).collect();
    assert_eq!(
        format!("\"{}\"", prefixed_hex(&bytes)),
        serde_json::to_string(&HexBytes(bytes.clone())).unwrap()
    );
    assert_eq!(prefixed_hex([]), "0x");
    assert_eq!(prefixed_hex([0xab, 0x01]), "0xab01");
}

#[test]
fn decode_prefixed_hex_accepts_either_prefix_or_none() {
    for text in ["0xab01", "0XAB01", "ab01"] {
        assert_eq!(decode_prefixed_hex(text).unwrap(), vec![0xab, 0x01]);
    }
    assert!(decode_prefixed_hex("0xzz").is_err());
}

#[test]
fn decode_fixed_hex_reports_hex_and_length_errors_distinctly() {
    assert_eq!(decode_fixed_hex::<2>("0xab01").unwrap(), [0xab, 0x01]);
    assert!(matches!(
        decode_fixed_hex::<2>("0xab"),
        Err(FixedHexError::InvalidLength {
            len: 1,
            expected: 2
        })
    ));
    assert!(matches!(
        decode_fixed_hex::<2>("0xab0g"),
        Err(FixedHexError::InvalidHex(_))
    ));
    assert_eq!(
        decode_fixed_hex::<32>("0x00").unwrap_err().to_string(),
        "invalid length 1, expected 32 bytes (64 hex characters)"
    );
}

#[test]
fn sha256_prefixed_hex_matches_reference_digest() {
    assert_eq!(
        sha256_prefixed_hex(b"abc"),
        "0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
