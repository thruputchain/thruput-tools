//! Native application-error fixtures. No serving catalogue or retry policy changes.
use jsonrpsee::{
    RpcModule,
    core::client::{ClientT, Error as ClientError},
    server::Server,
    types::ErrorObjectOwned,
};
use serde_json::{Value, json};
use thruput_json_rpc_api::params::NamedParams;
use thruput_json_rpc_client::HttpClientBuilder;
use thruput_json_rpc_types::{
    error::{TxInvalidDetail, native as err},
    requests::EmptyRequest,
};

#[path = "support/schema.rs"]
mod schema;

const HASH: &str = "11111111111111111111111111111111";
const OTHER_HASH: &str = "11111111111111111111111111111112";

fn envelope(code: i32, data: Value) -> Value {
    let mut error = json!({"code": code, "message": "diagnostic only"});
    error["data"] = data;
    error
}

fn known(method: &str, code: i32, data: Value) -> err::ErrorData {
    let raw = envelope(code, data);
    let decoded = err::DecodedError::decode(method, raw.clone());
    assert_eq!(decoded.raw(), &raw);
    match decoded {
        err::DecodedError::Known { data, .. } => *data,
        _ => panic!("expected known {method} error: {raw}"),
    }
}

fn unclassified(method: &str, raw: Value) {
    let expected = raw.clone();
    let decoded = err::DecodedError::decode(method, raw);
    assert_eq!(decoded.raw(), &expected);
    assert!(
        matches!(decoded, err::DecodedError::Unclassified { .. }),
        "expected unclassified {method} error: {expected}"
    );
}

fn validator(method: &str, code: i32) -> jsonschema::Validator {
    let mut document = err::method_data_schema(method, code).unwrap();
    // Retain root definitions while selecting the complete schema below $ref.
    document["fixture"] = document.clone();
    schema::at(&document, "/fixture")
}

fn agrees(method: &str, code: i32, data: Value, valid: bool) {
    assert_eq!(
        validator(method, code).is_valid(&data),
        valid,
        "schema mismatch for {method}: {data}"
    );
    if valid {
        known(method, code, data);
    } else {
        unclassified(method, envelope(code, data));
    }
}

fn has_future_reason(data: &err::ErrorData) -> bool {
    matches!(
        data,
        err::ErrorData::InvalidParams {
            reason: Some(err::Reason::Unrecognized(_)),
            ..
        } | err::ErrorData::InvalidCursor {
            reason: Some(err::Reason::Unrecognized(_)),
            ..
        } | err::ErrorData::TransactionInvalid {
            reason: Some(err::Reason::Unrecognized(_)),
            ..
        } | err::ErrorData::TransactionRejected {
            reason: Some(err::Reason::Unrecognized(_)),
            ..
        } | err::ErrorData::HistoryUnavailable {
            reason: Some(err::Reason::Unrecognized(_)),
            ..
        } | err::ErrorData::BackendDataInvalid {
            reason: Some(err::Reason::Unrecognized(_)),
            ..
        }
    )
}

fn receipt() -> Value {
    json!({"transactionHash": HASH, "blockHeight": "42", "execution": {
        "result": "return", "returnData": "0x"
    }, "events": [], "gasUsed": "9007199254740993"})
}

fn cases() -> Vec<(&'static str, i32, Value)> {
    vec![
        ("getVersionInfo", -32602, json!({"kind": "invalidParams"})),
        ("getEvents", -32602, json!({"kind": "invalidCursor"})),
        (
            "sendTransaction",
            -32000,
            json!({"kind": "transactionInvalid", "admission": "notAttempted"}),
        ),
        (
            "sendTransaction",
            -32001,
            json!({"kind": "transactionRejected", "admission": "rejected"}),
        ),
        (
            "simulateInvocation",
            -32012,
            json!({"kind": "stateHeightUnavailable", "requestedStateBlockHeight": "43", "observedStateBlockHeight": "42"}),
        ),
        ("getEvents", -32013, json!({"kind": "historyUnavailable"})),
        (
            "getContractArtifact",
            -32014,
            json!({"kind": "contractArtifactUnavailable", "hash": HASH}),
        ),
        (
            "faucet_requestAirdrop",
            -32030,
            json!({"kind": "fundingAttemptStopped"}),
        ),
        (
            "dev_fundAccount",
            -32031,
            json!({"kind": "localFundingUnresolved", "transactionHash": HASH, "signedTransaction": "0x00"}),
        ),
        ("getBalance", -32050, json!({"kind": "backendUnavailable"})),
        ("getBalance", -32051, json!({"kind": "backendTimeout"})),
        ("getBalance", -32053, json!({"kind": "overloaded"})),
        (
            "getTransactionReceipt",
            -32054,
            json!({"kind": "backendDataInvalid"}),
        ),
    ]
}

