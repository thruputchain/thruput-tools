use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
#[cfg(all(feature = "dev-node", unix))]
use std::process::{ExitStatus, Stdio};
#[cfg(all(feature = "dev-node", unix))]
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(all(feature = "dev-node", unix))]
use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use tempfile::TempDir;

fn groundhog() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
    command.env_remove("GROUNDHOG_RUNTIME_MANIFEST");
    command
}

#[cfg(feature = "dev-node")]
fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("bug: cli crate has parent")
        .parent()
        .expect("bug: tools dir has parent")
        .parent()
        .expect("bug: crates dir has workspace parent")
        .to_path_buf()
}

#[cfg(feature = "dev-node")]
fn target_dir() -> std::path::PathBuf {
    let root = workspace_root();
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(raw) => {
            let path = std::path::PathBuf::from(raw);
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        }
        None => root.join("target"),
    }
}

#[cfg(feature = "dev-node")]
fn fake_hogd_dev_binary() -> std::path::PathBuf {
    let binary = target_dir()
        .join("debug")
        .join("examples")
        .join(format!("fake_hogd_dev{}", std::env::consts::EXE_SUFFIX));
    if binary.is_file() {
        return binary;
    }

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args([
            "build",
            "--quiet",
            "-p",
            "groundhog-node-supervisor",
            "--example",
            "fake_hogd_dev",
        ])
        .current_dir(workspace_root())
        .status()
        .expect("bug: cargo should run to build fake_hogd_dev example");
    assert!(
        status.success(),
        "failed to build fake_hogd_dev example; status: {status}",
    );
    assert!(
        binary.is_file(),
        "fake_hogd_dev example missing after build at {}",
        binary.display(),
    );
    binary
}

#[cfg(all(feature = "dev-node", unix))]
fn send_sigint(child: &std::process::Child) {
    let pid = i32::try_from(child.id()).expect("bug: child pid should fit i32");
    // SAFETY: sends SIGINT to the spawned CLI process id.
    let rc = unsafe { libc::kill(pid, libc::SIGINT) };
    assert_eq!(rc, 0, "failed to send SIGINT to child pid {pid}");
}

#[cfg(all(feature = "dev-node", unix))]
fn wait_child(mut child: std::process::Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("bug: child status is readable") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("child did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(all(feature = "dev-node", unix))]
struct ChildGuard(Option<std::process::Child>);

#[cfg(all(feature = "dev-node", unix))]
impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    fn child(&self) -> &std::process::Child {
        self.0.as_ref().expect("bug: child guard is armed")
    }

    fn child_mut(&mut self) -> &mut std::process::Child {
        self.0.as_mut().expect("bug: child guard is armed")
    }

    fn into_inner(mut self) -> std::process::Child {
        self.0.take().expect("bug: child guard is armed")
    }
}

#[cfg(all(feature = "dev-node", unix))]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(all(feature = "dev-node", unix))]
fn read_pid_file(path: &std::path::Path) -> i32 {
    std::fs::read_to_string(path)
        .expect("bug: fake hogd-dev should write pid file")
        .trim()
        .parse()
        .expect("bug: fake hogd-dev pid should parse")
}

#[cfg(all(feature = "dev-node", unix))]
fn wait_pid_gone(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: `kill(pid, 0)` does not deliver a signal; it only checks
        // whether the process still exists.
        let rc = unsafe { libc::kill(pid, 0) };
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(all(feature = "dev-node", unix))]
fn read_line_with_timeout(
    stdout: std::process::ChildStdout,
    timeout: Duration,
) -> std::io::Result<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let result = reader.read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    rx.recv_timeout(timeout)
        .expect("timed out waiting for child stdout line")
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
    assert!(stdout.contains("view"));
    assert!(stdout.contains("storage"));
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

#[cfg(all(feature = "dev-node", unix))]
#[test]
fn node_hogd_dev_binary_json_starts_supervisor() {
    let fake = fake_hogd_dev_binary();
    let pid_dir = TempDir::new().unwrap();
    let pid_path = pid_dir.path().join("hogd.pid");
    let child = groundhog()
        .arg("--json")
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hogd-dev-binary")
        .arg(fake)
        .env("GROUNDHOG_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("GROUNDHOG_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bug: groundhog node should spawn");
    let mut child = ChildGuard::new(child);

    let stdout = child
        .child_mut()
        .stdout
        .take()
        .expect("bug: stdout should be piped");
    let line = read_line_with_timeout(stdout, Duration::from_secs(10))
        .expect("bug: startup JSON should be readable");
    let payload: serde_json::Value =
        serde_json::from_str(&line).expect("bug: startup line should be JSON");

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["command"], "node");
    assert_eq!(payload["data"]["backend"]["kind"], "hogd_dev");
    assert_eq!(payload["data"]["backend"]["planes"]["submit"], "scheduler");
    assert_eq!(payload["data"]["backend"]["planes"]["chain"], "scheduler");
    assert_eq!(payload["data"]["backend"]["planes"]["read"], "indexer");
    assert_eq!(payload["data"]["backend"]["planes"]["view"], "stub");
    let scheduler = payload["data"]["backend"]["endpoints"]["scheduler_coord"]
        .as_str()
        .expect("bug: SchedulerCoord endpoint should be a string");
    assert!(
        scheduler.starts_with("127.0.0.1:"),
        "SchedulerCoord endpoint should be loopback, got {scheduler}",
    );
    let rpc_url = payload["data"]["rpc_url"]
        .as_str()
        .expect("bug: rpc_url string")
        .to_string();
    assert!(rpc_url.starts_with("http://127.0.0.1:"));

    let tip = tokio::runtime::Runtime::new()
        .expect("bug: tokio runtime should start")
        .block_on(async {
            let client = HttpClientBuilder::default()
                .build(&rpc_url)
                .expect("bug: JSON-RPC client should build");
            client
                .chain_tip()
                .await
                .expect("bug: CLI-started supervisor should serve chainTip")
        });
    assert_eq!(*tip.height, 0);

    let pid = read_pid_file(&pid_path);
    send_sigint(child.child());
    let status = wait_child(child.into_inner(), Duration::from_secs(10));
    assert!(status.success(), "groundhog node should stop cleanly");
    assert!(
        wait_pid_gone(pid, Duration::from_secs(2)),
        "fake hogd-dev pid {pid} still alive after CLI exit",
    );
}

#[cfg(feature = "dev-node")]
#[test]
fn node_hogd_dev_missing_binary_fails_without_native_fallback() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("missing-hogd-dev");
    let output = groundhog()
        .arg("--json")
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hogd-dev-binary")
        .arg(&missing)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "startup failure should not print success JSON: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("bug: error should be JSON");
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "node");
    assert_eq!(stderr["error"]["code"], "node_supervisor_error");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .expect("bug: message string")
            .contains("configured hogd-dev binary path does not exist"),
    );
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
fn build_rv32_json_rejects_wasm_only_options_before_metadata() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .args(["--json", "build", "--target", "rv32", "--optimize"])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "build");
    assert_eq!(stderr["error"]["code"], "invalid_build_options");
}

