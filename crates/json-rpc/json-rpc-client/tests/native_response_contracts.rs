//! Native result fixtures, not production dispatch or backend qualification.
use jsonrpsee::{RpcModule, core::client::ClientT, server::Server};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thruput_json_rpc_api::params::NamedParams;
use thruput_json_rpc_client::{DecodedResponse, HttpClientBuilder};
use thruput_json_rpc_types::{requests as req, responses as res};
use thruput_open_rpc::{Project, ProjectInfo};
use thruput_open_rpc_macros::open_rpc;

#[path = "support/schema.rs"]
mod schema;

type Decode = fn(&Value) -> Result<bool, serde_json::Error>;
type Roundtrip = fn(Value) -> Value;

fn decode<T: DeserializeOwned + res::ResponseData>(raw: &Value) -> Result<bool, serde_json::Error> {
    match serde_json::from_value::<DecodedResponse<T>>(raw.clone())? {
        DecodedResponse::Known(_) => Ok(true),
        DecodedResponse::Unsupported { raw: retained } => {
            assert_eq!(&retained, raw);
            Ok(false)
        }
        _ => panic!("unrecognized reader result"),
    }
}

fn roundtrip<T: DeserializeOwned + Serialize>(raw: Value) -> Value {
    serde_json::to_value(serde_json::from_value::<T>(raw).unwrap()).unwrap()
}

macro_rules! core_results {
    ($($method:ident ($wire:literal): $request:ty => $response:ty),+ $(,)?) => {
        #[open_rpc]
        trait CoreResultFixture {
            $(#[method(name = $wire)]
            async fn $method(&self, request: $request) -> jsonrpsee::core::RpcResult<$response>;)+
        }
        fn cases() -> Vec<(&'static str, Decode, Roundtrip)> {
            vec![$(($wire, decode::<$response>, roundtrip::<$response>)),+]
        }
    };
}

core_results! {
    version("getVersionInfo"): req::EmptyRequest => res::VersionInfo,
    network("getNetworkInfo"): req::EmptyRequest => res::NetworkInfo,
    tip("getChainTip"): req::EmptyRequest => res::ChainTip,
    config("getChainConfig"): req::EmptyRequest => res::ChainConfig,
    limits("getServerLimits"): req::EmptyRequest => res::ServerLimits,
    send("sendTransaction"): req::SendTransactionRequest => res::SendTransactionResult,
    status("getTransactionStatus"): req::TransactionQuery => res::TransactionStatus,
    receipt("getTransactionReceipt"): req::TransactionQuery => res::ReceiptResult,
    simulation("simulateInvocation"): req::SimulateInvocationRequest => res::SimulationResult,
    simulations("simulateInvocations"): req::SimulateInvocationsRequest => res::SimulationResults,
    storage("getStorage"): req::StorageQuery => res::StorageResult,
    balance("getBalance"): req::AddressQuery => res::BalanceResult,
    auth("getRegisteredAuth"): req::AddressQuery => res::RegisteredAuthResult,
    events("getEvents"): req::EventQuery => res::EventPage,
    root("getTransactionsByRootAddress"): req::RootAddressQuery => res::TransactionRefPage,
    block("getBlockByHeight"): req::BlockQuery => res::BlockResult,
    block_transactions("getTransactionsByBlock"): req::BlockPageRequest => res::BlockTransactionPage,
}

fn spec() -> Value {
    let mut project = Project::new(ProjectInfo {
        title: "Native core request/result fixtures; not a serving API".into(),
        version: "14.0.0".into(),
    });
    project.module(CoreResultFixtureOpenRpc::module_doc);
    project.to_json()
}