#[test]
fn all_kinds_codes_and_declared_methods_have_consistent_schemas() {
    assert_eq!(err::ERROR_INFOS.len(), 12);
    let unique: std::collections::BTreeSet<_> = err::ERROR_INFOS.iter().map(|v| v.code).collect();
    assert_eq!(unique.len(), 12);
    assert_eq!(err::METHODS.len(), 23);
    for method in err::METHODS {
        let codes = err::method_error_codes(method).unwrap();
        assert_eq!(
            codes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            codes.len()
        );
        for code in codes {
            assert!(err::error_info_by_code(*code).is_some());
            validator(method, *code);
        }
    }
    for (method, code, data) in cases() {
        agrees(method, code, data.clone(), true);
        assert_eq!(
            serde_json::to_value(known(method, code, data.clone())).unwrap(),
            data
        );
        let mut extra = data.clone();
        extra["futureDiagnostic"] = json!({"retained": [1, 2]});
        let raw = envelope(code, extra);
        assert_eq!(err::DecodedError::decode(method, raw.clone()).raw(), &raw);
        unclassified(method, envelope(code - 100, data));
    }
    for code in [
        -32003, -32002, -32010, -32052, -32603, -32007, -32008, -32011,
    ] {
        assert!(err::data_schema(code).is_none());
    }
    assert!(err::method_error_codes("futureMethod").is_none());
    assert!(err::method_data_schema("getBalance", -32031).is_none());
    assert_eq!(thruput_json_rpc_api::WIRE_FORMAT_VERSION, "14.3.0");
}

#[test]
fn reason_vocabularies_are_optional_and_future_strings_keep_known_families() {
    let cases = [
        (
            "getVersionInfo",
            -32602,
            "invalidParams",
            vec![
                "missingField",
                "unknownField",
                "invalidType",
                "invalidFormat",
                "outOfRange",
                "conflictingFields",
                "invalidValue",
            ],
        ),
        (
            "getEvents",
            -32602,
            "invalidCursor",
            vec!["malformed", "unsupportedVersion", "queryMismatch"],
        ),
        (
            "sendTransaction",
            -32000,
            "transactionInvalid",
            vec![
                "invalidEncoding",
                "malformedTransaction",
                "invalidAuthEnvelope",
                "transactionTooLarge",
                "executorBoundViolation",
            ],
        ),
        (
            "sendTransaction",
            -32001,
            "transactionRejected",
            vec!["expired", "authenticationRejected"],
        ),
        (
            "getEvents",
            -32013,
            "historyUnavailable",
            vec!["outsideRetention"],
        ),
        (
            "getTransactionReceipt",
            -32054,
            "backendDataInvalid",
            vec![
                "invalidEncoding",
                "invalidCardinality",
                "unsupportedValue",
                "inconsistentResult",
            ],
        ),
    ];
    for (method, code, kind, reasons) in cases {
        let mut data = json!({"kind": kind});
        if method == "sendTransaction" {
            data["admission"] = json!("notAttempted");
        }
        agrees(method, code, data.clone(), true);
        for reason in reasons {
            data["reason"] = json!(reason);
            agrees(method, code, data.clone(), true);
            assert!(!has_future_reason(&known(method, code, data.clone())));
        }
        data["reason"] = json!("futureReason");
        assert!(!validator(method, code).is_valid(&data));
        assert!(has_future_reason(&known(method, code, data.clone())));
        for bad in [Value::Null, json!(1), json!({}), json!([])] {
            data["reason"] = bad;
            agrees(method, code, data.clone(), false);
        }
    }
}