#[test]
fn build_rv32_json_rejects_bindings_before_metadata() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .args(["--json", "build", "--target", "rv32", "--bindings", "ts"])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "build");
    assert_eq!(stderr["error"]["code"], "invalid_build_options");
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn build_rv32_json_success_reports_artifact_paths() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract-rv32\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let target_dir = tmp.path().join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo-contract-rv32");
    let rv32 = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.rv32a");
    let metadata = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.rv32.json");
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": target_dir,
        "packages": [{
            "name": "demo-contract-rv32",
            "manifest_path": tmp.path().join("Cargo.toml"),
        }],
    })
    .to_string();

    let cargo = fake_bin.join("cargo");
    std::fs::write(
        &cargo,
        format!(
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = \"metadata\" ]; then\n",
                "  printf '%s' '{}'\n",
                "  exit 0\n",
                "fi\n",
                "if [ \"$CARGO_PROFILE_RELEASE_STRIP\" != \"false\" ]; then\n",
                "  echo missing strip env >&2\n",
                "  exit 2\n",
                "fi\n",
                "/bin/mkdir -p '{}'\n",
                "printf 'ELF' > '{}'\n",
                "exit 0\n",
            ),
            metadata_json,
            raw_elf
                .parent()
                .expect("bug: raw elf parent should exist")
                .display(),
            raw_elf.display()
        ),
    )
    .unwrap();
    let rv32_lower = fake_bin.join("rv32-lower");
    std::fs::write(
        &rv32_lower,
        concat!(
            "#!/bin/sh\n",
            "while [ \"$#\" -gt 0 ]; do\n",
            "  case \"$1\" in\n",
            "    --out) out=\"$2\"; shift 2 ;;\n",
            "    --metadata) metadata=\"$2\"; shift 2 ;;\n",
            "    *) shift ;;\n",
            "  esac\n",
            "done\n",
            "printf 'RV32' > \"$out\"\n",
            "printf '{\"ok\":true}\\n' > \"$metadata\"\n",
        ),
    )
    .unwrap();

    for executable in [&cargo, &rv32_lower] {
        let mut perms = std::fs::metadata(executable).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(executable, perms).unwrap();
    }

    let output = groundhog()
        .args([
            "--json",
            "build",
            "--target",
            "rv32",
            "-p",
            "demo-contract-rv32",
        ])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read(&rv32).unwrap(), b"RV32");
    assert_eq!(
        std::fs::read_to_string(&metadata).unwrap(),
        "{\"ok\":true}\n"
    );

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "build");
    assert_eq!(stdout["data"]["target"], "rv32");
    assert_eq!(stdout["data"]["elf"], raw_elf.display().to_string());
    assert_eq!(stdout["data"]["rv32"], rv32.display().to_string());
    assert_eq!(stdout["data"]["metadata"], metadata.display().to_string());
    assert_eq!(stdout["data"]["size_bytes"], 4);
}

#[cfg(unix)]
#[test]
fn build_rv32_json_reports_missing_lowerer_after_metadata() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract-rv32\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": tmp.path().join("target"),
        "packages": [{
            "name": "demo-contract-rv32",
            "manifest_path": tmp.path().join("Cargo.toml"),
        }],
    })
    .to_string();

    let cargo = fake_bin.join("cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\nexit 1\n"
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&cargo).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cargo, perms).unwrap();

    let output = groundhog()
        .args([
            "--json",
            "build",
            "--target",
            "rv32",
            "-p",
            "demo-contract-rv32",
        ])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "build");
    assert_eq!(stderr["error"]["code"], "rv32_lower_not_found");
}

