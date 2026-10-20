use super::*;
use serde_json::{Value, json};

fn airdrop() -> Value {
    json!({"recipient": "1".repeat(32), "clientNonce": "0x01"})
}

fn funding() -> Value {
    let mut value = airdrop();
    value["rawAmount"] = json!("1");
    value["faucet"] = json!("1".repeat(32));
    value
}

#[test]
fn funding_requests_require_named_objects_and_all_fields() {
    for value in [airdrop(), funding()] {
        let is_funding = value.get("faucet").is_some();
        for field in value.as_object().unwrap().keys() {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            let mut null = value.clone();
            null[field] = Value::Null;
            for bad in [missing, null] {
                if is_funding {
                    assert!(serde_json::from_value::<FundAccountRequest>(bad).is_err());
                } else {
                    assert!(serde_json::from_value::<AirdropRequest>(bad).is_err());
                }
            }
        }
    }
    for bad in [json!([]), json!(["1".repeat(32), "0x01"]), Value::Null] {
        assert!(serde_json::from_value::<AirdropRequest>(bad.clone()).is_err());
        assert!(serde_json::from_value::<FundAccountRequest>(bad).is_err());
    }
    let mut extra = funding();
    extra["extra"] = json!(true);
    assert!(serde_json::from_value::<FundAccountRequest>(extra).is_err());
    let mut extra = airdrop();
    extra["extra"] = json!(true);
    assert!(serde_json::from_value::<AirdropRequest>(extra).is_err());
}

#[test]
fn nonce_retains_legacy_domain_with_canonical_native_hex() {
    for nonce in ["0x01".to_owned(), format!("0x{}", "ff".repeat(32))] {
        let mut value = airdrop();
        value["clientNonce"] = json!(nonce);
        let decoded: AirdropRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
    for nonce in [
        "0x".to_owned(),
        "01".to_owned(),
        "0xFf".to_owned(),
        "0x1".to_owned(),
        "0x01\n".to_owned(),
        format!("0x{}", "ff".repeat(33)),
    ] {
        let mut value = airdrop();
        value["clientNonce"] = json!(nonce);
        assert!(serde_json::from_value::<AirdropRequest>(value).is_err());
    }
}

#[test]
fn funding_amount_is_a_positive_canonical_bounded_decimal() {
    for amount in ["1".to_owned(), u128::MAX.to_string()] {
        let mut value = funding();
        value["rawAmount"] = json!(amount);
        let decoded: FundAccountRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
    for amount in [
        json!(0),
        json!(1),
        json!("0"),
        json!("01"),
        json!("+1"),
        json!("-1"),
        json!("1\n"),
        json!("340282366920938463463374607431768211456"),
    ] {
        let mut value = funding();
        value["rawAmount"] = amount;
        assert!(serde_json::from_value::<FundAccountRequest>(value).is_err());
    }
    let schema = serde_json::to_value(schemars::schema_for!(FundAccountRequest)).unwrap();
    let amount = &schema["properties"]["rawAmount"];
    assert_eq!(amount["type"], "string");
    assert_eq!(amount["x-integer-minimum"], "1");
    assert_eq!(amount["x-integer-maximum"], u128::MAX.to_string());
    assert_eq!(amount["pattern"], r"^[1-9][0-9]*(?![\s\S])");
}
