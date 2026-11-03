use crate::call::ResolvedCall;
use std::sync::{Arc, Mutex};
use thruput_json_rpc_types::view::ViewRequest;

use jsonrpsee::{
    RpcModule,
    http_client::{HttpClient, HttpClientBuilder},
    server::{ServerBuilder, ServerHandle},
};
use serde_json::{Value, json};
use thruput_json_rpc_types::{BlockHeight, NetworkHash, responses::EmittedEvent};

use super::*;

type Calls = Arc<Mutex<Vec<(&'static str, Value)>>>;

fn prepared() -> PreparedView {
    PreparedView {
        rpc_url: "http://127.0.0.1:1".into(),
        native_profile: serde_json::from_value(json!({
            "name":"test", "contracts":{}, "wallets":{}
        }))
        .unwrap(),
        method: "0x0101010101010101".into(),
        resolved: ResolvedCall {
            entrypoint: thruput_xdr::contract::ContractEntrypointID::from([1; 8]),
            metadata: None,
        },
        request: ViewRequest {
            auth_claim: None,
            min_height: None,
            contract: ContractAddress::from(thruput_xdr::types::ContractAddress::from([4; 32])),
            entrypoint: HexBytes(vec![1; 8]),
            calldata: HexBytes(vec![2, 3]),
            caller: Some(ContractAddress::from(
                thruput_xdr::types::ContractAddress::from([5; 32]),
            )),
            gas_limit: None,
        },
    }
}

fn simulation(output: InvocationOutput) -> SimulationResult {
    SimulationResult {
        state_block_height: BlockHeight::from(9_007_199_254_740_993),
        output,
        events: vec![EmittedEvent {
            address: prepared().request.contract,
            topics: vec![],
            data: HexBytes(vec![6]),
        }],
        gas_used: u64::MAX,
    }
}

#[test]
fn report_preserves_native_simulation_fields_and_wide_integers() {
    let report = NativeViewReport::new(
        &prepared(),
        simulation(InvocationOutput::Return {
            return_data: HexBytes(vec![7]),
        }),
    );
    let value = serde_json::to_value(CommandResult::new("view", report)).unwrap();
    assert_eq!(value["ok"], true);
    let data = &value["data"];
    assert_eq!(data["schemaVersion"], 4);
    assert_eq!(data["stateBlockHeight"], "9007199254740993");
    assert_eq!(data["gasUsed"], u64::MAX.to_string());
    assert_eq!(
        data["output"],
        json!({"result":"return","returnData":"0x07"})
    );
    assert_eq!(
        data["events"][0]["address"],
        json!(prepared().request.contract)
    );
    assert_eq!(data["address"], json!(prepared().request.contract));
    assert!(data.get("height").is_none());
    assert!(data.get("contract").is_none());
    assert!(data.get("simulation").is_none());
    let schema = serde_json::to_value(schemars::schema_for!(NativeViewReport)).unwrap();
    assert!(schema.to_string().contains("stateBlockHeight"));
}

#[test]
fn abort_and_fault_are_command_failures_with_native_evidence() {
    for output in [
        InvocationOutput::Abort {
            abort_data: HexBytes(vec![0xab]),
        },
        InvocationOutput::Fault {},
    ] {
        let error = execution_failure(&output).unwrap();
        let report = NativeViewReport::new(&prepared(), simulation(output.clone()));
        let value =
            serde_json::to_value(CommandResult::failure_with_error("view", report, error)).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["data"]["output"], json!(output));
        assert_eq!(value["error"]["details"]["output"], json!(output));
        assert_eq!(value["data"]["events"][0]["data"], "0x06");
    }
}

async fn endpoint(
    wire: &str,
    methods: &[&str],
    simulation: Value,
) -> (NativeClient<HttpClient>, ServerHandle, Calls) {
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    let version = json!({"wireFormat":wire,"implementation":"test","supportedMethods":methods});
    let network =
        json!({"networkHash":NetworkHash::from(thruput_xdr::types::SHA256Hash::from([9;32]))});
    for (method, response) in [
        ("getVersionInfo", version),
        ("getNetworkInfo", network),
        ("simulateInvocation", simulation),
    ] {
        module
            .register_method(method, move |params, calls, _| {
                let params: Value = params.parse()?;
                calls.lock().unwrap().push((method, params));
                Ok::<_, jsonrpsee::types::ErrorObjectOwned>(response.clone())
            })
            .unwrap();
    }
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let handle = server.start(module);
    let client = NativeClient::new(HttpClientBuilder::default().build(url).unwrap());
    (client, handle, calls)
}

#[tokio::test]
async fn simulation_uses_only_native_methods_and_named_address_request() {
    let response = json!(simulation(InvocationOutput::Return {
        return_data: HexBytes(vec![])
    }));
    let (client, handle, calls) = endpoint("15.0.0", &["simulateInvocation"], response).await;
    fetch_simulation(&client, &prepared(), None).await.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "getVersionInfo");
    assert_eq!(calls[1].0, "simulateInvocation");
    assert_eq!(
        calls[1].1,
        json!({
            "address": prepared().request.contract,
            "entrypoint":"0x0101010101010101", "calldata":"0x0203",
            "caller":prepared().request.caller,
        })
    );
    handle.stop().unwrap();
}

