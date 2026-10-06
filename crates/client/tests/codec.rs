use groundhog_contract_spec::ContractField;
use groundhog_contract_spec::ContractSpecId;
use groundhog_contract_spec::ContractSpecMethod;
use groundhog_contract_spec::ContractSpecString;
use groundhog_contract_spec::ContractType;
use groundhog_contract_spec::ContractTypeDef;
use groundhog_contract_spec::ContractUnionCase;
use groundhog_contract_spec::ContractUnionCaseType;
use groundhog_contract_spec::ContractUnionDef;

use groundhog_client::codec::*;

fn spec_string(s: &str) -> ContractSpecString {
    s.try_into().unwrap()
}

/// A spec id for these fixtures: one crate, one module, so the declared name
/// is what tells the types apart -- which is all these tests turn on. Tests
/// that turn on identity itself live in the spec tool's canonicalizer.
fn spec_id(declared: &str) -> ContractSpecId {
    ContractSpecId {
        crate_name: spec_string("counter"),
        major: spec_string("1"),
        module_path: spec_string("counter"),
        declared: spec_string(declared),
    }
}

fn numeric_tuple_metadata(ty: ContractType) -> CallMetadata {
    let tuple = ContractType::Tuple {
        tuple: vec![ty].try_into().unwrap(),
    };
    CallMetadata::Spec(SpecMethodMetadata {
        interface_name: "Numbers".into(),
        version: "1".into(),
        method: ContractSpecMethod {
            name: spec_string("echo"),
            args: vec![ContractField {
                name: spec_string("values"),
                ty: tuple.clone(),
            }]
            .try_into()
            .unwrap(),
            return_type: tuple,
        },
        type_defs: vec![],
        throws: vec![],
    })
}

#[test]
fn nested_wide_integers_reject_numbers_and_noncanonical_strings() {
    for ty in [
        ContractType::U64,
        ContractType::I64,
        ContractType::U128,
        ContractType::I128,
        ContractType::U256,
        ContractType::I256,
    ] {
        let metadata = numeric_tuple_metadata(ty);
        for value in [
            serde_json::json!(0),
            serde_json::json!(7),
            serde_json::json!(9_007_199_254_740_993u64),
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!(""),
            serde_json::json!("01"),
            serde_json::json!("-01"),
            serde_json::json!("+1"),
            serde_json::json!("-0"),
            serde_json::json!("-"),
            serde_json::json!(" 1"),
            serde_json::json!("1\n"),
            serde_json::json!("1.0"),
            serde_json::json!("1e3"),
        ] {
            let argument = serde_json::json!([value]).to_string();
            assert!(
                encode_args(std::slice::from_ref(&argument), Some(&metadata)).is_err(),
                "accepted {argument} with {metadata:?}"
            );
        }
    }
}

fn assert_numeric_tuple_roundtrip<T: std::fmt::Display + xdrs::WriteXDR>(
    ty: ContractType,
    values: &[T],
) {
    let metadata = numeric_tuple_metadata(ty);
    for value in values {
        let json = serde_json::json!([value.to_string()]);
        let encoded = encode_args(&[json.to_string()], Some(&metadata)).unwrap();
        assert_eq!(encoded, xdrs::cxs::encode(value).unwrap(), "{json}");
        assert_eq!(decode_return(&encoded, Some(&metadata)), Some(json));
    }
}

#[test]
fn nested_wide_integers_preserve_precision_and_type_boundaries() {
    assert_numeric_tuple_roundtrip(
        ContractType::U64,
        &[0, 1, (1 << 53) - 1, 1 << 53, (1 << 53) + 1, u64::MAX],
    );
    assert_numeric_tuple_roundtrip(
        ContractType::I64,
        &[i64::MIN, -((1 << 53) + 1), -1, 0, (1 << 53) + 1, i64::MAX],
    );
    assert_numeric_tuple_roundtrip(ContractType::U128, &[0, 1, u128::MAX]);
    assert_numeric_tuple_roundtrip(ContractType::I128, &[i128::MIN, -1, 0, i128::MAX]);
    assert_numeric_tuple_roundtrip(
        ContractType::U256,
        &[
            "0".parse().unwrap(),
            "1".parse().unwrap(),
            groundhog_u256::U256::MAX,
        ],
    );
    assert_numeric_tuple_roundtrip(
        ContractType::I256,
        &[
            groundhog_u256::I256::MIN,
            "-1".parse().unwrap(),
            "0".parse().unwrap(),
            "1".parse().unwrap(),
            groundhog_u256::I256::MAX,
        ],
    );
}

