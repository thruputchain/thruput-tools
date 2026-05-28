use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const LOCAL_FAST_CATALOG: &[CatalogScenario] = &[
    CatalogScenario {
        file: "local-chain-tip-smoke.json",
        expect_passed: true,
        expect_receipt_metrics: false,
        expect_restart: false,
        expect_restart_loss: false,
        expect_contract_view: false,
        expect_contract_storage: false,
        allow_restart_failure: false,
    },
    CatalogScenario {
        file: "local-wallet-submit-smoke.json",
        expect_passed: true,
        expect_receipt_metrics: false,
        expect_restart: false,
        expect_restart_loss: false,
        expect_contract_view: false,
        expect_contract_storage: false,
        allow_restart_failure: false,
    },
    CatalogScenario {
        file: "local-receipt-poll-smoke.json",
        expect_passed: true,
        expect_receipt_metrics: true,
        expect_restart: false,
        expect_restart_loss: false,
        expect_contract_view: false,
        expect_contract_storage: false,
        allow_restart_failure: false,
    },
    CatalogScenario {
        file: "local-mixed-wallet-receipt-smoke.json",
        expect_passed: true,
        expect_receipt_metrics: true,
        expect_restart: false,
        expect_restart_loss: false,
        expect_contract_view: false,
        expect_contract_storage: false,
        allow_restart_failure: false,
    },
    CatalogScenario {
        file: "local-restart-chain-tip-smoke.json",
        expect_passed: true,
        expect_receipt_metrics: false,
        expect_restart: true,
        expect_restart_loss: false,
        expect_contract_view: false,
        expect_contract_storage: false,
        allow_restart_failure: false,
    },
    CatalogScenario {
        file: "local-restart-mixed-artifact.json",
        expect_passed: false,
        expect_receipt_metrics: false,
        expect_restart: true,
        expect_restart_loss: true,
        expect_contract_view: false,
        expect_contract_storage: false,
        allow_restart_failure: true,
    },
];

const LOCAL_CONTRACT_CATALOG: &[CatalogScenario] = &[
    CatalogScenario {
        file: "local-fibonacci-view-smoke.json",
        expect_passed: true,
        expect_receipt_metrics: false,
        expect_restart: false,
        expect_restart_loss: false,
        expect_contract_view: true,
        expect_contract_storage: false,
        allow_restart_failure: false,
    },
    CatalogScenario {
        file: "local-fibonacci-storage-read-smoke.json",
        expect_passed: true,
        expect_receipt_metrics: false,
        expect_restart: false,
        expect_restart_loss: false,
        expect_contract_view: false,
        expect_contract_storage: true,
        allow_restart_failure: false,
    },
];

const LOCAL_SOAK_CATALOG: &[CatalogScenario] = &[CatalogScenario {
    file: "local-mixed-wallet-receipt-soak.json",
    expect_passed: true,
    expect_receipt_metrics: true,
    expect_restart: false,
    expect_restart_loss: false,
    expect_contract_view: false,
    expect_contract_storage: false,
    allow_restart_failure: false,
}];

#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
struct CatalogScenario {
    file: &'static str,
    expect_passed: bool,
    expect_receipt_metrics: bool,
    expect_restart: bool,
    expect_restart_loss: bool,
    expect_contract_view: bool,
    expect_contract_storage: bool,
    allow_restart_failure: bool,
}