fn identifier() -> String {
    "1".repeat(32)
}
fn event() -> Value {
    json!({"address": identifier(), "topics": [format!("0x{}", "01".repeat(32))], "data": "0xab"})
}
fn invocation() -> Value {
    json!({"output": {"result": "return", "returnData": "0x"}, "events": [event()], "gasUsed": "9007199254740993"})
}
fn receipt() -> Value {
    json!({"transactionHash": identifier(), "blockHeight": "42", "execution": {"result": "return", "returnData": "0x"}, "events": [event()], "gasUsed": "9007199254740993"})
}
fn costs() -> Value {
    json!({"baseDataItemCost": "1", "bytestringCostPerByte": "2", "baseContractDeploymentCost": "3",
        "contractDeploymentCostPerByte": "4", "authPerKeyCost": "5", "rcqCostPerEntry": "6",
        "replayCacheCostPerEntry": "7", "balanceCost": "8"})
}
fn valid(method: &str) -> Value {
    match method {
        "getVersionInfo" => {
            json!({"wireFormat": "14.0.0", "supportedMethods": ["getVersionInfo"], "implementation": "test"})
        }
        "getNetworkInfo" => json!({"networkHash": identifier()}),
        "getChainTip" => json!({"blockHeight": "42", "blockHash": identifier()}),
        "getChainConfig" => json!({"stated": {"maxSha256Buffers": 8}, "execution": {
            "observedTip": valid("getChainTip"), "configHash": identifier(), "effectiveLimits": {}, "storageCosts": costs()}}),
        "getServerLimits" => {
            json!({"maxSimulationGas": "100000", "maxSimulationCalldataBytes": "4096",
            "maxEventsPerPage": 100, "maxTransactionsPerPage": 200, "maxCursorBytes": "256",
            "simulationBatch": {"maxInvocations": 10, "maxCalldataBytes": 4096, "maxGas": "100000",
                "maxBackendResponseBytes": 65536, "timeoutMs": "1000"}})
        }
        "sendTransaction" => json!({"transactionHash": identifier(), "admission": "accepted"}),
        "getTransactionStatus" => json!({"status": "committed", "blockHeight": "42"}),
        "getTransactionReceipt" => receipt(),
        "simulateInvocation" => {
            let mut value = invocation();
            value["stateBlockHeight"] = json!("42");
            value
        }
        "simulateInvocations" => json!({"stateBlockHeight": "42", "results": [invocation()]}),
        "getStorage" => {
            json!({"stateBlockHeight": "42", "item": {"kind": "nonnegativeInt", "value": "9007199254740993"}})
        }
        "getBalance" => {
            json!({"address": identifier(), "stateBlockHeight": "42", "rawBalance": "9007199254740993"})
        }
        "getRegisteredAuth" => {
            json!({"address": identifier(), "stateBlockHeight": "42", "configuration": {
            "signers": [{"keyId": 3, "publicKey": {"scheme": "ed25519", "key": identifier()}, "maxWeight": 2}], "requiredWeight": 2}})
        }
        "getEvents" => {
            json!({"events": [{"transactionHash": identifier(), "blockHeight": "42", "sequenceNumber": 0, "event": event()}],
            "nextCursor": "0x01ab", "oldestIndexedBlockHeight": "0", "latestIndexedBlockHeight": "43"})
        }
        "getTransactionsByRootAddress" | "getTransactionsByBlock" => {
            json!({"transactions": [{"transactionHash": identifier(), "blockHeight": "42"}],
            "nextCursor": null, "oldestIndexedBlockHeight": "0", "latestIndexedBlockHeight": "43"})
        }
        "getBlockByHeight" => {
            json!({"blockHash": identifier(), "protocol": 1, "networkHash": identifier(), "configHash": identifier(),
            "proposer": identifier(), "blockTimepoint": "9007199254740993", "blockHeight": "42", "txsHash": identifier(),
            "storageHash": identifier(), "eventsHash": identifier(), "prevBlockHash": identifier(), "proposalTxsHash": identifier(), "upgrades": ["0xab"]})
        }
        _ => panic!("unknown method {method}"),
    }
}