#[test]
fn framework_errors_and_malformed_envelopes_preserve_raw_without_admission() {
    for code in [
        -32700, -32600, -32601, -32603, -32007, -32008, -32010, -32011, -32000, -32001,
    ] {
        unclassified(
            "sendTransaction",
            json!({"code": code, "message": "framework"}),
        );
        for data in [
            Value::Null,
            json!("response too large"),
            json!(42),
            json!([]),
            json!({}),
        ] {
            unclassified("sendTransaction", envelope(code, data));
        }
    }
    for raw in [
        Value::Null,
        json!([]),
        json!("error"),
        json!({"code": -32602}),
        json!({"code": "-32602", "message": "error", "data": {"kind": "invalidParams"}}),
        json!({"code": -32602, "message": 3, "data": {"kind": "invalidParams"}}),
    ] {
        unclassified("sendTransaction", raw);
    }
    unclassified(
        "sendTransaction",
        envelope(
            -32051,
            json!({"kind": "future", "admission": "notAttempted"}),
        ),
    );
    unclassified(
        "sendTransaction",
        envelope(
            -32051,
            json!({"kind": "backendTimeout", "admission": "future"}),
        ),
    );
    unclassified(
        "futureMethod",
        envelope(-32602, json!({"kind": "invalidParams"})),
    );
}

#[test]
fn submission_evidence_is_required_only_for_submission_errors() {
    for (code, kind) in [
        (-32050, "backendUnavailable"),
        (-32051, "backendTimeout"),
        (-32053, "overloaded"),
        (-32054, "backendDataInvalid"),
    ] {
        agrees("getBalance", code, json!({"kind": kind}), true);
        agrees("sendTransaction", code, json!({"kind": kind}), false);
        for admission in ["notAttempted", "rejected", "unknown"] {
            let data = json!({"kind": kind, "admission": admission, "transactionHash": HASH});
            agrees("sendTransaction", code, data.clone(), true);
            agrees("getBalance", code, data.clone(), false);
            agrees("faucet_requestAirdrop", code, data, false);
        }
    }
    for admission in ["rejected", "unknown", "accepted"] {
        agrees(
            "sendTransaction",
            -32000,
            json!({"kind": "transactionInvalid", "admission": admission}),
            false,
        );
    }
    agrees(
        "sendTransaction",
        -32001,
        json!({"kind": "transactionRejected", "admission": "unknown"}),
        false,
    );
    agrees(
        "sendTransaction",
        -32602,
        json!({"kind": "invalidParams", "admission": "notAttempted"}),
        false,
    );
    agrees(
        "getBalance",
        -32602,
        json!({"kind": "invalidCursor"}),
        false,
    );
    let observed = json!({"kind": "backendDataInvalid", "reason": "inconsistentResult", "observation": {"transactionHash": HASH, "blockHeight": "42"}});
    agrees("getTransactionReceipt", -32054, observed.clone(), true);
    agrees("getBalance", -32054, observed.clone(), false);
    let mut submit = observed;
    submit["admission"] = json!("unknown");
    agrees("sendTransaction", -32054, submit, false);
}

#[test]
fn native_bound_details_preserve_the_legacy_projection() {
    let details = [
        TxInvalidDetail::TooManyContracts {
            attached: 5,
            max: 4,
        },
        TxInvalidDetail::ContractTooLarge {
            index: 1,
            size_bytes: 300,
            max_bytes: 256,
        },
        TxInvalidDetail::DuplicateContract {
            index: 2,
            first_index: 1,
        },
        TxInvalidDetail::Rv32MemoryTooLarge {
            index: 0,
            memory_bytes: 300,
            max_bytes: 256,
        },
        TxInvalidDetail::Rv32CodeTooLarge {
            index: 0,
            code_bytes: 300,
            max_bytes: 256,
        },
    ];
    for detail in details {
        let projected = serde_json::to_value(detail).unwrap();
        let native: err::ExecutorBoundDetail = serde_json::from_value(projected.clone()).unwrap();
        assert_eq!(serde_json::to_value(native).unwrap(), projected);
        let data = json!({"kind": "transactionInvalid", "admission": "notAttempted", "reason": "executorBoundViolation", "detail": projected});
        agrees("sendTransaction", -32000, data.clone(), true);
        for key in projected
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k != "reason")
        {
            for value in [json!(0), json!(u32::MAX)] {
                let mut valid = data.clone();
                valid["detail"][key] = value;
                agrees("sendTransaction", -32000, valid, true);
            }
            for value in [
                json!(-1),
                json!(u64::from(u32::MAX) + 1),
                json!("1"),
                Value::Null,
            ] {
                let mut bad = data.clone();
                bad["detail"][key] = value;
                agrees("sendTransaction", -32000, bad, false);
            }
            let mut missing = data.clone();
            missing["detail"].as_object_mut().unwrap().remove(key);
            agrees("sendTransaction", -32000, missing, false);
        }
    }
    let future = json!({"kind": "transactionInvalid", "admission": "notAttempted", "reason": "futureReason", "detail": {"reason": "futureBound", "limit": "999"}});
    assert!(matches!(
        known("sendTransaction", -32000, future.clone()),
        err::ErrorData::TransactionInvalid {
            detail: Some(err::BoundDetail::Unrecognized(_)),
            ..
        }
    ));
    assert!(!validator("sendTransaction", -32000).is_valid(&future));
    let malformed = json!({"kind": "transactionInvalid", "admission": "notAttempted", "reason": "futureReason", "detail": {"reason": "tooManyContracts", "attached": 4}});
    unclassified("sendTransaction", envelope(-32000, malformed));
}

