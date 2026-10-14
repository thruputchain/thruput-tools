use super::*;

#[test]
fn shared_protocol_vectors() {
    // Vendored verbatim from thruput-protocol commit e9e5e64.
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/text-encoding.json")).unwrap();
    for vector in vectors["valid"].as_array().unwrap() {
        let bytes: Vec<u8> = vector["bytes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
            .collect();
        let text = vector["base58"].as_str().unwrap();
        assert_eq!(encode(&bytes), text);
        assert_eq!(decode(text, bytes.len()).unwrap(), bytes);
        if let Some(keys) = vector.get("strkey") {
            let fixed: [u8; 32] = bytes.try_into().unwrap();
            assert_eq!(address_strkey(&fixed), keys["contract"].as_str().unwrap());
            assert_eq!(
                public_key_strkey(&fixed),
                keys["publicKey"].as_str().unwrap()
            );
            assert_eq!(
                secret_seed_strkey(&fixed).as_str(),
                keys["seed"].as_str().unwrap()
            );
        }
    }
    for text in vectors["invalidBase58"].as_array().unwrap() {
        assert!(decode(text.as_str().unwrap(), 256).is_err());
    }
    for text in vectors["invalidStrkey"].as_array().unwrap() {
        assert!(parse_public_key(text.as_str().unwrap()).is_err());
    }
}

#[test]
fn published_base58_example_and_leading_zeros() {
    assert_eq!(encode(b"Hello World"), "JxF12TrwUP45BMd");
    assert_eq!(decode("JxF12TrwUP45BMd", 11).unwrap(), b"Hello World");
    assert_eq!(encode(&[0; 32]), "1".repeat(32));
    assert_eq!(decode_fixed::<32>(&"1".repeat(32)).unwrap(), [0; 32]);
    assert_eq!(encode(&[]), "");
    assert_eq!(decode("", 0).unwrap(), Vec::<u8>::new());
    assert_eq!(decode_fixed::<0>("").unwrap(), [0u8; 0]);
}

#[test]
fn checks_limits_before_and_after_conversion() {
    assert_eq!(max_encoded_len(0), 0);
    assert_eq!(max_encoded_len(32), 45);
    assert_eq!(max_encoded_len(usize::MAX), usize::MAX);
    assert_eq!(decode("1", 0), Err(Error::TooLong { max_bytes: 0 }));
    // Within the conservative text bound, but decodes to 33 leading zero bytes.
    assert_eq!(
        decode(&"1".repeat(33), 32),
        Err(Error::TooLong { max_bytes: 32 })
    );
    assert_eq!(
        decode_fixed::<32>(&"1".repeat(31)),
        Err(Error::InvalidLength {
            expected: 32,
            actual: 31
        })
    );
    assert_eq!(
        decode_fixed::<32>(&"z".repeat(46)),
        Err(Error::TooLong { max_bytes: 32 })
    );
    for text in ["0", "O", "I", "l", " 1", "1 ", "1\n", "é", "１", "0x01"] {
        assert_eq!(decode(text, 32), Err(Error::InvalidBase58));
    }
}

#[test]
fn typed_strkeys_preserve_bytes_and_reject_wrong_types() {
    let bytes = [0; 32];
    let c = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";
    let g = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
    let s = "SAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSU2";
    assert_eq!(address_strkey(&bytes), c);
    assert_eq!(public_key_strkey(&bytes), g);
    assert_eq!(secret_seed_strkey(&bytes).as_str(), s);
    assert_eq!(parse_address(c).unwrap(), bytes);
    assert_eq!(parse_public_key(g).unwrap(), bytes);
    assert_eq!(*parse_secret_seed(s).unwrap(), bytes);
    assert!(parse_address(g).is_err());
    assert!(parse_address(s).is_err());
    assert!(parse_public_key(c).is_err());
    assert!(parse_secret_seed(g).is_err());
    assert!(parse_secret_seed(c).is_err());
    for text in [c, g, s] {
        assert!(decode_fixed::<32>(text).is_err());
    }
    assert!(parse_public_key(&g.to_lowercase()).is_err());
    assert!(parse_address(&format!("{c}=")).is_err());
    assert!(parse_secret_seed(&"ab".repeat(32)).is_err());
    let invalid_secret = "SAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAASECRET";
    let error = parse_secret_seed(invalid_secret).unwrap_err();
    assert!(!format!("{error:?} {error}").contains(invalid_secret));
}

#[test]
fn base58_is_not_case_normalized_or_detected_as_hex() {
    assert_ne!(decode("ab", 2).unwrap(), decode("AB", 2).unwrap());
    // This is valid base58, not a hexadecimal compatibility path.
    assert_eq!(decode("ff", 2).unwrap(), [8, 194]);
    for byte in 0..=255 {
        let bytes = [byte; 32];
        let encoded = encode(&bytes);
        assert_eq!(decode_fixed::<32>(&encoded).unwrap(), bytes);
        assert_eq!(parse_address(&encoded).unwrap(), bytes);
        assert_eq!(parse_public_key(&encoded).unwrap(), bytes);
        assert_eq!(*parse_secret_seed(&encoded).unwrap(), bytes);
    }
}
