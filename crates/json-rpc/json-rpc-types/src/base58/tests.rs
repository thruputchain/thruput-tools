use super::*;
use crate::hex::{HexBytes, Topic};
use thruput_xdr::types::{ContractAddress, SHA256Hash, StorageLongHash};

#[test]
fn shared_vectors_keep_payload_hex_independent_of_identifier_base58() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../encoding/fixtures/text-encoding.json"
    ))
    .unwrap();
    for vector in vectors["valid"].as_array().unwrap() {
        let bytes: Vec<u8> = vector["bytes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
            .collect();
        assert_eq!(
            serde_json::to_value(HexBytes(bytes.clone())).unwrap(),
            vector["hex"]
        );
        if let Ok(fixed) = <[u8; 32]>::try_from(bytes) {
            let identifier = Base58(ContractAddress::from(fixed));
            assert_eq!(serde_json::to_value(identifier).unwrap(), vector["base58"]);
            let decoded: Base58<ContractAddress> =
                serde_json::from_value(vector["base58"].clone()).unwrap();
            assert_eq!(decoded, identifier);
        }
    }
}

#[test]
fn identifier_and_payload_encodings_are_distinct_at_equal_lengths() {
    #[derive(Serialize, Deserialize)]
    struct Record {
        address: Base58<ContractAddress>,
        hash: Base58<SHA256Hash>,
        payload: HexBytes,
        topic: Topic,
    }
    let bytes = [0u8; 32];
    let record = Record {
        address: Base58(ContractAddress::from(bytes)),
        hash: Base58(SHA256Hash::from(bytes)),
        payload: HexBytes(bytes.to_vec()),
        topic: Topic(bytes),
    };
    let value = serde_json::to_value(record).unwrap();
    assert_eq!(value["address"], "1".repeat(32));
    assert_eq!(value["hash"], "1".repeat(32));
    assert_eq!(value["payload"], format!("0x{}", "00".repeat(32)));
    assert_eq!(value["topic"], value["payload"]);
    let decoded: Record = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.address.0.as_ref(), bytes);
    assert_eq!(decoded.hash.0.as_ref(), bytes);
    assert_eq!(decoded.payload.0, bytes);
    assert_eq!(decoded.topic.0, bytes);
}

#[test]
fn machine_identifiers_reject_hex_strkeys_and_wrong_decoded_lengths() {
    for text in [
        format!("0x{}", "00".repeat(32)),
        "ab".repeat(32),
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".into(),
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF".into(),
        "1".repeat(31),
        "1".repeat(33),
        format!(" {}", "1".repeat(32)),
        format!("{}\n", "1".repeat(32)),
        String::new(),
    ] {
        assert!(serde_json::from_value::<Base58<ContractAddress>>(text.into()).is_err());
    }
}

#[test]
fn schema_documents_decoded_length_and_preserves_case() {
    let schema = serde_json::to_value(schemars::schema_for!(Base58<SHA256Hash>)).unwrap();
    assert_eq!(schema["minLength"], 32);
    assert_eq!(schema["maxLength"], 45);
    assert_eq!(schema["pattern"], "^[1-9A-HJ-NP-Za-km-z]+$");
    assert!(
        schema["description"]
            .as_str()
            .unwrap()
            .contains("exactly 32 bytes")
    );
    let hash = Base58(SHA256Hash::from([0x42; 32]));
    let text = serde_json::to_string(&hash).unwrap();
    let decoded: Base58<SHA256Hash> = serde_json::from_str(&text).unwrap();
    assert_eq!(decoded, hash);
    assert!(text.contains(&hash.to_string()));
}

#[test]
fn long_hash_is_still_a_typed_digest_not_an_opaque_payload() {
    let hash = Base58(StorageLongHash::from([0x42; 2048]));
    let text = serde_json::to_string(&hash).unwrap();
    let decoded: Base58<StorageLongHash> = serde_json::from_str(&text).unwrap();
    assert_eq!(decoded, hash);
    assert!(!text.contains("0x"));
}
