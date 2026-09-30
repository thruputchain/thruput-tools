use groundhog_json_rpc_types::strkey::StrKey;
use groundhog_json_rpc_types::{ContractAddress, Ed25519PublicKey};

const ZERO_CONTRACT: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";
const ZERO_PUBLIC_KEY: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
const SEQUENTIAL_CONTRACT: &str = "CAAACAQDAQCQMBYIBEFAWDANBYHRAEISCMKBKFQXDAMRUGY4DUPB6N4O";
const SEQUENTIAL_PUBLIC_KEY: &str = "GAAACAQDAQCQMBYIBEFAWDANBYHRAEISCMKBKFQXDAMRUGY4DUPB7JZX";

fn sequential_bytes() -> [u8; 32] {
    std::array::from_fn(|i| u8::try_from(i).unwrap())
}

fn contract(bytes: [u8; 32]) -> ContractAddress {
    StrKey(bytes.into())
}

fn public_key(bytes: [u8; 32]) -> Ed25519PublicKey {
    StrKey(bytes.into())
}

#[test]
fn contract_addresses_serialize_as_c_strkeys() {
    for (bytes, expected) in [
        ([0; 32], ZERO_CONTRACT),
        (sequential_bytes(), SEQUENTIAL_CONTRACT),
    ] {
        let address = contract(bytes);
        assert_eq!(address.to_string(), expected);
        assert_eq!(format!("{address:?}"), expected);
        assert_eq!(
            serde_json::to_string(&address).unwrap(),
            format!("\"{expected}\"")
        );
        let decoded: ContractAddress = serde_json::from_str(&format!("\"{expected}\"")).unwrap();
        assert_eq!(decoded, address);
    }
}

#[test]
fn public_keys_serialize_as_g_strkeys() {
    for (bytes, expected) in [
        ([0; 32], ZERO_PUBLIC_KEY),
        (sequential_bytes(), SEQUENTIAL_PUBLIC_KEY),
    ] {
        let key = public_key(bytes);
        assert_eq!(key.to_string(), expected);
        assert_eq!(
            serde_json::to_string(&key).unwrap(),
            format!("\"{expected}\"")
        );
        let decoded: Ed25519PublicKey = serde_json::from_str(&format!("\"{expected}\"")).unwrap();
        assert_eq!(decoded, key);
    }
}

#[test]
fn legacy_hex_is_accepted_and_rendered_as_a_strkey() {
    let bytes = sequential_bytes();
    let hex = hex::encode(bytes);
    for text in [
        format!("0x{hex}"),
        format!("0X{hex}"),
        hex.clone(),
        format!("0x{}", hex.to_uppercase()),
    ] {
        let address: ContractAddress = text.parse().unwrap();
        assert_eq!(address, contract(bytes));
        assert_eq!(address.to_string(), SEQUENTIAL_CONTRACT);
        let key: Ed25519PublicKey = serde_json::from_value(text.into()).unwrap();
        assert_eq!(key, public_key(bytes));
    }
}

#[test]
fn strkeys_of_another_kind_are_rejected() {
    let secret_seed = "SAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSU2";
    for text in [ZERO_PUBLIC_KEY, secret_seed] {
        assert!(text.parse::<ContractAddress>().is_err(), "{text}");
    }
    for text in [ZERO_CONTRACT, secret_seed] {
        assert!(text.parse::<Ed25519PublicKey>().is_err(), "{text}");
    }
}

#[test]
fn malformed_text_is_rejected_without_echoing_it() {
    let mut bad_checksum = SEQUENTIAL_CONTRACT.to_owned();
    bad_checksum.replace_range(bad_checksum.len() - 1.., "A");
    for text in [
        bad_checksum.as_str(),
        &SEQUENTIAL_CONTRACT.to_lowercase(),
        "0xabcd",
        &format!("0x{}", "ab".repeat(33)),
        "",
    ] {
        let error = text.parse::<ContractAddress>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "contract address must be a C StrKey or 64 hex characters, optionally prefixed with 0x"
        );
        if !text.is_empty() {
            assert!(!error.to_string().contains(text));
        }
    }
    let error = "0xabcd".parse::<Ed25519PublicKey>().unwrap_err();
    assert_eq!(
        error.to_string(),
        "Ed25519 public key must be a G StrKey or 64 hex characters, optionally prefixed with 0x"
    );
    assert!(serde_json::from_str::<ContractAddress>("\"0xabcd\"").is_err());
}

#[test]
fn display_honors_width_and_alignment() {
    let address = contract([0; 32]);
    assert_eq!(format!("{address:<58}|"), format!("{ZERO_CONTRACT}  |"));
    assert_eq!(format!("{address:>58}|"), format!("  {ZERO_CONTRACT}|"));
}

#[test]
fn json_schema_describes_the_strkey_form() {
    let schema = serde_json::to_value(schemars::schema_for!(ContractAddress)).unwrap();
    assert_eq!(schema["title"], "StrKey_ContractAddress");
    assert_eq!(schema["type"], "string");
    assert_eq!(schema["pattern"], "^C[A-Z2-7]{55}$");
    let schema = serde_json::to_value(schemars::schema_for!(Ed25519PublicKey)).unwrap();
    assert_eq!(schema["pattern"], "^G[A-Z2-7]{55}$");
}
