use std::path::PathBuf;

use serde_json::json;
use thruput_contract_spec::ContractArrayType;
use thruput_contract_spec::ContractEnumDef;
use thruput_contract_spec::ContractEnumVariant;
use thruput_contract_spec::ContractField;
use thruput_contract_spec::ContractSpecId;
use thruput_contract_spec::ContractSpecMethod;
use thruput_contract_spec::ContractSpecString;
use thruput_contract_spec::ContractStructDef;
use thruput_contract_spec::ContractType;
use thruput_contract_spec::ContractTypeDef;
use thruput_contract_spec::ContractUnionCase;
use thruput_contract_spec::ContractUnionCaseType;
use thruput_contract_spec::ContractUnionDef;
use thruput_contract_spec::ContractVectorType;

use thruput_client::codec::*;

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
            effect: None,
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
            thruput_u256::U256::MAX,
        ],
    );
    assert_numeric_tuple_roundtrip(
        ContractType::I256,
        &[
            thruput_u256::I256::MIN,
            "-1".parse().unwrap(),
            "0".parse().unwrap(),
            "1".parse().unwrap(),
            thruput_u256::I256::MAX,
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
fn spec_string_text_rejects_invalid_characters() {
    for (case, text) in [
        ("double quote", "bad\"name"),
        ("backslash", "bad\\name"),
        ("space", "bad name"),
        ("tab", "bad\tname"),
    ] {
        let err = spec_string_text(&spec_string(text)).unwrap_err();
        assert!(
            err.to_string().contains("invalid character"),
            "{case}: {err}"
        );
    }
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
            effect: None,
        },
        type_defs: counter_error_type_defs(),
        throws: vec![thruput_contract_spec::ContractExceptionType {
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
            effect: None,
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
fn strkey_contract_arguments_match_base58_and_reject_wrong_type() {
    let base58_address = thruput_encoding::encode(&[0x42; 32]);
    let contract_strkey = thruput_signing::strkey::format_contract_address(&[0x42; 32].into());
    let public_key_strkey = thruput_signing::strkey::format_public_key(&[0x42; 32]);
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
        encode_args(std::slice::from_ref(&base58_address), Some(&metadata)).unwrap(),
        encode_args(std::slice::from_ref(&contract_strkey), Some(&metadata)).unwrap()
    );
    assert!(encode_args(&[public_key_strkey], Some(&metadata)).is_err());
    assert!(
        encode_args(
            &["CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".into()],
            Some(&metadata)
        )
        .is_err()
    );
}

#[test]
fn typed_identifiers_preserve_binary_bytes_and_decode_to_base58() {
    let bytes = [0x42; 32];
    let text = thruput_encoding::encode(&bytes);
    for ty in ["ContractAddress", "Ed25519PublicKey", "SHA256Hash"] {
        let metadata = CallMetadata::Local(ContractMetadata {
            interface_name: "test".into(),
            version: "1".into(),
            source: std::path::PathBuf::default(),
            method: MethodMetadata {
                arg_types: vec![ty.into()],
                return_type: Some(ty.into()),
            },
        });
        let encoded = encode_args(std::slice::from_ref(&text), Some(&metadata)).unwrap();
        assert_eq!(encoded, bytes, "{ty}");
        assert_eq!(
            decode_return(&encoded, Some(&metadata)),
            Some(text.clone().into())
        );
        assert!(encode_args(&[format!("0x{}", "42".repeat(32))], Some(&metadata)).is_err());
    }
    let metadata = CallMetadata::Local(ContractMetadata {
        interface_name: "test".into(),
        version: "1".into(),
        source: std::path::PathBuf::default(),
        method: MethodMetadata {
            arg_types: vec!["ContractEntrypointID".into()],
            return_type: Some("ContractEntrypointID".into()),
        },
    });
    let selector = "0x4242424242424242".to_owned();
    let encoded = encode_args(std::slice::from_ref(&selector), Some(&metadata)).unwrap();
    assert_eq!(encoded, [0x42; 8]);
    assert_eq!(
        decode_return(&encoded, Some(&metadata)),
        Some(selector.into())
    );
}

#[test]
fn spec_address_and_same_length_raw_bytes_have_distinct_text_encodings() {
    let bytes = [0x42; 32];
    let text = thruput_encoding::encode(&bytes);
    for (ty, value) in [
        (ContractType::Address, text),
        (ContractType::Bytes, format!("0x{}", "42".repeat(32))),
    ] {
        let metadata = numeric_tuple_metadata(ty);
        let input = serde_json::json!([value]);
        let encoded = encode_args(&[input.to_string()], Some(&metadata)).unwrap();
        assert_eq!(decode_return(&encoded, Some(&metadata)), Some(input));
    }
}

#[test]
fn strkey_public_key_arguments_match_base58_and_reject_wrong_type() {
    let base58_address = thruput_encoding::encode(&[0x42; 32]);
    let contract_strkey = thruput_signing::strkey::format_contract_address(&[0x42; 32].into());
    let public_key_strkey = thruput_signing::strkey::format_public_key(&[0x42; 32]);
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
        encode_args(std::slice::from_ref(&base58_address), Some(&metadata)).unwrap(),
        encode_args(std::slice::from_ref(&public_key_strkey), Some(&metadata)).unwrap()
    );
    assert!(encode_args(&[contract_strkey], Some(&metadata)).is_err());
}

#[test]
fn strkey_spec_address_arguments_match_base58_and_reject_wrong_type() {
    let base58_address = thruput_encoding::encode(&[0x42; 32]);
    let contract_strkey = thruput_signing::strkey::format_contract_address(&[0x42; 32].into());
    let public_key_strkey = thruput_signing::strkey::format_public_key(&[0x42; 32]);
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
            effect: None,
        },
        type_defs: vec![],
        throws: vec![],
    });
    assert_eq!(
        encode_args(std::slice::from_ref(&base58_address), Some(&metadata)).unwrap(),
        encode_args(std::slice::from_ref(&contract_strkey), Some(&metadata)).unwrap()
    );
    assert!(encode_args(&[public_key_strkey], Some(&metadata)).is_err());
    assert!(
        encode_args(
            &["CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".into()],
            Some(&metadata)
        )
        .is_err()
    );
}

