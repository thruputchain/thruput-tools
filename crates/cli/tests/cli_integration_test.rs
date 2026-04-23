use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn groundhog() -> Command {
    Command::new(env!("CARGO_BIN_EXE_groundhog"))
}

#[test]
fn help_shows_subcommands() {
    let output = groundhog().arg("--help").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("new"));
    assert!(stdout.contains("init"));
    assert!(stdout.contains("status"));
    assert!(stdout.contains("build"));
    assert!(stdout.contains("test"));
    assert!(stdout.contains("deploy"));
    assert!(stdout.contains("call"));
    assert!(stdout.contains("keys"));
    assert!(stdout.contains("login"));
    assert!(stdout.contains("logout"));
    assert!(stdout.contains("completion"));
}

#[test]
fn version_flag_works() {
    let output = groundhog().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("groundhog"));
}

#[test]
fn new_creates_project() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("new")
        .arg("my-test-project")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dir = tmp.path().join("my-test-project");
    assert!(dir.join("Cargo.toml").exists());
    assert!(dir.join("groundhog.toml").exists());
    assert!(dir.join("src/lib.rs").exists());
    assert!(dir.join("src/implementation.rs").exists());
    assert!(dir.join("tests/test.rs").exists());
    assert!(dir.join("tests/test_wasm.rs").exists());
    assert!(dir.join(".cargo/config.toml").exists());
    assert!(dir.join(".cargo/contract.toml").exists());
    assert!(dir.join("rust-toolchain.toml").exists());
}

#[test]
fn new_json_outputs_structured_payload() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("new")
        .arg("json-project")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "new");
    assert!(stdout["warnings"].as_array().unwrap().is_empty());
    assert_eq!(stdout["data"]["project_name"], "json-project");
    assert_eq!(stdout["data"]["template"], "counter");
}

#[test]
fn new_with_stub_template() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("new")
        .arg("stub-proj")
        .arg("--template")
        .arg("stub")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());

    let imp = std::fs::read_to_string(tmp.path().join("stub-proj/src/implementation.rs")).unwrap();
    assert!(imp.contains("pub struct Contract"));
}

#[test]
fn init_json_delegates_to_status() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("init")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "status");
    assert_eq!(stdout["data"]["environment"]["cargo"], true);
    assert_eq!(stdout["data"]["credentials"]["executor_credentials"], false);
    assert!(!stdout["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn status_json_outputs_structured_payload() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "status");
    assert!(stdout["data"]["environment"]["cargo"].as_bool().unwrap());
    assert!(
        !stdout["data"]["credentials"]["executor_credentials"]
            .as_bool()
            .unwrap()
    );
    assert!(!stdout["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn status_json_flattens_project_findings_into_warnings() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("groundhog.toml"), "[profile.default]\n").unwrap();

    let output = groundhog()
        .arg("--json")
        .arg("status")
        .arg("--profile")
        .arg("staging")
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = stdout["warnings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding["code"] == "profile_not_found")
    );
}

#[test]
fn quiet_status_exits_one_with_no_output_when_missing_state() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("-q")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
#[serial_test::serial]
fn quiet_status_exits_zero_with_no_output_when_fully_configured() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    unsafe {
        std::env::set_var("CARGO_HOME", &cargo_home);
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    groundhog_cli::auth::ensure_cargo_registry(&cargo_home.join("config.toml")).unwrap();
    groundhog_cli::keys::generate(
        &Some("alice".into()),
        false,
        "keys.generate",
        &groundhog_cli::Print::quiet(),
    )
    .unwrap();
    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "test".into(),
        password: "pass".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();

    let output = groundhog()
        .arg("-q")
        .arg("status")
        .env("CARGO_HOME", &cargo_home)
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    unsafe {
        std::env::remove_var("CARGO_HOME");
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}

#[test]
fn status_human_output_shows_sections() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    // Will exit 1 (warnings), but stderr should have
    // human-readable section content.
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cargo: installed"));
    assert!(stderr.contains("registry:"));
    assert!(stderr.contains("keys: none"));
    assert!(stderr.contains("credentials: NOT stored"));
}

#[test]
fn new_fails_on_existing_dir() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("exists")).unwrap();

    let output = groundhog()
        .arg("new")
        .arg("exists")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
}

#[test]
fn build_fails_outside_project() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("build")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cargo metadata failed"));
}