#[test]
fn integer_heights_follow_native_domains() {
    let height_cases = [
        (
            "simulateInvocation",
            -32012,
            json!({"kind": "stateHeightUnavailable", "requestedStateBlockHeight": "43", "observedStateBlockHeight": "42"}),
            "/requestedStateBlockHeight",
        ),
        (
            "simulateInvocation",
            -32012,
            json!({"kind": "stateHeightUnavailable", "requestedStateBlockHeight": "43", "observedStateBlockHeight": "42"}),
            "/observedStateBlockHeight",
        ),
        (
            "getEvents",
            -32013,
            json!({"kind": "historyUnavailable", "oldestIndexedBlockHeight": "0"}),
            "/oldestIndexedBlockHeight",
        ),
        (
            "getEvents",
            -32013,
            json!({"kind": "historyUnavailable", "latestIndexedBlockHeight": "0"}),
            "/latestIndexedBlockHeight",
        ),
        (
            "getTransactionReceipt",
            -32054,
            json!({"kind": "backendDataInvalid", "observation": {"transactionHash": HASH, "blockHeight": "0"}}),
            "/observation/blockHeight",
        ),
    ];
    for (method, code, data, pointer) in height_cases {
        for value in [
            json!("0"),
            json!("9007199254740993"),
            json!(i64::MAX.to_string()),
        ] {
            let mut valid = data.clone();
            *valid.pointer_mut(pointer).unwrap() = value;
            agrees(method, code, valid, true);
        }
        for value in [
            json!(0),
            json!("-1"),
            json!("01"),
            json!("+1"),
            json!("1.0"),
            json!("9223372036854775808"),
            Value::Null,
        ] {
            let mut bad = data.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            agrees(method, code, bad, false);
        }
    }
}

#[test]
fn retry_delays_follow_native_domains() {
    for (code, kind) in [
        (-32050, "backendUnavailable"),
        (-32051, "backendTimeout"),
        (-32053, "overloaded"),
    ] {
        for value in [json!("0"), json!(u64::MAX.to_string())] {
            agrees(
                "getBalance",
                code,
                json!({"kind": kind, "retryAfterMs": value}),
                true,
            );
        }
        for value in [
            json!(0),
            json!("-1"),
            json!("00"),
            json!("18446744073709551616"),
            Value::Null,
        ] {
            agrees(
                "getBalance",
                code,
                json!({"kind": kind, "retryAfterMs": value}),
                false,
            );
        }
    }
}

#[test]
fn optional_fields_reject_explicit_null() {
    for (method, code, mut data) in cases() {
        let optional = match data["kind"].as_str().unwrap() {
            "invalidParams" | "invalidCursor" => vec!["reason", "path"],
            "transactionInvalid" => vec!["reason", "detail", "transactionHash"],
            "transactionRejected" => vec!["reason", "error", "transactionHash"],
            "historyUnavailable" => vec![
                "reason",
                "oldestIndexedBlockHeight",
                "latestIndexedBlockHeight",
            ],
            "fundingAttemptStopped" => vec![
                "serviceCode",
                "transactionHash",
                "signedTransaction",
                "receipt",
            ],
            "localFundingUnresolved" => vec!["cause"],
            "backendUnavailable" | "backendTimeout" | "overloaded" => vec!["retryAfterMs"],
            "backendDataInvalid" => vec!["reason", "observation"],
            _ => vec![],
        };
        for field in optional {
            data[field] = Value::Null;
            agrees(method, code, data.clone(), false);
            data.as_object_mut().unwrap().remove(field);
        }
    }
}