// Known producer schemas and tolerant readers deliberately differ for future
// variants. Expected None means malformed; Some(false) means unsupported/raw.
fn check(method: &str, raw: &Value, expected: Option<bool>, schema_valid: bool) {
    let document = spec();
    let (_, read, _) = cases().into_iter().find(|row| row.0 == method).unwrap();
    let index = document["methods"]
        .as_array()
        .unwrap()
        .iter()
        .position(|row| row["name"] == method)
        .unwrap();
    let schema = schema::at(&document, &format!("/methods/{index}/result/schema"));
    assert_eq!(read(raw).ok(), expected, "reader {method}: {raw}");
    assert_eq!(
        schema.is_valid(raw),
        schema_valid,
        "schema {method}: {raw}; {:?}",
        schema.iter_errors(raw).collect::<Vec<_>>()
    );
}

#[test]
fn all_core_results_have_known_schemas_and_roundtrip_without_legacy_fields() {
    let document = spec();
    assert_eq!(document["methods"].as_array().unwrap().len(), 17);
    for (method, _, encode) in cases() {
        let value = valid(method);
        check(method, &value, Some(true), true);
        assert_eq!(encode(value.clone()), value, "{method}");
        check(method, &json!([]), None, false);
        check(method, &json!("result"), None, false);
        check(method, &json!({}), None, false);
        let mut added = value;
        added["future"] = json!({"kind": "future", "status": "future", "result": "future"});
        check(method, &added, Some(true), true);
    }
    for method in ["getTransactionReceipt", "getBlockByHeight"] {
        check(method, &Value::Null, Some(true), true);
    }
    assert_eq!(thruput_json_rpc_api::WIRE_FORMAT_VERSION, "14.1.0");
}

#[test]
fn scheduler_status_keeps_an_optional_independent_attempt_diagnostic() {
    let diagnostic = json!({"attemptBlockHeight":"6", "cause": {
        "kind":"dbConflict", "conflict": {
            "address":identifier(), "key":"0x0102", "code":"insufficientFunds"
        }
    }});
    for state in ["pending", "retry", "dropped"] {
        let mut raw = json!({"status":state});
        if state != "pending" {
            raw["error"] = json!({"source":"executor", "code":17});
        }
        check("getTransactionStatus", &raw, Some(true), true);
        assert_eq!(roundtrip::<res::TransactionStatus>(raw.clone()), raw);
        raw["recentFailure"] = diagnostic.clone();
        check("getTransactionStatus", &raw, Some(true), true);
        assert_eq!(roundtrip::<res::TransactionStatus>(raw.clone()), raw);

        let mut unknown = raw.clone();
        unknown["recentFailure"]["cause"] = json!({"kind":"future", "payload":{"kept":true}});
        check("getTransactionStatus", &unknown, Some(false), false);
        // An unfamiliar nested cause cannot hide malformed known fields.
        unknown["recentFailure"]["attemptBlockHeight"] = json!(-1);
        check("getTransactionStatus", &unknown, None, false);
        for height in [
            json!(6),
            json!("-1"),
            json!("06"),
            json!("9223372036854775808"),
            Value::Null,
        ] {
            let mut invalid = raw.clone();
            invalid["recentFailure"]["attemptBlockHeight"] = height;
            check("getTransactionStatus", &invalid, None, false);
        }
        let mut invalid = raw.clone();
        invalid["recentFailure"]["cause"]["conflict"]
            .as_object_mut()
            .unwrap()
            .remove("key");
        check("getTransactionStatus", &invalid, None, false);
        raw["recentFailure"] = Value::Null;
        check("getTransactionStatus", &raw, None, false);
    }
}

#[test]
fn nullable_members_are_required_and_absence_is_not_inferred() {
    for (method, field) in [
        ("getStorage", "item"),
        ("getRegisteredAuth", "configuration"),
        ("getEvents", "nextCursor"),
        ("getTransactionsByRootAddress", "nextCursor"),
        ("getTransactionsByBlock", "nextCursor"),
    ] {
        let mut value = valid(method);
        value[field] = Value::Null;
        check(method, &value, Some(true), true);
        value.as_object_mut().unwrap().remove(field);
        check(method, &value, None, false);
    }
    for cursor in [
        json!("0x"),
        json!("0X01"),
        json!("0xabC0"),
        json!("0x1"),
        json!(12),
    ] {
        let mut value = valid("getEvents");
        value["nextCursor"] = cursor;
        check("getEvents", &value, None, false);
    }
    let mut empty = valid("getEvents");
    empty["events"] = json!([]);
    check("getEvents", &empty, Some(true), true);
}

