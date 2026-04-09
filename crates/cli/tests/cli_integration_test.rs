use std::process::Command;
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
    assert!(stderr.contains("not a groundhog project"));
    assert!(stderr.contains("hint:"));
    assert!(stderr.contains("groundhog new my-project"));
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
    assert_eq!(stderr["error"]["code"], "not_a_project");
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("groundhog new my-project")
    );
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
    assert_eq!(stderr["error"]["code"], "not_a_project");
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
fn deploy_returns_not_implemented() {
    let output = groundhog().arg("deploy").output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not yet implemented"));
    assert!(stderr.contains("hint:"));
}

#[test]
fn deploy_json_returns_structured_error() {
    let output = groundhog().arg("--json").arg("deploy").output().unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "deploy");
    assert_eq!(stderr["error"]["code"], "not_implemented");
}

#[test]
fn call_json_returns_structured_error() {
    let output = groundhog()
        .args(["--json", "call", "0x1234", "transfer"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "call");
    assert_eq!(stderr["error"]["code"], "not_implemented");
}

#[test]
fn call_returns_not_implemented() {
    let output = groundhog()
        .arg("call")
        .arg("0x1234")
        .arg("transfer")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not yet implemented"));
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