#[test]
#[ignore = "requires GROUNDHOG_SIM_HOGD_DEV_BINARY and a groundhog binary with dev-node support"]
fn local_target_chain_tip_smoke() {
    let hogd_dev_binary = std::env::var("GROUNDHOG_SIM_HOGD_DEV_BINARY")
        .expect("set GROUNDHOG_SIM_HOGD_DEV_BINARY to a hogd-dev binary");
    let groundhog_binary =
        std::env::var("GROUNDHOG_SIM_GROUNDHOG_BINARY").unwrap_or_else(|_| "groundhog".into());
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("sim crate should be two levels below repo root")
        .to_path_buf();
    let scenario = repo_root.join("examples/sim/local-chain-tip-smoke.json");
    let temp = tempfile::tempdir().expect("temp dir should be created");

    let mut command = Command::new(env!("CARGO_BIN_EXE_hoglab"));
    command
        .arg("run")
        .arg(&scenario)
        .arg("--groundhog-binary")
        .arg(groundhog_binary)
        .arg("--hogd-dev-binary")
        .arg(hogd_dev_binary);
    add_hogd_protocol_arg(&mut command);
    let status = command
        .arg("--port")
        .arg("0")
        .arg("--out-dir")
        .arg(temp.path())
        .status()
        .expect("hoglab should launch");

    assert!(status.success(), "hoglab run failed with status {status}");
    assert!(temp.path().join("result.json").exists());
    assert!(temp.path().join("result.jsonl").exists());
    assert!(temp.path().join("target-ownership.json").exists());
    assert!(temp.path().join("logs/local-node-0.stdout.log").exists());
    assert!(temp.path().join("logs/local-node-0.stderr.log").exists());

    let simulation = std::fs::read_to_string(temp.path().join("simulation.json"))
        .expect("simulation artifact should read");
    let rpc_url = json_string(&simulation, &["resolved", "rpcUrl"])
        .expect("simulation artifact should record resolved rpc url");
    assert_rpc_port_closed(&rpc_url);
}

#[test]
#[ignore = "requires GROUNDHOG_SIM_HOGD_DEV_BINARY and a groundhog binary with dev-node support"]
fn local_scenario_catalog_smoke() {
    run_catalog(LOCAL_FAST_CATALOG);
    if std::env::var("GROUNDHOG_SIM_CONTRACT_ARTIFACT_ROOT").is_ok() {
        run_catalog(LOCAL_CONTRACT_CATALOG);
    } else {
        eprintln!("skipping contract catalog; set GROUNDHOG_SIM_CONTRACT_ARTIFACT_ROOT to run it");
    }
}

#[test]
#[ignore = "requires GROUNDHOG_SIM_HOGD_DEV_BINARY, GROUNDHOG_SIM_RUN_SOAKS=1, and a groundhog binary with dev-node support"]
fn local_scenario_catalog_soak() {
    if std::env::var("GROUNDHOG_SIM_RUN_SOAKS").as_deref() != Ok("1") {
        eprintln!("skipping soak catalog; set GROUNDHOG_SIM_RUN_SOAKS=1 to run it");
        return;
    }
    run_catalog(LOCAL_SOAK_CATALOG);
}

