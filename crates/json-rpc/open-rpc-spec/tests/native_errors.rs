//! Native error documentation fixtures for the canonical public artifact.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use thruput_json_rpc_api::types::error::native as errors;
use thruput_open_rpc_spec::{build_spec, native::add_error_docs};

#[path = "../../json-rpc-client/tests/support/schema.rs"]
mod schema;

fn document() -> Value {
    // Only method shells: request/result DTOs have their own contract fixtures.
    json!({
        "openrpc": "1.2.6",
        "info": {"title": "Native error fixtures; not a serving API", "version": "0.0.0"},
        "methods": errors::METHODS.iter().map(|name| json!({
            "name": name, "params": [], "result": {"name": "result", "schema": {}}
        })).collect::<Vec<_>>(),
        "components": {"schemas": {"Preserved": {"type": "boolean"}}}
    })
}

fn decorated() -> Value {
    let mut document = document();
    add_error_docs(&mut document).unwrap();
    document
}

fn validator(document: &Value, method: &str, component: &str) -> jsonschema::Validator {
    let index = errors::METHODS
        .iter()
        .position(|name| *name == method)
        .unwrap();
    schema::at(
        document,
        &format!("/methods/{index}/x-data-schema/{component}"),
    )
}

#[test]
fn catalogue_has_twelve_components_thirteen_kinds_and_unique_method_codes() {
    let document = decorated();
    let components = document["components"]["errors"].as_object().unwrap();
    assert_eq!(components.len(), 12);
    assert_eq!(errors::METHODS.len(), 24);
    assert_eq!(
        document["components"]["schemas"]["Preserved"],
        json!({"type": "boolean"})
    );
    let mut codes = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for info in errors::ERROR_INFOS {
        assert!(codes.insert(info.code));
        assert_eq!(
            components[info.name],
            json!({"code": info.code, "message": info.message})
        );
        let schema = errors::data_schema(info.code).unwrap();
        for variant in schema["oneOf"].as_array().unwrap() {
            assert!(
                kinds.insert(
                    variant["properties"]["kind"]["const"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                )
            );
        }
    }
    assert_eq!(kinds.len(), 13);
    assert!(kinds.contains("invalidParams") && kinds.contains("invalidCursor"));
    for method in document["methods"].as_array().unwrap() {
        let name = method["name"].as_str().unwrap();
        let codes = errors::method_error_codes(name).unwrap();
        assert_eq!(codes.len(), codes.iter().collect::<BTreeSet<_>>().len());
        let references = method["errors"].as_array().unwrap();
        let schemas = method["x-data-schema"].as_object().unwrap();
        assert_eq!(references.len(), codes.len());
        let names = references
            .iter()
            .map(|reference| {
                assert_eq!(reference.as_object().unwrap().len(), 1);
                let pointer = reference["$ref"]
                    .as_str()
                    .unwrap()
                    .strip_prefix('#')
                    .unwrap();
                assert!(document.pointer(pointer).is_some());
                pointer.strip_prefix("/components/errors/").unwrap()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(names, schemas.keys().map(String::as_str).collect());
    }
    let mut repeated = document.clone();
    add_error_docs(&mut repeated).unwrap();
    assert_eq!(repeated, document);
}

#[test]
fn embedded_data_schemas_resolve_and_validate_real_payload_shapes() {
    let document = decorated();
    let hash = "11111111111111111111111111111111";
    for (method, component, payload) in [
        (
            "getNetworkInfo",
            "InvalidParams",
            json!({"kind": "invalidParams", "reason": "unknownField", "path": "/extra"}),
        ),
        (
            "getEvents",
            "InvalidParams",
            json!({"kind": "invalidCursor", "reason": "queryMismatch"}),
        ),
        (
            "sendTransaction",
            "TransactionInvalid",
            json!({"kind": "transactionInvalid", "admission": "notAttempted", "reason": "invalidEncoding"}),
        ),
        (
            "sendTransaction",
            "TransactionRejected",
            json!({"kind": "transactionRejected", "admission": "rejected", "reason": "authenticationRejected"}),
        ),
        (
            "simulateInvocation",
            "StateHeightUnavailable",
            json!({"kind": "stateHeightUnavailable", "requestedStateBlockHeight": "12", "observedStateBlockHeight": "11"}),
        ),
        (
            "getEvents",
            "HistoryUnavailable",
            json!({"kind": "historyUnavailable", "reason": "outsideRetention", "oldestIndexedBlockHeight": "12"}),
        ),
        (
            "faucet_requestAirdrop",
            "FundingAttemptStopped",
            json!({"kind": "fundingAttemptStopped", "serviceCode": "funding_disabled"}),
        ),
        (
            "dev_fundAccount",
            "LocalFundingUnresolved",
            json!({"kind": "localFundingUnresolved", "transactionHash": hash, "signedTransaction": "0x0102", "cause": {"kind": "fundingAttemptStopped"}}),
        ),
        (
            "sendTransaction",
            "BackendUnavailable",
            json!({"kind": "backendUnavailable", "admission": "unknown", "retryAfterMs": "9007199254740993"}),
        ),
        (
            "getBalance",
            "BackendTimeout",
            json!({"kind": "backendTimeout"}),
        ),
        (
            "sendTransaction",
            "Overloaded",
            json!({"kind": "overloaded", "admission": "notAttempted"}),
        ),
        (
            "getTransactionReceipt",
            "BackendDataInvalid",
            json!({"kind": "backendDataInvalid", "reason": "inconsistentResult", "observation": {"transactionHash": hash, "blockHeight": "42"}}),
        ),
    ] {
        let validator = validator(&document, method, component);
        assert!(
            validator.is_valid(&payload),
            "{method}/{component}: {:?}",
            validator.iter_errors(&payload).collect::<Vec<_>>()
        );
        let mut invalid = payload.clone();
        invalid["kind"] = json!("futureError");
        assert!(!validator.is_valid(&invalid), "{method}/{component}");
        assert!(!validator.is_valid(&json!({})), "{method}/{component}");
    }
}

#[test]
fn method_schemas_restrict_evidence_and_shared_code_refinements() {
    let document = decorated();
    let invalid_cursor = json!({"kind": "invalidCursor"});
    assert!(validator(&document, "getEvents", "InvalidParams").is_valid(&invalid_cursor));
    assert!(!validator(&document, "getBalance", "InvalidParams").is_valid(&invalid_cursor));

    let send_timeout = validator(&document, "sendTransaction", "BackendTimeout");
    assert!(!send_timeout.is_valid(&json!({"kind": "backendTimeout"})));
    assert!(send_timeout.is_valid(&json!({"kind": "backendTimeout", "admission": "unknown"})));
    let read_timeout = validator(&document, "getBalance", "BackendTimeout");
    assert!(read_timeout.is_valid(&json!({"kind": "backendTimeout"})));
    assert!(!read_timeout.is_valid(&json!({"kind": "backendTimeout", "admission": "unknown"})));
    let funding_timeout = validator(&document, "faucet_requestAirdrop", "BackendTimeout");
    assert!(
        !funding_timeout.is_valid(&json!({"kind": "backendTimeout", "admission": "notAttempted"}))
    );

    let invalid = validator(&document, "sendTransaction", "TransactionInvalid");
    assert!(!invalid.is_valid(&json!({"kind": "transactionInvalid", "admission": "unknown"})));
    let observed = json!({"kind": "backendDataInvalid", "observation": {"transactionHash": "11111111111111111111111111111111", "blockHeight": "42"}});
    assert!(
        validator(&document, "getTransactionReceipt", "BackendDataInvalid").is_valid(&observed)
    );
    assert!(
        !validator(&document, "getTransactionStatus", "BackendDataInvalid").is_valid(&observed)
    );
    let mut numeric = observed;
    numeric["observation"]["blockHeight"] = json!(42);
    assert!(
        !validator(&document, "getTransactionReceipt", "BackendDataInvalid").is_valid(&numeric)
    );
}

#[test]
fn invalid_input_is_rejected_without_partial_mutation() {
    let valid_method = json!({"name": "getBalance", "params": []});
    for mut invalid in [
        json!(null),
        json!({}),
        json!({"methods": {}}),
        json!({"methods": [valid_method, false]}),
        json!({"methods": [valid_method, {}]}),
        json!({"methods": [valid_method, {"name": 3, "params": []}]}),
        json!({"methods": [valid_method, {"name": "groundhog_getBalance", "params": []}]}),
        json!({"methods": [valid_method, {"name": "futureMethod", "params": []}]}),
        json!({"methods": [valid_method, valid_method]}),
        json!({"methods": [valid_method, {"name": "getEvents", "params": {}}]}),
        json!({"methods": [valid_method, {"name": "getEvents", "params": [], "result": false}]}),
        json!({"methods": [valid_method], "components": null}),
        json!({"methods": [valid_method], "components": {"errors": []}}),
        json!({"methods": [{"name": "getEvents", "params": [], "errors": {}}]}),
        json!({"methods": [{"name": "getEvents", "params": [], "x-data-schema": []}]}),
    ] {
        let original = invalid.clone();
        assert!(
            add_error_docs(&mut invalid).is_err(),
            "accepted: {original}"
        );
        assert_eq!(invalid, original);
    }
    let mut subset = json!({"methods": [valid_method]});
    add_error_docs(&mut subset).unwrap();
    assert_eq!(subset["methods"].as_array().unwrap().len(), 1);
    assert_eq!(
        subset["components"]["errors"].as_object().unwrap().len(),
        12
    );
}

struct OfflineMeta;
impl jsonschema::Retrieve for OfflineMeta {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        if uri.as_str().trim_end_matches('/') == "https://meta.json-schema.tools" {
            Ok(serde_json::from_str(include_str!(
                "../../json-rpc-client/tests/fixtures/json-schema-tools-meta.json"
            ))?)
        } else {
            Err(format!("unregistered offline schema: {uri}").into())
        }
    }
}

#[test]
fn decorated_document_passes_offline_official_openrpc_meta_schema() {
    let meta: Value = serde_json::from_str(include_str!(
        "../../json-rpc-client/tests/fixtures/openrpc-meta-1.14.9.json"
    ))
    .unwrap();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .with_retriever(OfflineMeta)
        .build(&meta)
        .unwrap();
    let document = decorated();
    assert!(
        validator.is_valid(&document),
        "{:?}",
        validator.iter_errors(&document).collect::<Vec<_>>()
    );
    let mut bad_error = document.clone();
    bad_error["components"]["errors"]["InvalidParams"]["x-data-schema"] = json!({});
    assert!(!validator.is_valid(&bad_error));
    let mut bad_reference = document;
    bad_reference["methods"][0]["errors"][0]["x-data-schema"] = json!({});
    assert!(!validator.is_valid(&bad_reference));
}

#[test]
fn canonical_artifact_has_idempotent_native_error_documentation() {
    let original = build_spec();
    let mut decorated = original.clone();
    add_error_docs(&mut decorated).unwrap();
    assert_eq!(decorated, original);
    let mut legacy = original.clone();
    legacy["methods"][0]["name"] = json!("groundhog_version");
    let before = legacy.clone();
    assert!(add_error_docs(&mut legacy).is_err());
    assert_eq!(legacy, before);
}