#[test]
fn height_amount_and_integer_domains_are_lossless() {
    for (method, pointer) in [
        ("getChainTip", "/blockHeight"),
        ("getTransactionReceipt", "/blockHeight"),
        ("getTransactionStatus", "/blockHeight"),
        ("getStorage", "/stateBlockHeight"),
        ("simulateInvocation", "/stateBlockHeight"),
        ("getChainConfig", "/execution/observedTip/blockHeight"),
        ("getEvents", "/latestIndexedBlockHeight"),
        ("getBlockByHeight", "/blockHeight"),
    ] {
        for height in [
            json!(0),
            json!("-1"),
            json!("01"),
            json!("9223372036854775808"),
            json!(null),
        ] {
            let mut value = valid(method);
            *value.pointer_mut(pointer).unwrap() = height;
            check(method, &value, None, false);
        }
        let mut value = valid(method);
        *value.pointer_mut(pointer).unwrap() = json!(i64::MAX.to_string());
        check(method, &value, Some(true), true);
    }
    for (method, pointer) in [("getBalance", "/rawBalance"), ("getStorage", "/item/value")] {
        for amount in [
            json!("-1"),
            json!(1),
            json!("01"),
            json!("170141183460469231731687303715884105728"),
        ] {
            let mut value = valid(method);
            *value.pointer_mut(pointer).unwrap() = amount;
            check(method, &value, None, false);
        }
        let mut value = valid(method);
        *value.pointer_mut(pointer).unwrap() = json!(i128::MAX.to_string());
        check(method, &value, Some(true), true);
    }
    for (method, pointer) in [
        ("getTransactionReceipt", "/gasUsed"),
        ("simulateInvocation", "/gasUsed"),
        ("getBlockByHeight", "/blockTimepoint"),
        ("getChainConfig", "/execution/storageCosts/balanceCost"),
    ] {
        let mut value = valid(method);
        *value.pointer_mut(pointer).unwrap() = json!(u64::MAX.to_string());
        check(method, &value, Some(true), true);
        *value.pointer_mut(pointer).unwrap() = json!("18446744073709551616");
        check(method, &value, None, false);
    }
    let mut value = valid("getChainConfig");
    value["execution"]["storageCosts"]["bytestringCostPerByte"] = json!(u32::MAX.to_string());
    check("getChainConfig", &value, Some(true), true);
    value["execution"]["storageCosts"]["bytestringCostPerByte"] = json!("4294967296");
    check("getChainConfig", &value, None, false);
    for (method, pointer) in [
        ("getBlockByHeight", "/protocol"),
        ("getServerLimits", "/maxEventsPerPage"),
        ("getRegisteredAuth", "/configuration/signers/0/keyId"),
        ("getEvents", "/events/0/sequenceNumber"),
    ] {
        let mut value = valid(method);
        *value.pointer_mut(pointer).unwrap() = json!(4_294_967_296_u64);
        check(method, &value, None, false);
    }
}

