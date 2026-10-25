//! Executable one-page history commands against a native endpoint.

#[path = "support/isolation.rs"]
mod isolation;

use jsonrpsee::{
    RpcModule,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use serde_json::{Value, json};
use std::{
    process::Command,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;

type Calls = Arc<Mutex<Vec<(&'static str, Value)>>>;

fn address() -> String {
    thruput_encoding::encode(&[0x42; 32])
}
fn hash() -> String {
    thruput_encoding::encode(&[0x43; 32])
}

async fn server(
    method: &'static str,
    result: Result<Value, ErrorObjectOwned>,
) -> (String, ServerHandle, Calls) {
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    for (method, result) in [
        (
            "getVersionInfo",
            Ok(
                json!({"wireFormat":"14.0.0", "implementation":"test", "supportedMethods":[method]}),
            ),
        ),
        (method, result),
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

fn command(url: &str, args: &[&str], success: bool) -> Value {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("thruput.toml"),
        format!("default_profile = 'default'\n[profile.default]\nrpc_url = '{url}'\n"),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
    command
        .args(["--json", "--non-interactive"])
        .args(args)
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
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
    let report: Value = if success {
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    } else {
        assert!(
            output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stderr).unwrap()
    };
    assert_eq!(report["ok"], success);
    report
}

fn root_page(cursor: &Value) -> Value {
    json!({"transactions":[{"transactionHash":hash(),"blockHeight":"9007199254740993"}],"nextCursor":cursor,"oldestIndexedBlockHeight":"1","latestIndexedBlockHeight":"9007199254740993"})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_history_uses_native_named_params_and_preserves_cli_report() {
    let (url, handle, calls) = server(
        "getTransactionsByRootAddress",
        Ok(root_page(&json!("0x0102"))),
    )
    .await;
    let report = command(&url, &["tx", "list", &address(), "--limit", "1"], true);
    assert_eq!(report["data"]["schemaVersion"], 2);
    assert_eq!(report["data"]["refs"][0]["txHash"], hash());
    assert_eq!(report["data"]["refs"][0]["height"], "9007199254740993");
    assert_eq!(report["data"]["cursor"], "0x0102");
    {
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[1].0, "getTransactionsByRootAddress");
        assert_eq!(
            recorded[1].1,
            json!({"address":address(),"limit":1,"start":{"order":"descending","anchor":{"kind":"atEnd"}}})
        );
    }
    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continuation_forwards_cursor_without_start_and_maps_terminal_marker() {
    let (url, handle, calls) =
        server("getTransactionsByRootAddress", Ok(root_page(&Value::Null))).await;
    let report = command(
        &url,
        &[
            "tx",
            "list",
            &address(),
            "--limit",
            "3",
            "--cursor",
            "0x0102",
        ],
        true,
    );
    assert_eq!(report["data"]["cursor"], "0x");
    assert_eq!(
        calls.lock().unwrap()[1].1,
        json!({"address":address(),"limit":3,"cursor":"0x0102"})
    );
    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_event_page_keeps_continuation_and_topic_filters() {
    let (url, handle, calls) = server("getEvents", Ok(json!({"events":[],"nextCursor":"0xaabb","oldestIndexedBlockHeight":"1","latestIndexedBlockHeight":"9007199254740993"}))).await;
    let topic = format!("0x{}", "12".repeat(32));
    let report = command(
        &url,
        &[
            "events",
            "list",
            &address(),
            "--topic1",
            &topic,
            "--cursor",
            "0x0102",
        ],
        true,
    );
    assert_eq!(report["data"]["schemaVersion"], 2);
    assert_eq!(report["data"]["events"], json!([]));
    assert_eq!(report["data"]["cursor"], "0xaabb");
    {
        let recorded = calls.lock().unwrap();
        assert_eq!(
            recorded.len(),
            2,
            "one-page command must not follow cursors automatically"
        );
        assert_eq!(
            recorded[1].1,
            json!({"filters":[{"addresses":[address()],"topics":{"topic0":[],"topic1":[topic],"topic2":[]}}],"cursor":"0x0102","limit":50})
        );
    }
    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_cursor_is_reported_without_retry_or_restart() {
    let error = json!({"kind":"invalidCursor"});
    for (method, command_name) in [
        ("getTransactionsByRootAddress", "tx"),
        ("getEvents", "events"),
    ] {
        let (url, handle, calls) = server(
            method,
            Err(ErrorObjectOwned::owned(
                -32602,
                "Invalid cursor",
                Some(error.clone()),
            )),
        )
        .await;
        let report = command(
            &url,
            &[command_name, "list", &address(), "--cursor", "0x0102"],
            false,
        );
        assert_eq!(
            report["error"]["details"]["rpcError"]["code"], -32602,
            "{report}"
        );
        assert_eq!(report["error"]["details"]["rpcError"]["data"], error);
        assert_eq!(calls.lock().unwrap().len(), 2);
        handle.stop().unwrap();
        handle.stopped().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_native_root_page_fails_instead_of_truncating() {
    let (url, handle, calls) = server(
        "getTransactionsByRootAddress",
        Ok(json!({"transactions":[
            {"transactionHash":hash(),"blockHeight":"5"},
            {"transactionHash":hash(),"blockHeight":"4"}
        ],"nextCursor":null,"oldestIndexedBlockHeight":"1","latestIndexedBlockHeight":"5"})),
    )
    .await;
    let report = command(&url, &["tx", "list", &address(), "--limit", "1"], false);
    assert_eq!(report["error"]["details"]["requestedLimit"], 1);
    assert_eq!(calls.lock().unwrap().len(), 2);
    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_cursor_is_rejected_locally_without_starting_a_new_walk() {
    for (method, command_name, code) in [
        (
            "getTransactionsByRootAddress",
            "tx",
            "invalid_transaction_cursor",
        ),
        ("getEvents", "events", "invalid_event_cursor"),
    ] {
        let (url, handle, calls) = server(method, Ok(Value::Null)).await;
        let report = command(
            &url,
            &[command_name, "list", &address(), "--cursor", "0x"],
            false,
        );
        assert_eq!(report["error"]["code"], code);
        assert_eq!(report["error"]["details"]["input"], "0x");
        assert!(
            calls.lock().unwrap().is_empty(),
            "empty cursor must fail before discovery or history RPC"
        );
        handle.stop().unwrap();
        handle.stopped().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_event_page_sends_a_start_and_preserves_event_identity() {
    let topic = format!("0x{}", "34".repeat(32));
    let (url, handle, calls) = server("getEvents", Ok(json!({
        "events":[{"transactionHash":hash(),"blockHeight":"9007199254740993","sequenceNumber":3,
            "event":{"address":address(),"topics":[topic],"data":"0x1234"}}],
        "nextCursor":null,"oldestIndexedBlockHeight":"1","latestIndexedBlockHeight":"9007199254740993"
    }))).await;
    let report = command(
        &url,
        &[
            "events",
            "list",
            &address(),
            "--topic0",
            &topic,
            "--limit",
            "1",
        ],
        true,
    );
    assert_eq!(report["data"]["cursor"], "0x");
    assert_eq!(report["data"]["events"][0]["txHash"], hash());
    assert_eq!(report["data"]["events"][0]["sequenceNumber"], 3);
    assert_eq!(report["data"]["events"][0]["contractAddress"], address());
    assert_eq!(report["data"]["events"][0]["data"], "0x1234");
    assert_eq!(
        calls.lock().unwrap()[1].1,
        json!({
            "filters":[{"addresses":[address()],"topics":{"topic0":[topic],"topic1":[],"topic2":[]}}],
            "start":{"order":"descending","anchor":{"kind":"atEnd"}},"limit":1
        })
    );
    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_native_event_page_retains_validation_evidence() {
    let event = json!({"transactionHash":hash(),"blockHeight":"5","sequenceNumber":0,
        "event":{"address":address(),"topics":[],"data":"0x"}});
    let (url, handle, calls) = server(
        "getEvents",
        Ok(json!({"events":[event,event],"nextCursor":null,
        "oldestIndexedBlockHeight":"1","latestIndexedBlockHeight":"5"})),
    )
    .await;
    let report = command(&url, &["events", "list", &address(), "--limit", "1"], false);
    assert_eq!(report["error"]["code"], "native_rpc_failure");
    assert_eq!(report["error"]["details"]["method"], "getEvents");
    assert_eq!(
        report["error"]["details"]["cause"]["code"],
        "rpc_method_failed"
    );
    assert!(
        report["error"]["details"]["cause"]["message"]
            .as_str()
            .unwrap()
            .contains("returned 2 events for requested limit 1")
    );
    assert_eq!(calls.lock().unwrap().len(), 2);
    handle.stop().unwrap();
    handle.stopped().await;
}
