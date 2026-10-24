//! The complete contract is generated from the public transport declaration.
use std::{cmp::Ordering, collections::BTreeSet};

use serde_json::{Value, json};
use thruput_json_rpc_api::{
    WIRE_FORMAT_VERSION, native::METHOD_NAMES, types::error::native as errors,
};
use thruput_open_rpc_spec::{build_spec, native};

const ADDRESS: &str = "11111111111111111111111111111111";

fn method_index(document: &Value, name: &str) -> usize {
    document["methods"]
        .as_array()
        .unwrap()
        .iter()
        .position(|method| method["name"] == name)
        .unwrap()
}

// Compare decimal bounds without losing precision at u128::MAX. Canonical
// spelling is checked separately by each generated schema's pattern.
fn decimal_cmp(left: &str, right: &str) -> Option<Ordering> {
    fn parts(value: &str) -> Option<(bool, &str)> {
        let negative = value.starts_with('-');
        let digits = value.strip_prefix('-').unwrap_or(value);
        (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .then_some((negative, digits.trim_start_matches('0')))
    }
    let (left_negative, left_digits) = parts(left)?;
    let (right_negative, right_digits) = parts(right)?;
    Some(match (left_negative, right_negative) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => {
            let magnitude = left_digits
                .len()
                .cmp(&right_digits.len())
                .then_with(|| left_digits.cmp(right_digits));
            if left_negative {
                magnitude.reverse()
            } else {
                magnitude
            }
        }
    })
}

struct IntegerBound {
    value: String,
    minimum: bool,
}
impl<'i> jsonschema::Keyword<'i> for IntegerBound {
    fn validate(&self, instance: &'i Value) -> Result<(), jsonschema::ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(jsonschema::ValidationError::custom(
                "decimal integer outside declared range",
            ))
        }
    }
    fn is_valid(&self, instance: &Value) -> bool {
        instance
            .as_str()
            .and_then(|value| decimal_cmp(value, &self.value))
            .is_some_and(|order| {
                if self.minimum {
                    order != Ordering::Less
                } else {
                    order != Ordering::Greater
                }
            })
    }
}

fn validator(document: &Value, name: &str, member: &str) -> jsonschema::Validator {
    let mut schema = document.clone();
    schema["$ref"] = json!(format!(
        "#/methods/{}/{member}",
        method_index(document, name)
    ));
    jsonschema::options()
        .with_keyword("x-integer-minimum", |_, bound: &Value, _| {
            Ok(Box::new(IntegerBound {
                value: bound.as_str().unwrap().to_owned(),
                minimum: true,
            }))
        })
        .with_keyword("x-integer-maximum", |_, bound: &Value, _| {
            Ok(Box::new(IntegerBound {
                value: bound.as_str().unwrap().to_owned(),
                minimum: false,
            }))
        })
        .build(&schema)
        .unwrap()
}

fn request(name: &str) -> Value {
    match name {
        "getVersionInfo"
        | "getNetworkInfo"
        | "getChainTip"
        | "getChainConfig"
        | "getServerLimits"
        | "dev_getPendingTransactionCount" => json!({}),
        "sendTransaction" => json!({"signedTransaction": "0x0102"}),
        "getTransactionStatus" | "getTransactionReceipt" => json!({"transactionHash": ADDRESS}),
        "simulateInvocation" => invocation(),
        "simulateInvocations" => json!({"invocations": [invocation()]}),
        "getStorage" => json!({"address": ADDRESS, "key": "0x01"}),
        "getBalance" | "getRegisteredAuth" => json!({"address": ADDRESS}),
        "getEvents" => {
            json!({"filters": [{"addresses": [ADDRESS]}], "start": {"order": "ascending", "anchor": {"kind": "atBeginning"}}})
        }
        "getTransactionsByRootAddress" => {
            json!({"address": ADDRESS, "start": {"order": "descending", "anchor": {"kind": "atHeight", "blockHeight": "9007199254740993"}}})
        }
        "getBlockByHeight" => json!({"blockHeight": "812"}),
        "getTransactionsByBlock" => json!({"blockHeight": "812", "start": {"order": "ascending"}}),
        "faucet_requestAirdrop" => json!({"recipient": ADDRESS, "clientNonce": "0x01"}),
        "dev_fundAccount" => {
            json!({"recipient": ADDRESS, "rawAmount": "1", "clientNonce": "0x01", "faucet": ADDRESS})
        }
        _ => panic!("missing request fixture: {name}"),
    }
}

fn invocation() -> Value {
    json!({"address": ADDRESS, "entrypoint": "0x0102030405060708", "calldata": "0x"})
}