#[tokio::test]
async fn incompatible_endpoint_stops_before_simulation_without_legacy_fallback() {
    for (wire, methods) in [("14.4.0", vec!["simulateInvocation"]), ("15.0.0", vec![])] {
        let (client, handle, calls) = endpoint(wire, &methods, Value::Null).await;
        assert!(fetch_simulation(&client, &prepared(), None).await.is_err());
        assert_eq!(calls.lock().unwrap().len(), 1);
        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn pinned_network_mismatch_stops_before_simulation() {
    let (client, handle, calls) = endpoint(
        "15.0.0",
        &["simulateInvocation", "getNetworkInfo"],
        Value::Null,
    )
    .await;
    let expected =
        thruput_xdr::types::NetworkHash::from(thruput_xdr::types::SHA256Hash::from([8; 32]));
    let error = fetch_simulation(&client, &prepared(), Some(&expected))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("network identity mismatch"));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|call| call.0).collect::<Vec<_>>(),
        ["getVersionInfo", "getNetworkInfo"]
    );
    handle.stop().unwrap();
}

#[tokio::test]
async fn unfamiliar_simulation_retains_complete_raw_result() {
    let raw = json!({"stateBlockHeight":"7", "output":{"result":"future","proof":"opaque"},
        "events":[], "gasUsed":"0", "futureField":{"evidence":42}});
    let (client, handle, _) = endpoint("15.0.0", &["simulateInvocation"], raw.clone()).await;
    let error = fetch_simulation(&client, &prepared(), None)
        .await
        .unwrap_err();
    let CliError::NativeRpcFailure { evidence, .. } = error else {
        panic!("expected native evidence")
    };
    assert_eq!(evidence, raw);
    handle.stop().unwrap();
}

fn manifest_profile(dir: &Path) -> crate::config::ResolvedProfile {
    use thruput_json_rpc_types::NetworkHashText;

    let path = dir.join("runtime.json");
    let manifest = json!({
        "schemaVersion": 5,
        "kind": "hogd-dev-runtime",
        "chainState": "fresh",
        "networkHash": NetworkHashText::from([8; 32]),
        "protocol": "hogp/test00000000000",
        "genesisHash": NetworkHashText::from([0x42; 32]),
        "producer": { "binary": "hog-devnode", "version": "0.1.0" },
        "endpoints": { "scheduler": "127.0.0.1:1000" },
        "bootstrapAccounts": [],
        "bootstrap": {
            "storageCostModel": {
                "hash": NetworkHashText::from([0x53; 32]),
                "baseDataItemCost": "12000", "bytestringCostPerByte": "10",
                "baseContractDeploymentCost": "12000", "contractDeploymentCostPerByte": "10",
                "authPerKeyCost": "1111", "rcqCostPerEntry": "8",
                "replayCacheCostPerEntry": "7", "balanceCost": "1234"
            }
        }
    });
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut profile = prepared().native_profile;
    profile.runtime_manifest = Some(path);
    // Isolate from the process environment without mutating it. The shared
    // loader separately tests CLI/environment/profile path precedence.
    profile.runtime_manifest_cli_override = true;
    profile
}

#[tokio::test]
async fn manifest_only_network_pin_rejects_mismatching_endpoint_before_simulation() {
    let dir = tempfile::tempdir().unwrap();
    let profile = manifest_profile(dir.path());
    assert!(profile.network_hash.is_none());
    assert!(profile.default_key.is_none());
    let expected = expected_network(&profile).unwrap().unwrap();
    assert_eq!(
        expected,
        thruput_xdr::types::NetworkHash::from(thruput_xdr::types::SHA256Hash::from([8; 32]))
    );
    let (client, handle, calls) = endpoint(
        "15.0.0",
        &["simulateInvocation", "getNetworkInfo"],
        Value::Null,
    )
    .await;
    let error = fetch_simulation(&client, &prepared(), Some(&expected))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("network identity mismatch"));
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.0)
            .collect::<Vec<_>>(),
        ["getVersionInfo", "getNetworkInfo"]
    );
    handle.stop().unwrap();
}

#[test]
fn configured_manifest_pin_takes_precedence_over_stale_profile_hash() {
    let dir = tempfile::tempdir().unwrap();
    let mut profile = manifest_profile(dir.path());
    profile.network_hash = Some(thruput_json_rpc_types::NetworkHashText::from([9; 32]).to_string());
    assert_eq!(
        expected_network(&profile).unwrap(),
        Some(thruput_xdr::types::NetworkHash::from(
            thruput_xdr::types::SHA256Hash::from([8; 32])
        ))
    );
}

#[test]
fn native_report_decodes_return_with_resolved_metadata() {
    use crate::codec::{CallMetadata, ContractMetadata, MethodMetadata};
    let mut prepared = prepared();
    prepared.resolved.metadata = Some(CallMetadata::Local(ContractMetadata {
        interface_name: "Counter".into(),
        version: "0.1.0".into(),
        source: std::path::PathBuf::from("/tmp/implementation.rs"),
        method: MethodMetadata {
            arg_types: vec![],
            return_type: Some("u64".into()),
        },
    }));
    let report = NativeViewReport::new(
        &prepared,
        simulation(InvocationOutput::Return {
            return_data: HexBytes(xdrs::cxs::encode(&7_u64).unwrap()),
        }),
    );
    assert_eq!(report.return_value, Some(json!("7")));
}