#[test]
#[ignore = "requires GROUNDHOG_SIM_HOGD_DEV_BINARY and a groundhog binary with dev-node support"]
fn local_restart_target_chain_tip_smoke() {
    let hogd_dev_binary = std::env::var("GROUNDHOG_SIM_HOGD_DEV_BINARY")
        .expect("set GROUNDHOG_SIM_HOGD_DEV_BINARY to a hogd-dev binary");
    let groundhog_binary =
        std::env::var("GROUNDHOG_SIM_GROUNDHOG_BINARY").unwrap_or_else(|_| "groundhog".into());
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("sim crate should be two levels below repo root")
        .to_path_buf();
    let scenario = repo_root.join("examples/sim/local-restart-chain-tip-smoke.json");
    let temp = tempfile::tempdir().expect("temp dir should be created");

    let mut command = Command::new(env!("CARGO_BIN_EXE_hoglab"));
    command
        .arg("run")
        .arg(&scenario)
        .arg("--groundhog-binary")
        .arg(groundhog_binary)
        .arg("--hogd-dev-binary")
        .arg(hogd_dev_binary);
    add_hogd_protocol_arg(&mut command);
    let status = command
        .arg("--port")
        .arg("0")
        .arg("--out-dir")
        .arg(temp.path())
        .status()
        .expect("hoglab should launch");

    assert!(status.success(), "hoglab run failed with status {status}");
    assert!(temp.path().join("logs/local-node-0.stdout.log").exists());
    assert!(temp.path().join("logs/local-node-0.stderr.log").exists());
    assert!(
        temp.path()
            .join("logs/local-node-1.attempt-0.stdout.log")
            .exists()
    );
    assert!(
        temp.path()
            .join("logs/local-node-1.attempt-0.stderr.log")
            .exists()
    );

    let result: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(temp.path().join("result.json"))
            .expect("result artifact should read"),
    )
    .expect("result artifact should read");
    assert_eq!(result["outcome"], "passed");
    let chaos_events = result["chaosEvents"]
        .as_array()
        .expect("result should contain chaosEvents array");
    let result_states: Vec<_> = chaos_events
        .iter()
        .map(|event| event["state"].as_str().expect("state should be a string"))
        .collect();
    assert_eq!(
        result_states,
        vec!["scheduled", "started", "completed", "recovered"]
    );
    assert!(
        chaos_events
            .iter()
            .all(|event| event["kind"] == "restart-target")
    );
    let recovered = chaos_events
        .iter()
        .find(|event| event["state"] == "recovered")
        .expect("recovered chaos event should exist");
    assert_distinct_restart_pids(
        recovered["detail"]
            .as_str()
            .expect("recovered event should carry restart detail"),
    );

    let stream = std::fs::read_to_string(temp.path().join("result.jsonl"))
        .expect("result stream should read");
    let stream_events = jsonl_values(&stream);
    let chaos_transitions: Vec<_> = stream_events
        .iter()
        .filter(|event| event["kind"] == "chaos-transition")
        .collect();
    let stream_states: Vec<_> = chaos_transitions
        .iter()
        .map(|event| event["state"].as_str().expect("state should be a string"))
        .collect();
    assert_eq!(
        stream_states,
        vec!["scheduled", "started", "completed", "recovered"]
    );
    assert!(
        chaos_transitions
            .iter()
            .all(|event| event["chaosKind"] == "restart-target")
    );

    let simulation = std::fs::read_to_string(temp.path().join("simulation.json"))
        .expect("simulation artifact should read");
    let rpc_url = json_string(&simulation, &["resolved", "rpcUrl"])
        .expect("simulation artifact should record resolved rpc url");
    assert_rpc_port_closed(&rpc_url);
}

#[test]
#[ignore = "requires GROUNDHOG_SIM_HOGD_DEV_BINARY and a groundhog binary with dev-node support"]
fn local_target_sweep_orphan_smoke() {
    let hogd_dev_binary = std::env::var("GROUNDHOG_SIM_HOGD_DEV_BINARY")
        .expect("set GROUNDHOG_SIM_HOGD_DEV_BINARY to a hogd-dev binary");
    let groundhog_binary =
        std::env::var("GROUNDHOG_SIM_GROUNDHOG_BINARY").unwrap_or_else(|_| "groundhog".into());
    let root = tempfile::tempdir().expect("temp dir should be created");
    let run_dir = root.path().join("orphan-run");
    let scenario = root.path().join("sweep-orphan.json");
    std::fs::write(
        &scenario,
        r#"{
  "schemaVersion": 1,
  "kind": "groundhog-simulation",
  "name": "local-sweep-orphan-smoke",
  "seed": "local-sweep-orphan-smoke",
  "target": {"provider": "local"},
  "loadShape": [{"duration": "30s", "users": 1, "ratePerUserHz": 1.0}],
  "workloads": [{"name": "chain-tip", "weight": 1}],
  "assertions": {"maxErrorRate": 0.001}
}"#,
    )
    .expect("scenario should write");

    let mut command = Command::new(env!("CARGO_BIN_EXE_hoglab"));
    command
        .arg("run")
        .arg(&scenario)
        .arg("--groundhog-binary")
        .arg(groundhog_binary)
        .arg("--hogd-dev-binary")
        .arg(hogd_dev_binary);
    add_hogd_protocol_arg(&mut command);
    let mut child = command
        .arg("--port")
        .arg("0")
        .arg("--out-dir")
        .arg(&run_dir)
        .spawn()
        .expect("hoglab should launch");

    let ownership_path = run_dir.join("target-ownership.json");
    let ownership = wait_for_ownership_process(&ownership_path);
    child.kill().expect("hoglab parent should be killable");
    child.wait().expect("hoglab parent should reap");

    let output = Command::new(env!("CARGO_BIN_EXE_hoglab"))
        .arg("sweep")
        .arg(root.path())
        .arg("--json")
        .output()
        .expect("hoglab sweep should launch");
    assert!(
        output.status.success(),
        "hoglab sweep failed: status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("sweep report should parse");
    assert_eq!(report["schemaVersion"], 1);
    let entries = report["entries"]
        .as_array()
        .expect("sweep report should contain entries");
    let reclaimed = entries
        .iter()
        .find(|entry| entry["state"] == "reclaimed")
        .expect("sweep should reclaim orphaned target");
    assert_eq!(reclaimed["pid"], ownership["process"]["pid"]);
    assert_eq!(reclaimed["port"], ownership["process"]["port"]);

    let port = ownership["process"]["port"]
        .as_u64()
        .expect("ownership should contain port");
    assert_port_closed(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        u16::try_from(port).expect("port should fit u16"),
    )));
}

