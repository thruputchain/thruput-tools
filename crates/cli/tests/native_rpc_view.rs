//! Executable CLI coverage for the explicit native simulation path.

#[path = "support/isolation.rs"]
mod isolation;

use std::{
    process::Command,
    sync::{Arc, Mutex},
};

use jsonrpsee::{
    RpcModule,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use serde_json::{Value, json};
use tempfile::TempDir;

type Calls = Arc<Mutex<Vec<(&'static str, Value)>>>;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(rpc: &str) -> Self {
        let dir = TempDir::new().unwrap();
        // Simulation must work without a key, wallet, or signer configuration.
        std::fs::write(
            dir.path().join("thruput.toml"),
            format!("default_profile = 'default'\n[profile.default]\nrpc_url = '{rpc}'\n"),
        )
        .unwrap();
        Self { dir }
    }

    fn view(&self, success: bool) -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
        command
            .args([
                "--json",
                "--non-interactive",
                "view",
                &address(),
                "0x0102030405060708",
                "--calldata",
                "0xabcd",
            ])
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("THRUPUT_PROFILE")
            .env_remove("THRUPUT_RUNTIME_MANIFEST");
        isolation::isolate_from_developer_env(&mut command);
        let output = command.output().unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Parsing the entire buffer rejects additional output or a second envelope.
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ok"], success);
        assert_eq!(report["command"], "view");
        report
    }
}

fn address() -> String {
    thruput_encoding::encode(&[0x42; 32])
}

fn discovery() -> Value {
    json!({"wireFormat":"15.0.0","implementation":"test","supportedMethods":["simulateInvocation"]})
}

fn simulation(output: &Value) -> Value {
    json!({
        "stateBlockHeight":"9007199254740993",
        "output":output,
        "events":[{"address":address(),"topics":[],"data":"0x1234"}],
        "gasUsed":"18446744073709551615",
    })
}

async fn server(
    discovery: Value,
    simulation: Result<Value, ErrorObjectOwned>,
) -> (String, ServerHandle, Calls) {
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    for (method, result) in [
        ("getVersionInfo", Ok(discovery)),
        ("simulateInvocation", simulation),
        (
            "groundhog_view",
            Ok(
                json!({"height":"42","output":{"result":"return","returnData":"0x09"},"events":[],"gasUsed":"17"}),
            ),
        ),
    ] {
        module
            .register_method(method, move |params, calls, _| {
                calls.lock().unwrap().push((method, params.parse()?));
                result.clone()
            })
            .unwrap();
    }
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    (url, server.start(module), calls)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_view_uses_native_wire_without_legacy_calls() {
    let response = simulation(&json!({"result":"return","returnData":"0x07"}));
    let (url, handle, calls) = server(discovery(), Ok(response.clone())).await;
    let fixture = Fixture::new(&url);
    let native = fixture.view(true);
    assert_eq!(native["data"]["schemaVersion"], 4);
    assert_eq!(native["data"]["address"], address());
    for field in ["stateBlockHeight", "output", "events", "gasUsed"] {
        assert_eq!(native["data"][field], response[field]);
    }
    assert!(native["data"].get("height").is_none());
    assert!(native["data"].get("contract").is_none());
    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded.iter().map(|call| call.0).collect::<Vec<_>>(),
        ["getVersionInfo", "simulateInvocation"]
    );
    assert_eq!(recorded[0].1, json!({}));
    assert_eq!(
        recorded[1].1,
        json!({"address":address(),"entrypoint":"0x0102030405060708","calldata":"0xabcd"})
    );
    handle.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_and_fault_exit_unsuccessfully_with_complete_simulation_on_stdout() {
    for (output, code) in [
        (
            json!({"result":"abort","abortData":"0x12"}),
            "simulation_aborted",
        ),
        (json!({"result":"fault"}), "simulation_faulted"),
    ] {
        let response = simulation(&output);
        let (url, handle, calls) = server(discovery(), Ok(response.clone())).await;
        let report = Fixture::new(&url).view(false);
        assert_eq!(report["data"]["schemaVersion"], 4);
        assert_eq!(report["error"]["code"], code);
        assert_eq!(report["error"]["details"]["output"], output);
        for field in ["stateBlockHeight", "output", "events", "gasUsed"] {
            assert_eq!(report["data"][field], response[field]);
        }
        assert_eq!(calls.lock().unwrap().len(), 2);
        handle.stop().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_simulation_output_keeps_raw_evidence_and_fails_without_retry() {
    let mut response = simulation(&json!({"result":"future","proof":{"opaque":true}}));
    response["newField"] = json!({"retain":"all"});
    let (url, handle, calls) = server(discovery(), Ok(response.clone())).await;
    let report = Fixture::new(&url).view(false);
    assert_eq!(report["data"]["schemaVersion"], 4);
    assert_eq!(report["data"]["evidence"], response);
    assert_eq!(report["error"]["details"], response);
    assert_eq!(calls.lock().unwrap().len(), 2);
    handle.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incompatible_discovery_stops_before_simulation_or_legacy_fallback() {
    let mut wrong_wire = discovery();
    wrong_wire["wireFormat"] = json!("13.0.0");
    let mut missing_method = discovery();
    missing_method["supportedMethods"] = json!([]);
    for version in [wrong_wire, missing_method, json!({"version":"foreign"})] {
        let (url, handle, calls) = server(version, Ok(Value::Null)).await;
        let report = Fixture::new(&url).view(false);
        assert_eq!(report["data"]["schemaVersion"], 4);
        assert!(!report["error"]["message"].as_str().unwrap().is_empty());
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "getVersionInfo");
        handle.stop().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_error_preserves_code_message_and_data_on_stdout() {
    let data = json!({"kind":"backendUnavailable","futureDetail":{"retain":true}});
    let error = ErrorObjectOwned::owned(-32050, "simulation backend unavailable", Some(data));
    let expected = json!(error);
    let (url, handle, calls) = server(discovery(), Err(error)).await;
    let report = Fixture::new(&url).view(false);
    assert_eq!(report["data"]["schemaVersion"], 4);
    assert_eq!(report["data"]["evidence"]["rpcError"], expected);
    assert_eq!(report["error"]["details"]["rpcError"], expected);
    assert_eq!(calls.lock().unwrap().len(), 2);
    handle.stop().unwrap();
}