#[test]
fn build_json_failure_is_structured() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("build")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "build");
    assert_eq!(stderr["error"]["code"], "cargo_metadata_failed");
}

#[test]
fn build_help_mentions_target_dir() {
    let output = groundhog().args(["build", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--target-dir"));
}

#[test]
fn test_help_mentions_target_dir() {
    let output = groundhog().args(["test", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--target-dir"));
}

#[test]
fn deploy_help_mentions_target_dir_and_dry_run() {
    let output = groundhog().args(["deploy", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--target-dir"));
    assert!(stdout.contains("--dry-run"));
}

#[test]
fn call_help_mentions_target_dir() {
    let output = groundhog().args(["call", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--target-dir"));
    assert!(stdout.contains("--dry-run"));
}

#[test]
fn build_json_prefers_metadata_failure_outside_project() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .args([
            "--json",
            "build",
            "--target-dir",
            "explicit-target",
            "--",
            "--target-dir",
            "nested",
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "build");
    assert_eq!(stderr["error"]["code"], "cargo_metadata_failed");
}

#[test]
fn login_json_returns_structured_error() {
    let output = groundhog().arg("--json").arg("login").output().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "login");
    assert_eq!(stderr["error"]["code"], "json_unsupported");
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("groundhog login")
    );
}

#[test]
fn test_fails_outside_project() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("test")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
}

#[test]
fn test_json_failure_is_structured() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("test")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "test");
    assert_eq!(stderr["error"]["code"], "cargo_metadata_failed");
}

#[test]
fn test_json_prefers_metadata_failure_outside_project() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .args([
            "--json",
            "test",
            "--target-dir",
            "explicit-target",
            "--",
            "--target-dir",
            "nested",
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "test");
    assert_eq!(stderr["error"]["code"], "cargo_metadata_failed");
}

#[test]
fn new_json_failure_is_structured() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("exists")).unwrap();

    let output = groundhog()
        .arg("--json")
        .arg("new")
        .arg("exists")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "new");
    assert_eq!(stderr["error"]["code"], "dir_exists");
}

#[test]
fn deploy_fails_without_config() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("deploy")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no rpc_url") || stderr.contains("no signing key"));
}

#[test]
fn deploy_json_fails_without_config() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("deploy")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "deploy");
    assert_eq!(stderr["error"]["code"], "missing_rpc_url");
}

