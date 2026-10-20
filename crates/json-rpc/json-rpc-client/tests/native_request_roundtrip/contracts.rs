//! Request-only fixtures for the complete core method map. Echo results here
//! are test acknowledgements, not proposed native response contracts.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Serialize, de::DeserializeOwned};
use thruput_json_rpc_types::requests::AddressQuery;

type Parse = fn(&Params<'_>) -> Result<Value, jsonrpsee::types::ErrorObjectOwned>;

fn named<T: DeserializeOwned + Serialize>(
    params: &Params<'_>,
) -> Result<Value, jsonrpsee::types::ErrorObjectOwned> {
    Ok(serde_json::to_value(parse_named::<T>(params)?).unwrap())
}

fn empty(params: &Params<'_>) -> Result<Value, jsonrpsee::types::ErrorObjectOwned> {
    Ok(serde_json::to_value(parse_empty(params)?).unwrap())
}

// Reuse the authoritative name/type map; this legacy request-only regression
// suite filters optional methods. Real adapters are covered by native_transport.
macro_rules! parser {
    (empty, $request:ty) => {
        empty
    };
    (named, $request:ty) => {
        named::<$request>
    };
}
macro_rules! core_requests {
    ($( $(#[$doc:meta])* $name:ident($wire:literal, $parse:ident, $decorate:ident): $request:ty => $response:ty; )*) => {
        fn parsers() -> Vec<(&'static str, Parse)> {
            let all: Vec<(&'static str, Parse)> = vec![$(($wire, parser!($parse, $request))),*];
            all.into_iter().filter(|(name, _)| is_core(name)).collect()
        }

        fn serialize_request(method: &str, value: Value) -> Box<RawValue> {
            match method {
                $($wire => NamedParams(serde_json::from_value::<$request>(value).unwrap())
                    .to_rpc_params().unwrap().unwrap(),)*
                _ => panic!("unknown fixture method {method}"),
            }
        }
    };
}
thruput_json_rpc_api::native_methods!(core_requests);

fn is_core(name: &str) -> bool {
    !name.starts_with("faucet_") && !name.starts_with("dev_")
}

fn request_spec() -> Value {
    let mut project = Project::new(ProjectInfo {
        title: "Native core request regression contract".into(),
        version: "14.0.0".into(),
    });
    project.module(thruput_json_rpc_api::native::NativeApiOpenRpc::module_doc);
    let mut document = project.to_json();
    document["methods"]
        .as_array_mut()
        .unwrap()
        .retain(|method| is_core(method["name"].as_str().unwrap()));
    document
}

fn address() -> String {
    serde_json::to_value(thruput_json_rpc_types::ContractAddress::from(
        thruput_xdr::types::ContractAddress::from([7; 32]),
    ))
    .unwrap()
    .as_str()
    .unwrap()
    .to_owned()
}

fn invocation() -> Value {
    json!({"address": address(), "entrypoint": "0x0102030405060708", "calldata": "0x"})
}

fn valid_request(method: &str) -> Value {
    match method {
        "sendTransaction" => json!({"signedTransaction": "0x0102"}),
        "getTransactionStatus" | "getTransactionReceipt" => json!({"transactionHash": address()}),
        "simulateInvocation" => invocation(),
        "simulateInvocations" => json!({"invocations": [invocation()]}),
        "getStorage" => json!({"address": address(), "key": "0x0102"}),
        "getBalance" | "getRegisteredAuth" => json!({"address": address()}),
        "getEvents" => {
            json!({"filters": [{"addresses": [address()]}], "start": {"order": "ascending", "anchor": {"kind": "atBeginning"}}})
        }
        "getTransactionsByRootAddress" => {
            json!({"address": address(), "start": {"order": "descending", "anchor": {"kind": "atHeight", "blockHeight": "9007199254740993"}}})
        }
        "getBlockByHeight" => json!({"blockHeight": "9223372036854775807"}),
        "getTransactionsByBlock" => json!({"blockHeight": "812", "start": {"order": "ascending"}}),
        _ => json!({}),
    }
}

fn validator(document: &Value, index: usize) -> jsonschema::Validator {
    super::schema::at(document, &format!("/methods/{index}/x-params-schema"))
}

fn assert_case(method: &str, value: &Value, accepted: bool) {
    let document = request_spec();
    let index = document["methods"]
        .as_array()
        .unwrap()
        .iter()
        .position(|m| m["name"] == method)
        .unwrap();
    let schema = validator(&document, index);
    let parser = parsers()
        .into_iter()
        .find(|(name, _)| *name == method)
        .unwrap()
        .1;
    let raw = value.to_string();
    assert_eq!(
        parser(&Params::new(Some(&raw))).is_ok(),
        accepted,
        "runtime {method}: {value}"
    );
    assert_eq!(
        schema.is_valid(value),
        accepted,
        "schema {method}: {value}; {:?}",
        schema.iter_errors(value).collect::<Vec<_>>()
    );
}

#[test]
fn all_core_methods_put_required_parameters_first() {
    let document = request_spec();
    let methods = document["methods"].as_array().unwrap();
    assert_eq!(methods.len(), 17);
    for method in methods {
        let mut saw_optional = false;
        for param in method["params"].as_array().unwrap() {
            if param["required"].as_bool().unwrap_or(false) {
                assert!(
                    !saw_optional,
                    "{}: required {} follows an optional parameter",
                    method["name"], param["name"]
                );
            } else {
                saw_optional = true;
            }
        }
    }
}

#[test]
fn all_core_request_schemas_match_strict_parsing() {
    let document = request_spec();
    assert_eq!(document["methods"].as_array().unwrap().len(), 17);
    let meta: Value =
        serde_json::from_str(include_str!("../fixtures/openrpc-meta-1.14.9.json")).unwrap();
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .with_retriever(OfflineMeta)
        .build(&meta)
        .unwrap()
        .validate(&document)
        .unwrap();
    for (method, _) in parsers() {
        let valid = valid_request(method);
        assert_case(method, &valid, true);
        let mut extra = valid.clone();
        extra["unexpected"] = json!(true);
        assert_case(method, &extra, false);
        if valid != json!({}) {
            for malformed in [json!(null), json!([]), json!({}), json!({"request": valid})] {
                assert_case(method, &malformed, false);
            }
        }
    }
}

#[test]
fn simulation_nested_fields_and_wide_integers_match_schema() {
    let mut array_auth = invocation();
    array_auth["assumedAuth"] = json!(["sourceAddress", 1, 1]);
    assert_case("simulateInvocation", &array_auth, false);
    assert_case(
        "simulateInvocations",
        &json!({"invocations": [[address(), "0x0102030405060708", "0x"]]}),
        false,
    );
    for field in ["caller", "assumedAuth", "gasLimit", "minStateBlockHeight"] {
        let mut request = invocation();
        request[field] = Value::Null;
        assert_case("simulateInvocation", &request, false);
    }
    for gas in [
        json!(0),
        json!("0"),
        json!("01"),
        json!("18446744073709551616"),
    ] {
        let mut request = invocation();
        request["gasLimit"] = gas;
        assert_case("simulateInvocation", &request, false);
    }
    let mut request = invocation();
    request["gasLimit"] = json!("18446744073709551615");
    request["minStateBlockHeight"] = json!("9007199254740993");
    request["assumedAuth"] = json!({"kind": "pubkeySignature", "pubkeyHash": address(), "accountNonce": "18446744073709551615"});
    assert_case("simulateInvocation", &request, true);
    request["assumedAuth"]["nonce"] = json!("1");
    assert_case("simulateInvocation", &request, false);
    for field in ["allowedKeys", "requiredWeight"] {
        let mut request = invocation();
        request["assumedAuth"] =
            json!({"kind": "sourceAddress", "allowedKeys": 1, "requiredWeight": 1});
        request["assumedAuth"][field] = json!(4_294_967_296_u64);
        assert_case("simulateInvocation", &request, false);
    }
    for entrypoint in ["0x01", "0102030405060708", "0x01020304050607AB"] {
        let mut request = invocation();
        request["entrypoint"] = json!(entrypoint);
        assert_case("simulateInvocation", &request, false);
    }
    for request in [
        json!({"invocations": []}),
        json!({"invocations": [invocation()], "minStateBlockHeight": null}),
    ] {
        assert_case("simulateInvocations", &request, false);
    }
    let mut item = invocation();
    item["minStateBlockHeight"] = json!("1");
    assert_case(
        "simulateInvocations",
        &json!({"invocations": [item]}),
        false,
    );
}

#[test]
fn history_cross_field_rules_match_schema() {
    let mut array_anchor = valid_request("getEvents");
    array_anchor["start"]["anchor"] = json!(["atBeginning"]);
    assert_case("getEvents", &array_anchor, false);
    assert_case(
        "getTransactionsByBlock",
        &json!({"blockHeight": "1", "start": ["ascending"]}),
        false,
    );
    assert_case(
        "getEvents",
        &json!({"filters": [[[address()]]], "cursor": "0x01"}),
        false,
    );
    assert_case(
        "getEvents",
        &json!({"filters": [{"addresses": [address()], "topics": []}], "cursor": "0x01"}),
        false,
    );
    for method in [
        "getEvents",
        "getTransactionsByRootAddress",
        "getTransactionsByBlock",
    ] {
        let original = valid_request(method);
        for limit in [json!(0), json!(null), json!("1"), json!(4_294_967_296_u64)] {
            let mut request = original.clone();
            request["limit"] = limit;
            assert_case(method, &request, false);
        }
        let mut request = original.clone();
        request["cursor"] = json!("0x0102");
        assert_case(method, &request, false);
        request.as_object_mut().unwrap().remove("start");
        assert_case(method, &request, true);
        request["limit"] = json!(u32::MAX);
        // Configured endpoint caps are checked during server integration.
        assert_case(method, &request, true);
        for cursor in [json!("0x"), json!(null), json!("0x0"), json!("0X0102")] {
            request["cursor"] = cursor;
            assert_case(method, &request, false);
        }
        request.as_object_mut().unwrap().remove("cursor");
        assert_case(method, &request, false);
        let mut request = original;
        request["start"]["extra"] = json!(true);
        assert_case(method, &request, false);
    }
    for (order, kind) in [("ascending", "atEnd"), ("descending", "atBeginning")] {
        let mut request = valid_request("getEvents");
        request["start"] = json!({"order": order, "anchor": {"kind": kind}});
        assert_case("getEvents", &request, false);
    }
    let mut request = valid_request("getEvents");
    request["start"]["anchor"]["extra"] = json!(1);
    assert_case("getEvents", &request, false);
    for filter in [
        json!({"addresses": []}),
        json!({"addresses": [address()], "topics": null}),
        json!({"addresses": [address()], "topics": {"topic3": []}}),
        json!({"addresses": [address()], "topics": {"topic0": ["0x01"]}}),
    ] {
        let mut request = valid_request("getEvents");
        request["filters"] = json!([filter]);
        assert_case("getEvents", &request, false);
    }
}

#[test]
fn identifier_decoding_and_production_registration_keep_their_boundaries() {
    for address in [
        "0".repeat(32),
        format!("0x{}", "ab".repeat(32)),
        "1".repeat(31),
        "z".repeat(44),
    ] {
        let raw = json!({"address": address}).to_string();
        assert!(named::<AddressQuery>(&Params::new(Some(&raw))).is_err());
    }
    // Shared Base58 schemas describe alphabet/encoded length, not exact decoded
    // length. This semantic check belongs to the existing typed codec; do not
    // claim that a generic schema validator alone validates identifiers.
    let document = request_spec();
    let index = document["methods"]
        .as_array()
        .unwrap()
        .iter()
        .position(|m| m["name"] == "getBalance")
        .unwrap();
    let wrong_width = json!({"address": "1".repeat(33)});
    assert!(validator(&document, index).is_valid(&wrong_width));
    assert!(named::<AddressQuery>(&Params::new(Some(&wrong_width.to_string()))).is_err());

    let production = GroundhogRpcServer::unified(
        Backend::default(),
        network(),
        RpcServerConfig::default().with_tip_refresh_interval_ms(0),
        BuildInfo::for_tests(),
    )
    .into_rpc();
    for (native, _) in parsers() {
        assert!(!production.method_names().any(|name| name == native));
    }
    assert_eq!(thruput_json_rpc_api::WIRE_FORMAT_VERSION, "14.0.0");
}

fn register_request_echo<T: Send + Sync + 'static>(
    context: T,
    calls: &Arc<AtomicUsize>,
    captured: &Captured,
) -> RpcModule<T> {
    let mut module = RpcModule::new(context);
    for (method, parse) in parsers() {
        let calls = calls.clone();
        let captured = captured.clone();
        module
            .register_method(method, move |params, _, _| {
                record(&captured, method, &params);
                let request = parse(&params)?;
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, jsonrpsee::types::ErrorObjectOwned>(request)
            })
            .unwrap();
    }
    module
}

#[tokio::test]
async fn all_core_requests_use_direct_named_http_params() {
    for split in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Captured::default();
        let config = RpcServerConfig::default().with_tip_refresh_interval_ms(0);
        let module = if split {
            register_request_echo(
                GroundhogRpcServer::new(
                    Unused,
                    Backend::default(),
                    Unused,
                    Unused,
                    network(),
                    config,
                    BuildInfo::for_tests(),
                ),
                &calls,
                &captured,
            )
            .remove_context()
        } else {
            register_request_echo(
                GroundhogRpcServer::unified(
                    Backend::default(),
                    network(),
                    config,
                    BuildInfo::for_tests(),
                ),
                &calls,
                &captured,
            )
            .remove_context()
        };
        let transport = Server::builder().build("127.0.0.1:0").await.unwrap();
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", transport.local_addr().unwrap()))
            .unwrap();
        let handle = transport.start(module);
        for (method, _) in parsers() {
            let original = valid_request(method);
            let serialized = serialize_request(method, original.clone());
            let expected: Value = serde_json::from_str(serialized.get()).unwrap();
            assert_eq!(
                expected, original,
                "serialization must preserve the request"
            );
            let result: Value = client
                .request(method, RawParams(Some(serialized.get())))
                .await
                .unwrap();
            assert_eq!(result, expected);
            assert_eq!(
                captured.lock().unwrap().last().unwrap().1.as_deref(),
                Some(serialized.get())
            );
            let before = calls.load(Ordering::SeqCst);
            let mut invalid = expected;
            invalid["extra"] = json!(true);
            let invalid = invalid.to_string();
            let error = client
                .request::<Value, _>(method, RawParams(Some(&invalid)))
                .await
                .unwrap_err();
            let jsonrpsee::core::client::Error::Call(error) = error else {
                panic!("{error}")
            };
            assert_eq!(error.code(), -32602);
            assert_eq!(calls.load(Ordering::SeqCst), before);
        }
        handle.stop().unwrap();
        handle.stopped().await;
    }
}