#[test]
fn encodes_single_typed_argument_from_local_metadata() {
    let metadata = ContractMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        source: PathBuf::from("/tmp/implementation.rs"),
        method: MethodMetadata {
            arg_types: vec!["u64".into()],
            return_type: None,
        },
    };
    let encoded = encode_args(&["5".into()], Some(&CallMetadata::Local(metadata)))
        .expect("bug: typed calldata should encode");
    let decoded: u64 =
        xdrs::cxs::decode(&encoded).expect("bug: encoded calldata should decode as u64");
    assert_eq!(decoded, 5);
}

#[test]
fn decodes_simple_return_value() {
    let metadata = ContractMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        source: PathBuf::from("/tmp/implementation.rs"),
        method: MethodMetadata {
            arg_types: vec![],
            return_type: Some("u64".into()),
        },
    };
    let bytes = xdrs::cxs::encode(&7_u64).expect("bug: u64 should encode");
    let decoded = decode_return(&bytes, Some(&CallMetadata::Local(metadata)))
        .expect("bug: simple return value should decode");
    assert_eq!(decoded, serde_json::json!("7"));
}

#[test]
fn encodes_and_decodes_local_str_values() {
    let metadata = ContractMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        source: PathBuf::from("/tmp/implementation.rs"),
        method: MethodMetadata {
            arg_types: vec!["&str".into()],
            return_type: Some("&str".into()),
        },
    };
    let encoded = encode_args(
        &["hello".into()],
        Some(&CallMetadata::Local(metadata.clone())),
    )
    .expect("bug: local string calldata should encode");
    let decoded = decode_return(&encoded, Some(&CallMetadata::Local(metadata)))
        .expect("bug: local string return should decode");
    assert_eq!(decoded, serde_json::json!("hello"));
}