#[cfg(unix)]
#[test]
fn build_rv32_json_reports_lowerer_failure_details() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract-rv32\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let target_dir = tmp.path().join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo-contract-rv32");
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": target_dir,
        "packages": [{
            "name": "demo-contract-rv32",
            "manifest_path": tmp.path().join("Cargo.toml"),
        }],
    })
    .to_string();

    let cargo = fake_bin.join("cargo");
    std::fs::write(
        &cargo,
        format!(
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = \"metadata\" ]; then\n",
                "  printf '%s' '{}'\n",
                "  exit 0\n",
                "fi\n",
                "/bin/mkdir -p '{}'\n",
                "printf 'ELF' > '{}'\n",
                "exit 0\n",
            ),
            metadata_json,
            raw_elf
                .parent()
                .expect("bug: raw elf parent should exist")
                .display(),
            raw_elf.display()
        ),
    )
    .unwrap();
    let rv32_lower = fake_bin.join("rv32-lower");
    std::fs::write(
        &rv32_lower,
        "#!/bin/sh\nprintf 'lower stdout\\n'\nprintf 'lower stderr\\n' >&2\nexit 42\n",
    )
    .unwrap();

    for executable in [&cargo, &rv32_lower] {
        let mut perms = std::fs::metadata(executable).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(executable, perms).unwrap();
    }

    let output = groundhog()
        .args([
            "--json",
            "build",
            "--target",
            "rv32",
            "-p",
            "demo-contract-rv32",
        ])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "build");
    assert_eq!(stderr["error"]["code"], "rv32_lower_failed");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("lower stderr")
    );
    assert_eq!(stderr["error"]["details"]["stdout"], "lower stdout\n");
    assert_eq!(stderr["error"]["details"]["stderr"], "lower stderr\n");
}

#[test]
fn build_help_mentions_target_dir() {
    let output = groundhog().args(["build", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--target-dir"));
    assert!(stdout.contains("--target"));
    assert!(stdout.contains("rv32"));
}

#[test]
fn test_help_mentions_target_dir() {
    let output = groundhog().args(["test", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--target-dir"));
    assert!(stdout.contains("--watch"));
    assert!(stdout.contains("--integration"));
    assert!(stdout.contains("hogd-dev"));
    assert!(stdout.contains("tests/integration.rs"));
    assert!(!stdout.contains("not wired yet"));
}

#[test]
fn node_help_mentions_hogd_dev_binary_and_log_contract() {
    let output = groundhog().args(["node", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-v, --verbose"));
    assert!(stdout.contains("--verbose"));
    assert!(stdout.contains("--verbose is also honored with --json"));
    assert!(stdout.contains("--hogd-dev-binary"));
    assert!(stdout.contains("--dev-account-count"));
    assert!(stdout.contains("--hogd-protocol"));
    assert!(stdout.contains("GROUNDHOG_HOGD_DEV_BINARY"));
    assert!(stdout.contains("RUST_LOG"));
    assert!(stdout.contains("hogd_timestamp_ms"));
    assert!(stdout.contains("hogd_target"));
    assert!(stdout.contains("hogd_file"));
    assert!(stdout.contains("hogd_line"));
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
    assert!(stdout.contains("0x-prefixed 8-byte entrypoint ID"));
    assert!(stdout.contains("raw hex calldata"));
}

#[test]
fn storage_help_mentions_read_only_storage() {
    let output = groundhog().args(["storage", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("groundhog_getStorageAt"));
    assert!(stdout.contains("--profile"));
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

#[cfg(not(feature = "dev-node"))]
#[test]
fn test_integration_json_reports_unavailable() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("test")
        .arg("--integration")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "test");
    assert_eq!(stderr["error"]["code"], "integration_tests_unavailable");
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .expect("bug: hint should be present")
            .contains("dev-node")
    );
}

#[test]
fn test_integration_rejects_rerun() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("test")
        .arg("--integration")
        .arg("--rerun")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "test");
    assert_eq!(stderr["error"]["code"], "invalid_test_options");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .expect("bug: message should be present")
            .contains("--rerun")
    );
}

#[test]
fn test_integration_rejects_watch() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("test")
        .arg("--integration")
        .arg("--watch")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "test");
    assert_eq!(stderr["error"]["code"], "invalid_test_options");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .expect("bug: message should be present")
            .contains("--watch")
    );
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .expect("bug: hint should be present")
            .contains("conflicting")
    );
}

#[cfg(all(feature = "dev-node", unix))]
#[test]
fn test_integration_reports_local_node_startup_failure() {
    use std::os::unix::fs::PermissionsExt;

    let hogd_dev = fake_hogd_dev_binary();
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".cargo")).unwrap();
    std::fs::write(tmp.path().join(".cargo/contract.toml"), "").unwrap();
    std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
    std::fs::write(tmp.path().join("tests/integration.rs"), "").unwrap();

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": tmp.path().join("target"),
        "packages": [{
            "name": "demo-contract",
            "manifest_path": tmp.path().join("Cargo.toml"),
        }],
    })
    .to_string();
    std::fs::write(
        &cargo,
        format!(
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = \"metadata\" ]; then\n",
                "  printf '%s' '{}'\n",
                "  exit 0\n",
                "fi\n",
                "if [ \"$1\" = \"--config\" ]; then\n",
                "  shift 2\n",
                "fi\n",
                "if [ \"$1\" = \"build\" ]; then\n",
                "  for arg in \"$@\"; do\n",
                "    if [ \"$arg\" = \"--nocapture\" ]; then\n",
                "      exit 88\n",
                "    fi\n",
                "  done\n",
                "  /bin/mkdir -p '{}'\n",
                "  printf '\\000asm\\001\\000\\000\\000' > '{}'\n",
                "  exit 0\n",
                "fi\n",
                "exit 0\n",
            ),
            metadata_json,
            tmp.path()
                .join("target/wasm32-unknown-unknown/release")
                .display(),
            tmp.path()
                .join("target/wasm32-unknown-unknown/release/demo_contract.wasm")
                .display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&cargo).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cargo, perms).unwrap();

    let output = groundhog()
        .args([
            "--json",
            "test",
            "--integration",
            "-p",
            "demo-contract",
            "--",
            "--nocapture",
        ])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .env("GROUNDHOG_HOGD_DEV_BINARY", &hogd_dev)
        .env("GROUNDHOG_FAKE_HOGD_DEV_MODE", "exit-7")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "test");
    assert_eq!(stderr["error"]["code"], "node_supervisor_error");
}