fn json_string(payload: &str, path: &[&str]) -> Option<String> {
    let mut value: &serde_json::Value = &serde_json::from_str(payload).ok()?;
    for part in path {
        value = value.get(*part)?;
    }
    value.as_str().map(str::to_owned)
}

fn add_hogd_protocol_arg(command: &mut Command) {
    if let Ok(protocol) = std::env::var("GROUNDHOG_SIM_HOGD_PROTOCOL")
        && !protocol.trim().is_empty()
    {
        command.arg("--hogd-protocol").arg(protocol);
    }
}

fn wait_for_ownership_process(path: &std::path::Path) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents)
            && value.get("process").is_some()
        {
            return value;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "target ownership with process metadata was not written at {}",
        path.display()
    );
}

fn jsonl_values(payload: &str) -> Vec<serde_json::Value> {
    payload
        .lines()
        .map(|line| serde_json::from_str(line).expect("jsonl event should parse"))
        .collect()
}

fn assert_distinct_restart_pids(detail: &str) {
    let old_pid = detail
        .split_whitespace()
        .find_map(|part| part.strip_prefix("oldPid="))
        .expect("old pid should be present");
    let new_pid = detail
        .split_whitespace()
        .find_map(|part| part.strip_prefix("newPid="))
        .expect("new pid should be present");

    assert_ne!(old_pid, "unknown");
    assert_ne!(new_pid, "unknown");
    assert_ne!(old_pid, new_pid);
}

fn assert_rpc_port_closed(rpc_url: &str) {
    let address = rpc_url
        .strip_prefix("http://")
        .expect("local smoke rpc url should be http")
        .parse::<std::net::SocketAddr>()
        .expect("local smoke rpc url should contain host:port");
    assert_port_closed(address);
}