#[test]
fn deploy_dry_run_json_outputs_prepared_transaction() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!("[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let generate = groundhog()
        .args(["keys", "generate", "alice"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(
        generate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&generate.stderr)
    );

    // Write a minimal WASM file for the deploy to read.
    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();

    let output = groundhog()
        .args([
            "--json",
            "deploy",
            "--dry-run",
            "--wasm",
            wasm_path.to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "deploy");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(stdout["data"]["dry_run"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(stdout["data"]["wasm_bytes"], 8);
    assert!(
        stdout["data"]["predicted_address"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
    assert!(
        stdout["data"]["signed_tx"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
}

#[test]
fn deploy_live_json_submits_and_returns_receipt() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_deploy_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!("[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let generate = groundhog()
        .args(["keys", "generate", "alice"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(
        generate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&generate.stderr)
    );

    // Write a minimal WASM file for the deploy to read.
    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();

    let output = groundhog()
        .args(["--json", "deploy", "--wasm", wasm_path.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "deploy");
    assert_eq!(stdout["warnings"], serde_json::json!([]));

    // The mock returns 0xbb..bb as the deployed address via return_data.
    let expected_address = format!("0x{}", "bb".repeat(32));
    assert_eq!(stdout["data"]["address"], expected_address);
    assert_eq!(stdout["data"]["address_source"], "receipt");
    assert_eq!(stdout["data"]["height"], 42);
    assert_eq!(stdout["data"]["status"]["result"], "ok");
    assert!(
        stdout["data"]["tx_hash"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
}

#[test]
fn call_json_fails_without_config() {
    let output = groundhog()
        .args(["--json", "call", "0x1234", "transfer"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "call");
    assert_eq!(stderr["error"]["code"], "missing_rpc_url");
}

#[test]
fn call_dry_run_json_outputs_prepared_transaction() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!("[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let generate = groundhog()
        .args(["keys", "generate", "alice"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(
        generate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&generate.stderr)
    );

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args([
            "--json",
            "call",
            "--dry-run",
            &contract,
            "0x0123456789abcdef",
            "0x0102",
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "call");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(stdout["data"]["dry_run"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(stdout["data"]["contract"], contract);
    assert_eq!(stdout["data"]["method"], "0x0123456789abcdef");
    assert_eq!(stdout["data"]["entrypoint_id"], "0x0123456789abcdef");
    assert_eq!(stdout["data"]["calldata"], "0x0102");
    assert_eq!(stdout["data"]["selector"]["source"], "explicit_entrypoint");
    assert!(stdout["data"]["selector"]["interface"].is_null());
    assert!(
        stdout["data"]["signed_tx"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
}

#[test]
fn call_fails_without_config() {
    let output = groundhog()
        .arg("call")
        .arg("0x1234")
        .arg("transfer")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rpc_url") || stderr.contains("profile"));
}

#[test]
fn invalid_template_rejected() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("new")
        .arg("bad")
        .arg("--template")
        .arg("nope")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("possible values"));
    assert!(stderr.contains("counter"));
    assert!(stderr.contains("stub"));
}

#[test]
fn completion_json_outputs_structured_payload() {
    let output = groundhog()
        .arg("--json")
        .arg("completion")
        .arg("bash")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "completion");
    assert!(stdout["warnings"].as_array().unwrap().is_empty());
    assert_eq!(stdout["data"]["shell"], "bash");
    assert!(
        stdout["data"]["script"]
            .as_str()
            .unwrap()
            .contains("groundhog")
    );
}

#[test]
fn keys_json_flow_works() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");

    let generate = groundhog()
        .arg("--json")
        .arg("keys")
        .arg("generate")
        .arg("alice")
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(
        generate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&generate.stderr)
    );
    let generated: serde_json::Value = serde_json::from_slice(&generate.stdout).unwrap();
    assert_eq!(generated["ok"], true);
    assert_eq!(generated["command"], "keys.generate");
    assert!(generated["warnings"].as_array().unwrap().is_empty());
    assert_eq!(generated["data"]["key"]["name"], "alice");
    assert_eq!(generated["data"]["key"]["is_default"], true);

    let listed = groundhog()
        .arg("--json")
        .arg("keys")
        .arg("ls")
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(listed.status.success());
    let listed_json: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed_json["data"]["keys"].as_array().unwrap().len(), 1);
    assert_eq!(listed_json["data"]["keys"][0]["name"], "alice");

    let used = groundhog()
        .arg("--json")
        .arg("keys")
        .arg("use")
        .arg("alice")
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(used.status.success());
    let used_json: serde_json::Value = serde_json::from_slice(&used.stdout).unwrap();
    assert_eq!(used_json["command"], "keys.use");
    assert_eq!(used_json["data"]["key"]["is_default"], true);

    let removed = groundhog()
        .arg("--json")
        .arg("keys")
        .arg("rm")
        .arg("alice")
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(removed.status.success());
    let removed_json: serde_json::Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(removed_json["command"], "keys.rm");
    assert_eq!(removed_json["data"]["cleared_default"], true);
}

#[test]
fn logout_json_has_envelope_structure() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("logout")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "logout");
    assert!(stdout["data"]["config_dir"].is_string());
    // cargo logout may warn (no token stored) — if so,
    // the warning is collected into the envelope.
    let warnings = stdout["warnings"].as_array().unwrap();
    if !warnings.is_empty() {
        assert_eq!(warnings[0]["code"], "cargo_logout_failed");
    }
    // In JSON mode, cargo output is captured and
    // print.warn() is a no-op, so stderr is empty.
    assert!(output.stderr.is_empty());
}

#[test]
fn keys_ls_human_output_uses_stdout() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");

    let generate = groundhog()
        .arg("keys")
        .arg("generate")
        .arg("alice")
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(generate.status.success());

    let listed = groundhog()
        .arg("keys")
        .arg("ls")
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(listed.status.success());

    let stdout = String::from_utf8_lossy(&listed.stdout);
    let stderr = String::from_utf8_lossy(&listed.stderr);
    assert!(stdout.contains("alice"));
    assert!(!stderr.contains("alice"));
}

#[test]
fn keys_generate_duplicate_fails() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("cfg");

    let first = groundhog()
        .args(["keys", "generate", "dup"])
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert!(first.status.success());

    let second = groundhog()
        .args(["keys", "generate", "dup"])
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("already exists"));
    assert!(stderr.contains("hint:"));
}

#[test]
fn keys_use_nonexistent_fails() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .args(["--json", "keys", "use", "ghost"])
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "keys.use");
    assert_eq!(stderr["error"]["code"], "key_not_found");
}

#[test]
fn keys_rm_nonexistent_fails() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .args(["--json", "keys", "rm", "ghost"])
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "keys.rm");
    assert_eq!(stderr["error"]["code"], "key_not_found");
}

/// Read one HTTP POST request from a TCP stream and return the parsed JSON body.
fn read_jsonrpc_request(
    listener: &TcpListener,
    deadline: Instant,
    label: &str,
) -> (std::net::TcpStream, serde_json::Value) {
    let (stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {label} request"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("failed to accept mock RPC connection for {label}: {err}"),
        }
    };

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap();
        }
    }

    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (stream, request)
}

/// Send a JSON-RPC response and close the connection.
#[allow(clippy::needless_pass_by_value)]
fn send_jsonrpc_response(
    mut stream: std::net::TcpStream,
    request_id: &serde_json::Value,
    result: serde_json::Value,
) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id.clone(),
        "result": result,
    });
    let response_body = serde_json::to_vec(&response).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response_body.len()
    )
    .unwrap();
    stream.write_all(&response_body).unwrap();
    stream.flush().unwrap();
}

