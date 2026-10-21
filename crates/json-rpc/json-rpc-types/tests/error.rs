use std::collections::HashSet;

use thruput_json_rpc_types::error::*;

#[test]
fn documented_error_codes_are_unique_and_lookupable() {
    let mut codes = HashSet::new();
    let mut names = HashSet::new();

    for info in ERROR_INFOS {
        assert!(codes.insert(info.code), "duplicate code {}", info.code);
        assert!(names.insert(info.name), "duplicate name {}", info.name);
        assert_eq!(error_info_by_code(info.code), Some(info));
        assert!(!info.message.is_empty());
        assert!(!info.description.is_empty());
    }

    let documented_codes: HashSet<i32> = ERROR_INFOS.iter().map(|info| info.code).collect();
    let public_codes: HashSet<i32> = self::codes::ALL.iter().copied().collect();
    assert_eq!(
        documented_codes, public_codes,
        "codes::ALL and ERROR_INFOS must stay one-to-one"
    );
}

#[test]
fn tx_invalid_detail_serializes_with_reason_tag_and_camel_case_fields() {
    let cases = [
        (
            TxInvalidDetail::TooManyContracts {
                attached: 5,
                max: 4,
            },
            serde_json::json!({"reason": "tooManyContracts", "attached": 5, "max": 4}),
            "transaction attaches 5 contracts; chain permits 4",
        ),
        (
            TxInvalidDetail::ContractTooLarge {
                index: 1,
                size_bytes: 262_145,
                max_bytes: 262_144,
            },
            serde_json::json!({
                "reason": "contractTooLarge",
                "index": 1,
                "sizeBytes": 262_145,
                "maxBytes": 262_144,
            }),
            "contract 1 in contractsToDeploy is 262145 bytes; limit is 262144",
        ),
        (
            TxInvalidDetail::DuplicateContract {
                index: 2,
                first_index: 0,
            },
            serde_json::json!({"reason": "duplicateContract", "index": 2, "firstIndex": 0}),
            "contract 2 in contractsToDeploy duplicates contract 0",
        ),
        (
            TxInvalidDetail::Rv32MemoryTooLarge {
                index: 0,
                memory_bytes: 131_072,
                max_bytes: 65_536,
            },
            serde_json::json!({
                "reason": "rv32MemoryTooLarge",
                "index": 0,
                "memoryBytes": 131_072,
                "maxBytes": 65_536,
            }),
            "contract 0 in contractsToDeploy declares 131072 bytes of guest memory; limit is 65536",
        ),
        (
            TxInvalidDetail::Rv32CodeTooLarge {
                index: 3,
                code_bytes: 65_540,
                max_bytes: 65_536,
            },
            serde_json::json!({
                "reason": "rv32CodeTooLarge",
                "index": 3,
                "codeBytes": 65_540,
                "maxBytes": 65_536,
            }),
            "contract 3 in contractsToDeploy carries 65540 bytes of code; limit is 65536",
        ),
        (
            TxInvalidDetail::GasLimitTooHigh {
                gas_limit: 5_000_000_000,
                max_gas: 1_000_000_000,
            },
            serde_json::json!({
                "reason": "gasLimitTooHigh",
                "gasLimit": "5000000000",
                "maxGas": "1000000000",
            }),
            "transaction budgets 5000000000 gas; chain permits 1000000000",
        ),
    ];

    for (detail, expected_json, expected_message) in cases {
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json, expected_json);
        let roundtrip: TxInvalidDetail = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip, detail);
        assert_eq!(detail.to_string(), expected_message);
    }
}

#[test]
fn every_catalog_code_maps_to_bounded_outcome() {
    const ALLOWED: &[&str] = &[
        "ok",
        "invalid_params",
        "tx_invalid",
        "tx_rejected",
        "not_found",
        "not_implemented",
        "backend_unavailable",
        "backend_timeout",
        "view_height_unavailable",
        "overloaded",
        "internal_error",
    ];

    for code in self::codes::ALL {
        let outcome = outcome_for_code(*code);
        assert!(
            ALLOWED.contains(&outcome),
            "code {code} mapped to unbounded outcome {outcome}"
        );
    }
    assert_eq!(outcome_for_code(-32601), "not_implemented");
    assert_eq!(outcome_for_code(-32603), "internal_error");
    assert_eq!(outcome_for_code(12345), "internal_error");
}

#[test]
fn outcome_mapping_is_exact() {
    let cases = [
        (codes::INVALID_PARAMS, "invalid_params"),
        (codes::TX_INVALID, "tx_invalid"),
        (codes::TX_REJECTED, "tx_rejected"),
        (codes::TX_NOT_FOUND, "not_found"),
        (codes::BLOCK_NOT_FOUND, "not_found"),
        (codes::BACKEND_UNAVAILABLE, "backend_unavailable"),
        (codes::BACKEND_TIMEOUT, "backend_timeout"),
        (codes::NOT_IMPLEMENTED, "not_implemented"),
        (codes::OVERLOADED, "overloaded"),
        (-32700, "invalid_params"),
        (-32600, "invalid_params"),
        (-32601, "not_implemented"),
        (-32603, "internal_error"),
        (-32005, "invalid_params"),
        (-32006, "overloaded"),
        (-32007, "invalid_params"),
        (-32008, "internal_error"),
        (-32009, "overloaded"),
        // jsonrpsee also uses -32010 for too-large batch requests, but that
        // pre-dispatch error is only visible to HTTP transport metrics. The
        // Groundhog application catalog owns -32010 as BlockNotFound.
        (-32010, "not_found"),
        (codes::VIEW_HEIGHT_UNAVAILABLE, "view_height_unavailable"),
    ];

    for (code, outcome) in cases {
        assert_eq!(outcome_for_code(code), outcome, "code {code}");
    }
}