#[test]
fn encodes_and_decodes_local_native_256_values() {
    for (ty, value) in [
        ("U256", thruput_u256::U256::MAX.to_string()),
        ("thruput_sdk::U256", thruput_u256::U256::MAX.to_string()),
        ("sdk::U256", thruput_u256::U256::MAX.to_string()),
        ("groundhog_sdk::U256", thruput_u256::U256::MAX.to_string()),
        ("I256", thruput_u256::I256::MIN.to_string()),
        ("thruput_sdk::I256", thruput_u256::I256::MIN.to_string()),
        ("sdk::I256", thruput_u256::I256::MIN.to_string()),
        ("groundhog_sdk::I256", thruput_u256::I256::MIN.to_string()),
    ] {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: PathBuf::from("/tmp/implementation.rs"),
            method: MethodMetadata {
                arg_types: vec![ty.into()],
                return_type: Some(ty.into()),
            },
        };

        let encoded = encode_args(
            std::slice::from_ref(&value),
            Some(&CallMetadata::Local(metadata.clone())),
        )
        .expect("bug: local native 256-bit calldata should encode");
        let decoded = decode_return(&encoded, Some(&CallMetadata::Local(metadata)))
            .expect("bug: local native 256-bit return should decode");

        assert_eq!(decoded, json!(value));
    }
}

#[test]
fn encodes_and_decodes_embedded_spec_structured_arguments() {
    let metadata = sample_structured_spec_metadata(
        vec![
            contract_field("payload", named_type("Payload")),
            contract_field("mode", named_type("Mode")),
            contract_field(
                "maybe_count",
                ContractType::Option {
                    option_inner: Box::new(ContractType::U32),
                },
            ),
            contract_field(
                "digest",
                ContractType::Array {
                    array: ContractArrayType {
                        element: Box::new(ContractType::U8),
                        len: 4,
                    },
                },
            ),
            contract_field(
                "flags",
                ContractType::Vector {
                    vector: ContractVectorType {
                        element: Box::new(ContractType::Bool),
                        max_len: 8,
                    },
                },
            ),
            contract_field(
                "pair",
                ContractType::Tuple {
                    tuple: vec![ContractType::U32, ContractType::String]
                        .try_into()
                        .unwrap(),
                },
            ),
        ],
        ContractType::Void,
    );

    let encoded = encode_args(
        &[
            r#"{"count":7,"bytes":"hi"}"#.into(),
            "Ready".into(),
            "5".into(),
            "0x01020304".into(),
            "[true,false,true]".into(),
            r#"[7,"ok"]"#.into(),
        ],
        Some(&CallMetadata::Spec(metadata.clone())),
    )
    .expect("bug: embedded spec calldata should encode");

    assert_eq!(
        decode_spec_arguments(&metadata, &encoded),
        vec![
            json!({"count": 7, "bytes": "0x6869"}),
            json!("Ready"),
            json!(5),
            json!("0x01020304"),
            json!([true, false, true]),
            json!([7, "ok"]),
        ]
    );
}

