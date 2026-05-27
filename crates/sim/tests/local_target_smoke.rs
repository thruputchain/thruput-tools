use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

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

    let status = Command::new(env!("CARGO_BIN_EXE_hoglab"))
        .arg("run")
        .arg(&scenario)
        .arg("--groundhog-binary")
        .arg(groundhog_binary)
        .arg("--hogd-dev-binary")
        .arg(hogd_dev_binary)
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

    let simulation = std::fs::read_to_string(temp.path().join("simulation.json"))
        .expect("simulation artifact should read");
    let rpc_url = json_string(&simulation, &["resolved", "rpcUrl"])
        .expect("simulation artifact should record resolved rpc url");
    assert_rpc_port_closed(&rpc_url);
}

fn json_string(payload: &str, path: &[&str]) -> Option<String> {
    let mut value: &serde_json::Value = &serde_json::from_str(payload).ok()?;
    for part in path {
        value = value.get(*part)?;
    }
    value.as_str().map(str::to_owned)
}

fn assert_rpc_port_closed(rpc_url: &str) {
    let address = rpc_url
        .strip_prefix("http://")
        .expect("local smoke rpc url should be http")
        .parse::<std::net::SocketAddr>()
        .expect("local smoke rpc url should contain host:port");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("local target RPC port {address} is still accepting connections after cleanup");
}