#[cfg(unix)]
#[test]
fn test_json_failed_run_emits_report_on_stdout() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".cargo")).unwrap();
    std::fs::write(tmp.path().join(".cargo/contract.toml"), "").unwrap();

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": tmp.path().join("target"),
        "packages": [{
            "name": "demo-contract",
            "manifest_path": tmp.path().join("Cargo.toml"),
        }],
    })
    .to_string();
    std::fs::write(
        &cargo,
        format!(
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = \"metadata\" ]; then\n",
                "  printf '%s' '{}'\n",
                "  exit 0\n",
                "fi\n",
                "if [ \"$1\" = \"--config\" ]; then\n",
                "  shift 2\n",
                "fi\n",
                "if [ \"$1\" = \"build\" ]; then\n",
                "  /bin/mkdir -p '{}'\n",
                "  printf '\\000asm\\001\\000\\000\\000' > '{}'\n",
                "  exit 0\n",
                "fi\n",
                "if [ \"$1\" = \"test\" ]; then\n",
                "  printf 'running 2 tests\\n'\n",
                "  printf 'test tests::ok ... ok\\n'\n",
                "  printf 'test tests::fails ... FAILED\\n'\n",
                "  printf 'test result: FAILED. 1 passed; 1 failed; 0 ignored; ",
                "0 measured; 2 filtered out; finished in 0.01s\\n'\n",
                "  exit 101\n",
                "fi\n",
                "exit 0\n",
            ),
            metadata_json,
            tmp.path()
                .join("target/wasm32-unknown-unknown/release")
                .display(),
            tmp.path()
                .join("target/wasm32-unknown-unknown/release/demo_contract.wasm")
                .display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&cargo).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cargo, perms).unwrap();

    let output = groundhog()
        .arg("--json")
        .arg("test")
        .arg("-p")
        .arg("demo-contract")
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        output.stderr.is_empty(),
        "expected empty stderr, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["command"], "test");
    assert!(stdout["warnings"].as_array().unwrap().is_empty());
    assert_eq!(stdout["data"]["schemaVersion"], 1);
    assert!(stdout["data"]["projectRoot"].as_str().is_some());
    assert_eq!(stdout["data"]["success"], false);
    assert_eq!(stdout["data"]["summary"]["source"], "parsed");
    assert_eq!(stdout["data"]["summary"]["passed"], 1);
    assert_eq!(stdout["data"]["summary"]["failed"], 1);
    assert_eq!(stdout["data"]["summary"]["filteredOut"], 2);
    assert_eq!(stdout["data"]["runs"][0]["success"], false);
    assert_eq!(stdout["data"]["runs"][0]["summary"]["source"], "parsed");
    assert_eq!(stdout["data"]["runs"][0]["summary"]["filteredOut"], 2);
    assert_eq!(stdout["data"]["runs"][0]["tests"][0]["name"], "tests::ok");
    assert_eq!(stdout["data"]["runs"][0]["tests"][0]["status"], "passed");
    assert_eq!(
        stdout["data"]["runs"][0]["tests"][1]["name"],
        "tests::fails"
    );
    assert_eq!(stdout["data"]["runs"][0]["tests"][1]["status"], "failed");
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
fn deploy_dry_run_uses_manifest_dev_wallet_address_without_dev_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &advertised_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

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
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let signed_tx = stdout["data"]["signed_tx"]
        .as_str()
        .expect("bug: dry-run output includes signed tx");

    assert_eq!(signed_tx_invoked_address(signed_tx), advertised_wallet);
}

#[test]
fn deploy_dry_run_uses_project_local_node_state_rpc_and_manifest() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &advertised_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        "[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\n",
    )
    .unwrap();
    write_local_node_state(&project, &rpc_url, &manifest_path);

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
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let signed_tx = stdout["data"]["signed_tx"]
        .as_str()
        .expect("bug: dry-run output includes signed tx");

    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(signed_tx_invoked_address(signed_tx), advertised_wallet);
}

#[test]
fn deploy_dry_run_runtime_manifest_env_overrides_profile() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let profile_wallet = advertised_dev_wallet_address(0);
    let env_wallet = advertised_dev_wallet_address(1);
    let profile_manifest = write_runtime_manifest(&project, 0, &profile_wallet, None);
    let env_manifest = write_runtime_manifest(tmp.path(), 0, &env_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            profile_manifest.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

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
        .env("GROUNDHOG_RUNTIME_MANIFEST", &env_manifest)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let signed_tx = stdout["data"]["signed_tx"]
        .as_str()
        .expect("bug: dry-run output includes signed tx");

    assert_eq!(signed_tx_invoked_address(signed_tx), env_wallet);
}

