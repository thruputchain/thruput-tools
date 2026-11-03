//! Executable one-page history commands against a native endpoint.

#[path = "support/isolation.rs"]
mod isolation;
#[path = "support/rpc_recorder.rs"]
mod rpc_recorder;

use jsonrpsee::types::ErrorObjectOwned;
use rpc_recorder::RecordingServer;
use serde_json::{Value, json};
use std::process::Command;
use tempfile::TempDir;

fn address() -> String {
    thruput_encoding::encode(&[0x42; 32])
}
fn hash() -> String {
    thruput_encoding::encode(&[0x43; 32])
}

fn server(method: &'static str, result: Result<Value, ErrorObjectOwned>) -> RecordingServer {
    RecordingServer::builder()
        .method("getVersionInfo", move |_| {
            Ok(json!({"wireFormat":"15.0.0", "implementation":"test", "supportedMethods":[method]}))
        })
        .method(method, move |_| result.clone())
        .start()
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
    json!({"transactions":[{"transactionHash":hash(),"blockHeight":"9007199254740993","isRootTarget":true,"wasInvoked":true}],"nextCursor":cursor,"coverage":{"fromBlockHeight":"1","throughBlockHeight":"9007199254740993"}})
}

#[test]
fn root_history_uses_native_named_params_and_preserves_cli_report() {
    let endpoint = server("getTransactionsByAddress", Ok(root_page(&json!("0x0102"))));
    let report = command(
        endpoint.url(),
        &["tx", "list", &address(), "--limit", "1"],
        true,
    );
    assert_eq!(report["data"]["schemaVersion"], 2);
    assert_eq!(report["data"]["refs"][0]["txHash"], hash());
    assert_eq!(report["data"]["refs"][0]["height"], "9007199254740993");
    assert_eq!(report["data"]["cursor"], "0x0102");
    {
        let recorded = endpoint.stop();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[1].0, "getTransactionsByAddress");
        assert_eq!(
            recorded[1].1,
            json!({"address":address(),"limit":1,"start":{"order":"descending","anchor":{"kind":"atEnd"}}})
        );
    }
}

#[test]
fn continuation_forwards_cursor_without_start_and_maps_terminal_marker() {
    let endpoint = server("getTransactionsByAddress", Ok(root_page(&Value::Null)));
    let report = command(
        endpoint.url(),
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
        endpoint.stop()[1].1,
        json!({"address":address(),"limit":3,"cursor":"0x0102"})
    );
}

#[test]
fn empty_event_page_keeps_continuation_and_topic_filters() {
    let endpoint = server(
        "getEvents",
        Ok(
            json!({"events":[],"nextCursor":"0xaabb","oldestIndexedBlockHeight":"1","latestIndexedBlockHeight":"9007199254740993"}),
        ),
    );
    let topic = format!("0x{}", "12".repeat(32));
    let report = command(
        endpoint.url(),
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
        let recorded = endpoint.stop();
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
}

#[test]
fn invalid_cursor_is_reported_without_retry_or_restart() {
    let error = json!({"kind":"invalidCursor"});
    for (method, command_name) in [("getTransactionsByAddress", "tx"), ("getEvents", "events")] {
        let endpoint = server(
            method,
            Err(ErrorObjectOwned::owned(
                -32602,
                "Invalid cursor",
                Some(error.clone()),
            )),
        );
        let report = command(
            endpoint.url(),
            &[command_name, "list", &address(), "--cursor", "0x0102"],
            false,
        );
        assert_eq!(
            report["error"]["details"]["rpcError"]["code"], -32602,
            "{report}"
        );
        assert_eq!(report["error"]["details"]["rpcError"]["data"], error);
        assert_eq!(endpoint.stop().len(), 2);
    }
}

#[test]
fn oversized_native_root_page_fails_instead_of_truncating() {
    let endpoint = server(
        "getTransactionsByAddress",
        Ok(json!({"transactions":[
            {"transactionHash":hash(),"blockHeight":"5","isRootTarget":true,"wasInvoked":true},
            {"transactionHash":hash(),"blockHeight":"4","isRootTarget":true,"wasInvoked":true}
        ],"nextCursor":null,"coverage":{"fromBlockHeight":"1","throughBlockHeight":"5"}})),
    );
    let report = command(
        endpoint.url(),
        &["tx", "list", &address(), "--limit", "1"],
        false,
    );
    assert_eq!(report["error"]["details"]["requestedLimit"], 1);
    assert_eq!(endpoint.stop().len(), 2);
}

#[test]
fn empty_cursor_is_rejected_locally_without_starting_a_new_walk() {
    for (method, command_name, code) in [
        (
            "getTransactionsByAddress",
            "tx",
            "invalid_transaction_cursor",
        ),
        ("getEvents", "events", "invalid_event_cursor"),
    ] {
        let endpoint = server(method, Ok(Value::Null));
        let report = command(
            endpoint.url(),
            &[command_name, "list", &address(), "--cursor", "0x"],
            false,
        );
        assert_eq!(report["error"]["code"], code);
        assert_eq!(report["error"]["details"]["input"], "0x");
        assert!(
            endpoint.stop().is_empty(),
            "empty cursor must fail before discovery or history RPC"
        );
    }
}

#[test]
fn initial_event_page_sends_a_start_and_preserves_event_identity() {
    let topic = format!("0x{}", "34".repeat(32));
    let endpoint = server(
        "getEvents",
        Ok(json!({
            "events":[{"transactionHash":hash(),"blockHeight":"9007199254740993","sequenceNumber":3,
                "event":{"address":address(),"topics":[topic],"data":"0x1234"}}],
            "nextCursor":null,"oldestIndexedBlockHeight":"1","latestIndexedBlockHeight":"9007199254740993"
        })),
    );
    let report = command(
        endpoint.url(),
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
        endpoint.stop()[1].1,
        json!({
            "filters":[{"addresses":[address()],"topics":{"topic0":[topic],"topic1":[],"topic2":[]}}],
            "start":{"order":"descending","anchor":{"kind":"atEnd"}},"limit":1
        })
    );
}

#[test]
fn oversized_native_event_page_retains_validation_evidence() {
    let event = json!({"transactionHash":hash(),"blockHeight":"5","sequenceNumber":0,
        "event":{"address":address(),"topics":[],"data":"0x"}});
    let endpoint = server(
        "getEvents",
        Ok(json!({"events":[event,event],"nextCursor":null,
        "oldestIndexedBlockHeight":"1","latestIndexedBlockHeight":"5"})),
    );
    let report = command(
        endpoint.url(),
        &["events", "list", &address(), "--limit", "1"],
        false,
    );
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
    assert_eq!(endpoint.stop().len(), 2);
}
