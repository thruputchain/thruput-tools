use serde_json::json;
use thruput_json_rpc_types::view::{ViewBatchRequest, ViewRequest};

fn call() -> serde_json::Value {
    json!({
        "contract": thruput_encoding::encode(&[0x00; 32]),
        "entrypoint": "0x0000000000000000", "calldata": "0x",
    })
}

#[test]
fn single_view_height_is_optional_and_lossless() {
    let omitted: ViewRequest = serde_json::from_value(call()).unwrap();
    assert!(omitted.min_height.is_none());
    assert_eq!(serde_json::to_value(omitted).unwrap(), call());
    for height in ["0", "9007199254740993", "9223372036854775807"] {
        let mut value = call();
        value["minHeight"] = json!(height);
        let request: ViewRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(request.min_height.as_ref().unwrap().to_string(), height);
        assert_eq!(serde_json::to_value(request).unwrap(), value);
    }
}

#[test]
fn single_view_height_rejects_noncanonical_or_out_of_range_values() {
    for height in [
        json!(42),
        json!("01"),
        json!("+1"),
        json!("1.0"),
        json!("9223372036854775808"),
    ] {
        let mut value = call();
        value["minHeight"] = height;
        assert!(serde_json::from_value::<ViewRequest>(value).is_err());
    }
}

#[test]
fn batch_freshness_is_group_level_and_never_ignored_on_entries() {
    assert!(
        serde_json::from_value::<ViewBatchRequest>(json!({"calls":[call()],"minHeight":"42"}))
            .is_ok()
    );
    for height in [json!("42"), json!(null)] {
        let mut entry = call();
        entry["minHeight"] = height;
        assert!(serde_json::from_value::<ViewBatchRequest>(json!({"calls":[entry]})).is_err());
    }
    let mut entry = call();
    entry["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ViewBatchRequest>(json!({"calls":[entry]})).is_err());
}