#[test]
fn deploy_dry_run_blank_runtime_manifest_env_falls_back_to_profile() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let profile_wallet = advertised_dev_wallet_address(0);
    let profile_manifest = write_runtime_manifest(&project, 0, &profile_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            profile_manifest.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

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
        .env("GROUNDHOG_RUNTIME_MANIFEST", "   ")
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let signed_tx = stdout["data"]["signed_tx"]
        .as_str()
        .expect("bug: dry-run output includes signed tx");

    assert_eq!(signed_tx_invoked_address(signed_tx), profile_wallet);
}

#[cfg(unix)]
#[test]
fn deploy_dry_run_rejects_runtime_manifest_symlink() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &advertised_wallet, None);
    let symlink_path = tmp.path().join("runtime-manifest-link.json");
    std::os::unix::fs::symlink(&manifest_path, &symlink_path).unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            symlink_path.display()
        ),
    )
    .unwrap();

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
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "local_dev_account_unavailable");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a symlink")
    );
}

#[test]
fn deploy_dry_run_rejects_manifest_public_key_mismatch_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &advertised_wallet, Some([0x99; 32]));
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

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
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "local_dev_account_unavailable");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not match derivation seed")
    );
}

#[test]
fn deploy_dry_run_rejects_manifest_seed_that_does_not_match_signer() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest_with_seed(
        tmp.path(),
        0,
        &advertised_wallet,
        "alternate-dev-seed",
        None,
    );
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

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
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "local_dev_account_unavailable");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("expected 0x")
    );
}

#[test]
fn deploy_dev_key_requires_runtime_manifest() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    std::fs::write(
        project.join("groundhog.toml"),
        "[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\n",
    )
    .unwrap();

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
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "deploy");
    assert_eq!(stderr["error"]["code"], "local_dev_account_unavailable");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .expect("bug: error message should be string")
            .contains("runtime_manifest"),
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
    assert!(stdout["data"]["transaction_detail"].is_null());
    assert!(
        stdout["data"]["tx_hash"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
}

#[test]
fn deploy_live_json_includes_transaction_detail_when_dev_method_exists() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) =
        spawn_deploy_rpc_with_detail(42, DevDetailRpcResponse::Result(sample_detail_result(42)));
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

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["transaction_detail"]["gasConsumed"], 12450);
    assert_eq!(
        stdout["data"]["transaction_detail"]["storageDeltas"][0]["kind"],
        "bytestringSet"
    );
    assert_eq!(
        stdout["data"]["transaction_detail"]["trace"]["status"]["result"],
        "return"
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
fn storage_json_fails_without_config() {
    let address = format!("0x{}", "11".repeat(32));
    let key = format!("0x{}", "42".repeat(32));
    let output = groundhog()
        .args(["--json", "storage", &address, &key])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "storage");
    assert_eq!(stderr["error"]["code"], "missing_rpc_url");
}

#[test]
fn view_json_fails_without_config() {
    let address = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["--json", "view", &address, "0x0102030405060708"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "view");
    assert_eq!(stderr["error"]["code"], "missing_rpc_url");
}

#[test]
fn view_json_returns_raw_output() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let entrypoint = "0x0102030405060708";
    let calldata = "0xdeadbeef";
    let (rpc_url, rpc_server) = spawn_view_rpc(&address, entrypoint, calldata);
    std::fs::write(
        project.join("groundhog.toml"),
        format!("[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let output = groundhog()
        .args([
            "--json",
            "view",
            &address,
            entrypoint,
            "--calldata",
            calldata,
        ])
        .current_dir(&project)
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
    assert_eq!(stdout["command"], "view");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(stdout["data"]["schemaVersion"], 1);
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["contract"], address);
    assert_eq!(stdout["data"]["entrypointId"], entrypoint);
    assert_eq!(stdout["data"]["calldata"], calldata);
    assert_eq!(stdout["data"]["caller"], serde_json::Value::Null);
    assert_eq!(stdout["data"]["height"], 12);
    assert_eq!(stdout["data"]["output"]["result"], "return");
    assert_eq!(stdout["data"]["output"]["returnData"], "0x0102");
    assert_eq!(stdout["data"]["events"], serde_json::json!([]));
}

#[test]
fn view_json_resolves_contract_alias_and_from_address() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let caller = format!("0x{}", "22".repeat(32));
    let entrypoint = "0x0102030405060708";
    let calldata = "0x";
    let (rpc_url, rpc_server) = spawn_view_rpc_with_caller(&address, entrypoint, calldata, &caller);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.contracts]\ncounter = \"{address}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args([
            "--json",
            "view",
            "counter",
            entrypoint,
            "--calldata",
            calldata,
            "--from",
            &caller,
        ])
        .current_dir(&project)
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
    assert_eq!(stdout["command"], "view");
    assert_eq!(stdout["data"]["contract"], address);
    assert_eq!(stdout["data"]["caller"], caller);
}