#[test]
fn funding_stops_preserve_evidence_without_claiming_submission_admission() {
    for method in ["faucet_requestAirdrop", "dev_fundAccount"] {
        let full = json!({"kind": "fundingAttemptStopped", "transactionHash": HASH, "signedTransaction": "0x00", "receipt": receipt(), "serviceCode": "airdrop_transaction_failed"});
        agrees(method, -32030, full.clone(), true);
        for field in ["admission", "observation", "cause"] {
            let mut bad = full.clone();
            bad[field] = json!("unknown");
            agrees(method, -32030, bad, false);
        }
        for field in ["signedTransaction", "receipt"] {
            let mut missing_hash = full.clone();
            missing_hash
                .as_object_mut()
                .unwrap()
                .remove("transactionHash");
            missing_hash
                .as_object_mut()
                .unwrap()
                .retain(|k, _| k == "kind" || k == field);
            agrees(method, -32030, missing_hash, false);
        }
        let mut mismatch = full.clone();
        mismatch["receipt"]["transactionHash"] = json!(OTHER_HASH);
        assert!(validator(method, -32030).is_valid(&mismatch));
        unclassified(method, envelope(-32030, mismatch));
        let mut future = full.clone();
        future["receipt"]["execution"] = json!({"result": "future", "retain": {"x": 3}});
        assert!(matches!(
            known(method, -32030, future.clone()),
            err::ErrorData::FundingAttemptStopped(_)
        ));
        assert!(!validator(method, -32030).is_valid(&future));
        future["receipt"]["gasUsed"] = json!(7);
        unclassified(method, envelope(-32030, future));
        for value in ["", &"a".repeat(129)] {
            agrees(
                method,
                -32030,
                json!({"kind": "fundingAttemptStopped", "serviceCode": value}),
                false,
            );
        }
        for value in ["é", &"é".repeat(128)] {
            agrees(
                method,
                -32030,
                json!({"kind": "fundingAttemptStopped", "serviceCode": value}),
                true,
            );
        }
    }
    agrees(
        "getBalance",
        -32054,
        json!({"kind": "backendDataInvalid", "serviceCode": "stopped"}),
        false,
    );
    agrees(
        "faucet_requestAirdrop",
        -32050,
        json!({"kind": "backendUnavailable", "serviceCode": "future_service_code"}),
        true,
    );
}

#[test]
fn evidence_objects_validate_known_fields_even_with_future_refinements() {
    for code in [i32::MIN, 0, i32::MAX] {
        agrees(
            "sendTransaction",
            -32001,
            json!({"kind": "transactionRejected", "admission": "rejected", "error": {"source": "auth", "code": code}}),
            true,
        );
    }
    for value in [
        json!(i64::from(i32::MIN) - 1),
        json!(i64::from(i32::MAX) + 1),
        json!("1"),
        Value::Null,
    ] {
        agrees(
            "sendTransaction",
            -32001,
            json!({"kind": "transactionRejected", "admission": "rejected", "error": {"source": "auth", "code": value}}),
            false,
        );
    }
    let future = json!({"kind": "transactionRejected", "admission": "rejected", "reason": "futureReason", "error": {"source": "futureSubsystem", "code": 1}});
    known("sendTransaction", -32001, future.clone());
    assert!(!validator("sendTransaction", -32001).is_valid(&future));
    let mut malformed = future;
    malformed["error"].as_object_mut().unwrap().remove("code");
    unclassified("sendTransaction", envelope(-32001, malformed));
    for data in [json!([]), json!(["backendTimeout", "unknown"])] {
        agrees("sendTransaction", -32051, data, false);
    }
    for bad in [
        json!([]),
        json!([HASH, "42"]),
        json!({"transactionHash": HASH}),
        json!({"blockHeight": "42"}),
    ] {
        agrees(
            "getTransactionReceipt",
            -32054,
            json!({"kind": "backendDataInvalid", "observation": bad}),
            false,
        );
    }
    for bad in ["0X00", "00", "0x0", "0xAB"] {
        agrees(
            "dev_fundAccount",
            -32031,
            json!({"kind": "localFundingUnresolved", "transactionHash": HASH, "signedTransaction": bad}),
            false,
        );
    }
    // Base58's JSON Schema describes spelling; decoding also enforces byte width.
    unclassified(
        "sendTransaction",
        envelope(
            -32051,
            json!({"kind": "backendTimeout", "admission": "unknown", "transactionHash": "1"}),
        ),
    );
}

