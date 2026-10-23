use serde_json::json;
use thruput_json_rpc_types::view::{ViewBatchRequest, ViewHeightUnavailable};

#[test]
fn height_unavailable_detail_uses_lossless_decimal_strings() {
    let height = 9_007_199_254_740_993_i64;
    let detail = ViewHeightUnavailable {
        requested: height.into(),
        observed: (height - 1).into(),
    };
    assert_eq!(
        serde_json::to_value(detail).unwrap(),
        json!({
            "requested": "9007199254740993", "observed": "9007199254740992",
        })
    );
}

#[test]
fn minimum_height_rejects_json_numbers() {
    let valid = json!({ "calls": [], "minHeight": "9007199254740993" });
    let request: ViewBatchRequest = serde_json::from_value(valid).unwrap();
    assert_eq!(*request.min_height.unwrap(), 9_007_199_254_740_993);
    assert!(
        serde_json::from_value::<ViewBatchRequest>(json!({
            "calls": [], "minHeight": 42,
        }))
        .is_err()
    );
}