#[test]
fn encodes_and_decodes_embedded_spec_unions() {
    let metadata = sample_structured_spec_metadata(
        vec![contract_field("envelope", named_type("Envelope"))],
        named_type("Envelope"),
    );

    let full_case = json!({
        "case": "Full",
        "value": {
            "payload": {
                "count": 9,
                "bytes": "0xaabb"
            },
            "enabled": true
        }
    });
    let encoded = encode_args(
        &[full_case.to_string()],
        Some(&CallMetadata::Spec(metadata.clone())),
    )
    .expect("bug: union calldata should encode");

    let type_defs =
        build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
    let mut reader = xdrs::cxs::buf_reader::BufReader::new(&encoded);
    let decoded =
        decode_spec_value_from_reader(&metadata.method.args[0].ty, &mut reader, &type_defs, "arg0")
            .expect("bug: union arg should decode");
    assert_eq!(decoded, full_case);
    assert_eq!(reader.remaining(), 0);

    let pair_case = json!({
        "case": "Pair",
        "value": ["Busy", 9]
    });
    let pair_bytes = encode_args(
        &[pair_case.to_string()],
        Some(&CallMetadata::Spec(metadata.clone())),
    )
    .expect("bug: tuple union case should encode");
    let decoded_pair = decode_return(&pair_bytes, Some(&CallMetadata::Spec(metadata.clone())))
        .expect("bug: union return should decode");
    assert_eq!(decoded_pair, pair_case);

    let empty_bytes = encode_args(
        &["Empty".into()],
        Some(&CallMetadata::Spec(metadata.clone())),
    )
    .expect("bug: void union shorthand should encode");
    let decoded_empty = decode_return(&empty_bytes, Some(&CallMetadata::Spec(metadata)))
        .expect("bug: void union return should decode");
    assert_eq!(decoded_empty, json!({"case": "Empty"}));
}

#[test]
fn encodes_and_decodes_embedded_spec_large_integers() {
    let u128_value = u128::MAX.to_string();
    let u128_metadata = sample_single_arg_spec_metadata(ContractType::U128);
    let u128_bytes = encode_args(
        std::slice::from_ref(&u128_value),
        Some(&CallMetadata::Spec(u128_metadata.clone())),
    )
    .expect("bug: embedded spec u128 should encode");
    assert_eq!(
        decode_spec_arguments(&u128_metadata, &u128_bytes),
        vec![json!(u128_value.clone())]
    );
    assert_eq!(
        decode_return(&u128_bytes, Some(&CallMetadata::Spec(u128_metadata)))
            .expect("bug: embedded spec u128 return should decode"),
        json!(u128_value)
    );

    let i128_value = i128::MIN.to_string();
    let i128_metadata = sample_single_arg_spec_metadata(ContractType::I128);
    let i128_bytes = encode_args(
        std::slice::from_ref(&i128_value),
        Some(&CallMetadata::Spec(i128_metadata.clone())),
    )
    .expect("bug: embedded spec i128 should encode");
    assert_eq!(
        decode_spec_arguments(&i128_metadata, &i128_bytes),
        vec![json!(i128_value.clone())]
    );
    assert_eq!(
        decode_return(&i128_bytes, Some(&CallMetadata::Spec(i128_metadata)))
            .expect("bug: embedded spec i128 return should decode"),
        json!(i128_value)
    );

    let u256_value = thruput_u256::U256::MAX.to_string();
    let i256_value = thruput_u256::I256::MIN.to_string();
    let metadata = sample_single_arg_spec_metadata(ContractType::Tuple {
        tuple: vec![ContractType::U256, ContractType::I256]
            .try_into()
            .unwrap(),
    });
    let argument = json!([u256_value, i256_value]).to_string();
    let bytes = encode_args(&[argument], Some(&CallMetadata::Spec(metadata.clone())))
        .expect("bug: embedded spec native 256-bit integers should encode");

    assert_eq!(
        decode_spec_arguments(&metadata, &bytes),
        vec![json!([
            thruput_u256::U256::MAX.to_string(),
            thruput_u256::I256::MIN.to_string()
        ])]
    );
}

