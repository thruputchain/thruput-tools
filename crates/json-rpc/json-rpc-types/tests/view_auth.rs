use serde_json::json;
use thruput_json_rpc_types::view::{ViewAuthClaim, ViewBatchRequest, ViewRequest};

fn call() -> serde_json::Value {
    json!({"contract": thruput_encoding::encode(&[0x07; 32]), "entrypoint":"0x0000000000000000", "calldata":"0x"})
}

#[test]
fn optional_claims_round_trip_on_single_and_batch_calls() {
    for claim in [
        json!({"kind":"sourceAddress","allowedKeys":0,"requiredWeight":0}),
        json!({"kind":"sourceAddress","allowedKeys":u32::MAX,"requiredWeight":u32::MAX}),
        json!({"kind":"pubkeySignature","publicKey":thruput_encoding::encode(&[0xab; 32]),"nonce":"18446744073709551615"}),
    ] {
        let mut input = call();
        input["authClaim"] = claim.clone();
        let single: ViewRequest = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(single).unwrap(), input);
        let batch: ViewBatchRequest =
            serde_json::from_value(json!({"calls":[input,call()]})).unwrap();
        assert!(batch.calls[0].auth_claim.is_some());
        assert!(batch.calls[1].auth_claim.is_none());
        assert_eq!(
            serde_json::to_value(&batch).unwrap()["calls"][0]["authClaim"],
            claim
        );
        assert!(
            serde_json::to_value(&batch).unwrap()["calls"][1]
                .get("authClaim")
                .is_none()
        );
    }
    assert!(
        serde_json::from_value::<ViewRequest>(call())
            .unwrap()
            .auth_claim
            .is_none()
    );
}

#[test]
fn malformed_claims_are_rejected_consistently() {
    for claim in [
        json!({"kind":"unknown"}),
        json!({"kind":"sourceAddress","allowedKeys":-1,"requiredWeight":0}),
        json!({"kind":"sourceAddress","allowedKeys":4_294_967_296_u64,"requiredWeight":0}),
        json!({"kind":"sourceAddress","allowedKeys":0,"requiredWeight":"1"}),
        json!({"kind":"sourceAddress","allowedKeys":0,"requiredWeight":0,"signature":"0x"}),
        json!({"kind":"pubkeySignature","publicKey":"0x01","nonce":"0"}),
        json!({"kind":"pubkeySignature","publicKey":thruput_encoding::encode(&[0x00; 32]),"nonce":0}),
        json!({"kind":"pubkeySignature","publicKey":thruput_encoding::encode(&[0x00; 32]),"nonce":"01"}),
        json!({"kind":"pubkeySignature","publicKey":thruput_encoding::encode(&[0x00; 32]),"nonce":"18446744073709551616"}),
    ] {
        let mut input = call();
        input["authClaim"] = claim;
        assert!(serde_json::from_value::<ViewRequest>(input.clone()).is_err());
        assert!(serde_json::from_value::<ViewBatchRequest>(json!({"calls":[input]})).is_err());
    }
}

#[test]
fn public_claims_use_existing_protocol_variants() {
    use thruput_xdr::auth_claim::AuthClaim;
    let source: AuthClaim = ViewAuthClaim::SourceAddress {
        allowed_keys: 5,
        required_weight: 3,
    }
    .into();
    let AuthClaim::kSourceAddressAuthData {
        source_address_claim,
    } = source
    else {
        panic!("source variant")
    };
    assert_eq!(source_address_claim.allowed_keys, 5);
    assert_eq!(source_address_claim.required_weight, 3);
    let claim: ViewAuthClaim = serde_json::from_value(json!({"kind":"pubkeySignature","publicKey":thruput_encoding::encode(&[0xab; 32]),"nonce":"9007199254740993"})).unwrap();
    let AuthClaim::kPubkeySignature { pubkey_preimage } = AuthClaim::from(claim) else {
        panic!("pubkey variant")
    };
    assert_eq!(pubkey_preimage.nonce, 9_007_199_254_740_993);
}