#[test]
fn view_json_uses_manifest_dev_account_as_caller_without_dev_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let caller = advertised_dev_wallet_address(0);
    let entrypoint = "0x0102030405060708";
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &caller, None);
    let (rpc_url, rpc_server) = spawn_view_rpc_with_caller(&address, entrypoint, "0x", &caller);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "view", &address, entrypoint, "--from", "dev-0"])
        .current_dir(&project)
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
    assert_eq!(stdout["command"], "view");
    assert_eq!(stdout["data"]["caller"], caller);
}

#[test]
fn view_json_surfaces_abort_as_success_body() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let entrypoint = "0x0102030405060708";
    let (rpc_url, rpc_server) = spawn_view_rpc_with_output(
        &address,
        entrypoint,
        "0x",
        serde_json::json!({
            "result": "abort",
            "abortData": "0x0a0b",
        }),
    );
    std::fs::write(
        project.join("groundhog.toml"),
        format!("[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "view", &address, entrypoint])
        .current_dir(&project)
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
    assert_eq!(stdout["data"]["output"]["result"], "abort");
    assert_eq!(stdout["data"]["output"]["abortData"], "0x0a0b");
    assert_eq!(stdout["data"]["returnValue"], serde_json::Value::Null);
}

#[test]
fn view_json_surfaces_fault_as_success_body() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let entrypoint = "0x0102030405060708";
    let (rpc_url, rpc_server) = spawn_view_rpc_with_output(
        &address,
        entrypoint,
        "0x",
        serde_json::json!({
            "result": "fault",
        }),
    );
    std::fs::write(
        project.join("groundhog.toml"),
        format!("[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "view", &address, entrypoint])
        .current_dir(&project)
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
    assert_eq!(stdout["data"]["output"]["result"], "fault");
    assert_eq!(stdout["data"]["returnValue"], serde_json::Value::Null);
}

#[test]
fn view_human_prints_event_details() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let entrypoint = "0x0102030405060708";
    let topic = format!("0x{}", "22".repeat(32));
    let event_data = "0x1234";
    let (rpc_url, rpc_server) = spawn_view_rpc_with_events(
        &address,
        entrypoint,
        "0x",
        serde_json::json!([{
            "contractAddress": address,
            "topics": [topic],
            "data": event_data,
        }]),
    );
    std::fs::write(
        project.join("groundhog.toml"),
        format!("[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let output = groundhog()
        .args(["view", &address, entrypoint, "--calldata", "0x"])
        .current_dir(&project)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("View returned"));
    assert!(stderr.contains("events: 1"));
    assert!(stderr.contains(&format!(
        "    [0] {address} topic0={topic} data={event_data}"
    )));
}

#[test]
fn storage_json_reads_latest_item() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_storage_rpc(&address, &key);
    std::fs::write(
        project.join("groundhog.toml"),
        format!("[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "storage", &address, &key])
        .current_dir(&project)
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
    assert_eq!(stdout["command"], "storage");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["key"], key);
    assert_eq!(stdout["data"]["block_height"], 12);
    assert_eq!(stdout["data"]["item"]["kind"], "bytestring");
    assert_eq!(
        stdout["data"]["item"]["bytes"],
        "0x6c6f63616c2d6e6f64652d73746f72616765"
    );
}

#[test]
fn storage_json_uses_project_local_node_state_with_global_dev_key() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(config_home.join("groundhog")).unwrap();
    std::fs::write(
        config_home.join("groundhog").join("config.toml"),
        "default_key = \"dev-0\"\n",
    )
    .unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let key = format!("0x{}", "42".repeat(32));
    let manifest_path =
        write_runtime_manifest(tmp.path(), 0, &advertised_dev_wallet_address(0), None);
    let (rpc_url, rpc_server) = spawn_storage_rpc(&address, &key);
    std::fs::write(project.join("groundhog.toml"), "[profile.default]\n").unwrap();
    write_local_node_state(&project, &rpc_url, &manifest_path);

    let output = groundhog()
        .args(["--json", "storage", &address, &key])
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
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(stdout["data"]["block_height"], 12);
}

#[test]
fn storage_json_resolves_contract_alias() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_storage_rpc(&address, &key);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.contracts]\ncounter = \"{address}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "storage", "counter", &key])
        .current_dir(&project)
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
    assert_eq!(stdout["command"], "storage");
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["key"], key);
}

#[test]
fn storage_json_rejects_short_key_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        "[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["--json", "storage", &address, "0x1234"])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "storage");
    assert_eq!(stderr["error"]["code"], "invalid_storage_key");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exactly 32 bytes")
    );
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("32-byte storage key")
    );
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
fn call_dry_run_uses_manifest_dev_wallet_address_without_dev_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &advertised_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

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
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let signed_tx = stdout["data"]["signed_tx"]
        .as_str()
        .expect("bug: dry-run output includes signed tx");

    assert_eq!(signed_tx_invoked_address(signed_tx), advertised_wallet);
}

#[test]
fn call_live_human_appends_transaction_detail_sections() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_call_receipt(42),
        DevDetailRpcResponse::Result(sample_detail_result(42)),
    );
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
        .args(["call", &contract, "0x0123456789abcdef", "0x0102"])
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Call succeeded"));
    assert!(stderr.contains("Storage deltas (1):"));
    assert!(stderr.contains("Events (1):"));
    assert!(stderr.contains("Return: 0x0102"));
    assert!(stderr.contains("Gas: 12,450 / 1,000,000,000"));
}