#[test]
fn unknown_tags_retain_the_whole_result_without_hiding_invalid_siblings() {
    for (method, pointer, tag) in [
        ("getTransactionReceipt", "/execution", "result"),
        ("simulateInvocation", "/output", "result"),
        ("simulateInvocations", "/results/0/output", "result"),
        ("getStorage", "/item", "kind"),
        (
            "getRegisteredAuth",
            "/configuration/signers/0/publicKey",
            "scheme",
        ),
    ] {
        let mut value = valid(method);
        *value.pointer_mut(pointer).unwrap() =
            json!({(tag): "future", "nested": {"bytes": [1,2,3], "unknown": true}});
        check(method, &value, Some(false), false);
        let required = value
            .as_object()
            .unwrap()
            .keys()
            .find(|key| !pointer.starts_with(&format!("/{key}")))
            .unwrap()
            .clone();
        value.as_object_mut().unwrap().remove(&required);
        check(method, &value, None, false);
    }
    check(
        "getTransactionStatus",
        &json!({"status": "future", "nested": {"proof": "retained"}}),
        Some(false),
        false,
    );
    let mut value = receipt();
    value["execution"] = json!({"result": "future"});
    value["blockHeight"] = json!("-1");
    check("getTransactionReceipt", &value, None, false);
    let mut value = valid("getRegisteredAuth");
    value["configuration"]["signers"][0]["publicKey"] = json!({"scheme": "future"});
    value["configuration"]["signers"][0]["keyId"] = json!("3");
    check("getRegisteredAuth", &value, None, false);
    let mut value = valid("simulateInvocations");
    value["results"][0]["output"] = json!({"result": "future"});
    value["results"]
        .as_array_mut()
        .unwrap()
        .push(json!({"output": {"result": "return"}, "gasUsed": "1", "events": []}));
    check("simulateInvocations", &value, None, false);
}

#[test]
fn known_variants_require_payloads_and_keep_their_semantics() {
    for execution in [
        json!({"result": "return", "returnData": "0x"}),
        json!({"result": "abort", "abortData": "0xab"}),
        json!({"result": "fault"}),
        json!({"result": "nonDeterministicError"}),
        json!({"result": "failedGasSponsorship"}),
    ] {
        let mut value = receipt();
        value["execution"] = execution;
        check("getTransactionReceipt", &value, Some(true), true);
    }
    for execution in [
        json!({"result": "return"}),
        json!({"result": "abort"}),
        json!({"result": "return", "returnData": null}),
        json!({"result": "return", "returnData": "0xAB"}),
        json!({"result": 1}),
        json!({}),
        json!(["return", "0x"]),
    ] {
        let mut value = receipt();
        value["execution"] = execution;
        check("getTransactionReceipt", &value, None, false);
    }
    let mut value = receipt();
    value["execution"] = json!({"result": "other", "extra": {"retain": 1}});
    check("getTransactionReceipt", &value, Some(false), true);
    let mut value = valid("getStorage");
    value["item"] = json!({"kind": "unknown", "typeId": 19, "payload": [1,2]});
    check("getStorage", &value, Some(false), true);
    value["item"].as_object_mut().unwrap().remove("typeId");
    check("getStorage", &value, None, false);
    let mut value = valid("getRegisteredAuth");
    value["configuration"]["signers"][0]["publicKey"] = json!({"scheme": "unknown", "schemeId": 8});
    check("getRegisteredAuth", &value, Some(false), true);
    value["configuration"]["signers"][0]["publicKey"] = json!({"scheme": "unknown"});
    check("getRegisteredAuth", &value, None, false);
    for field in ["gasUsed", "events", "blockHeight", "transactionHash"] {
        let mut value = receipt();
        value.as_object_mut().unwrap().remove(field);
        check("getTransactionReceipt", &value, None, false);
    }
}

#[test]
fn scheduler_diagnostics_preserve_unknown_strings_and_added_fields() {
    for status in ["retry", "dropped"] {
        for source in ["auth", "scheduler", "executor", "other", "future"] {
            let value = json!({"status": status, "error": {"source": source, "code": -17, "new": {"detail": 4}}});
            check(
                "getTransactionStatus",
                &value,
                Some(!matches!(source, "other" | "future")),
                source != "future",
            );
        }
    }
    for kind in ["dbConflict", "contractRejected", "other", "future"] {
        let cause = match kind {
            "dbConflict" => {
                json!({"kind": kind, "conflict": {"address": identifier(), "key": "0x", "code": "future", "new": {"details": 4}}})
            }
            "contractRejected" => json!({"kind": kind, "contractIndex": 0, "code": "future"}),
            _ => json!({"kind": kind, "payload": {"nested": true}}),
        };
        let value = json!({"status": "recentFailure", "attemptBlockHeight": "42", "cause": cause});
        check("getTransactionStatus", &value, Some(false), kind == "other");
    }
    let mut value = valid("sendTransaction");
    value["admission"] = json!("future");
    check("sendTransaction", &value, Some(false), false);
    value["admission"] = json!(false);
    check("sendTransaction", &value, None, false);
}

