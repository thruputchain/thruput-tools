use groundhog_json_rpc_types::view::{
    RootInvocationOutput, ViewBatchRequest, ViewBatchResponse, ViewCallResult,
    ViewHeightUnavailable,
};
use serde_json::json;

#[test]
fn batch_wide_integers_use_lossless_decimal_strings() {
    let height = 9_007_199_254_740_993_i64;
    let response = ViewBatchResponse {
        height: height.into(),
        results: vec![ViewCallResult {
            output: RootInvocationOutput::Fault,
            events: vec![],
            gas_used: 17,
        }],
    };
    let encoded = serde_json::to_value(response).unwrap();
    assert_eq!(encoded["height"], "9007199254740993");
    assert_eq!(encoded["results"][0]["gasUsed"], "17");
    let decoded: ViewBatchResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(*decoded.height, height);
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