#[test]
fn typed_native_256_arguments_enforce_ranges() {
    const U256_OVERFLOW: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639936";
    const I256_OVERFLOW: &str =
        "57896044618658097711785492504343953926634992332820282019728792003956564819968";
    const I256_UNDERFLOW: &str =
        "-57896044618658097711785492504343953926634992332820282019728792003956564819969";

    for (ty, value, expected) in [
        (ContractType::U256, U256_OVERFLOW, "U256"),
        (ContractType::I256, I256_OVERFLOW, "I256"),
        (ContractType::I256, I256_UNDERFLOW, "I256"),
    ] {
        let error = encode_args(
            &[value.to_owned()],
            Some(&CallMetadata::Spec(sample_single_arg_spec_metadata(ty))),
        )
        .expect_err("bug: out-of-range native 256-bit argument must fail");

        assert!(error.to_string().contains(expected), "{error}");
        assert!(error.to_string().contains("does not fit"), "{error}");
    }
}

#[test]
fn encodes_and_decodes_embedded_spec_addresses() {
    let address = thruput_encoding::encode(&[0x11; 32]);
    let metadata = sample_single_arg_spec_metadata(ContractType::Address);
    let encoded = encode_args(
        std::slice::from_ref(&address),
        Some(&CallMetadata::Spec(metadata.clone())),
    )
    .expect("bug: embedded spec address should encode");

    assert_eq!(
        decode_spec_arguments(&metadata, &encoded),
        vec![json!(address.clone())]
    );
    assert_eq!(
        decode_return(&encoded, Some(&CallMetadata::Spec(metadata)))
            .expect("bug: embedded spec address return should decode"),
        json!(address)
    );
}

#[test]
fn spec_encode_rejects_arguments_that_do_not_fit_the_declared_type() {
    let payload_arg = || {
        sample_structured_spec_metadata(
            vec![contract_field("payload", named_type("Payload"))],
            ContractType::Void,
        )
    };
    // (case, metadata, argument, error path, error message)
    let cases = [
        (
            "byte vector over max length",
            sample_single_arg_spec_metadata(ContractType::Vector {
                vector: ContractVectorType {
                    element: Box::new(ContractType::U8),
                    max_len: 2,
                },
            }),
            "0x010203",
            "argument 1 (`value`)",
            "byte vector exceeds max length 2 (got 3)",
        ),
        (
            "array length mismatch",
            sample_single_arg_spec_metadata(ContractType::Array {
                array: ContractArrayType {
                    element: Box::new(ContractType::Bool),
                    len: 2,
                },
            }),
            "[true]",
            "argument 1 (`value`)",
            "array expects length 2 but got 1",
        ),
        (
            "struct given as array with wrong field count",
            payload_arg(),
            "[7]",
            "argument 1 (`payload`)",
            "expected 2 field value(s) but got 1",
        ),
        (
            "unexpected struct field",
            payload_arg(),
            r#"{"count":7,"bytes":"hi","extra":1}"#,
            "argument 1 (`payload`)",
            "unexpected field `extra`",
        ),
        (
            "missing struct field",
            payload_arg(),
            r#"{"count":7}"#,
            "argument 1 (`payload`)",
            "missing field `bytes`",
        ),
        (
            "payload for void union case",
            sample_structured_spec_metadata(
                vec![contract_field("envelope", named_type("Envelope"))],
                ContractType::Void,
            ),
            r#"{"case":"Empty","value":true}"#,
            "argument 1 (`envelope`)",
            "union case `Empty` does not take a payload",
        ),
    ];

    for (case, metadata, argument, path, message) in cases {
        let err =
            encode_args(&[argument.into()], Some(&CallMetadata::Spec(metadata))).expect_err(case);
        assert_encode_error(case, err, path, message);
    }
}

#[test]
fn rejects_decoded_vectors_exceeding_max_length() {
    let metadata = sample_single_arg_spec_metadata(ContractType::Vector {
        vector: ContractVectorType {
            element: Box::new(ContractType::Bool),
            max_len: 1,
        },
    });
    let type_defs =
        build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
    let bytes = xdrs::cxs::encode(&2_u32).expect("bug: vector length should encode");

    let err = decode_spec_value(&metadata.method.return_type, &bytes, &type_defs)
        .expect_err("bug: oversized decoded vectors should fail");
    assert_decode_error(err, "return value", "decoded vector length 2 exceeds max 1");
}