fn run_catalog(catalog: &[CatalogScenario]) {
    let hogd_dev_binary = std::env::var("GROUNDHOG_SIM_HOGD_DEV_BINARY")
        .expect("set GROUNDHOG_SIM_HOGD_DEV_BINARY to a hogd-dev binary");
    let groundhog_binary =
        std::env::var("GROUNDHOG_SIM_GROUNDHOG_BINARY").unwrap_or_else(|_| "groundhog".into());
    let repo_root = repo_root();
    let (root, _temp_guard) = catalog_root();
    eprintln!("catalog root {}", root.display());

    for scenario in catalog {
        let scenario_path = repo_root.join("examples/sim").join(scenario.file);
        let run_dir = root.join(scenario.file.trim_end_matches(".json"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_hoglab"));
        command
            .arg("run")
            .arg(&scenario_path)
            .arg("--groundhog-binary")
            .arg(&groundhog_binary)
            .arg("--hogd-dev-binary")
            .arg(&hogd_dev_binary);
        if scenario.expect_contract_view || scenario.expect_contract_storage {
            command.arg("--contract-artifact-root").arg(
                std::env::var("GROUNDHOG_SIM_CONTRACT_ARTIFACT_ROOT").expect(
                    "set GROUNDHOG_SIM_CONTRACT_ARTIFACT_ROOT for contract catalog scenarios",
                ),
            );
        }
        add_hogd_protocol_arg(&mut command);
        let output = command
            .arg("--port")
            .arg("0")
            .arg("--out-dir")
            .arg(&run_dir)
            .output()
            .expect("hoglab should launch");

        if scenario.allow_restart_failure {
            assert!(
                output.status.success() || run_dir.join("result.json").exists(),
                "catalog scenario {} failed without result artifact: status={} stdout={} stderr={}",
                scenario.file,
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        } else {
            assert!(
                output.status.success(),
                "catalog scenario {} failed: status={} stdout={} stderr={}",
                scenario.file,
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        eprintln!(
            "catalog scenario {} run dir {}",
            scenario.file,
            run_dir.display()
        );
        assert_common_local_artifacts(&run_dir);
        let result = read_result(&run_dir);
        if scenario.expect_passed {
            assert_eq!(result["outcome"], "passed", "{}", scenario.file);
        }
        if scenario.expect_receipt_metrics {
            assert_present_receipt_metrics(&result, scenario.file);
        }
        if scenario.expect_restart {
            assert_restart_artifacts(&result, &run_dir, scenario.file);
        }
        if scenario.expect_restart_loss {
            assert_restart_related_failure(&result, scenario.file);
        }
        if scenario.expect_contract_view {
            assert_contract_view_artifacts(&result, &run_dir, scenario.file);
        }
        if scenario.expect_contract_storage {
            assert_contract_storage_artifacts(&result, &run_dir, scenario.file);
        }

        let simulation = std::fs::read_to_string(run_dir.join("simulation.json"))
            .expect("simulation artifact should read");
        let rpc_url = json_string(&simulation, &["resolved", "rpcUrl"])
            .expect("simulation artifact should record resolved rpc url");
        assert_rpc_port_closed(&rpc_url);
    }
}

fn catalog_root() -> (PathBuf, Option<tempfile::TempDir>) {
    if let Ok(root) = std::env::var("GROUNDHOG_SIM_CATALOG_ROOT") {
        let root = PathBuf::from(root);
        std::fs::create_dir_all(&root).expect("catalog root should be created");
        return (root, None);
    }

    if std::env::var("KEEP_WORKDIR").as_deref() == Ok("1") {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_millis();
        let root = std::env::temp_dir().join(format!(
            "groundhog-sim-catalog-{}-{now}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("catalog root should be created");
        return (root, None);
    }

    let temp = tempfile::tempdir().expect("temp dir should be created");
    (temp.path().to_path_buf(), Some(temp))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("sim crate should be two levels below repo root")
        .to_path_buf()
}

fn assert_common_local_artifacts(run_dir: &std::path::Path) {
    assert!(run_dir.join("result.json").exists());
    assert!(run_dir.join("result.jsonl").exists());
    assert!(run_dir.join("simulation.json").exists());
    assert!(run_dir.join("runtime-manifest.json").exists());
    assert!(run_dir.join("target-ownership.json").exists());
    assert!(run_dir.join("logs/local-node-0.stdout.log").exists());
    assert!(run_dir.join("logs/local-node-0.stderr.log").exists());
}

fn read_result(run_dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(
        &std::fs::read(run_dir.join("result.json")).expect("result artifact should read"),
    )
    .expect("result artifact should parse")
}

fn assert_present_receipt_metrics(result: &serde_json::Value, scenario: &str) {
    let receipt_poll = &result["workloadMetrics"]["receipt-poll"]["receiptPoll"];
    let present = receipt_poll["present"].as_u64().unwrap_or_default();

    assert!(
        present > 0,
        "{scenario} should observe at least one submitted receipt"
    );
}

fn assert_restart_artifacts(result: &serde_json::Value, run_dir: &std::path::Path, scenario: &str) {
    let chaos_events = result["chaosEvents"]
        .as_array()
        .expect("result should contain chaosEvents array");
    let result_states: Vec<_> = chaos_events
        .iter()
        .map(|event| event["state"].as_str().expect("state should be a string"))
        .collect();
    assert_eq!(
        result_states,
        vec!["scheduled", "started", "completed", "recovered"],
        "{scenario}"
    );
    assert!(
        chaos_events
            .iter()
            .all(|event| event["kind"] == "restart-target"),
        "{scenario}"
    );
    let stream =
        std::fs::read_to_string(run_dir.join("result.jsonl")).expect("result stream should read");
    let stream_events = jsonl_values(&stream);
    let chaos_transitions: Vec<_> = stream_events
        .iter()
        .filter(|event| event["kind"] == "chaos-transition")
        .collect();
    let stream_states: Vec<_> = chaos_transitions
        .iter()
        .map(|event| event["state"].as_str().expect("state should be a string"))
        .collect();
    assert_eq!(
        stream_states,
        vec!["scheduled", "started", "completed", "recovered"],
        "{scenario}"
    );
    assert!(
        chaos_transitions
            .iter()
            .all(|event| event["chaosKind"] == "restart-target"),
        "{scenario}"
    );
}

fn assert_restart_related_failure(result: &serde_json::Value, scenario: &str) {
    let receipt_errors = &result["workloadMetrics"]["receipt-poll"]["errorCounts"];
    let restarted = receipt_errors["target-restarted"]
        .as_u64()
        .unwrap_or_default();

    assert!(
        restarted > 0,
        "{scenario} should record target-restarted receipt failures"
    );
}

fn assert_contract_view_artifacts(
    result: &serde_json::Value,
    run_dir: &std::path::Path,
    scenario: &str,
) {
    let contracts = result["contracts"]
        .as_array()
        .expect("result should contain contracts array");
    assert_eq!(contracts.len(), 1, "{scenario}");
    assert_eq!(contracts[0]["name"], "fibonacci", "{scenario}");
    assert_eq!(contracts[0]["status"], "completed", "{scenario}");
    assert!(
        matches!(
            contracts[0]["addressSource"].as_str(),
            Some("derived" | "receipt")
        ),
        "{scenario}"
    );
    assert!(contracts[0]["txHash"].as_str().is_some(), "{scenario}");
    assert!(contracts[0]["address"].as_str().is_some(), "{scenario}");

    let metrics = &result["workloadMetrics"]["view-call"]["counts"];
    assert!(
        metrics["completed"].as_u64().unwrap_or_default() > 0,
        "{scenario} should complete view-call workloads"
    );

    let stream =
        std::fs::read_to_string(run_dir.join("result.jsonl")).expect("result stream should read");
    let events = jsonl_values(&stream);
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "contract-deploy-started"),
        "{scenario}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "contract-deploy-completed"),
        "{scenario}"
    );
    assert!(
        events.iter().any(|event| {
            event["kind"] == "workload-completed"
                && event["workload"] == "view-call"
                && event["viewResult"] == "return"
                && event["viewReturnData"] == "0x37"
        }),
        "{scenario} should record returned view data"
    );
}

fn assert_contract_storage_artifacts(
    result: &serde_json::Value,
    run_dir: &std::path::Path,
    scenario: &str,
) {
    let contracts = result["contracts"]
        .as_array()
        .expect("result should contain contracts array");
    assert_eq!(contracts.len(), 1, "{scenario}");
    assert_eq!(contracts[0]["name"], "fibonacci", "{scenario}");
    assert_eq!(contracts[0]["status"], "completed", "{scenario}");
    assert!(contracts[0]["address"].as_str().is_some(), "{scenario}");

    let metrics = &result["workloadMetrics"]["storage-read"]["counts"];
    assert!(
        metrics["completed"].as_u64().unwrap_or_default() > 0,
        "{scenario} should complete storage-read workloads"
    );

    let stream =
        std::fs::read_to_string(run_dir.join("result.jsonl")).expect("result stream should read");
    let events = jsonl_values(&stream);
    assert!(
        events.iter().any(|event| {
            event["kind"] == "workload-completed"
                && event["workload"] == "storage-read"
                && event["rpcMethod"] == "groundhog_getStorageAt"
                && event["chainHeight"].as_i64().is_some()
                && event["storageReadName"] == "fib-zero-key"
                && event["storageContract"].as_str().is_some()
                && event["storageKey"]
                    == "0x0000000000000000000000000000000000000000000000000000000000000000"
                && event["storageItemPresent"] == false
        }),
        "{scenario} should record storage-read fields"
    );
}

fn assert_port_closed(address: std::net::SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("local target RPC port {address} is still accepting connections after cleanup");
}