fn result(name: &str) -> Value {
    match name {
        "getVersionInfo" => {
            json!({"wireFormat": "14.0.0", "supportedMethods": METHOD_NAMES, "implementation": "test"})
        }
        "getNetworkInfo" => json!({"networkHash": ADDRESS}),
        "getChainTip" => json!({"blockHeight": "812", "blockHash": ADDRESS}),
        "getChainConfig" => json!({"stated": {}}),
        "getServerLimits" => {
            json!({"maxSimulationGas": "100", "maxSimulationCalldataBytes": "100", "maxEventsPerPage": 10, "maxTransactionsPerPage": 10, "maxCursorBytes": "100"})
        }
        "sendTransaction" => json!({"transactionHash": ADDRESS, "admission": "accepted"}),
        "getTransactionStatus" => json!({"status": "committed", "blockHeight": "812"}),
        "getTransactionReceipt" => receipt(),
        "simulateInvocation" => {
            json!({"stateBlockHeight": "812", "output": {"result": "return", "returnData": "0x"}, "events": [], "gasUsed": "0"})
        }
        "simulateInvocations" => {
            json!({"stateBlockHeight": "812", "results": [{"output": {"result": "return", "returnData": "0x"}, "events": [], "gasUsed": "0"}]})
        }
        "getStorage" => json!({"stateBlockHeight": "812", "item": null}),
        "getBalance" => json!({"address": ADDRESS, "stateBlockHeight": "812", "rawBalance": "0"}),
        "getRegisteredAuth" => {
            json!({"address": ADDRESS, "stateBlockHeight": "812", "configuration": null})
        }
        "getEvents" => {
            json!({"events": [], "nextCursor": null, "oldestIndexedBlockHeight": "0", "latestIndexedBlockHeight": "812"})
        }
        "getTransactionsByRootAddress" | "getTransactionsByBlock" => {
            json!({"transactions": [], "nextCursor": null, "oldestIndexedBlockHeight": "0", "latestIndexedBlockHeight": "812"})
        }
        "getBlockByHeight" => json!({
            "blockHash": ADDRESS, "protocol": 1, "networkHash": ADDRESS,
            "configHash": ADDRESS, "proposer": ADDRESS, "blockTimepoint": "0",
            "blockHeight": "812", "txsHash": ADDRESS, "storageHash": ADDRESS,
            "eventsHash": ADDRESS, "prevBlockHash": ADDRESS, "proposalTxsHash": ADDRESS,
            "upgrades": []
        }),
        "faucet_requestAirdrop" => {
            json!({"attemptStatus": "pending", "recipient": ADDRESS, "rawAmount": "1", "retryAfterSeconds": "1"})
        }
        "dev_fundAccount" => {
            json!({"recipient": ADDRESS, "rawAmount": "1", "transactionHash": ADDRESS, "signedTransaction": "0x01"})
        }
        "dev_getPendingTransactionCount" => json!({"count": "9007199254740993"}),
        _ => panic!("missing result fixture: {name}"),
    }
}

fn receipt() -> Value {
    json!({"transactionHash": ADDRESS, "blockHeight": "812", "execution": {"result": "return", "returnData": "0x"}, "events": [], "gasUsed": "9007199254740993"})
}

#[test]
fn native_contract_is_the_canonical_serving_artifact() {
    assert_eq!(native::build_spec(), build_spec());
    assert_eq!(build_spec()["info"]["version"], WIRE_FORMAT_VERSION);
    assert_eq!(WIRE_FORMAT_VERSION, "14.1.1");
}

#[test]
fn complete_method_map_has_strict_named_params_required_first_and_error_parity() {
    let document = native::build_spec();
    let methods = document["methods"].as_array().unwrap();
    let names: BTreeSet<_> = methods
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(methods.len(), 20);
    assert_eq!(names, METHOD_NAMES.iter().copied().collect());
    assert_eq!(names, errors::METHODS.iter().copied().collect());
    for method in methods {
        let name = method["name"].as_str().unwrap();
        assert_eq!(method["paramStructure"], "by-name", "{name}");
        assert_eq!(
            method["x-params-schema"]["additionalProperties"], false,
            "{name}"
        );
        let mut optional = false;
        for param in method["params"].as_array().unwrap() {
            if param["required"].as_bool().unwrap_or(false) {
                assert!(!optional, "{name}: required parameter after optional");
            } else {
                optional = true;
            }
        }
        let expected: Vec<_> = errors::method_error_codes(name)
            .unwrap()
            .iter()
            .map(|code| {
                let info = errors::error_info_by_code(*code).unwrap();
                json!({"$ref": format!("#/components/errors/{}", info.name)})
            })
            .collect();
        assert_eq!(method["errors"], json!(expected), "{name}");
        assert_eq!(
            method["x-data-schema"].as_object().unwrap().len(),
            expected.len()
        );
        let params = validator(&document, name, "x-params-schema");
        let valid = request(name);
        assert!(
            params.is_valid(&valid),
            "{name}: {:?}",
            params.iter_errors(&valid).collect::<Vec<_>>()
        );
        let mut unknown = valid;
        unknown["unrecognized"] = json!(true);
        assert!(!params.is_valid(&unknown), "{name}");
        assert!(
            !params.is_valid(&json!([])),
            "canonical params for {name} must be an object"
        );
        let results = validator(&document, name, "result/schema");
        let valid = result(name);
        assert!(
            results.is_valid(&valid),
            "{name}: {:?}",
            results.iter_errors(&valid).collect::<Vec<_>>()
        );
        assert!(
            !results.is_valid(&json!({})),
            "{name}: result must not be an unconstrained test echo"
        );
    }
}