#[test]
fn call_live_human_warns_when_transaction_detail_fails() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_call_receipt(42),
        DevDetailRpcResponse::Error {
            code: -32000,
            message: "detail backend unavailable",
        },
    );
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
        .args(["call", &contract, "0x0123456789abcdef", "0x0102"])
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Call succeeded"));
    assert!(stderr.contains("warning: transaction detail unavailable"));
    assert!(stderr.contains("detail backend unavailable"));
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

    stream.set_nonblocking(false).unwrap();
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

#[allow(clippy::needless_pass_by_value)]
fn send_jsonrpc_error_response(
    mut stream: std::net::TcpStream,
    request_id: &serde_json::Value,
    code: i32,
    message: &str,
) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id.clone(),
        "error": {
            "code": code,
            "message": message,
        },
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

fn spawn_storage_rpc(address: &str, key: &str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    let expected_address = address.to_owned();
    let expected_key = key.to_owned();

    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "getStorageAt");
        assert_eq!(request["method"], "groundhog_getStorageAt");
        assert_eq!(request["params"][0], expected_address);
        assert_eq!(request["params"][1], expected_key);

        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({
                "blockHeight": 12,
                "item": {
                    "kind": "bytestring",
                    "bytes": "0x6c6f63616c2d6e6f64652d73746f72616765",
                },
            }),
        );
    });

    (format!("http://{addr}"), handle)
}

fn spawn_view_rpc(
    address: &str,
    entrypoint: &str,
    calldata: &str,
) -> (String, thread::JoinHandle<()>) {
    spawn_view_rpc_inner(
        address,
        entrypoint,
        calldata,
        None,
        serde_json::json!({
            "result": "return",
            "returnData": "0x0102",
        }),
    )
}

fn spawn_view_rpc_with_caller(
    address: &str,
    entrypoint: &str,
    calldata: &str,
    caller: &str,
) -> (String, thread::JoinHandle<()>) {
    spawn_view_rpc_inner(
        address,
        entrypoint,
        calldata,
        Some(caller),
        serde_json::json!({
            "result": "return",
            "returnData": "0x0102",
        }),
    )
}

fn spawn_view_rpc_with_output(
    address: &str,
    entrypoint: &str,
    calldata: &str,
    output: serde_json::Value,
) -> (String, thread::JoinHandle<()>) {
    spawn_view_rpc_inner(address, entrypoint, calldata, None, output)
}

fn spawn_view_rpc_with_events(
    address: &str,
    entrypoint: &str,
    calldata: &str,
    events: serde_json::Value,
) -> (String, thread::JoinHandle<()>) {
    spawn_view_rpc_inner_with_events(
        address,
        entrypoint,
        calldata,
        None,
        serde_json::json!({
            "result": "return",
            "returnData": "0x",
        }),
        events,
    )
}

fn spawn_view_rpc_inner(
    address: &str,
    entrypoint: &str,
    calldata: &str,
    caller: Option<&str>,
    output: serde_json::Value,
) -> (String, thread::JoinHandle<()>) {
    spawn_view_rpc_inner_with_events(
        address,
        entrypoint,
        calldata,
        caller,
        output,
        serde_json::json!([]),
    )
}

fn spawn_view_rpc_inner_with_events(
    address: &str,
    entrypoint: &str,
    calldata: &str,
    caller: Option<&str>,
    output: serde_json::Value,
    events: serde_json::Value,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    let expected_address = address.to_owned();
    let expected_entrypoint = entrypoint.to_owned();
    let expected_calldata = calldata.to_owned();
    let expected_caller = caller.map(ToOwned::to_owned);

    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "view");
        assert_eq!(request["method"], "groundhog_view");
        assert_eq!(request["params"][0]["contract"], expected_address);
        assert_eq!(request["params"][0]["entrypoint"], expected_entrypoint);
        assert_eq!(request["params"][0]["calldata"], expected_calldata);
        match expected_caller {
            Some(caller) => assert_eq!(request["params"][0]["caller"], caller),
            None => assert!(request["params"][0].get("caller").is_none()),
        }

        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({
                "output": output,
                "events": events,
                "height": 12,
            }),
        );
    });

    (format!("http://{addr}"), handle)
}

fn advertised_dev_wallet_address(index: u8) -> String {
    let mut address = [0x77_u8; 32];
    address[31] = index;
    format!("0x{}", hex::encode(address))
}

fn write_runtime_manifest(
    dir: &std::path::Path,
    index: usize,
    address: &str,
    public_key_override: Option<[u8; 32]>,
) -> std::path::PathBuf {
    write_runtime_manifest_with_seed(
        dir,
        index,
        address,
        groundhog_signing::DEFAULT_DEV_ACCOUNT_SEED,
        public_key_override,
    )
}

