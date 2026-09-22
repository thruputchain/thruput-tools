use serde_json::json;

use groundhog_json_rpc_types::airdrop::{ClientNonce, ClientNonceError, MAX_CLIENT_NONCE_BYTES};

#[test]
fn client_nonce_roundtrips() {
    let nonce = ClientNonce::new(vec![0x01, 0xab]).expect("nonce should be valid");
    let encoded = serde_json::to_value(&nonce).expect("nonce should serialize");
    assert_eq!(encoded, json!("0x01ab"));
    assert_eq!(
        serde_json::from_value::<ClientNonce>(encoded).expect("nonce should deserialize"),
        nonce
    );
}

#[test]
fn client_nonce_enforces_resource_bounds() {
    assert_eq!(ClientNonce::new(Vec::new()), Err(ClientNonceError::Empty));
    assert_eq!(
        ClientNonce::new(vec![0; MAX_CLIENT_NONCE_BYTES + 1]),
        Err(ClientNonceError::TooLong)
    );
}

#[test]
fn client_nonce_accepts_legacy_hex_forms_and_serializes_canonically() {
    for (encoded, canonical) in [
        (r#""01""#, "0x01"),
        (r#""0X01""#, "0x01"),
        (r#""0xAB""#, "0xab"),
    ] {
        let nonce = serde_json::from_str::<ClientNonce>(encoded)
            .expect("legacy nonce form should deserialize");
        assert_eq!(
            serde_json::to_value(nonce).expect("nonce should serialize"),
            json!(canonical)
        );
    }
}

#[test]
fn airdrop_request_accepts_legacy_recipient_forms() {
    for recipient in ["11".repeat(32), format!("0X{}", "11".repeat(32))] {
        let request = serde_json::json!({
            "recipient": recipient,
            "clientNonce": "0x01"
        });
        assert!(
            serde_json::from_value::<groundhog_json_rpc_types::airdrop::AirdropRequest>(request)
                .is_ok()
        );
    }
}

#[test]
fn airdrop_response_accepts_future_additive_fields() {
    let response = serde_json::json!({
        "attemptStatus": "pending",
        "recipient": format!("0x{}", "11".repeat(32)),
        "rawAmount": "100000000",
        "retryAfterSeconds": 5,
        "futureField": true
    });

    assert!(
        serde_json::from_value::<groundhog_json_rpc_types::airdrop::AirdropAttemptResponse>(
            response
        )
        .is_ok()
    );
}
