//! Native call preflight and ambiguous-submission evidence through the CLI binary.

#[path = "support/isolation.rs"]
mod isolation;

use std::{
    process::{Command, Output},
    sync::{Arc, Mutex},
};

use jsonrpsee::{
    RpcModule,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use thruput_xdr::transaction::SignedTransaction;

const NETWORK: &str = "US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx";
const METHODS: [&str; 7] = [
    "getVersionInfo",
    "getNetworkInfo",
    "getChainTip",
    "getBalance",
    "sendTransaction",
    "getTransactionStatus",
    "getTransactionReceipt",
];
type Calls = Arc<Mutex<Vec<(&'static str, Value)>>>;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(rpc: &str) -> Self {
        let fixture = Self {
            dir: TempDir::new().unwrap(),
        };
        std::fs::write(
            fixture.dir.path().join("thruput.toml"),
            format!("default_profile = 'default'\n[profile.default]\ndefault_key = 'owner'\nrpc_url = '{rpc}'\nnetwork_hash = '{NETWORK}'\n"),
        )
        .unwrap();
        for args in [
            vec!["keys", "generate", "owner"],
            vec!["wallet", "create", "--force"],
        ] {
            let output = fixture.command().args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
        command
            .args(["--json", "--non-interactive"])
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("THRUPUT_PROFILE")
            .env_remove("THRUPUT_RUNTIME_MANIFEST");
        isolation::isolate_from_developer_env(&mut command);
        command
    }

    fn call_command(&self) -> Command {
        let mut command = self.command();
        command.args([
            "call",
            &thruput_encoding::encode(&[0x42; 32]),
            "0x0102030405060708",
            "0xabcd",
            "--force",
        ]);
        command
    }

    fn call(&self) -> Output {
        self.call_command().output().unwrap()
    }
}

fn transaction_hash(request: &Value) -> String {
    let encoded = request["signedTransaction"].as_str().unwrap();
    let bytes = hex::decode(encoded.strip_prefix("0x").unwrap()).unwrap();
    let signed: SignedTransaction = xdrs::txs::decode(&bytes).unwrap();
    thruput_encoding::encode(
        thruput_signing::transaction_hash(&signed.tx)
            .unwrap()
            .as_ref(),
    )
}

fn submission_error(hash: &str) -> Value {
    json!({
        "code":-32050,
        "message":"response lost after submission",
        "data":{
            "kind":"backendUnavailable",
            "admission":"unknown",
            "transactionHash":hash,
            "futureDetail":{"retain":true},
        },
    })
}

async fn server(network_matches: bool, balance: i128) -> (String, ServerHandle, Calls) {
    server_with_stalled_send(network_matches, balance, None).await
}

async fn server_with_stalled_send(
    network_matches: bool,
    balance: i128,
    release_send: Option<Arc<tokio::sync::Notify>>,
) -> (String, ServerHandle, Calls) {
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    for method in METHODS {
        let release_send = release_send.clone();
        module
            .register_async_method(method, move |params, calls, _| {
                let release_send = release_send.clone();
                async move {
                    let request: Value = params.parse()?;
                    calls.lock().unwrap().push((method, request.clone()));
                    match method {
                        "getVersionInfo" => Ok(json!({
                            "wireFormat":"14.0.0",
                            "implementation":"test",
                            "supportedMethods":METHODS,
                        })),
                        "getNetworkInfo" => {
                            let network = if network_matches {
                                NETWORK.to_owned()
                            } else {
                                thruput_encoding::encode(&[0xff; 32])
                            };
                            Ok(json!({"networkHash":network}))
                        }
                        "getChainTip" => Ok(json!({
                            "blockHeight":"42",
                            "blockHash":thruput_encoding::encode(&[1;32]),
                        })),
                        "getBalance" => Ok(json!({
                            "address":request["address"],
                            "stateBlockHeight":"42",
                            "rawBalance":balance.to_string(),
                        })),
                        "sendTransaction" => {
                            if let Some(release) = release_send {
                                release.notified().await;
                            }
                            let error = submission_error(&transaction_hash(&request));
                            Err(ErrorObjectOwned::owned(
                                -32050,
                                "response lost after submission",
                                Some(error["data"].clone()),
                            ))
                        }
                        _ => Err(ErrorObjectOwned::owned(
                            -32050,
                            "unexpected receipt polling after failed submission",
                            None::<()>,
                        )),
                    }
                }
            })
            .unwrap();
    }
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    (url, server.start(module), calls)
}

fn preflight_report(output: &Output) -> Value {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let report: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(report["error"]["code"], "native_rpc_failure", "{report}");
    report
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn underfunded_call_reports_decimal_capacity_without_submitting() {
    let (url, handle, calls) = server(true, 1).await;
    let fixture = Fixture::new(&url);
    let report = preflight_report(&fixture.call());
    assert_eq!(
        report["error"]["details"]["preflightReason"],
        "insufficientCapacity"
    );
    assert_eq!(report["error"]["details"]["availableRaw"], "1");
    assert_eq!(report["error"]["details"]["requiredRaw"], "25000000");
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.0)
            .collect::<Vec<_>>(),
        [
            "getVersionInfo",
            "getNetworkInfo",
            "getChainTip",
            "getBalance"
        ]
    );
    handle.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_mismatch_stops_before_balance_or_submission() {
    let (url, handle, calls) = server(false, 100_000_000).await;
    let fixture = Fixture::new(&url);
    let report = preflight_report(&fixture.call());
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("network identity mismatch")
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_error_retains_canonical_hash_and_original_error_without_repeating_write() {
    let (url, handle, calls) = server(true, 100_000_000).await;
    let fixture = Fixture::new(&url);
    let output = fixture.call();
    assert!(!output.status.success());
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["data"]["schemaVersion"], 4);
    assert_eq!(report["data"]["submission"], "uncertain");
    assert_eq!(report["error"]["code"], "native_rpc_failure");
    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded.iter().map(|call| call.0).collect::<Vec<_>>(),
        [
            "getVersionInfo",
            "getNetworkInfo",
            "getChainTip",
            "getBalance",
            "getVersionInfo",
            "getNetworkInfo",
            "sendTransaction"
        ]
    );
    let hash = transaction_hash(&recorded.last().unwrap().1);
    assert_eq!(report["data"]["transactionHash"], hash);
    assert_eq!(report["data"]["evidence"]["transactionHash"], hash);
    let expected = submission_error(&hash);
    assert_eq!(report["data"]["evidence"]["cause"]["rpcError"], expected);
    assert_eq!(report["error"]["details"]["cause"]["rpcError"], expected);
    handle.stop().unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_during_submission_preserves_uncertainty_and_hash_without_resubmission() {
    use std::{process::Stdio, time::Duration};

    let release_send = Arc::new(tokio::sync::Notify::new());
    let (url, handle, calls) =
        server_with_stalled_send(true, 100_000_000, Some(release_send.clone())).await;
    let fixture = Fixture::new(&url);
    let child = tokio::process::Command::from(fixture.call_command())
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    // The observed request proves the write began; the server withholds its reply.
    // Every failure path drops and kills the owned child before releasing the handler.
    let result: Result<Output, String> = async {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|call| call.0 == "sendTransaction")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "CLI did not reach sendTransaction".to_owned())?;
        let signal = Command::new("/bin/kill")
            .args(["-INT", &pid.to_string()])
            .output()
            .map_err(|error| error.to_string())?;
        if !signal.status.success() {
            return Err(format!(
                "failed to signal child {pid}: {}",
                String::from_utf8_lossy(&signal.stderr)
            ));
        }
        tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .map_err(|_| "CLI did not handle SIGINT during submission".to_owned())?
            .map_err(|error| error.to_string())
    }
    .await;
    release_send.notify_one();
    handle.stop().unwrap();
    let output = result.unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["data"]["schemaVersion"], 4);
    assert_eq!(report["data"]["submission"], "uncertain");
    let recorded = calls.lock().unwrap();
    let writes: Vec<_> = recorded
        .iter()
        .filter(|call| call.0 == "sendTransaction")
        .collect();
    assert_eq!(writes.len(), 1);
    let hash = transaction_hash(&writes[0].1);
    assert_eq!(report["data"]["transactionHash"], hash);
    assert!(
        !recorded
            .iter()
            .any(|call| matches!(call.0, "getTransactionStatus" | "getTransactionReceipt"))
    );
}