#[test]
fn unresolved_local_funding_only_wraps_a_consistent_stop() {
    let mut full = json!({"kind": "localFundingUnresolved", "transactionHash": HASH, "signedTransaction": "0x00", "cause": {"kind": "fundingAttemptStopped", "transactionHash": HASH, "signedTransaction": "0x00", "receipt": receipt()}});
    agrees("dev_fundAccount", -32031, full.clone(), true);
    for field in ["transactionHash", "signedTransaction"] {
        let mut missing = full.clone();
        missing.as_object_mut().unwrap().remove(field);
        agrees("dev_fundAccount", -32031, missing, false);
    }
    for (field, value) in [
        ("transactionHash", json!(OTHER_HASH)),
        ("signedTransaction", json!("0x01")),
    ] {
        let mut mismatch = full.clone();
        mismatch["cause"][field] = value;
        unclassified("dev_fundAccount", envelope(-32031, mismatch));
    }
    for cause in [
        json!({"kind": "backendTimeout"}),
        json!({"kind": "future"}),
        json!({"kind": "localFundingUnresolved", "transactionHash": HASH, "signedTransaction": "0x00"}),
        json!({"kind": "fundingAttemptStopped", "admission": "notAttempted"}),
        json!({"kind": "fundingAttemptStopped", "signedTransaction": "0x00"}),
    ] {
        full["cause"] = cause;
        agrees("dev_fundAccount", -32031, full.clone(), false);
    }
    full["cause"] = json!({"kind": "fundingAttemptStopped"});
    agrees("dev_fundAccount", -32031, full.clone(), true);
    unclassified("faucet_requestAirdrop", envelope(-32031, full));
}

#[tokio::test]
async fn http_call_errors_preserve_known_future_and_framework_data() {
    let fixtures = [
        (
            "known",
            "sendTransaction",
            -32051,
            json!({"kind": "backendTimeout", "admission": "unknown"}),
            true,
        ),
        (
            "future_reason",
            "getTransactionReceipt",
            -32054,
            json!({"kind": "backendDataInvalid", "reason": "futureReason", "extra": [1, 2]}),
            true,
        ),
        (
            "malformed",
            "sendTransaction",
            -32051,
            json!({"kind": "backendTimeout", "admission": "future"}),
            false,
        ),
        (
            "framework",
            "sendTransaction",
            -32008,
            json!("maximum response size exceeded"),
            false,
        ),
    ];
    let mut module = RpcModule::new(());
    for (name, _, code, data, _) in &fixtures {
        let error = ErrorObjectOwned::owned(*code, "diagnostic only", Some(data.clone()));
        module
            .register_method(name, move |_, (), _| Err::<Value, _>(error.clone()))
            .unwrap();
    }
    let server = Server::builder().build("127.0.0.1:0").await.unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", server.local_addr().unwrap()))
        .unwrap();
    let handle = server.start(module);
    for (name, method, code, data, expected_known) in fixtures {
        let error = client
            .request::<Value, _>(name, NamedParams(EmptyRequest {}))
            .await
            .unwrap_err();
        let ClientError::Call(error) = error else {
            panic!("expected JSON-RPC call error");
        };
        let raw = serde_json::to_value(error).unwrap();
        assert_eq!(raw, envelope(code, data));
        let decoded = err::DecodedError::decode(method, raw.clone());
        assert_eq!(decoded.raw(), &raw);
        assert_eq!(
            matches!(decoded, err::DecodedError::Known { .. }),
            expected_known
        );
    }
    handle.stop().unwrap();
    handle.stopped().await;
}

#[test]
fn paths_are_json_pointers_without_invalid_escapes() {
    for kind in ["invalidParams", "invalidCursor"] {
        for path in [
            "",
            "/address",
            "/invocations/0/address",
            "/~0~1",
            "/line\nbreak",
        ] {
            agrees(
                "getEvents",
                -32602,
                json!({"kind": kind, "path": path}),
                true,
            );
        }
        for path in ["address", "#/address", "/~", "/~2", "/foo~bar"] {
            agrees(
                "getEvents",
                -32602,
                json!({"kind": kind, "path": path}),
                false,
            );
        }
    }
}

#[test]
fn producer_service_codes_have_a_checked_constructor() {
    for code in ["stopped".to_owned(), "é".repeat(128)] {
        let typed = err::ServiceCode::try_from(code.clone()).unwrap();
        assert_eq!(typed.as_str(), code);
        assert_eq!(serde_json::to_value(typed).unwrap(), json!(code));
    }
    for code in [String::new(), "é".repeat(129)] {
        assert!(err::ServiceCode::try_from(code).is_err());
    }
}