#[test]
fn prefers_named_enum_variants_over_numeric_string_discriminants() {
    let metadata = sample_numeric_name_spec_metadata(named_type("NumericEnum"));
    let encoded = encode_args(
        &[r#""1""#.into()],
        Some(&CallMetadata::Spec(metadata.clone())),
    )
    .expect("bug: numeric-string enum variant should encode by name");

    let decoded = decode_return(&encoded, Some(&CallMetadata::Spec(metadata)))
        .expect("bug: numeric-string enum variant should decode");
    assert_eq!(decoded, json!("1"));
}

#[test]
fn prefers_named_union_cases_over_numeric_string_discriminants() {
    let metadata = sample_numeric_name_spec_metadata(named_type("NumericUnion"));
    let encoded = encode_args(
        &[r#""1""#.into()],
        Some(&CallMetadata::Spec(metadata.clone())),
    )
    .expect("bug: numeric-string union case should encode by name");

    let decoded = decode_return(&encoded, Some(&CallMetadata::Spec(metadata)))
        .expect("bug: numeric-string union case should decode");
    assert_eq!(decoded, json!({"case": "1"}));
}

#[test]
fn normalize_type_name_strips_references_without_clobbering_prefixes() {
    assert_eq!(normalize_type_name("&str"), "str");
    assert_eq!(normalize_type_name("&mut str"), "str");
    assert_eq!(normalize_type_name("&'a str"), "str");
    assert_eq!(normalize_type_name("&String"), "String");
    assert_eq!(normalize_type_name("&strange"), "strange");
}

#[test]
fn normalize_type_name_recognizes_qualified_native_256_types() {
    assert_eq!(normalize_type_name("sdk::U256"), "U256");
    assert_eq!(normalize_type_name("&sdk::I256"), "I256");
    assert_eq!(normalize_type_name("groundhog_sdk::U256"), "U256");
    assert_eq!(normalize_type_name("&'a groundhog_sdk::I256"), "I256");
    assert_eq!(normalize_type_name("other::U256"), "other::U256");
}

fn sample_structured_spec_metadata(
    args: Vec<ContractField>,
    return_type: ContractType,
) -> SpecMethodMetadata {
    SpecMethodMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        method: ContractSpecMethod {
            name: spec_string("call"),
            args: args.try_into().unwrap(),
            return_type,
            effect: None,
        },
        type_defs: vec![
            ContractTypeDef::TypeDefStruct {
                struct_def: ContractStructDef {
                    name: spec_id("Payload"),
                    fields: vec![
                        contract_field("count", ContractType::U32),
                        contract_field(
                            "bytes",
                            ContractType::Vector {
                                vector: ContractVectorType {
                                    element: Box::new(ContractType::U8),
                                    max_len: u32::MAX,
                                },
                            },
                        ),
                    ]
                    .try_into()
                    .unwrap(),
                },
            },
            ContractTypeDef::TypeDefEnum {
                enum_def: ContractEnumDef {
                    name: spec_id("Mode"),
                    variants: vec![
                        ContractEnumVariant {
                            name: spec_string("Ready"),
                            value: 1,
                        },
                        ContractEnumVariant {
                            name: spec_string("Busy"),
                            value: 2,
                        },
                    ]
                    .try_into()
                    .unwrap(),
                },
            },
            ContractTypeDef::TypeDefUnion {
                union_def: ContractUnionDef {
                    name: spec_id("Envelope"),
                    cases: vec![
                        ContractUnionCase {
                            name: spec_string("Empty"),
                            discriminant: 0,
                            case_type: ContractUnionCaseType::UnionCaseVoid,
                        },
                        ContractUnionCase {
                            name: spec_string("Full"),
                            discriminant: 1,
                            case_type: ContractUnionCaseType::UnionCaseStruct {
                                struct_fields: vec![
                                    contract_field("payload", named_type("Payload")),
                                    contract_field("enabled", ContractType::Bool),
                                ]
                                .try_into()
                                .unwrap(),
                            },
                        },
                        ContractUnionCase {
                            name: spec_string("Pair"),
                            discriminant: 2,
                            case_type: ContractUnionCaseType::UnionCaseTuple {
                                tuple: vec![named_type("Mode"), ContractType::U32]
                                    .try_into()
                                    .unwrap(),
                            },
                        },
                    ]
                    .try_into()
                    .unwrap(),
                },
            },
        ],
        throws: vec![],
    }
}

fn sample_numeric_name_spec_metadata(return_type: ContractType) -> SpecMethodMetadata {
    SpecMethodMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        method: ContractSpecMethod {
            name: spec_string("call"),
            args: vec![contract_field("value", return_type.clone())]
                .try_into()
                .unwrap(),
            return_type,
            effect: None,
        },
        type_defs: vec![
            ContractTypeDef::TypeDefEnum {
                enum_def: ContractEnumDef {
                    name: spec_id("NumericEnum"),
                    variants: vec![
                        ContractEnumVariant {
                            name: spec_string("1"),
                            value: 7,
                        },
                        ContractEnumVariant {
                            name: spec_string("2"),
                            value: 8,
                        },
                    ]
                    .try_into()
                    .unwrap(),
                },
            },
            ContractTypeDef::TypeDefUnion {
                union_def: ContractUnionDef {
                    name: spec_id("NumericUnion"),
                    cases: vec![
                        ContractUnionCase {
                            name: spec_string("1"),
                            discriminant: 7,
                            case_type: ContractUnionCaseType::UnionCaseVoid,
                        },
                        ContractUnionCase {
                            name: spec_string("2"),
                            discriminant: 8,
                            case_type: ContractUnionCaseType::UnionCaseVoid,
                        },
                    ]
                    .try_into()
                    .unwrap(),
                },
            },
        ],
        throws: vec![],
    }
}