#[test]
fn candidate_schemas_keep_native_field_shapes_and_numeric_bounds() {
    let document = native::build_spec();
    let simulate = validator(&document, "simulateInvocation", "x-params-schema");
    let mut call = invocation();
    call["contract"] = call["address"].take();
    call.as_object_mut().unwrap().remove("address");
    assert!(!simulate.is_valid(&call));
    let height = validator(&document, "getBlockByHeight", "x-params-schema");
    for invalid in [
        json!(812),
        json!("0812"),
        json!("-1"),
        json!("9223372036854775808"),
        Value::Null,
    ] {
        assert!(!height.is_valid(&json!({"blockHeight": invalid})));
    }
    assert!(height.is_valid(&json!({"blockHeight": "9223372036854775807"})));
    let blocks = validator(&document, "getBlockByHeight", "result/schema");
    assert!(blocks.is_valid(&Value::Null));
    let receipts = validator(&document, "getTransactionReceipt", "result/schema");
    assert!(receipts.is_valid(&Value::Null));
    let mut missing_bytes = receipt();
    missing_bytes["execution"]
        .as_object_mut()
        .unwrap()
        .remove("returnData");
    assert!(!receipts.is_valid(&missing_bytes));
    let mut numeric_gas = receipt();
    numeric_gas["gasUsed"] = json!(0);
    assert!(!receipts.is_valid(&numeric_gas));
    let mut extra = receipt();
    extra["futureField"] = json!({"nested": true});
    assert!(receipts.is_valid(&extra));
    let pending = validator(&document, "dev_getPendingTransactionCount", "result/schema");
    assert!(!pending.is_valid(&json!({"count": 0})));
    let airdrop = validator(&document, "faucet_requestAirdrop", "result/schema");
    let mut succeeded = json!({"attemptStatus": "succeeded", "recipient": ADDRESS, "rawAmount": "1", "transactionHash": ADDRESS, "blockHeight": "812", "execution": {"result": "return", "returnData": "0x"}});
    assert!(airdrop.is_valid(&succeeded));
    succeeded["execution"] = json!({"result": "abort", "abortData": "0x"});
    assert!(!airdrop.is_valid(&succeeded));
    let funding = validator(&document, "dev_fundAccount", "x-params-schema");
    let mut largest = request("dev_fundAccount");
    largest["rawAmount"] = json!("340282366920938463463374607431768211455");
    assert!(funding.is_valid(&largest));
    for value in [
        json!("0"),
        json!("01"),
        json!(1),
        json!("340282366920938463463374607431768211456"),
    ] {
        let mut request = request("dev_fundAccount");
        request["rawAmount"] = value;
        assert!(!funding.is_valid(&request));
    }
}

fn check_references(value: &Value, document: &Value) {
    match value {
        Value::Object(fields) => {
            let integer = match fields.get("type") {
                Some(Value::String(kind)) => kind == "integer",
                Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "integer"),
                _ => false,
            };
            assert!(
                !(integer
                    && matches!(
                        fields.get("format").and_then(Value::as_str),
                        Some("int64" | "uint64" | "int128" | "uint128")
                    )),
                "wide integers must be decimal strings: {value}"
            );
            if let Some(reference) = fields.get("$ref") {
                let pointer = reference
                    .as_str()
                    .unwrap()
                    .strip_prefix('#')
                    .expect("schema references must stay local");
                assert!(
                    document.pointer(pointer).is_some(),
                    "unresolved reference: {reference}"
                );
            }
            for value in fields.values() {
                check_references(value, document);
            }
        }
        Value::Array(values) => {
            for value in values {
                check_references(value, document);
            }
        }
        _ => {}
    }
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
fn complete_candidate_resolves_references_and_passes_official_meta_schema() {
    let document = native::build_spec();
    check_references(&document, &document);
    let meta: Value = serde_json::from_str(include_str!(
        "../../json-rpc-client/tests/fixtures/openrpc-meta-1.14.9.json"
    ))
    .unwrap();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .with_retriever(OfflineMeta)
        .build(&meta)
        .unwrap();
    assert!(
        validator.is_valid(&document),
        "{:?}",
        validator.iter_errors(&document).collect::<Vec<_>>()
    );
}