#[test]
fn nested_wide_integers_reject_out_of_range_strings() {
    for (ty, values) in [
        (ContractType::U64, ["-1", "18446744073709551616"]),
        (
            ContractType::I64,
            ["-9223372036854775809", "9223372036854775808"],
        ),
        (
            ContractType::U128,
            ["-1", "340282366920938463463374607431768211456"],
        ),
        (
            ContractType::I128,
            [
                "-170141183460469231731687303715884105729",
                "170141183460469231731687303715884105728",
            ],
        ),
        (
            ContractType::U256,
            [
                "-1",
                "115792089237316195423570985008687907853269984665640564039457584007913129639936",
            ],
        ),
        (
            ContractType::I256,
            [
                "-57896044618658097711785492504343953926634992332820282019728792003956564819969",
                "57896044618658097711785492504343953926634992332820282019728792003956564819968",
            ],
        ),
    ] {
        let metadata = numeric_tuple_metadata(ty);
        for value in values {
            let argument = serde_json::json!([value]).to_string();
            assert!(
                encode_args(&[argument], Some(&metadata)).is_err(),
                "accepted {value} with {metadata:?}"
            );
        }
    }
}

#[test]
fn nested_narrow_integers_still_accept_json_numbers() {
    for ty in [ContractType::U8, ContractType::U32, ContractType::I32] {
        let metadata = numeric_tuple_metadata(ty);
        let encoded = encode_args(&["[7]".into()], Some(&metadata)).unwrap();
        assert_eq!(
            decode_return(&encoded, Some(&metadata)),
            Some(serde_json::json!([7]))
        );
    }
}

#[test]
fn spec_string_text_accepts_valid_identifiers() {
    assert_eq!(
        spec_string_text(&spec_string("Counter")).unwrap(),
        "Counter"
    );
    assert_eq!(
        spec_string_text(&spec_string("get_balance")).unwrap(),
        "get_balance"
    );
    assert_eq!(
        spec_string_text(&spec_string("ERC20Error")).unwrap(),
        "ERC20Error"
    );
    assert_eq!(spec_string_text(&spec_string("0.1.0")).unwrap(), "0.1.0");
}

#[test]
fn spec_string_text_rejects_quotes() {
    let s = spec_string("bad\"name");
    let err = spec_string_text(&s).unwrap_err();
    assert!(err.to_string().contains("invalid character"), "{err}");
}

#[test]
fn spec_string_text_rejects_backslash() {
    let s = spec_string("bad\\name");
    let err = spec_string_text(&s).unwrap_err();
    assert!(err.to_string().contains("invalid character"), "{err}");
}

#[test]
fn spec_string_text_rejects_whitespace() {
    let s = spec_string("bad name");
    let err = spec_string_text(&s).unwrap_err();
    assert!(err.to_string().contains("invalid character"), "{err}");
}

#[test]
fn spec_string_text_rejects_tab() {
    let s = spec_string("bad\tname");
    let err = spec_string_text(&s).unwrap_err();
    assert!(err.to_string().contains("invalid character"), "{err}");
}

const COUNTER_ERROR_SELECTOR: u64 = 0xb199_47ac_b2fa_14ff;

fn counter_error_type_defs() -> Vec<ContractTypeDef> {
    vec![ContractTypeDef::TypeDefUnion {
        union_def: ContractUnionDef {
            name: spec_id("CounterError"),
            cases: vec![
                ContractUnionCase {
                    name: spec_string("Underflow"),
                    discriminant: 0,
                    case_type: ContractUnionCaseType::UnionCaseVoid,
                },
                ContractUnionCase {
                    name: spec_string("Bounds"),
                    discriminant: 1,
                    case_type: ContractUnionCaseType::UnionCaseTuple {
                        tuple: vec![ContractType::U64].try_into().unwrap(),
                    },
                },
            ]
            .try_into()
            .unwrap(),
        },
    }]
}