#[test]
fn signed_diagnostic_bounds_match_the_reader() {
    let mut key = valid("getRegisteredAuth");
    key["configuration"]["signers"][0]["publicKey"] = json!({"scheme": "unknown", "schemeId": 1});
    let rows = [
        (
            "getTransactionStatus",
            json!({"status": "retry", "error": {"source": "scheduler", "code": 0}}),
            "/error/code",
            true,
        ),
        (
            "getStorage",
            json!({"stateBlockHeight": "1", "item": {"kind": "unknown", "typeId": 0}}),
            "/item/typeId",
            false,
        ),
        (
            "getStorage",
            json!({"stateBlockHeight": "1", "item": {"kind": "deployedContract", "hash": identifier(), "size": 0, "contractType": 0}}),
            "/item/contractType",
            true,
        ),
        (
            "getRegisteredAuth",
            key,
            "/configuration/signers/0/publicKey/schemeId",
            false,
        ),
    ];
    for (method, original, pointer, supported) in rows {
        for boundary in [i32::MIN, i32::MAX] {
            let mut value = original.clone();
            *value.pointer_mut(pointer).unwrap() = json!(boundary);
            check(method, &value, Some(supported), true);
        }
        for invalid in [
            json!(-2_147_483_649_i64),
            json!(2_147_483_648_i64),
            json!("1"),
            Value::Null,
        ] {
            let mut value = original.clone();
            *value.pointer_mut(pointer).unwrap() = invalid;
            check(method, &value, None, false);
        }
    }
}

#[test]
fn existing_diagnostic_vocabularies_and_storage_variants_are_preserved() {
    let vocabularies = [
        (
            "dbConflict",
            serde_json::to_value(schemars::schema_for!(
                thruput_json_rpc_types::transaction::ConflictCode
            ))
            .unwrap(),
        ),
        (
            "contractRejected",
            serde_json::to_value(schemars::schema_for!(
                thruput_json_rpc_types::transaction::ContractRejectCode
            ))
            .unwrap(),
        ),
    ];
    for (kind, schema) in vocabularies {
        let codes: Vec<&Value> = schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|variant| {
                variant.get("const").into_iter().chain(
                    variant
                        .get("enum")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                )
            })
            .collect();
        assert!(!codes.is_empty());
        for code in codes {
            let cause = if kind == "dbConflict" {
                json!({"kind": kind, "conflict": {"address": identifier(), "key": "0x", "code": code}})
            } else {
                json!({"kind": kind, "contractIndex": 0, "code": code})
            };
            check(
                "getTransactionStatus",
                &json!({"status": "recentFailure", "attemptBlockHeight": "42", "cause": cause}),
                Some(code != "other"),
                true,
            );
        }
    }
    for status in ["pending", "unknown", "other"] {
        check(
            "getTransactionStatus",
            &json!({"status": status}),
            Some(status != "other"),
            true,
        );
    }
    for item in [
        json!({"kind": "bytestring", "bytes": "0x"}),
        json!({"kind": "authData"}),
        json!({"kind": "retrocausalQueue"}),
        json!({"kind": "replayCacheEntry"}),
    ] {
        check(
            "getStorage",
            &json!({"stateBlockHeight": "0", "item": item}),
            Some(true),
            true,
        );
    }
    for output in [
        json!({"result": "return", "returnData": "0x"}),
        json!({"result": "abort", "abortData": "0xab"}),
        json!({"result": "fault"}),
    ] {
        let mut single = valid("simulateInvocation");
        single["output"] = output.clone();
        check("simulateInvocation", &single, Some(true), true);
        let mut batch = valid("simulateInvocations");
        batch["results"][0]["output"] = output;
        check("simulateInvocations", &batch, Some(true), true);
    }
}

