use super::*;
use crate::responses::DecodedResponse;
use serde_json::json;

fn succeeded() -> Value {
    json!({"attemptStatus":"succeeded", "recipient":"1".repeat(32), "rawAmount":"1", "transactionHash":"1".repeat(32), "blockHeight":"2", "execution":{"result":"return", "returnData":"0x"}})
}

#[test]
fn success_requires_committed_return_data() {
    let value = succeeded();
    let response: AirdropAttempt = serde_json::from_value(value.clone()).unwrap();
    assert!(response.is_supported());
    assert_eq!(serde_json::to_value(response).unwrap(), value);
    for execution in [
        json!({"result":"return"}),
        json!({"result":"fault"}),
        json!({"result":"abort","abortData":"0x"}),
        json!({"result":"failedGasSponsorship"}),
    ] {
        let mut value = succeeded();
        value["execution"] = execution;
        assert!(serde_json::from_value::<AirdropAttempt>(value).is_err());
    }
    for height in [json!(2), json!("-1"), json!("9223372036854775808")] {
        let mut value = succeeded();
        value["blockHeight"] = height;
        assert!(serde_json::from_value::<AirdropAttempt>(value).is_err());
    }
}

#[test]
fn unknown_attempt_and_execution_preserve_complete_result() {
    let mut nested = succeeded();
    nested["execution"] = json!({"result":"future", "nested":{"value":42}});
    for value in [
        nested,
        json!({"attemptStatus":"future", "nested":{"value":42}}),
    ] {
        let response: DecodedResponse<AirdropAttempt> =
            serde_json::from_value(value.clone()).unwrap();
        assert!(matches!(response, DecodedResponse::Unsupported { raw } if raw == value));
    }
    let mut value = succeeded();
    value["additional"] = json!({"future": true});
    assert!(matches!(
        serde_json::from_value::<DecodedResponse<AirdropAttempt>>(value).unwrap(),
        DecodedResponse::Known(_)
    ));
}

#[test]
fn pending_hash_is_optional_but_not_nullable_and_delay_is_wide() {
    let mut value = json!({"attemptStatus":"pending", "recipient":"1".repeat(32), "rawAmount":"1", "retryAfterSeconds":u64::MAX.to_string()});
    assert!(serde_json::from_value::<AirdropAttempt>(value.clone()).is_ok());
    value["transactionHash"] = json!("1".repeat(32));
    assert!(serde_json::from_value::<AirdropAttempt>(value.clone()).is_ok());
    value["transactionHash"] = Value::Null;
    assert!(serde_json::from_value::<AirdropAttempt>(value.clone()).is_err());
    value.as_object_mut().unwrap().remove("transactionHash");
    value["retryAfterSeconds"] = json!(1);
    assert!(serde_json::from_value::<AirdropAttempt>(value).is_err());
}

#[test]
fn funding_and_count_use_native_fields_and_decimal_domains() {
    let value = json!({"recipient":"1".repeat(32), "rawAmount":u128::MAX.to_string(), "transactionHash":"1".repeat(32), "signedTransaction":"0x00"});
    let decoded: FundingSubmission = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    for amount in [json!("0"), json!("01"), json!(1)] {
        let mut invalid = value.clone();
        invalid["rawAmount"] = amount;
        assert!(serde_json::from_value::<FundingSubmission>(invalid).is_err());
    }
    for count in ["0".to_owned(), u64::MAX.to_string()] {
        let value = json!({"count":count});
        let decoded: PendingTransactionCount = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
    for value in [
        json!({"count":1}),
        json!({"count":"18446744073709551616"}),
        json!(["1"]),
    ] {
        assert!(serde_json::from_value::<PendingTransactionCount>(value).is_err());
    }
}

#[test]
fn succeeded_schema_has_only_return_execution() {
    let schema = serde_json::to_value(schemars::schema_for!(AirdropExecution)).unwrap();
    assert_eq!(
        schema["anyOf"][0]["properties"]["result"]["const"],
        "return"
    );
    assert_eq!(schema["anyOf"].as_array().unwrap().len(), 1);
    assert_eq!(
        schema["anyOf"][0]["required"],
        json!(["result", "returnData"])
    );
}