fn throwing_spec_metadata() -> SpecMethodMetadata {
    SpecMethodMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        method: ContractSpecMethod {
            name: spec_string("decrement"),
            args: vec![].try_into().unwrap(),
            return_type: ContractType::Void,
        },
        type_defs: counter_error_type_defs(),
        throws: vec![groundhog_contract_spec::ContractExceptionType {
            selector: COUNTER_ERROR_SELECTOR,
            ty: ContractType::Named {
                named: spec_id("CounterError"),
            },
        }],
    }
}

/// Encode an abort buffer: cxs varint selector followed by a payload
/// value encoded through the spec arg encoder for `payload_ty`.
fn encode_abort_bytes(
    selector: u64,
    payload_ty: ContractType,
    type_defs: Vec<ContractTypeDef>,
    payload_json: &str,
) -> Vec<u8> {
    let payload_metadata = SpecMethodMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        method: ContractSpecMethod {
            name: spec_string("throwing"),
            args: vec![ContractField {
                name: spec_string("exn"),
                ty: payload_ty,
            }]
            .try_into()
            .unwrap(),
            return_type: ContractType::Void,
        },
        type_defs,
        throws: vec![],
    };
    let payload = encode_args(
        &[payload_json.to_owned()],
        Some(&CallMetadata::Spec(payload_metadata)),
    )
    .expect("bug: abort payload should encode");
    let mut bytes = xdrs::cxs::encode(&selector).expect("bug: selector should encode");
    bytes.extend_from_slice(&payload);
    bytes
}

#[test]
fn decode_abort_decodes_declared_spec_exception() {
    let metadata = CallMetadata::Spec(throwing_spec_metadata());
    let expected = serde_json::json!({"case": "Bounds", "value": ["7"]});
    let bytes = encode_abort_bytes(
        COUNTER_ERROR_SELECTOR,
        ContractType::Named {
            named: spec_id("CounterError"),
        },
        counter_error_type_defs(),
        &expected.to_string(),
    );

    let decoded =
        decode_abort(&bytes, Some(&metadata)).expect("bug: declared spec exception should decode");

    // COUNTER_ERROR_SELECTOR collides with the ambient ERC20Error selector;
    // a declared spec exception must win over the ambient fallback.
    assert_eq!(decoded.exception.as_deref(), Some("CounterError"));
    assert_eq!(decoded.value, expected);
    assert_eq!(decoded.exception_crate, None);
    assert_eq!(decoded.exception_version, None);
}

#[test]
fn decoded_abort_label_annotates_ambient_exceptions_only() {
    let ambient = DecodedAbort {
        exception: Some("LibraryExn".to_owned()),
        value: serde_json::Value::Null,
        exception_crate: Some("sdk"),
        exception_version: Some("1"),
    };
    assert_eq!(
        ambient.exception_label().as_deref(),
        Some("LibraryExn (sdk, v1)")
    );

    let spec_declared = DecodedAbort {
        exception: Some("CounterError".to_owned()),
        value: serde_json::Value::Null,
        exception_crate: None,
        exception_version: None,
    };
    assert_eq!(
        spec_declared.exception_label().as_deref(),
        Some("CounterError")
    );
}

#[test]
fn decode_abort_reports_unknown_selector() {
    let metadata = CallMetadata::Spec(throwing_spec_metadata());
    let bytes = encode_abort_bytes(
        0x1234_5678_9abc_def0,
        ContractType::Named {
            named: spec_id("CounterError"),
        },
        counter_error_type_defs(),
        "Underflow",
    );

    let err =
        decode_abort(&bytes, Some(&metadata)).expect_err("bug: unknown selector must be reported");

    let message = err.to_string();
    assert!(message.contains("0x123456789abcdef0"), "{message}");
    assert!(message.contains("does not match"), "{message}");
    // The parsed-off payload is reported separately from the selector.
    let payload_hex = hex::encode(&bytes[9..]);
    assert!(message.contains(&payload_hex), "{message}");
}

#[test]
fn decode_abort_reports_trailing_bytes() {
    let metadata = CallMetadata::Spec(throwing_spec_metadata());
    let mut bytes = encode_abort_bytes(
        COUNTER_ERROR_SELECTOR,
        ContractType::Named {
            named: spec_id("CounterError"),
        },
        counter_error_type_defs(),
        "Underflow",
    );
    bytes.push(0x00);

    let err =
        decode_abort(&bytes, Some(&metadata)).expect_err("bug: trailing bytes must be reported");

    let message = err.to_string();
    assert!(message.contains("CounterError"), "{message}");
    assert!(message.contains("unread byte"), "{message}");
    assert!(message.contains("0xb19947acb2fa14ff"), "{message}");
}