#[test]
fn discovery_optionals_and_nested_objects_follow_the_native_contract() {
    for (method, pointer) in [
        ("getVersionInfo", "/commitHash"),
        ("getChainConfig", "/execution"),
        ("getServerLimits", "/simulationBatch"),
    ] {
        let mut value = valid(method);
        value[pointer.trim_start_matches('/')] = Value::Null;
        check(method, &value, None, false);
        value
            .as_object_mut()
            .unwrap()
            .remove(pointer.trim_start_matches('/'));
        check(method, &value, Some(true), true);
    }
    for (method, pointer) in [
        ("getChainConfig", "/stated"),
        ("getChainConfig", "/execution/storageCosts"),
        ("getServerLimits", "/simulationBatch"),
        ("getRegisteredAuth", "/configuration"),
        ("getRegisteredAuth", "/configuration/signers/0"),
        ("getEvents", "/events/0/event"),
    ] {
        let mut value = valid(method);
        *value.pointer_mut(pointer).unwrap() = json!([]);
        check(method, &value, None, false);
    }
    let mut value = valid("getVersionInfo");
    value.as_object_mut().unwrap().remove("wireFormat");
    check("getVersionInfo", &value, None, false);
    let mut value = valid("getChainConfig");
    value["stated"]["maxSha256Buffers"] = Value::Null;
    check("getChainConfig", &value, None, false);
    for bad in [json!("0x01"), json!(format!("0x{}", "AB".repeat(32)))] {
        let mut value = receipt();
        value["events"][0]["topics"][0] = bad;
        check("getTransactionReceipt", &value, None, false);
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
                "fixtures/json-schema-tools-meta.json"
            ))?)
        } else {
            Err(format!("unregistered offline schema: {uri}").into())
        }
    }
}

#[test]
fn native_result_document_passes_offline_openrpc_validation() {
    let meta: Value =
        serde_json::from_str(include_str!("fixtures/openrpc-meta-1.14.9.json")).unwrap();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .with_retriever(OfflineMeta)
        .build(&meta)
        .unwrap();
    let generated = spec();
    assert!(
        validator.is_valid(&generated),
        "{:?}",
        validator.iter_errors(&generated).collect::<Vec<_>>()
    );
    let mut broken = generated;
    broken["methods"][0]["result"] = json!(false);
    assert!(!validator.is_valid(&broken));
}

#[tokio::test]
async fn http_reader_distinguishes_known_unsupported_and_malformed_results() {
    // A synthetic responder tests decoding through ClientT, not hogd behavior.
    let mut module = RpcModule::new(());
    module
        .register_method("known", |_, (), _| receipt())
        .unwrap();
    let mut future = receipt();
    future["execution"] = json!({"result": "future", "payload": {"keep": [1,2]}});
    let expected = future.clone();
    module
        .register_method("future", move |_, (), _| future.clone())
        .unwrap();
    let mut invalid = receipt();
    invalid["execution"] = json!({"result": "return"});
    module
        .register_method("malformed", move |_, (), _| invalid.clone())
        .unwrap();
    let transport = Server::builder().build("127.0.0.1:0").await.unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", transport.local_addr().unwrap()))
        .unwrap();
    let handle = transport.start(module);
    let result: DecodedResponse<res::ReceiptResult> = client
        .request("known", NamedParams(req::EmptyRequest {}))
        .await
        .unwrap();
    assert!(matches!(result, DecodedResponse::Known(Some(_))));
    let result: DecodedResponse<res::ReceiptResult> = client
        .request("future", NamedParams(req::EmptyRequest {}))
        .await
        .unwrap();
    match result {
        DecodedResponse::Unsupported { raw } => assert_eq!(raw, expected),
        _ => panic!("future execution must remain unsupported"),
    }
    assert!(
        client
            .request::<DecodedResponse<res::ReceiptResult>, _>(
                "malformed",
                NamedParams(req::EmptyRequest {})
            )
            .await
            .is_err()
    );
    handle.stop().unwrap();
    handle.stopped().await;
}