fn spawn_chain_tip_rpc(height: i64) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "chainTip");
        assert_eq!(request["method"], "groundhog_chainTip");

        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({
                "height": height,
                "blockHash": format!("0x{}", "00".repeat(32)),
            }),
        );

        // Verify no additional requests arrive (dry-run must not broadcast).
        let follow_up_deadline = Instant::now() + Duration::from_millis(200);
        loop {
            match listener.accept() {
                Ok(_) => {
                    panic!(
                        "unexpected additional RPC request after groundhog_chainTip; dry-run \
                         should not broadcast"
                    );
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= follow_up_deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("failed to accept follow-up mock RPC connection: {err}"),
            }
        }
    });

    (format!("http://{addr}"), handle)
}

/// Mock RPC server that handles the full deploy flow:
/// `chainTip` → `sendRawTransaction` → `getTransactionReceipt`.
///
/// Returns a 32-byte dummy address (`0xbb..bb`) as the deploy `return_data` so the live
/// path exercises the receipt-based address extraction.
fn spawn_deploy_rpc(height: i64) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    let tx_hash = format!("0x{}", "aa".repeat(32));

    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);

        // 1. chainTip
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "chainTip");
        assert_eq!(request["method"], "groundhog_chainTip");
        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({
                "height": height,
                "blockHash": format!("0x{}", "00".repeat(32)),
            }),
        );

        // 2. sendRawTransaction
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "sendRawTransaction");
        assert_eq!(request["method"], "groundhog_sendRawTransaction");
        assert!(
            request["params"][0].as_str().unwrap().starts_with("0x"),
            "sendRawTransaction should receive 0x-prefixed tx hex"
        );
        send_jsonrpc_response(stream, &request["id"], serde_json::json!(tx_hash));

        // 3. getTransactionReceipt
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "getTransactionReceipt");
        assert_eq!(request["method"], "groundhog_getTransactionReceipt");
        let contract_address = format!("0x{}", "bb".repeat(32));
        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({
                "txHash": tx_hash,
                "height": height,
                "status": { "result": "ok" },
                "events": [],
                "returnData": contract_address,
            }),
        );
    });

    (format!("http://{addr}"), handle)
}
