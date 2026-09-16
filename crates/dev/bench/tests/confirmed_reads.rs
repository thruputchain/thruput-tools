use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NETWORK: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct State {
    active: AtomicUsize,
    peak: AtomicUsize,
    statuses: AtomicUsize,
    receipts: AtomicUsize,
    invalid: bool,
    delay: Duration,
}

async fn serve(mut socket: TcpStream, state: Arc<State>) {
    let mut reader = BufReader::new(&mut socket);
    let mut line = String::new();
    let mut length = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap() == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse::<usize>().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    let request: Value = serde_json::from_slice(&body).unwrap();
    let method = request["method"].as_str().unwrap();
    let result = match method {
        "groundhog_version" => {
            json!({"wireFormat":"3.0.0","implementation":"fixture","commitHash":"target-commit"})
        }
        "groundhog_networkInfo" => json!({"networkHash":NETWORK}),
        "groundhog_chainTip" => json!({"height":7}),
        "groundhog_getTransactionStatus" => {
            assert_eq!(request["params"], json!([HASH]));
            let count = state.statuses.fetch_add(1, Ordering::SeqCst);
            if count > 0 && state.invalid {
                json!({"status":"pending"})
            } else {
                json!({"status":"committed","height":7})
            }
        }
        "groundhog_getTransactionReceipt" => {
            assert_eq!(request["params"], json!([HASH]));
            let count = state.receipts.fetch_add(1, Ordering::SeqCst);
            json!({"txHash":if count > 0 && state.invalid {NETWORK} else {HASH},"height":7,"status":{"result":"deterministicError","kind":"fault"},"events":[]})
        }
        _ => panic!("unexpected method {method}"),
    };
    let is_read =
        method == "groundhog_getTransactionStatus" || method == "groundhog_getTransactionReceipt";
    if is_read {
        let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
        state.peak.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(state.delay).await;
    }
    let body =
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":request["id"],"result":result})).unwrap();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(header.as_bytes()).await;
    let _ = socket.write_all(&body).await;
    if is_read {
        state.active.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn benchmark(
    method: &str,
    invalid: bool,
    delay: Duration,
    extra: &[&str],
) -> (std::process::Output, Arc<State>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(State {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        statuses: AtomicUsize::new(0),
        receipts: AtomicUsize::new(0),
        invalid,
        delay,
    });
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::spawn(serve(socket, server_state.clone()));
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let corpus = dir.path().join("hashes.json");
    std::fs::write(
        &corpus,
        serde_json::to_vec(&json!({"schemaVersion":1,"networkHash":NETWORK,"hashes":[HASH]}))
            .unwrap(),
    )
    .unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_groundhog-bench"));
    command
        .args([
            "--url",
            &format!("http://{address}"),
            "--method",
            method,
            "--hashes-file",
        ])
        .arg(&corpus)
        .args(["--workers", "4", "--duration", "1", "--json"])
        .args(extra)
        .env("TOKIO_WORKER_THREADS", "2");
    if !extra.contains(&"--warmup") {
        command.args(["--warmup", "0"]);
    }
    if !extra.contains(&"--rate") && !extra.contains(&"--unthrottled") {
        command.args(["--outstanding", "3"]);
    }
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    server.abort();
    (output, state)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_outstanding_cap_and_v2_result_contract() {
    let (output, state) = benchmark(
        "getTransactionStatus",
        false,
        Duration::from_millis(40),
        &[],
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schemaVersion"], 2);
    assert_eq!(result["outcome"], "passed");
    assert_eq!(result["kind"], "groundhog-bench-result");
    assert!(result["successfulLatencyUs"]["p50"].as_u64().unwrap() >= 40_000);
    assert_eq!(result["configuration"]["outstanding"], 3);
    assert_eq!(result["configuration"]["effectiveMaxInflight"], 3);
    assert!(result["configuration"]["maxInflightPerWorker"].is_null());
    assert_eq!(result["runtimeWorkerThreads"], 2);
    assert_eq!(
        state.peak.load(Ordering::SeqCst),
        3,
        "cap must be global, not per worker"
    );
    assert_eq!(
        result["target"]["gatewayVersion"]["value"]["commitHash"],
        "target-commit"
    );
    assert!(result["target"]["runnerSupplied"].is_null());
    assert_eq!(result["benchmarkBinarySha256"]["availability"], "observed");
    assert_eq!(result["corpus"]["count"], 1);
    assert!(result["requests"]["successful"].as_u64().unwrap() > 0);
    assert_eq!(result["requests"]["failed"], 0);
    assert_eq!(
        result["successfulLatencyUs"]["samples"],
        result["requests"]["successful"]
    );
    assert_eq!(
        result["rates"]["successfulRequestsPerSecond"]
            .as_f64()
            .unwrap(),
        result["requests"]["successful"].as_f64().unwrap()
    );
    assert_eq!(
        result["measurement"]["inclusion"],
        "started-and-completed-in-window"
    );
    assert_eq!(result["measurement"]["windowMs"], 1000);
    assert!(result["run"]["hashesFile"].as_str().is_some());
    assert!(result["configuration"].get("url").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_failures_are_reported_and_fail_the_command() {
    for method in ["getTransactionStatus", "getTransactionReceipt"] {
        let (output, _) = benchmark(method, true, Duration::from_millis(20), &[]).await;
        assert!(!output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["outcome"], "failed");
        assert_eq!(result["requests"]["successful"], 0);
        assert!(result["requests"]["errors"]["semantic"].as_u64().unwrap() > 0);
        assert!(result["successfulLatencyUs"].is_null());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_completions_do_not_inflate_throughput() {
    let (output, _) = benchmark(
        "getTransactionReceipt",
        false,
        Duration::from_millis(1200),
        &["--setup-timeout-secs", "10"],
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["requests"]["successful"], 0);
    assert_eq!(result["rates"]["successfulRequestsPerSecond"], 0.0);
    assert_eq!(result["drained"]["successful"], 3);
    assert!(result["measurement"]["drainMs"].as_f64().unwrap() >= 150.0);
    assert!(result["successfulLatencyUs"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corpus_setup_obeys_one_total_deadline() {
    let (output, _) = benchmark(
        "getTransactionStatus",
        false,
        Duration::from_millis(700),
        &["--setup-timeout-secs", "1"],
    )
    .await;
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "setup failure must not emit a measured result"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("deadline exceeded"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saturated_open_loop_fails_instead_of_silently_dropping_the_rate() {
    let (output, _) = benchmark(
        "getTransactionStatus",
        false,
        Duration::from_millis(1200),
        &[
            "--rate",
            "100",
            "--connections-per-worker",
            "1",
            "--max-skew",
            "0.1",
        ],
    )
    .await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot sustain the requested rate"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warmup_saturation_cannot_pass_an_empty_measurement() {
    let (output, _) = benchmark(
        "getTransactionStatus",
        false,
        Duration::from_millis(2200),
        &["--warmup", "1"],
    )
    .await;
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["outcome"], "empty");
    assert_eq!(result["requests"]["total"], 0);
    assert_eq!(result["drained"]["total"], 0);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no requests in the measured window"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn existing_load_modes_keep_their_per_worker_cap() {
    for extra in [
        vec!["--rate", "8", "--connections-per-worker", "1"],
        vec!["--unthrottled", "--connections-per-worker", "1"],
    ] {
        let (output, state) = benchmark(
            "getTransactionStatus",
            false,
            Duration::from_millis(20),
            &extra,
        )
        .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["outcome"], "passed");
        assert!(result["configuration"]["outstanding"].is_null());
        assert_eq!(result["configuration"]["maxInflightPerWorker"], 2);
        assert_eq!(result["configuration"]["effectiveMaxInflight"], 8);
        assert!(state.peak.load(Ordering::SeqCst) <= 8);
        if extra.contains(&"--rate") {
            assert_eq!(result["configuration"]["mode"], "rate_limited");
            assert_eq!(
                result["requests"]["total"].as_u64().unwrap()
                    + result["drained"]["total"].as_u64().unwrap(),
                8
            );
        } else {
            assert_eq!(result["configuration"]["mode"], "unthrottled");
            assert_eq!(state.peak.load(Ordering::SeqCst), 8);
        }
    }
}