#[test]
fn decode_abort_reports_undecodable_declared_payload() {
    let metadata = CallMetadata::Spec(throwing_spec_metadata());
    let mut bytes =
        xdrs::cxs::encode(&COUNTER_ERROR_SELECTOR).expect("bug: selector should encode");
    bytes.extend_from_slice(&xdrs::cxs::encode(&99i32).expect("bug: discriminant should encode"));

    let err = decode_abort(&bytes, Some(&metadata))
        .expect_err("bug: undecodable payload must be reported");

    let message = err.to_string();
    assert!(message.contains("CounterError"), "{message}");
    assert!(message.contains("unknown union discriminant"), "{message}");
}

#[test]
fn decode_abort_reports_unreadable_selector() {
    let err = decode_abort(&[], None).expect_err("bug: empty abort buffer must be reported");
    assert!(err.to_string().contains("selector"), "{err}");
}

#[test]
fn strkey_contract_arguments_match_hex_and_reject_wrong_type() {
    let hex_address = format!("0x{}", "42".repeat(32));
    let contract_strkey = groundhog_signing::strkey::format_contract_address(&[0x42; 32].into());
    let public_key_strkey = groundhog_signing::strkey::format_public_key(&[0x42; 32]);
    let metadata = CallMetadata::Local(ContractMetadata {
        interface_name: "test".into(),
        version: "1".into(),
        source: std::path::PathBuf::default(),
        method: MethodMetadata {
            arg_types: vec!["ContractAddress".into()],
            return_type: None,
        },
    });
    assert_eq!(
        encode_args(std::slice::from_ref(&hex_address), Some(&metadata)).unwrap(),
        encode_args(std::slice::from_ref(&contract_strkey), Some(&metadata)).unwrap()
    );
    assert!(encode_args(&[public_key_strkey], Some(&metadata)).is_err());
}

#[test]
fn strkey_public_key_arguments_match_hex_and_reject_wrong_type() {
    let hex_address = format!("0x{}", "42".repeat(32));
    let contract_strkey = groundhog_signing::strkey::format_contract_address(&[0x42; 32].into());
    let public_key_strkey = groundhog_signing::strkey::format_public_key(&[0x42; 32]);
    let metadata = CallMetadata::Local(ContractMetadata {
        interface_name: "test".into(),
        version: "1".into(),
        source: std::path::PathBuf::default(),
        method: MethodMetadata {
            arg_types: vec!["Ed25519PublicKey".into()],
            return_type: None,
        },
    });
    assert_eq!(
        encode_args(std::slice::from_ref(&hex_address), Some(&metadata)).unwrap(),
        encode_args(std::slice::from_ref(&public_key_strkey), Some(&metadata)).unwrap()
    );
    assert!(encode_args(&[contract_strkey], Some(&metadata)).is_err());
}

#[test]
fn strkey_spec_address_arguments_match_hex_and_reject_wrong_type() {
    let hex_address = format!("0x{}", "42".repeat(32));
    let contract_strkey = groundhog_signing::strkey::format_contract_address(&[0x42; 32].into());
    let public_key_strkey = groundhog_signing::strkey::format_public_key(&[0x42; 32]);
    let metadata = CallMetadata::Spec(SpecMethodMetadata {
        interface_name: "test".into(),
        version: "1".into(),
        method: ContractSpecMethod {
            name: spec_string("send"),
            args: vec![ContractField {
                name: spec_string("to"),
                ty: ContractType::Address,
            }]
            .try_into()
            .unwrap(),
            return_type: ContractType::Void,
        },
        type_defs: vec![],
        throws: vec![],
    });
    assert_eq!(
        encode_args(std::slice::from_ref(&hex_address), Some(&metadata)).unwrap(),
        encode_args(std::slice::from_ref(&contract_strkey), Some(&metadata)).unwrap()
    );
    assert!(encode_args(&[public_key_strkey], Some(&metadata)).is_err());
}