fn sample_single_arg_spec_metadata(arg_type: ContractType) -> SpecMethodMetadata {
    SpecMethodMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        method: ContractSpecMethod {
            name: spec_string("call"),
            args: vec![contract_field("value", arg_type.clone())]
                .try_into()
                .unwrap(),
            return_type: arg_type,
            effect: None,
        },
        type_defs: vec![],
        throws: vec![],
    }
}

fn contract_field(name: &str, ty: ContractType) -> ContractField {
    ContractField {
        name: spec_string(name),
        ty,
    }
}

fn decode_spec_arguments(metadata: &SpecMethodMetadata, encoded: &[u8]) -> Vec<serde_json::Value> {
    let type_defs =
        build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
    let mut reader = xdrs::cxs::buf_reader::BufReader::new(encoded);
    let values = metadata
        .method
        .args
        .iter()
        .enumerate()
        .map(|(index, field)| {
            decode_spec_value_from_reader(
                &field.ty,
                &mut reader,
                &type_defs,
                &format!("arg{index}"),
            )
            .expect("bug: spec argument should decode")
        })
        .collect();
    assert_eq!(reader.remaining(), 0);
    values
}

fn named_type(name: &str) -> ContractType {
    ContractType::Named {
        named: spec_id(name),
    }
}

fn assert_encode_error(case: &str, err: Error, path: &str, expected_message: &str) {
    match err {
        Error::CallEncode {
            path: actual_path,
            message,
        } => {
            assert_eq!(actual_path, path, "{case}");
            assert_eq!(message, expected_message, "{case}");
        }
        other => panic!("bug: {case}: expected Error::CallEncode, got {other:?}"),
    }
}

fn assert_decode_error(err: Error, path: &str, expected_message: &str) {
    match err {
        Error::CallDecode {
            path: actual_path,
            message,
        } => {
            assert_eq!(actual_path, path);
            assert_eq!(message, expected_message);
        }
        other => panic!("bug: expected Error::CallDecode, got {other:?}"),
    }
}