fn write_runtime_manifest_with_seed(
    dir: &std::path::Path,
    index: usize,
    address: &str,
    seed: &str,
    public_key_override: Option<[u8; 32]>,
) -> std::path::PathBuf {
    let signer = groundhog_signing::dev_account::derive_dev_account_signer(seed, index);
    let public_key = public_key_override.unwrap_or_else(|| signer.verifying_key_bytes());
    let path = dir.join(format!("runtime-manifest-{index}.json"));
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "kind": "hogd-dev-runtime",
        "networkId": "groundhog-local",
        "protocol": "hogp/test",
        "genesisHash": format!("0x{}", "42".repeat(32)),
        "producer": {
            "binary": "hogd-dev",
            "version": "0.1.0"
        },
        "endpoints": {
            "scheduler": "127.0.0.1:1000"
        },
        "bootstrapAccountDerivation": {
            "scheme": "groundhog-bootstrap-account-v1",
            "seed": seed
        },
        "bootstrapAccounts": [{
            "index": index,
            "role": "local_user",
            "publicKey": format!("0x{}", hex::encode(public_key)),
            "walletAddress": address,
            "initialStorageTokens": groundhog_signing::DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS.to_string()
        }],
        "bootstrap": {
            "devWallet": {
                "contentHash": format!("0x{}", "51".repeat(32)),
                "deployedCodeHash": format!("0x{}", "52".repeat(32)),
                "artifactHint": "./dev-wallet.wasm",
                "deployEntrypoint": "0xadac498222148e4f",
                "doActionEntrypoint": "0x646f416374696f6e"
            },
            "storageCostModel": {
                "hash": format!("0x{}", "53".repeat(32)),
                "baseDataItemCost": "12000",
                "bytestringCostPerByte": "10",
                "baseContractDeploymentCost": "12000",
                "contractDeploymentCostPerByte": "10",
                "authPerKeyCost": "1111",
                "rcqCostPerEntry": "8",
                "replayCacheCostPerEntry": "7",
                "balanceCost": "1234"
            }
        }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    path
}

fn write_local_node_state(
    project: &std::path::Path,
    rpc_url: &str,
    manifest_path: &std::path::Path,
) {
    let state_dir = project.join(".groundhog");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(
        state_dir.join("local-node.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "rpcUrl": rpc_url,
            "runtimeManifest": manifest_path,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn signed_tx_invoked_address(tx_hex: &str) -> String {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex)).unwrap();
    let signed_tx: groundhog_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&bytes).unwrap();
    format!(
        "0x{}",
        hex::encode(signed_tx.tx.invocation.invoked_address.as_ref())
    )
}

enum DevDetailRpcResponse {
    MethodNotFound,
    Error { code: i32, message: &'static str },
    Result(serde_json::Value),
}

fn mock_tx_hash() -> String {
    format!("0x{}", "aa".repeat(32))
}

/// Mock RPC server that handles the full deploy flow:
/// `chainTip` → `sendRawTransaction` → `getTransactionReceipt`.
///
/// Returns a 32-byte dummy address (`0xbb..bb`) as the deploy `return_data` so the live
/// path exercises the receipt-based address extraction.
fn spawn_deploy_rpc(height: i64) -> (String, thread::JoinHandle<()>) {
    spawn_deploy_rpc_with_detail(height, DevDetailRpcResponse::MethodNotFound)
}

fn spawn_deploy_rpc_with_detail(
    height: i64,
    detail_response: DevDetailRpcResponse,
) -> (String, thread::JoinHandle<()>) {
    spawn_transaction_rpc(height, sample_deploy_receipt(height), detail_response)
}

fn spawn_transaction_rpc(
    height: i64,
    receipt: serde_json::Value,
    detail_response: DevDetailRpcResponse,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    let tx_hash = mock_tx_hash();

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
        assert_eq!(request["params"][0], tx_hash);
        send_jsonrpc_response(stream, &request["id"], receipt);

        // 4. groundhog_dev_getTransactionDetail
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "getTransactionDetail");
        assert_eq!(request["method"], "groundhog_dev_getTransactionDetail");
        assert_eq!(request["params"][0], tx_hash);
        match detail_response {
            DevDetailRpcResponse::MethodNotFound => {
                send_jsonrpc_error_response(stream, &request["id"], -32601, "Method not found");
            }
            DevDetailRpcResponse::Error { code, message } => {
                send_jsonrpc_error_response(stream, &request["id"], code, message);
            }
            DevDetailRpcResponse::Result(result) => {
                send_jsonrpc_response(stream, &request["id"], result);
            }
        }
    });

    (format!("http://{addr}"), handle)
}

fn sample_deploy_receipt(height: i64) -> serde_json::Value {
    serde_json::json!({
        "txHash": mock_tx_hash(),
        "height": height,
        "status": { "result": "ok" },
        "events": [],
        "returnData": format!("0x{}", "bb".repeat(32)),
    })
}

fn sample_call_receipt(height: i64) -> serde_json::Value {
    let contract_address = format!("0x{}", "11".repeat(32));
    serde_json::json!({
        "txHash": mock_tx_hash(),
        "height": height,
        "status": { "result": "ok" },
        "events": [{
            "contractAddress": contract_address,
            "topics": [format!("0x{}", "22".repeat(32))],
            "data": "0x1234",
        }],
        "returnData": "0x0102",
    })
}

fn sample_detail_result(height: i64) -> serde_json::Value {
    let contract_address = format!("0x{}", "11".repeat(32));
    serde_json::json!({
        "receipt": sample_call_receipt(height),
        "gasConsumed": 12450,
        "gasLimit": 1_000_000_000_u64,
        "storageDeltas": [{
            "address": contract_address,
            "key": "0xabcdef",
            "kind": "bytestringSet",
            "summary": "BytestringSet -> 0x9876",
            "flags": 0,
            "deltaXdr": "0x9876",
        }],
        "trace": {
            "invocation": {
                "address": contract_address,
                "entrypoint": "0x0123456789abcdef",
                "gasLimit": 1_000_000_000_u64,
                "calldata": "0x0102",
            },
            "systemCalls": [],
            "storageWrites": [],
            "status": {
                "result": "return",
                "buffer": "0x0102",
            },
        },
    })
}
