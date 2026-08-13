use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
#[cfg(unix)]
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use tempfile::TempDir;

mod support;

static CONFIG_HOME_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Write the `.cargo/contract.toml` overlay every contract build requires at
/// the cargo workspace root.
///
/// `groundhog build` passes it as `--config` for both the wasm and rv32
/// targets, so its absence refuses the build before cargo runs. Contents are
/// irrelevant here — only the fake cargo on `PATH` ever reads them.
fn write_contract_toml(workspace_root: &std::path::Path) {
    std::fs::create_dir_all(workspace_root.join(".cargo")).unwrap();
    std::fs::write(workspace_root.join(".cargo/contract.toml"), "").unwrap();
}

fn groundhog() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
    let config_home = std::env::temp_dir().join(format!(
        "groundhog-cli-test-config-{}-{}",
        std::process::id(),
        CONFIG_HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let cache_home = config_home.with_file_name(format!(
        "groundhog-cli-test-cache-{}-{}",
        std::process::id(),
        CONFIG_HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let groundhog_config = config_home.join("groundhog");
    std::fs::create_dir_all(&groundhog_config).expect("bug: test config directory should exist");
    std::fs::write(
        groundhog_config.join("config.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .expect("bug: test config should be writable");
    command.env_remove("GROUNDHOG_RUNTIME_MANIFEST");
    command.env_remove("GROUNDHOG_PROFILE");
    command.env_remove("GROUNDHOG_DEVNODE_BINARY");
    command.env_remove("HOGD_FFI_USER");
    command.env_remove("HOGD_FFI_PASSWORD");
    command.env("XDG_CONFIG_HOME", config_home);
    command.env("XDG_CACHE_HOME", cache_home);
    command
}

fn write_global_config(config_home: &std::path::Path, contents: &str) {
    let directory = config_home.join("groundhog");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("config.toml"), contents).unwrap();
}

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

#[cfg(unix)]
// Creates the minimum persistent on-disk state accepted by `groundhog chain dump`.
fn write_chain_data_fixture(root: &std::path::Path) {
    // The archive contract includes these database directories but excludes
    // transient runtime state such as endpoints and process identifiers.
    std::fs::create_dir_all(root.join("ramdb")).unwrap();
    std::fs::create_dir_all(root.join("events")).unwrap();
    std::fs::create_dir_all(root.join("indexer")).unwrap();
    std::fs::write(
        root.join("hog-devnode-data.json"),
        br#"{"schemaVersion":1,"kind":"hog-devnode-data"}"#,
    )
    .unwrap();
    std::fs::write(root.join("ramdb/0.log"), b"ramdb-state").unwrap();
    std::fs::write(root.join("events/block.events"), b"event-state").unwrap();
    std::fs::write(root.join("indexer/CURRENT"), b"MANIFEST-1\n").unwrap();
}

#[cfg(unix)]
#[test]
// Exercises the JSON CLI contract and the overwrite guard through the real binary.
fn chain_dump_and_restore_emit_structured_results_and_require_force() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    let archive = temp.path().join("chain.json");
    write_chain_data_fixture(&source);

    // Dump the fixture and verify both the machine-readable report and the
    // archive side effect exposed to callers.
    let dump = groundhog()
        .args(["--json", "chain", "dump", "--data-dir"])
        .arg(&source)
        .arg("--output")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(
        dump.status.success(),
        "{}",
        String::from_utf8_lossy(&dump.stderr)
    );
    let dump_json: serde_json::Value = serde_json::from_slice(&dump.stdout).unwrap();
    assert_eq!(dump_json["command"], "chain.dump");
    assert_eq!(dump_json["data"]["schemaVersion"], 1);
    assert_eq!(dump_json["data"]["fileCount"], 4);
    assert!(archive.is_file());

    // A populated destination must remain protected unless the caller opts in
    // to replacement with `--force`.
    write_chain_data_fixture(&destination);
    let refused = groundhog()
        .args(["--json", "chain", "restore", "--data-dir"])
        .arg(&destination)
        .arg("--input")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let refused_json: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(
        refused_json["error"]["code"],
        "chain_restore_requires_force"
    );

    // Make the destination observably different so the final assertion proves
    // that forced restore replaced its contents with the archived source state.
    std::fs::write(destination.join("ramdb/0.log"), b"different").unwrap();
    let restored = groundhog()
        .args(["--json", "chain", "restore", "--force", "--data-dir"])
        .arg(&destination)
        .arg("--input")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    // Successful restore reports through stdout and preserves the stable
    // command-specific schema version used by automation.
    let restore_json: serde_json::Value = serde_json::from_slice(&restored.stdout).unwrap();
    assert_eq!(restore_json["command"], "chain.restore");
    assert_eq!(restore_json["data"]["schemaVersion"], 1);
    assert_eq!(
        std::fs::read(destination.join("ramdb/0.log")).unwrap(),
        b"ramdb-state"
    );
}

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

#[cfg(unix)]
fn send_sigint(child: &std::process::Child) {
    let pid = i32::try_from(child.id()).expect("bug: child pid should fit i32");
    // SAFETY: sends SIGINT to the spawned CLI process id.
    let rc = unsafe { libc::kill(pid, libc::SIGINT) };
    assert_eq!(rc, 0, "failed to send SIGINT to child pid {pid}");
}

#[cfg(unix)]
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

#[cfg(unix)]
struct ChildGuard(Option<std::process::Child>);

#[cfg(unix)]
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

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(unix)]
fn read_pid_file(path: &std::path::Path) -> i32 {
    std::fs::read_to_string(path)
        .expect("bug: fake hogd-dev should write pid file")
        .trim()
        .parse()
        .expect("bug: fake hogd-dev pid should parse")
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
fn read_until_with_timeout<R>(
    output: R,
    needle: &'static str,
    timeout: Duration,
) -> std::io::Result<String>
where
    R: std::io::Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(output);
        let mut captured = String::new();
        let mut tx = Some(tx);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.is_some() {
                        captured.push_str(&line);
                    }
                    if captured.contains(needle)
                        && let Some(tx) = tx.take()
                    {
                        let _ = tx.send(Ok(captured.clone()));
                    }
                }
                Err(error) => {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(Err(error));
                    }
                    return;
                }
            }
        }
        if let Some(tx) = tx {
            let _ = tx.send(Ok(captured));
        }
    });
    rx.recv_timeout(timeout)
        .expect("timed out waiting for child stdout output")
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
    for removed_command in ["login", "logout"] {
        assert!(
            !stdout
                .lines()
                .any(|line| line.trim_start().starts_with(removed_command))
        );
    }
    assert!(stdout.contains("completion"));
}

#[test]
fn removed_auth_subcommands_are_rejected() {
    for removed_command in ["login", "logout"] {
        let output = groundhog().arg(removed_command).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
    }
}

#[test]
fn version_flag_works() {
    let output = groundhog().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected =
        option_env!("GROUNDHOG_DISTRIBUTION_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    assert_eq!(stdout, format!("groundhog {expected}\n"));
}

#[test]
fn config_help_describes_merged_inspection() {
    let output = groundhog()
        .args(["config", "get", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("merged from global and project configuration"));
    assert!(stdout.contains("--sources"));
    assert!(!stdout.contains("--global"));
    assert!(!stdout.contains("--project"));

    for removed_flag in ["--global", "--project"] {
        let rejected = groundhog()
            .args(["config", "get", removed_flag])
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("unexpected argument"));
    }
}

#[test]
fn config_parent_help_describes_management_and_scope_rules() {
    let output = groundhog().args(["config", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Inspect and manage persisted global and project profiles"));
    assert!(stdout.contains("pass --global or --project"));
    assert!(stdout.contains("omitted scope means global"));
    for command in ["get", "profiles", "new", "rm", "set", "unset", "use"] {
        assert!(
            stdout.contains(command),
            "missing config command `{command}`"
        );
    }
}

#[test]
fn config_mutation_help_describes_layer_specific_behavior() {
    let set = groundhog()
        .args(["config", "set", "--help"])
        .output()
        .unwrap();
    let unset = groundhog()
        .args(["config", "unset", "--help"])
        .output()
        .unwrap();
    let use_profile = groundhog()
        .args(["config", "use", "--help"])
        .output()
        .unwrap();

    assert!(set.status.success() && unset.status.success() && use_profile.status.success());
    assert!(String::from_utf8_lossy(&set.stdout).contains("must already exist in that layer"));
    assert!(String::from_utf8_lossy(&unset.stdout).contains("Missing fields"));
    assert!(String::from_utf8_lossy(&unset.stdout).contains("inherited global value"));
    assert!(String::from_utf8_lossy(&use_profile.stdout).contains("GROUNDHOG_PROFILE"));
    assert!(String::from_utf8_lossy(&use_profile.stdout).contains("either layer"));
}

#[test]
fn config_new_help_describes_atomic_creation_and_scope_rules() {
    let output = groundhog()
        .args(["config", "new", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("one atomic config update"));
    assert!(stdout.contains("omitted scope means global"));
    assert!(stdout.contains("choose --global or --project explicitly"));
    assert!(stdout.contains("cannot be used together"));
    assert!(stdout.contains("--network <NETWORK>"));
    assert!(stdout.contains("--use"));
}

#[test]
fn config_rm_help_describes_confirmation_and_deleted_contents() {
    let output = groundhog()
        .args(["config", "rm", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("contract aliases and wallet bindings"));
    assert!(stdout.contains("asks for confirmation"));
    assert!(stdout.contains("default profile selection is cleared"));
    assert!(stdout.contains("--global"));
    assert!(stdout.contains("--project"));
}

#[test]
fn config_get_human_output_shows_merged_values_and_optional_sources() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    write_global_config(
        &config_home,
        r#"default_profile = "devnet"

[profile.devnet]
network = "devnet"
rpc_url = "https://global-rpc.example"
runtime_manifest = "runtime/manifest.json"

[profile.devnet.contracts]
HOG = "0x01"
"#,
    );
    std::fs::write(
        project.join("groundhog.toml"),
        r#"default_profile = "devnet"

[profile.devnet]
rpc_url = "https://project-rpc.example"
faucet_url = "https://faucet.example"
runtime_artifacts_dir = "artifacts"

[profile.devnet.contracts]
counter = "0x02"
"#,
    )
    .unwrap();

    let merged = groundhog()
        .args(["config", "get", "--sources"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(merged.status.success());
    assert!(merged.stderr.is_empty());
    let merged_stdout = String::from_utf8_lossy(&merged.stdout);
    assert!(!merged_stdout.contains("Selected by:"));
    assert!(!merged_stdout.contains("Scope:"));
    assert!(!merged_stdout.contains("runtime_manifest"));
    assert!(!merged_stdout.contains("runtime_artifacts_dir"));
    assert!(!merged_stdout.contains('\u{1b}'));
    assert!(merged_stdout.contains("rpc_url"));
    assert!(merged_stdout.contains("https://project-rpc.example"));
    assert!(merged_stdout.contains("project"));
    assert!(merged_stdout.contains("contracts.HOG"));
    assert!(merged_stdout.contains("global"));

    let plain = groundhog()
        .args(["config", "get", "devnet"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(plain.status.success());
    let plain_stdout = String::from_utf8_lossy(&plain.stdout);
    assert!(plain_stdout.contains("Network           devnet"));
    assert!(plain_stdout.contains("https://project-rpc.example"));
    assert!(plain_stdout.contains("https://faucet.example"));
    assert!(!plain_stdout.contains("Selected by:"));
    assert!(!plain_stdout.contains("Scope:"));
    assert!(!plain_stdout.contains("Runtime manifest:"));
    assert!(!plain_stdout.contains("Runtime artifacts:"));
}

#[test]
fn config_get_json_is_versioned_merged_and_secret_free() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    write_global_config(
        &config_home,
        r#"default_profile = "devnet"
default_key = "top-level-secret-selection"

[executor]
username = "private-user"
password = "private-password"

[local_runtime_state]
node_id = "private-node"
rpc_url = "http://127.0.0.1:9545"
runtime_manifest = "/private/runtime.json"

[profile.devnet]
network = "devnet"
runtime_manifest = "runtime/manifest.json"

[profile.devnet.wallets.primary]
address = "0x01"
owner_public_key = "0x02"

[profile.broken]
network = "preview"
"#,
    );
    std::fs::write(
        project.join("groundhog.toml"),
        r#"default_profile = "devnet"

[profile.devnet]
rpc_url = "https://rpc.example"
"#,
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "config", "get", "devnet", "--sources"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("GROUNDHOG_PROFILE", "broken")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(payload["command"], "config.get");
    assert_eq!(payload["data"]["schemaVersion"], 1);
    assert_eq!(payload["data"]["selectionSource"]["kind"], "explicit");
    assert_eq!(payload["data"]["scope"], "resolved");
    assert_eq!(payload["data"]["rpcUrl"], "https://rpc.example");
    assert_eq!(payload["data"]["sources"]["rpcUrl"]["kind"], "project");
    assert_eq!(
        payload["data"]["sources"]["contracts"],
        serde_json::json!({})
    );
    assert_eq!(
        payload["data"]["wallets"]["primary"]["ownerPublicKey"],
        "0x02"
    );
    assert_eq!(
        payload["data"]["runtimeManifest"],
        config_home
            .join("groundhog/runtime/manifest.json")
            .display()
            .to_string()
    );
    assert!(!stdout.contains("private-user"));
    assert!(!stdout.contains("private-password"));
    assert!(!stdout.contains("top-level-secret-selection"));
    assert!(!stdout.contains("private-node"));
}

#[test]
fn config_profiles_json_lists_invalid_and_active_missing_profiles() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        r#"[profile.valid]
network = "devnet"

[profile.broken]
network = "preview"
"#,
    );

    let output = groundhog()
        .args(["--json", "config", "profiles"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("GROUNDHOG_PROFILE", "missing")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["command"], "config.profiles");
    assert_eq!(payload["data"]["schemaVersion"], 1);
    assert_eq!(payload["data"]["activeProfile"], "missing");
    assert_eq!(payload["data"]["selectionSource"]["kind"], "environment");
    let profiles = payload["data"]["profiles"].as_array().unwrap();
    assert_eq!(
        profiles
            .iter()
            .map(|row| row["profile"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["broken", "missing", "valid"]
    );
    assert_eq!(profiles[0]["status"], "invalid");
    assert!(
        profiles[0]["invalidReason"]
            .as_str()
            .unwrap()
            .contains("global config")
    );
    assert_eq!(profiles[1]["status"], "missing");
    assert_eq!(profiles[1]["active"], true);
    assert_eq!(profiles[2]["status"], "valid");
}

#[test]
fn config_profiles_human_output_marks_selection_and_invalid_reasons() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        r#"default_profile = "valid"

[profile.valid]
network = "devnet"

[profile.broken]
network = "preview"
"#,
    );

    let output = groundhog()
        .args(["config", "profiles"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains('\u{1b}'));
    assert_eq!(stdout.lines().next(), Some("Profiles"));
    assert!(stdout.contains("    PROFILE  NETWORK"));
    assert!(stdout.contains("  * valid"));
    assert!(stdout.contains("  ! broken"));
    assert!(!stdout.contains("Active profile:"));
    assert!(stdout.contains("broken"));
    assert!(stdout.contains("invalid"));
    assert!(stdout.contains("expected one of `local`, `devnet`, `mainnet`"));
    assert!(stdout.contains("valid"));
    assert!(stdout.contains("devnet"));
}

#[test]
fn config_profiles_succeeds_without_an_active_profile() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(&config_home, "[profile.devnet]\nnetwork = \"devnet\"\n");

    let output = groundhog()
        .args(["--json", "config", "profiles"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(payload["data"]["activeProfile"].is_null());
    assert!(payload["data"].get("selectionSource").is_none());
    assert_eq!(payload["data"]["profiles"][0]["profile"], "devnet");
}

#[test]
fn config_get_human_output_escapes_terminal_controls() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        r#"default_profile = "devnet"

[profile.devnet]
network = "devnet"
default_key = "alice\nforged\u001b\u202e"

[profile.devnet.contracts]
"alias\nrow" = "value\u001b"
"#,
    );

    let output = groundhog()
        .args(["config", "get"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(r"alice\nforged\u{1b}\u{202e}"));
    assert!(stdout.contains(r"alias\nrow"));
    assert!(stdout.contains(r"value\u{1b}"));
    assert!(!stdout.contains('\u{1b}'));
    assert!(!stdout.contains('\u{202e}'));
}

#[test]
fn config_human_errors_escape_dynamic_values() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config\nhome");
    write_global_config(&config_home, "default_profile = [\n");

    let malformed = groundhog()
        .args(["config", "profiles"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!malformed.status.success());
    let malformed_stderr = String::from_utf8(malformed.stderr).unwrap();
    assert!(malformed_stderr.contains(r"config\nhome"));
    assert!(!malformed_stderr.contains("config\nhome"));

    let safe_config_home = tmp.path().join("safe-config-home");
    write_global_config(
        &safe_config_home,
        "default_profile = \"devnet\"\n\n[profile.devnet]\nnetwork = \"devnet\"\n",
    );
    let missing = groundhog()
        .args(["config", "get", "missing\u{202e}"])
        .env("XDG_CONFIG_HOME", &safe_config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let missing_stderr = String::from_utf8(missing.stderr).unwrap();
    assert!(missing_stderr.contains(r"missing\u{202e}"));
    assert!(!missing_stderr.contains('\u{202e}'));
}

#[test]
fn config_get_sources_json_keeps_empty_map_groups() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "default_profile = \"devnet\"\n\n[profile.devnet]\nnetwork = \"devnet\"\n",
    );

    let output = groundhog()
        .args(["--json", "config", "get", "--sources"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        payload["data"]["sources"]["contracts"],
        serde_json::json!({})
    );
    assert_eq!(payload["data"]["sources"]["wallets"], serde_json::json!({}));
}

#[test]
fn config_get_reports_stable_selection_and_parse_errors() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(&config_home, "[profile.devnet]\nnetwork = \"devnet\"\n");
    let no_active = groundhog()
        .args(["--json", "config", "get"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let no_active_error: serde_json::Value = serde_json::from_slice(&no_active.stderr).unwrap();
    assert_eq!(no_active_error["error"]["code"], "no_active_profile");

    let missing = groundhog()
        .args(["--json", "config", "get", "missing"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let missing_error: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(missing_error["error"]["code"], "profile_not_found");

    write_global_config(&config_home, "[executor]\npassword = \"super-secret\n");
    let malformed = groundhog()
        .args(["--json", "config", "profiles"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let parse_error: serde_json::Value = serde_json::from_slice(&malformed.stderr).unwrap();
    assert_eq!(parse_error["command"], "config.profiles");
    assert_eq!(parse_error["error"]["code"], "config_parse_failed");
    assert!(parse_error["error"]["details"]["line"].is_number());
    assert!(parse_error["error"]["details"]["column"].is_number());
    let malformed_stderr = String::from_utf8(malformed.stderr).unwrap();
    assert!(!malformed_stderr.contains("super-secret"));

    let malformed_human = groundhog()
        .args(["config", "profiles"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(
        !String::from_utf8(malformed_human.stderr)
            .unwrap()
            .contains("super-secret")
    );
}

#[test]
fn config_new_json_creates_complete_profile_then_set_reports_noop() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "# retained\nunknown = true\n\n[executor]\nusername = \"private-user\"\npassword = \"private-password\"\n\n[local_runtime_state]\nnode_id = \"private-node\"\nrpc_url = \"http://127.0.0.1:8545\"\nruntime_manifest = \"/private/runtime.json\"\n",
    );

    let first = groundhog()
        .args([
            "--json",
            "config",
            "new",
            "ci",
            "--network",
            "devnet",
            "--rpc-url",
            "https://rpc.example",
            "--faucet-url",
            "https://faucet.example",
            "--default-key",
            "alice",
            "--use",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("GROUNDHOG_PROFILE", "local")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(first.status.success());
    assert!(first.stderr.is_empty());
    let first_payload: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_payload["command"], "config.new");
    assert_eq!(
        first_payload["data"],
        serde_json::json!({
            "schemaVersion": 1,
            "profile": "ci",
            "scope": "global",
            "path": config_home.join("groundhog/config.toml"),
            "changed": true,
            "createdFields": ["default-key", "faucet-url", "network", "rpc-url"],
            "selected": true,
        })
    );
    assert_eq!(
        first_payload["warnings"][0]["code"],
        "default_key_not_found"
    );
    assert_eq!(
        first_payload["warnings"][1]["code"],
        "environment_profile_override"
    );
    let first_stdout = String::from_utf8(first.stdout).unwrap();
    assert!(!first_stdout.contains("private-user"));
    assert!(!first_stdout.contains("private-password"));
    assert!(!first_stdout.contains("private-node"));

    let second = groundhog()
        .args([
            "--json",
            "config",
            "set",
            "network",
            "devnet",
            "--profile",
            "ci",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(second.status.success());
    let second_payload: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_payload["data"]["changed"], false);

    let contents = std::fs::read_to_string(config_home.join("groundhog/config.toml")).unwrap();
    assert!(contents.starts_with("# retained\nunknown = true\n"));
    assert!(contents.contains("default_profile = \"ci\""));
    assert!(contents.contains(
        "[profile.ci]\nnetwork = \"devnet\"\nrpc_url = \"https://rpc.example\"\nfaucet_url = \"https://faucet.example\"\ndefault_key = \"alice\""
    ));
    assert!(contents.contains("password = \"private-password\""));
    assert!(contents.contains("node_id = \"private-node\""));
    assert!(contents.contains("runtime_manifest = \"/private/runtime.json\""));
}

#[test]
fn config_new_human_output_identifies_destination_fields_and_selection() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(&config_home, "# retained\n");

    let output = groundhog()
        .args([
            "config",
            "new",
            "staging",
            "--network",
            "devnet",
            "--rpc-url",
            "https://rpc.example",
            "--use",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Created profile `staging`"));
    assert!(stderr.contains("scope: global"));
    assert!(stderr.contains("config.toml"));
    assert!(stderr.contains("fields: network, rpc-url"));
    assert!(stderr.contains("default: selected"));
}

#[test]
fn config_new_rejects_duplicate_and_invalid_profiles() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(&config_home, "[profile.devnet]\nnetwork = \"devnet\"\n");

    let duplicate = groundhog()
        .args(["--json", "config", "new", "devnet", "--network", "devnet"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    let duplicate_error: serde_json::Value = serde_json::from_slice(&duplicate.stderr).unwrap();
    assert_eq!(duplicate_error["command"], "config.new");
    assert_eq!(duplicate_error["error"]["code"], "profile_already_exists");
    assert_eq!(
        duplicate_error["error"]["message"],
        "profile `devnet` already exists in global config"
    );
    assert!(
        duplicate_error["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("groundhog config set")
    );
    assert_eq!(duplicate_error["error"]["details"]["profile"], "devnet");
    assert_eq!(duplicate_error["error"]["details"]["scope"], "global");

    let invalid = groundhog()
        .args(["--json", "config", "new", "staging", "--network", "preview"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let invalid_error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(invalid_error["error"]["code"], "config_value_invalid");
    assert_eq!(invalid_error["error"]["details"]["field"], "network");
    assert_eq!(
        invalid_error["error"]["details"]["message"],
        "must be one of local, devnet, or mainnet"
    );
    assert!(
        !std::fs::read_to_string(config_home.join("groundhog/config.toml"))
            .unwrap()
            .contains("staging")
    );
}

#[test]
fn config_rm_cancellation_preserves_profile() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let contents = "default_profile = \"staging\"\n\n[profile.staging]\nnetwork = \"devnet\"\n";
    write_global_config(&config_home, contents);
    let mut child = groundhog()
        .args(["config", "rm", "staging"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"n\n").unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Remove profile `staging` from global config?"));
    assert!(stderr.contains("Cancelled"));
    assert_eq!(
        std::fs::read_to_string(config_home.join("groundhog/config.toml")).unwrap(),
        contents
    );
}

#[test]
fn config_rm_confirmation_removes_all_profile_data_and_clears_default() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "default_profile = \"staging\"\nunknown = true\n\n[profile.staging]\nnetwork = \"devnet\"\n\n[profile.staging.contracts]\ncounter = \"0x1234\"\n",
    );
    let mut child = groundhog()
        .args(["config", "rm", "staging"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"y\n").unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Removed profile `staging`"));
    assert!(stderr.contains("Default profile selection cleared"));
    assert_eq!(
        std::fs::read_to_string(config_home.join("groundhog/config.toml")).unwrap(),
        "unknown = true\n"
    );
}

#[test]
fn config_rm_json_is_non_interactive_and_reports_stable_result() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "default_profile = \"staging\"\n\n[profile.staging]\nnetwork = \"devnet\"\n",
    );

    let output = groundhog()
        .args(["--json", "config", "rm", "staging"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["command"], "config.rm");
    assert_eq!(
        payload["data"],
        serde_json::json!({
            "schemaVersion": 1,
            "profile": "staging",
            "scope": "global",
            "path": config_home.join("groundhog/config.toml"),
            "changed": true,
            "clearedDefault": true,
        })
    );
}

#[test]
fn config_rm_quiet_is_non_interactive() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(&config_home, "[profile.staging]\nnetwork = \"devnet\"\n");

    let output = groundhog()
        .args(["--quiet", "config", "rm", "staging"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(
        std::fs::read_to_string(config_home.join("groundhog/config.toml")).unwrap(),
        ""
    );
}

#[test]
fn config_mutations_report_scope_and_value_errors() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    write_global_config(&config_home, "[profile.devnet]\nnetwork = \"devnet\"\n");
    std::fs::write(project.join("groundhog.toml"), "# project\n").unwrap();

    let ambiguous = groundhog()
        .args([
            "--json",
            "config",
            "set",
            "network",
            "devnet",
            "--profile",
            "devnet",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());
    let ambiguous_error: serde_json::Value = serde_json::from_slice(&ambiguous.stderr).unwrap();
    assert_eq!(ambiguous_error["error"]["code"], "config_scope_ambiguous");

    let conflicting = groundhog()
        .args([
            "--json",
            "config",
            "unset",
            "network",
            "--profile",
            "devnet",
            "--global",
            "--project",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(!conflicting.status.success());
    let conflict_error: serde_json::Value = serde_json::from_slice(&conflicting.stderr).unwrap();
    assert_eq!(conflict_error["error"]["code"], "config_scope_conflict");

    let invalid = groundhog()
        .args([
            "--json",
            "config",
            "set",
            "rpc-url",
            "relative/path",
            "--profile",
            "devnet",
            "--global",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let invalid_error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(invalid_error["error"]["code"], "config_value_invalid");

    let missing = groundhog()
        .args([
            "--json",
            "config",
            "set",
            "network",
            "devnet",
            "--profile",
            "missing",
            "--project",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let missing_error: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(missing_error["error"]["code"], "profile_not_found_in_scope");
    assert_eq!(missing_error["error"]["details"]["scope"], "project");

    let unavailable = groundhog()
        .args([
            "--json",
            "config",
            "unset",
            "network",
            "--profile",
            "devnet",
            "--project",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!unavailable.status.success());
    let unavailable_error: serde_json::Value = serde_json::from_slice(&unavailable.stderr).unwrap();
    assert_eq!(
        unavailable_error["error"]["code"],
        "project_config_unavailable"
    );
}

#[test]
fn config_mutation_reports_stable_malformed_toml_error() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(&config_home, "[profile.devnet]\nsecret = \"not-closed\n");

    let output = groundhog()
        .args([
            "--json",
            "config",
            "set",
            "network",
            "devnet",
            "--profile",
            "devnet",
            "--global",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(payload["command"], "config.set");
    assert_eq!(payload["error"]["code"], "config_parse_failed");
    assert!(payload["error"]["details"]["line"].is_number());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("not-closed")
    );
}

#[test]
fn config_unset_project_override_reports_inherited_global_value() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    write_global_config(
        &config_home,
        "[profile.devnet]\nrpc_url = \"https://global.example\"\n",
    );
    std::fs::write(
        project.join("groundhog.toml"),
        "[profile.devnet]\nrpc_url = \"https://project.example\"\nunknown = true\n",
    )
    .unwrap();

    let output = groundhog()
        .args([
            "config",
            "unset",
            "rpc-url",
            "--profile",
            "devnet",
            "--project",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Unset `rpc-url` in profile `devnet`"));
    assert!(stderr.contains("inherits `rpc-url` = `https://global.example`"));
    assert!(stderr.contains(project.join("groundhog.toml").to_string_lossy().as_ref()));
    let contents = std::fs::read_to_string(project.join("groundhog.toml")).unwrap();
    assert!(!contents.contains("rpc_url"));
    assert!(contents.contains("unknown = true"));
}

#[test]
fn config_use_warns_when_environment_profile_remains_active() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "default_profile = \"local\"\n\n[profile.local]\nnetwork = \"local\"\n\n[profile.devnet]\nnetwork = \"devnet\"\n",
    );

    let output = groundhog()
        .args(["--json", "config", "use", "devnet", "--global"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("GROUNDHOG_PROFILE", "local")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["command"], "config.use");
    assert_eq!(payload["data"]["profile"], "devnet");
    assert!(payload["data"].get("field").is_none());
    assert_eq!(
        payload["warnings"][0]["code"],
        "environment_profile_override"
    );
    let contents = std::fs::read_to_string(config_home.join("groundhog/config.toml")).unwrap();
    assert!(contents.starts_with("default_profile = \"devnet\""));
    assert!(contents.contains("[profile.local]"));
    assert!(contents.contains("[profile.devnet]"));
}

#[cfg(unix)]
#[test]
fn node_hogd_dev_binary_json_starts_supervisor() {
    let fake = fake_hogd_dev_binary();
    let pid_dir = TempDir::new().unwrap();
    let data_root = TempDir::new().unwrap();
    let data_dir = data_root.path().join("node-data");
    let config_home = TempDir::new().unwrap();
    let pid_path = pid_dir.path().join("hogd.pid");
    let child = groundhog()
        .arg("--json")
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-binary")
        .arg(fake)
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--dev-account-hog")
        .arg("25.0000001")
        .env("GROUNDHOG_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("GROUNDHOG_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .env("XDG_CONFIG_HOME", config_home.path())
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
    assert_eq!(payload["data"]["schemaVersion"], 3);
    assert_eq!(payload["data"]["chain_state"], "fresh");
    assert_eq!(payload["data"]["data_dir"], data_dir.display().to_string());
    assert_eq!(payload["data"]["experimental"], false);
    assert_eq!(payload["data"]["backend"]["kind"], "hogd_dev");
    assert_eq!(payload["data"]["backend"]["stability"], "supported");
    assert!(payload["data"]["backend"].get("limitations").is_none());
    assert_eq!(payload["data"]["backend"]["planes"]["submit"], "scheduler");
    assert_eq!(payload["data"]["backend"]["planes"]["chain"], "scheduler");
    assert_eq!(payload["data"]["backend"]["planes"]["read"], "indexer");
    assert_eq!(payload["data"]["backend"]["planes"]["view"], "stub");
    assert_eq!(payload["data"]["local_runtime_state"]["ok"], true);
    assert!(
        payload["data"]["local_runtime_state"]["path"]
            .as_str()
            .expect("bug: local_runtime_state path should be a string")
            .ends_with("groundhog/config.toml")
    );
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
    let manifest_path = payload["data"]["runtime_manifest"]["path"]
        .as_str()
        .expect("bug: runtime manifest path should be a string");
    assert_split_bootstrap_accounts(std::path::Path::new(manifest_path), "250000001");

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
    let global_config = std::fs::read_to_string(config_home.path().join("groundhog/config.toml"))
        .expect("groundhog node should leave config readable");
    assert!(!global_config.contains("local_runtime_state"));
    assert!(
        wait_pid_gone(pid, Duration::from_secs(2)),
        "fake hogd-dev pid {pid} still alive after CLI exit",
    );
}

#[cfg(unix)]
#[test]
fn node_human_startup_reports_chain_state() {
    let fake = fake_hogd_dev_binary();
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("node-data");
    let temp_root = temp.path().join("tmp & root");
    std::fs::create_dir(&temp_root).unwrap();
    let pid_path = temp.path().join("hogd.pid");
    let config_home = temp.path().join("config");
    let child = groundhog()
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-binary")
        .arg(fake)
        .arg("--data-dir")
        .arg(&data_dir)
        .env("GROUNDHOG_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("GROUNDHOG_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("TMPDIR", &temp_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bug: groundhog node should spawn");
    let mut child = ChildGuard::new(child);

    let stderr = child
        .child_mut()
        .stderr
        .take()
        .expect("bug: stderr should be piped");
    let output = read_until_with_timeout(stderr, "dev-key manifest", Duration::from_secs(10))
        .expect("bug: human startup output should be readable");

    assert!(output.contains("chain state"), "startup output: {output}");
    assert!(output.contains("fresh"), "startup output: {output}");
    assert!(
        output.contains("export GROUNDHOG_RUNTIME_MANIFEST='") && output.contains("tmp & root"),
        "startup output: {output}"
    );

    let pid = read_pid_file(&pid_path);
    send_sigint(child.child());
    let status = wait_child(child.into_inner(), Duration::from_secs(10));
    assert!(status.success(), "groundhog node should stop cleanly");
    assert!(
        wait_pid_gone(pid, Duration::from_secs(2)),
        "fake hogd-dev pid {pid} still alive after CLI exit",
    );
}

#[cfg(unix)]
#[test]
fn node_hogd_dev_profile_write_failure_stops_supervisor() {
    let fake = fake_hogd_dev_binary();
    let pid_dir = TempDir::new().unwrap();
    let config_home = TempDir::new().unwrap();
    let pid_path = pid_dir.path().join("hogd.pid");
    let invalid_config_home = config_home.path().join("not-a-directory");
    std::fs::write(&invalid_config_home, b"not a directory").unwrap();

    let output = groundhog()
        .arg("--json")
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-binary")
        .arg(fake)
        .env("GROUNDHOG_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("GROUNDHOG_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .env("XDG_CONFIG_HOME", &invalid_config_home)
        .output()
        .expect("bug: groundhog node should run");

    assert!(
        !output.status.success(),
        "profile write failure should make groundhog node fail"
    );
    let pid = read_pid_file(&pid_path);
    assert!(
        wait_pid_gone(pid, Duration::from_secs(2)),
        "fake hogd-dev pid {pid} still alive after profile write failure"
    );
}

#[test]
fn node_hogd_dev_missing_binary_fails_without_native_fallback() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("missing-hog-devnode");
    let output = groundhog()
        .arg("--json")
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-binary")
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
            .contains("configured hog-devnode binary path does not exist"),
    );
}

#[test]
fn new_creates_project() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("work & space");
    std::fs::create_dir(&workspace).unwrap();
    // The CLI derives the project path from `env::current_dir()`, which
    // resolves symlinks (macOS temp dirs live under `/var` -> `/private/var`),
    // so compare against the canonicalized workspace path.
    let workspace = workspace.canonicalize().unwrap();
    let output = groundhog()
        .arg("new")
        .arg("my-test-project")
        .current_dir(&workspace)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dir = workspace.join("my-test-project");
    assert!(dir.join("Cargo.toml").exists());
    assert!(dir.join("groundhog.toml").exists());
    assert!(dir.join("build.rs").exists());
    assert!(dir.join("src/lib.rs").exists());
    assert!(dir.join("src/implementation.rs").exists());
    assert!(dir.join("tests/integration.rs").exists());
    assert!(dir.join("tests/test.rs").exists());
    assert!(dir.join("tests/test_wasm.rs").exists());
    assert!(dir.join(".cargo/config.toml").exists());
    assert!(dir.join(".cargo/contract.toml").exists());
    assert!(dir.join("rust-toolchain.toml").exists());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Creating project `my-test-project`"));
    assert!(stderr.contains("[ok] Created project `my-test-project`"));
    assert!(stderr.contains("Project Ready"));
    assert!(stderr.contains("Project: my-test-project"));
    assert!(stderr.contains("Template: counter"));
    assert!(stderr.contains("Config: "));
    assert!(stderr.contains("Next:"));
    assert!(stderr.contains(&format!("cd '{}'", dir.display())));
    assert!(!stderr.contains("Cargo Registry:"));
    assert!(!stderr.contains("cargo login --registry groundhog"));
    assert!(stderr.contains("groundhog build"));
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

#[cfg(unix)]
fn write_executable(path: &std::path::Path, script: &str) {
    std::fs::write(path, script).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

/// How a stub `cargo` reacts to an uninstalled `RUSTUP_TOOLCHAIN` override.
///
/// This is the behavior the scaffold probe keys on, so the stubs model it
/// directly rather than modelling whether a `rustup` binary happens to exist.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum StubCargo {
    /// Rustup's shim: refuses an override it cannot resolve.
    RustupShim,
    /// Distro-packaged cargo: ignores `RUSTUP_TOOLCHAIN` entirely.
    IgnoresRustup,
}

/// Build a `PATH` containing only `directory`, holding a stub `cargo` of the
/// given kind and optionally a stub `rustup`.
///
/// Isolating `PATH` is what makes the probe deterministic rather than
/// dependent on how the host that runs the suite is provisioned.
#[cfg(unix)]
fn path_with_stub_cargo(
    directory: &std::path::Path,
    cargo: StubCargo,
    rustup_present: bool,
) -> std::ffi::OsString {
    let script = match cargo {
        StubCargo::RustupShim => {
            "#!/bin/sh\n\
             if [ -n \"$RUSTUP_TOOLCHAIN\" ]; then\n\
             \x20 echo \"error: override toolchain '$RUSTUP_TOOLCHAIN' is not installed\" >&2\n\
             \x20 exit 1\n\
             fi\n\
             echo 'cargo 1.96.0 (stub)'\n"
        }
        StubCargo::IgnoresRustup => "#!/bin/sh\necho 'cargo 1.75.0 (distro stub)'\n",
    };
    write_executable(&directory.join("cargo"), script);
    if rustup_present {
        write_executable(
            &directory.join("rustup"),
            "#!/bin/sh\necho 'rustup 1.99.0'\n",
        );
    }
    directory.as_os_str().to_owned()
}

/// Scaffold into an isolated `PATH` and return the parsed `--json` envelope.
#[cfg(unix)]
fn scaffold_with_stub_cargo(
    project: &str,
    cargo: StubCargo,
    rustup_present: bool,
) -> (TempDir, serde_json::Value) {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();

    let output = groundhog()
        .arg("--json")
        .arg("new")
        .arg(project)
        .env("PATH", path_with_stub_cargo(&bin, cargo, rustup_present))
        .current_dir(&workspace)
        .output()
        .unwrap();

    // Scaffolding still succeeds: the warning is advisory, not fatal.
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        workspace.join(project).join("rust-toolchain.toml").exists(),
        "scaffold should be written regardless of toolchain warnings"
    );
    let stdout = serde_json::from_slice(&output.stdout).unwrap();
    (tmp, stdout)
}

#[cfg(unix)]
#[test]
fn new_warns_when_cargo_ignores_rustup() {
    let (_tmp, stdout) =
        scaffold_with_stub_cargo("distro-cargo-project", StubCargo::IgnoresRustup, false);

    assert_eq!(stdout["ok"], true);
    let warnings = stdout["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
    assert_eq!(warnings[0]["code"], "cargo_not_rustup_managed");
    assert!(
        warnings[0]["message"]
            .as_str()
            .unwrap()
            .contains("riscv32im-unknown-none-elf"),
        "message should name the rv32 target: {}",
        warnings[0]["message"]
    );
    assert!(
        warnings[0]["hint"].as_str().unwrap().contains("rustup.rs"),
        "hint should point at the installer: {}",
        warnings[0]["hint"]
    );
}

/// A distro-packaged `cargo` ignores `rust-toolchain.toml` even when rustup is
/// installed alongside it, so presence of `rustup` must not silence the
/// warning.
#[cfg(unix)]
#[test]
fn new_warns_when_distro_cargo_shadows_installed_rustup() {
    let (_tmp, stdout) =
        scaffold_with_stub_cargo("shadowed-rustup-project", StubCargo::IgnoresRustup, true);

    let warnings = stdout["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
    assert_eq!(warnings[0]["code"], "cargo_not_rustup_managed");
    assert!(
        warnings[0]["hint"].as_str().unwrap().contains("PATH"),
        "hint should explain the shadowing fix: {}",
        warnings[0]["hint"]
    );
}

#[cfg(unix)]
#[test]
fn new_omits_warning_when_cargo_is_rustup_managed() {
    let (_tmp, stdout) =
        scaffold_with_stub_cargo("rustup-cargo-project", StubCargo::RustupShim, true);

    // A missing target or toolchain is self-healing under rustup, so a
    // rustup-managed cargo means no scaffold warnings at all.
    assert!(
        stdout["warnings"].as_array().unwrap().is_empty(),
        "warnings: {}",
        stdout["warnings"]
    );
}

#[cfg(unix)]
#[test]
fn new_warns_on_stderr_when_cargo_ignores_rustup() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();

    let output = groundhog()
        .arg("new")
        .arg("distro-cargo-human")
        .env(
            "PATH",
            path_with_stub_cargo(&bin, StubCargo::IgnoresRustup, false),
        )
        .current_dir(&workspace)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: cargo_not_rustup_managed:"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("hint: "), "stderr: {stderr}");
    // The project is still usable output, so the summary card must survive.
    assert!(stderr.contains("Project Ready"), "stderr: {stderr}");
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
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env_remove("HOGD_FFI_PASSWORD")
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
fn init_human_output_shows_setup_sections() {
    let tmp = TempDir::new().unwrap();
    let mut child = groundhog()
        .arg("init")
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env_remove("HOGD_FFI_PASSWORD")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"n\nn\nn\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Groundhog Setup"));
    assert!(stderr.contains("Set up your local development environment."));
    assert!(stderr.contains("[1/5] Environment"));
    assert!(stderr.contains("[2/5] Profiles"));
    assert!(stderr.contains("[3/5] Signing key"));
    assert!(stderr.contains("[4/5] Mock-executor download credentials"));
    assert!(stderr.contains("[5/5] Project"));
    assert!(stderr.contains("Setup Complete"));
    assert!(stderr.contains("Config:"));
    assert!(stderr.contains("Profile:"));
    assert!(stderr.contains("Local RPC:"));
    assert!(stderr.contains("Next:"));
    assert!(stderr.contains("groundhog new my-contract"));
}

#[test]
fn init_human_output_points_to_created_project() {
    let tmp = TempDir::new().unwrap();
    let mut child = groundhog()
        .arg("init")
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env_remove("HOGD_FFI_PASSWORD")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"y\nn\ny\ndemo-contract\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Setup Complete"));
    assert!(stderr.contains("Key: default (0x"));
    assert!(stderr.contains("Project:"));
    assert!(stderr.contains("demo-contract"));
    assert!(stderr.contains("Next:"));
    assert!(stderr.contains("cd demo-contract"));
    assert!(stderr.contains("groundhog build"));
    assert!(!stderr.contains("groundhog new my-contract"));
    assert!(tmp.path().join("demo-contract/groundhog.toml").exists());
}

#[test]
fn init_human_output_builds_existing_project() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("existing-contract");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        "[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();
    let mut child = groundhog()
        .arg("init")
        .current_dir(&project)
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env_remove("HOGD_FFI_PASSWORD")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    child.stdin.as_mut().unwrap().write_all(b"y\nn\n").unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Setup Complete"));
    assert!(stderr.contains("Project:"));
    assert!(stderr.contains("existing-contract"));
    assert!(stderr.contains("Next:"));
    assert!(stderr.contains("groundhog build"));
    assert!(!stderr.contains("groundhog new my-contract"));
    assert!(!stderr.contains("cd existing-contract"));
}

#[test]
fn status_json_outputs_structured_payload() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env_remove("HOGD_FFI_PASSWORD")
        .output()
        .unwrap();

    assert!(!output.status.success());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "status");
    assert!(stdout["data"]["environment"]["cargo"].as_bool().unwrap());
    assert!(stdout["data"]["environment"]["rust_toolchain"]["available"].is_boolean());
    assert!(stdout["data"]["environment"]["rust_toolchain"]["rust_src"].is_string());
    assert!(stdout["data"]["environment"]["rv32_target"].is_string());
    assert!(stdout["data"]["environment"]["devnode"]["reachable"].is_boolean());
    assert!(stdout["data"]["environment"]["dev_wallet_artifact"]["valid"].is_boolean());
    assert!(stdout["data"]["environment"]["wasm_target"].is_string());
    assert!(stdout["data"]["environment"]["wasm_opt"].is_boolean());
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
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let output = groundhog()
        .arg("--json")
        .arg("status")
        .arg("--profile")
        .arg("staging")
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env_remove("HOGD_FFI_PASSWORD")
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
fn status_json_reports_project_default_wallet_and_dangling_finding() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\ndefault_wallet = \"missing\"\n",
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "status"])
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env_remove("HOGD_FFI_PASSWORD")
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let project = &stdout["data"]["project"];
    assert_eq!(project["project_default_wallet"], "missing");
    assert_eq!(project["effective_default_wallet"], "missing");
    assert_eq!(project["default_wallet_source"], "project");
    assert!(
        stdout["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "default_wallet_not_saved")
    );
}

#[cfg(unix)]
#[test]
fn status_json_reports_reachable_devnode() {
    let tmp = TempDir::new().unwrap();
    let devnode = support::write_versioned_devnode(tmp.path());
    let output = groundhog()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("GROUNDHOG_DEVNODE_BINARY", &devnode)
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = &stdout["data"]["environment"]["devnode"];
    assert_eq!(status["reachable"], true);
    assert_eq!(status["path"], devnode.display().to_string());
    assert_eq!(status["version"], "0.1.12");
    let artifact = &stdout["data"]["environment"]["dev_wallet_artifact"];
    assert_eq!(artifact["available"], true);
    assert_eq!(artifact["valid"], true);
    assert!(
        artifact["path"]
            .as_str()
            .unwrap()
            .ends_with("contracts/dev_wallet.rv32a")
    );
    assert_eq!(artifact["sha256"].as_str().unwrap().len(), 64);
    assert!(
        stdout["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "devnode_unavailable")
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
        .env_remove("HOGD_FFI_PASSWORD")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
#[serial_test::serial]
#[cfg(unix)]
fn quiet_status_exits_zero_with_no_output_when_fully_configured() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let devnode = support::write_versioned_devnode(tmp.path());

    unsafe {
        std::env::set_var("CARGO_HOME", &cargo_home);
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

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
    groundhog_cli::config::initialize_global_profiles().unwrap();

    let output = groundhog()
        .arg("-q")
        .arg("status")
        .env("CARGO_HOME", &cargo_home)
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("GROUNDHOG_DEVNODE_BINARY", devnode)
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
        .env_remove("HOGD_FFI_PASSWORD")
        .output()
        .unwrap();

    // Will exit 1 (warnings), but stderr should have
    // human-readable section content.
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Groundhog Status"));
    assert!(stderr.contains("Cargo: installed"));
    assert!(stderr.contains("Rust Toolchain:"));
    assert!(stderr.contains("RV32 Target:"));
    assert!(stderr.contains("hog-devnode:"));
    assert!(stderr.contains("Dev Wallet:"));
    assert!(!stderr.contains("Wasm Target:"));
    assert!(!stderr.contains("wasm-opt:"));
    assert!(stderr.contains("Global Registry:"));
    assert!(stderr.contains("Keys: none"));
    assert!(stderr.contains("Mock Executor Auth: not configured"));
}

#[test]
fn status_json_detects_mock_executor_auth_from_environment() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("HOGD_FFI_PASSWORD", "secret")
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["credentials"]["executor_credentials"], true);
    assert_eq!(stdout["data"]["credentials"]["source"], "environment");
    assert!(
        stdout["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "no_credentials")
    );
}

#[test]
fn status_json_detects_mock_executor_auth_from_config() {
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join("groundhog");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "[executor]\nusername = \"marmot\"\npassword = \"secret\"\n",
    )
    .unwrap();

    let output = groundhog()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env_remove("HOGD_FFI_PASSWORD")
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["credentials"]["executor_credentials"], true);
    assert_eq!(stdout["data"]["credentials"]["source"], "config");
}

#[test]
fn status_json_rejects_empty_mock_executor_user() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("HOGD_FFI_USER", "")
        .env("HOGD_FFI_PASSWORD", "secret")
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["credentials"]["executor_credentials"], false);
    assert!(stdout["data"]["credentials"].get("source").is_none());
    assert!(
        stdout["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "no_credentials")
    );
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
fn build_default_json_success_reports_rv32_artifact_paths() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract-rv32\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    write_contract_toml(tmp.path());
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let target_dir = tmp.path().join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo_contract_rv32");
    let rv32 = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.rv32a");
    let metadata = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.rv32.json");
    let elf_fixture = tmp.path().join("fixture.elf");
    std::fs::write(&elf_fixture, support::minimal_rv32_elf()).unwrap();
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
                "/bin/cp '{}' '{}'\n",
                "exit 0\n",
            ),
            metadata_json,
            raw_elf
                .parent()
                .expect("bug: raw elf parent should exist")
                .display(),
            elf_fixture.display(),
            raw_elf.display()
        ),
    )
    .unwrap();

    let mut perms = std::fs::metadata(&cargo).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cargo, perms).unwrap();

    let output = groundhog()
        .args(["--json", "build", "-p", "demo-contract-rv32"])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(std::fs::read(&rv32).unwrap().starts_with(b"GHRV32A1"));
    let metadata_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&metadata).unwrap()).unwrap();
    assert_eq!(
        metadata_json["artifact_format_fingerprint"],
        format!("{:#018x}", groundhog_rv32_lower::FORMAT_FINGERPRINT)
    );

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "build");
    assert_eq!(stdout["data"]["target"], "rv32");
    assert_eq!(stdout["data"]["elf"], raw_elf.display().to_string());
    assert_eq!(stdout["data"]["rv32"], rv32.display().to_string());
    assert_eq!(stdout["data"]["metadata"], metadata.display().to_string());
    // v4 stores only the high-water prefix, so the artifact is well under 64 KiB.
    assert!(stdout["data"]["size_bytes"].as_u64().unwrap() < 64 * 1024);
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
    write_contract_toml(tmp.path());

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let target_dir = tmp.path().join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo_contract_rv32");
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
                "printf 'not an elf' > '{}'\n",
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
    assert_eq!(stderr["error"]["code"], "rv32_lower_failed");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("input is not an ELF file")
    );
    assert_eq!(
        stderr["error"]["details"]["message"],
        "input is not an ELF file"
    );
}

#[test]
fn build_help_mentions_target_dir() {
    let output = groundhog().args(["build", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--target-dir"));
    assert!(stdout.contains("--target <TARGET>"));
    assert!(stdout.contains("rv32"));
}

#[test]
fn test_help_mentions_target_dir() {
    let output = groundhog().args(["test", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--target-dir"));
    assert!(stdout.contains("--target <TARGET>"));
    assert!(stdout.contains("rv32"));
    assert!(stdout.contains("--watch"));
    assert!(stdout.contains("--integration"));
    assert!(stdout.contains("--differential"));
    assert!(stdout.contains("-v, --verbose"));
    assert!(stdout.contains("RUST_LOG"));
    assert!(stdout.contains("hog-devnode"));
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
    assert!(stdout.contains("--hog-devnode-binary"));
    assert!(stdout.contains("--data-dir"));
    assert!(stdout.contains("persistent hog-devnode state"));
    assert!(stdout.contains("--dev-account-count"));
    assert!(stdout.contains("--dev-account-hog <HOG>"));
    assert!(stdout.contains("Initial HOG allocated to each dev-1+ user account"));
    assert!(stdout.contains("--hogd-protocol"));
    assert!(stdout.contains("GROUNDHOG_DEVNODE_BINARY"));
    assert!(stdout.contains("RUST_LOG"));
    assert!(stdout.contains("hogd_timestamp_ms"));
    assert!(stdout.contains("hogd_target"));
    assert!(stdout.contains("hogd_file"));
    assert!(stdout.contains("hogd_line"));
}

#[test]
fn node_rejects_ramdb_size_without_a_recognized_unit() {
    let output = groundhog()
        .args(["node", "--ramdb-bytes", "2.5G"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("K/M/G"),
        "expected unit hint in stderr, got: {stderr}"
    );
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

#[cfg(unix)]
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
                "  printf '%s' \"$GROUNDHOG_TEST_METADATA_JSON\"\n",
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
            "--target",
            "wasm",
            "-p",
            "demo-contract",
            "--",
            "--nocapture",
        ])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .env("GROUNDHOG_TEST_METADATA_JSON", metadata_json)
        .env("GROUNDHOG_DEVNODE_BINARY", &hogd_dev)
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
fn test_integration_default_rv32_build_reaches_local_node_startup() {
    use std::os::unix::fs::PermissionsExt;

    let hogd_dev = fake_hogd_dev_binary();
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    write_contract_toml(tmp.path());
    std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
    std::fs::write(tmp.path().join("tests/integration.rs"), "").unwrap();

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let target_dir = tmp.path().join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo_contract");
    let elf_fixture = tmp.path().join("fixture.elf");
    std::fs::write(&elf_fixture, support::minimal_rv32_elf()).unwrap();
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": target_dir,
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
                // Contract builds run as `cargo --config <overlay> <subcmd>`,
                // so step past the overlay before matching the subcommand.
                "if [ \"$1\" = \"--config\" ]; then shift 2; fi\n",
                "if [ \"$1\" = \"metadata\" ]; then\n",
                "  printf '%s' \"$GROUNDHOG_TEST_METADATA_JSON\"\n",
                "  exit 0\n",
                "fi\n",
                "if [ \"$1\" = \"build\" ]; then\n",
                "  /bin/mkdir -p '{}'\n",
                "  /bin/cp '{}' '{}'\n",
                "  exit 0\n",
                "fi\n",
                "exit 1\n",
            ),
            raw_elf
                .parent()
                .expect("bug: raw elf parent should exist")
                .display(),
            elf_fixture.display(),
            raw_elf.display(),
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&cargo).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cargo, perms).unwrap();

    let output = groundhog()
        .args(["--json", "test", "--integration", "-p", "demo-contract"])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .env("GROUNDHOG_TEST_METADATA_JSON", metadata_json)
        .env("GROUNDHOG_DEVNODE_BINARY", &hogd_dev)
        .env("GROUNDHOG_FAKE_HOGD_DEV_MODE", "exit-7")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        tmp.path()
            .join("target/groundhog/demo_contract.rv32a")
            .is_file(),
        "integration should lower an RV32 artifact before node startup by default",
    );
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "test");
    assert_eq!(stderr["error"]["code"], "node_supervisor_error");
}

#[cfg(unix)]
fn prepare_failed_rv32_test_fixture(tmp: &TempDir) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

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
    let raw_elf = tmp
        .path()
        .join("target/riscv32im-unknown-none-elf/release/demo_contract");
    let elf_fixture = tmp.path().join("fixture.elf");
    std::fs::write(&elf_fixture, support::minimal_rv32_elf()).unwrap();
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
                "  /bin/cp '{}' '{}'\n",
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
            raw_elf
                .parent()
                .expect("bug: raw elf parent should exist")
                .display(),
            elf_fixture.display(),
            raw_elf.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&cargo).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cargo, perms).unwrap();
    fake_bin
}

#[cfg(unix)]
#[test]
fn test_json_failed_run_emits_report_on_stdout() {
    let tmp = TempDir::new().unwrap();
    let fake_bin = prepare_failed_rv32_test_fixture(&tmp);

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
    let schema_version = groundhog_cli::test_json::TEST_JSON_SCHEMA_VERSION;
    assert_eq!(stdout["data"]["schemaVersion"], schema_version);
    assert_eq!(stdout["data"]["mode"], "rv32");
    assert_rv32_test_artifact(&stdout["data"]);
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

#[cfg(unix)]
#[test]
fn test_rv32_harness_receives_resolved_cargo_target_dir() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    write_contract_toml(tmp.path());

    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let target_dir = tmp.path().join("configured-target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo_contract");
    let elf_fixture = tmp.path().join("fixture.elf");
    std::fs::write(&elf_fixture, support::minimal_rv32_elf()).unwrap();
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": target_dir,
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
                // Contract builds run as `cargo --config <overlay> <subcmd>`,
                // so step past the overlay before matching the subcommand.
                "if [ \"$1\" = \"--config\" ]; then shift 2; fi\n",
                "if [ \"$1\" = \"metadata\" ]; then\n",
                "  printf '%s' '{}'\n",
                "  exit 0\n",
                "fi\n",
                "if [ \"$1\" = \"build\" ]; then\n",
                "  /bin/mkdir -p '{}'\n",
                "  /bin/cp '{}' '{}'\n",
                "  exit 0\n",
                "fi\n",
                "if [ \"$1\" = \"test\" ]; then\n",
                "  if [ \"$CARGO_TARGET_DIR\" != '{}' ]; then\n",
                "    printf 'unexpected CARGO_TARGET_DIR: %s\\n' \"$CARGO_TARGET_DIR\" >&2\n",
                "    exit 42\n",
                "  fi\n",
                "  printf 'running 1 test\\n'\n",
                "  printf 'test tests::ok ... ok\\n'\n",
                "  printf 'test result: ok. 1 passed; 0 failed; 0 ignored; ",
                "0 measured; 0 filtered out; finished in 0.01s\\n'\n",
                "  exit 0\n",
                "fi\n",
                "exit 1\n",
            ),
            metadata_json,
            raw_elf
                .parent()
                .expect("bug: raw elf parent should exist")
                .display(),
            elf_fixture.display(),
            raw_elf.display(),
            target_dir.display(),
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&cargo).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cargo, perms).unwrap();

    let output = groundhog()
        .args(["--json", "test", "-p", "demo-contract"])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["data"]["mode"], "rv32");
    assert_eq!(
        stdout["data"]["artifact"]["path"].as_str().unwrap(),
        target_dir
            .join("groundhog/demo_contract.rv32a")
            .display()
            .to_string()
    );
}

fn assert_rv32_test_artifact(data: &serde_json::Value) {
    assert_eq!(data["artifact"]["kind"], "rv32");
    assert!(
        data["artifact"]["path"]
            .as_str()
            .unwrap()
            .ends_with("target/groundhog/demo_contract.rv32a")
    );
}

#[cfg(unix)]
#[test]
fn test_json_wasm_target_emits_wasm_report() {
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
                "  printf 'running 1 test\\n'\n",
                "  printf 'test tests::ok ... ok\\n'\n",
                "  printf 'test result: ok. 1 passed; 0 failed; 0 ignored; ",
                "0 measured; 0 filtered out; finished in 0.01s\\n'\n",
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
        .args(["--json", "test", "--target", "wasm", "-p", "demo-contract"])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "expected empty stderr, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["data"]["mode"], "wasm");
    assert_wasm_test_artifact(&stdout["data"]);
}

fn assert_wasm_test_artifact(data: &serde_json::Value) {
    assert_eq!(data["artifact"]["kind"], "wasm");
    assert!(
        data["artifact"]["path"]
            .as_str()
            .unwrap()
            .ends_with("target/groundhog/demo_contract.wasm")
    );
    assert!(data.get("wasm").is_none());
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
    let config_home = tmp.path().join("config-home");
    let output = groundhog()
        .arg("deploy")
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no active profile"));
}

#[test]
fn deploy_json_fails_without_config() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let output = groundhog()
        .arg("--json")
        .arg("deploy")
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "deploy");
    assert_eq!(stderr["error"]["code"], "no_active_profile");
}

#[test]
fn deploy_dry_run_json_outputs_prepared_transaction() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let owner = write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    let trading = format!("0x{}", "bb".repeat(32));
    let mut config = std::fs::read_to_string(project.join("groundhog.toml")).unwrap();
    write!(
        config,
        "\n[profile.default.wallets.trading]\naddress = \"{trading}\"\nowner_public_key = \"{owner}\"\n"
    )
    .unwrap();
    std::fs::write(project.join("groundhog.toml"), config).unwrap();

    // Write a minimal WASM file for the deploy to read.
    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();

    let output = groundhog()
        .args([
            "--json",
            "deploy",
            "--dry-run",
            "--wallet",
            "trading",
            "--artifact",
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
    assert_eq!(
        stdout["data"]["schemaVersion"],
        groundhog_cli::deploy::DEPLOY_JSON_SCHEMA_VERSION
    );
    assert_eq!(stdout["data"]["dry_run"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(stdout["data"]["artifact_kind"], "wasm");
    assert_eq!(
        stdout["data"]["artifact_path"].as_str().unwrap(),
        wasm_path.to_string_lossy()
    );
    assert_eq!(stdout["data"]["artifact_bytes"], 8);
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
    assert_eq!(
        signed_tx_deploy_contract_kind(stdout["data"]["signed_tx"].as_str().unwrap()),
        "wasm"
    );
    let signed_tx = decode_signed_tx(stdout["data"]["signed_tx"].as_str().unwrap());
    assert_eq!(
        format!(
            "0x{}",
            hex::encode(signed_tx.tx.invocation.invoked_address.as_ref())
        ),
        trading
    );
}

#[test]
fn deploy_dry_run_uses_hosted_descriptor_entrypoint() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    add_faucet_to_default_profile(&project, &faucet_url);
    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();

    let output = groundhog()
        .args([
            "--json",
            "deploy",
            "--dry-run",
            "--artifact",
            wasm_path.to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    descriptor_server.join().unwrap();
    rpc_server.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["wallet_entrypoint_id"], "0xf969542572bd6860");
    assert_eq!(
        stdout["data"]["wallet_entrypoint_source"],
        "network_descriptor"
    );
    assert_eq!(
        signed_tx_entrypoint(stdout["data"]["signed_tx"].as_str().unwrap()),
        "0xf969542572bd6860"
    );
}

#[test]
fn wallet_create_dry_run_json_outputs_prepared_transaction() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();

    let faucet_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &faucet_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(project.join("dev-wallet.wasm"), b"dev-wallet").unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let generate = groundhog()
        .args(["keys", "generate", "owner"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(generate.status.success());

    let output = groundhog()
        .args([
            "--json",
            "wallet",
            "create",
            "--key",
            "owner",
            "--name",
            "trading",
            "--dry-run",
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
    assert_eq!(stdout["command"], "wallet.create");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(
        stdout["data"]["schemaVersion"],
        groundhog_cli::wallet::WALLET_CREATE_JSON_SCHEMA_VERSION
    );
    assert_eq!(stdout["data"]["schemaVersion"], 3);
    assert_eq!(stdout["data"]["dry_run"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["name"], "trading");
    assert_eq!(stdout["data"]["key"], "owner");
    assert_eq!(stdout["data"]["faucet"]["key"], "dev-0");
    assert_eq!(stdout["data"]["faucet"]["role"], "faucet");
    assert_eq!(stdout["data"]["prefund_raw_amount"], "200000000");
    assert!(stdout["data"]["wallet"].as_str().unwrap().starts_with("0x"));
    assert!(
        stdout["data"]["owner_public_key"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
    assert_eq!(
        std::fs::canonicalize(stdout["data"]["artifact_path"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(project.join("dev-wallet.wasm")).unwrap()
    );
    assert!(stdout["data"].get("signed_prefund_tx").is_none());
    assert!(stdout["data"].get("signed_tx").is_none());
}

#[test]
fn wallet_create_dry_run_defaults_name_to_key() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();

    let faucet_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &faucet_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(project.join("dev-wallet.wasm"), b"dev-wallet").unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let generate = groundhog()
        .args(["keys", "generate", "owner"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(generate.status.success());

    let output = groundhog()
        .args(["--json", "wallet", "create", "--key", "owner", "--dry-run"])
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
    assert_eq!(stdout["command"], "wallet.create");
    assert_eq!(
        stdout["data"]["schemaVersion"],
        groundhog_cli::wallet::WALLET_CREATE_JSON_SCHEMA_VERSION
    );
    assert_eq!(stdout["data"]["schemaVersion"], 3);
    assert_eq!(stdout["data"]["dry_run"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["name"], "owner");
    assert_eq!(stdout["data"]["key"], "owner");
    assert_eq!(stdout["data"]["prefund_raw_amount"], "200000000");
    assert!(stdout["data"].get("signed_prefund_tx").is_none());
    assert!(stdout["data"].get("signed_tx").is_none());
}

#[test]
fn wallet_create_remote_dry_run_does_not_call_faucet() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        "[profile.devnet]\nrpc_url = \"http://127.0.0.1:9545\"\nfaucet_url = \"http://127.0.0.1:9\"\n",
    )
    .unwrap();

    let output = groundhog()
        .args([
            "--json",
            "wallet",
            "create",
            "--key",
            "alice",
            "--profile",
            "devnet",
            "--dry-run",
        ])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "wallet.create");
    assert_eq!(stdout["data"]["dry_run"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["mode"], "remote");
    assert_eq!(stdout["data"]["key"], "alice");
    assert_eq!(stdout["data"]["name"], "alice");
    assert_eq!(stdout["data"]["wallet"], serde_json::Value::Null);
    assert_eq!(
        stdout["data"]["planned_requests"],
        serde_json::json!(["GET /v1/network", "POST /v1/create-wallet"])
    );
}

#[test]
fn wallet_ls_json_outputs_saved_wallets() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let alice = format!("0x{}", "ab".repeat(32));
    let bob = format!("0x{}", "cd".repeat(32));
    let alice_owner = format!("0x{}", "11".repeat(32));
    let bob_owner = format!("0x{}", "22".repeat(32));
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:9545\"\ndefault_wallet = \"alice\"\n\n[profile.default.wallets.alice]\naddress = \"{alice}\"\nowner_public_key = \"{alice_owner}\"\n\n[profile.default.wallets.bob]\naddress = \"{bob}\"\nowner_public_key = \"{bob_owner}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "wallet", "ls"])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "wallet.ls");
    assert_eq!(
        stdout["data"]["wallets"],
        serde_json::json!([
            { "name": "alice", "address": alice, "owner_public_key": alice_owner, "is_default": true },
            { "name": "bob", "address": bob, "owner_public_key": bob_owner, "is_default": false },
        ])
    );

    let human = groundhog()
        .args(["wallet", "ls"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(human.status.success());
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    assert!(
        human_stdout
            .lines()
            .any(|line| line.starts_with("* alice "))
    );
    assert!(human_stdout.lines().any(|line| line.starts_with("  bob ")));
    assert!(!human_stdout.contains("(default)"));
}

#[test]
fn wallet_use_persists_valid_owner_matched_default() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let owner = generate_key_public_key(&config_home, "alice");
    let address = format!("0x{}", "ab".repeat(32));
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\ndefault_key = \"alice\"\n\n[profile.default.wallets.trading]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "wallet", "use", " trading "])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["command"], "wallet.use");
    assert_eq!(stdout["data"]["name"], "trading");
    let contents = std::fs::read_to_string(project.join("groundhog.toml")).unwrap();
    assert!(
        contents.contains("default_wallet = \"trading\""),
        "config: {contents}"
    );
    let profile = groundhog_cli::config::resolve_profile(&project, Some("default")).unwrap();
    assert_eq!(profile.default_wallet.as_deref(), Some("trading"));
}

#[test]
fn wallet_rm_rejects_inherited_default_with_project_only_binding() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(config_home.join("groundhog")).unwrap();
    let address = format!("0x{}", "ab".repeat(32));
    let owner = format!("0x{}", "11".repeat(32));
    std::fs::write(
        config_home.join("groundhog/config.toml"),
        "[profile.devnet]\ndefault_wallet = \"alice\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.devnet]\n\n[profile.devnet.wallets.alice]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "wallet", "rm", "alice", "--profile", "devnet"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "inherited_default_wallet_removal");
    let profile = groundhog_cli::config::resolve_profile(&project, Some("devnet")).unwrap();
    assert!(profile.wallets.contains_key("alice"));
}

#[test]
fn wallet_rm_reports_inherited_default_after_clearing_project_default() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(config_home.join("groundhog")).unwrap();
    let owner = format!("0x{}", "11".repeat(32));
    std::fs::write(
        config_home.join("groundhog/config.toml"),
        format!(
            "[profile.devnet]\ndefault_wallet = \"global\"\n\n[profile.devnet.wallets.global]\naddress = \"0x{}\"\nowner_public_key = \"{owner}\"\n",
            "aa".repeat(32)
        ),
    )
    .unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.devnet]\ndefault_wallet = \"project\"\n\n[profile.devnet.wallets.project]\naddress = \"0x{}\"\nowner_public_key = \"{owner}\"\n",
            "bb".repeat(32)
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "wallet", "rm", "project", "--profile", "devnet"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["clearedConfiguredDefault"], true);
    assert_eq!(stdout["data"]["effectiveDefaultWallet"], "global");
}

#[test]
fn wallet_rm_rejects_dangling_same_named_inherited_default() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(config_home.join("groundhog")).unwrap();
    let owner = format!("0x{}", "11".repeat(32));
    std::fs::write(
        config_home.join("groundhog/config.toml"),
        "[profile.devnet]\ndefault_wallet = \"alice\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "[profile.devnet]\ndefault_wallet = \"alice\"\n\n[profile.devnet.wallets.alice]\naddress = \"0x{}\"\nowner_public_key = \"{owner}\"\n",
            "aa".repeat(32)
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "wallet", "rm", "alice", "--profile", "devnet"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(
        stderr["error"]["code"],
        "wallet_removal_exposes_missing_default"
    );
    assert_eq!(stderr["error"]["details"]["inheritedWallet"], "alice");
    let contents = std::fs::read_to_string(project.join("groundhog.toml")).unwrap();
    assert!(contents.contains("default_wallet = \"alice\""));
    assert!(contents.contains("[profile.devnet.wallets.alice]"));
}

#[test]
fn airdrop_dry_run_json_outputs_prepared_transaction() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let faucet_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &faucet_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let recipient_wallet = format!("0x{}", "ab".repeat(32));
    let recipient_owner = format!("0x{}", "cd".repeat(32));
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n\n[profile.default.wallets.alice]\naddress = \"{recipient_wallet}\"\nowner_public_key = \"{recipient_owner}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = groundhog()
        .args([
            "--json",
            "airdrop",
            "alice",
            "--amount",
            "1.25",
            "--dry-run",
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
    assert_eq!(stdout["command"], "airdrop");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(
        stdout["data"]["schemaVersion"],
        groundhog_cli::airdrop::AIRDROP_JSON_SCHEMA_VERSION
    );
    assert_eq!(stdout["data"]["dry_run"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["recipient"], "alice");
    assert_eq!(stdout["data"]["recipient_wallet"], recipient_wallet);
    assert_eq!(stdout["data"]["amount_hog"], "1.25");
    assert_eq!(stdout["data"]["raw_amount"], "12500000");
    assert_eq!(stdout["data"]["faucet"]["key"], "dev-0");
    assert_eq!(stdout["data"]["faucet"]["role"], "faucet");
    assert!(stdout["data"].get("signed_tx").is_none());
}

#[test]
fn airdrop_json_rejects_invalid_hog_amount_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let faucet_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &faucet_wallet, None);
    let recipient_wallet = format!("0x{}", "ab".repeat(32));
    let recipient_owner = format!("0x{}", "cd".repeat(32));
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\nruntime_manifest = \"{}\"\n\n[profile.default.wallets.alice]\naddress = \"{recipient_wallet}\"\nowner_public_key = \"{recipient_owner}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = groundhog()
        .args([
            "--json",
            "airdrop",
            "alice",
            "--amount",
            "1.00000001",
            "--dry-run",
        ])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["command"], "airdrop");
    assert_eq!(stderr["error"]["code"], "invalid_hog_amount");
    assert_eq!(stderr["error"]["details"]["input"], "1.00000001");
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("seven decimal")
    );
}

#[test]
fn transfer_dry_run_json_outputs_signed_wallet_wrapped_hog_transfer() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sender = advertised_dev_wallet_address(0);
    let recipient = format!("0x{}", "ab".repeat(32));
    let manifest_path = write_runtime_manifest(&project, 0, &sender, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n\n[profile.default.wallets.alice]\naddress = \"{recipient}\"\nowner_public_key = \"0x{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy(),
            "cd".repeat(32),
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "transfer", "alice", "10.5000000", "--dry-run"])
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
    let data = &stdout["data"];
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "transfer");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(data["schemaVersion"], 1);
    assert_eq!(data["profile"], "default");
    assert_eq!(data["rpcUrl"], rpc_url);
    assert_eq!(data["dryRun"], true);
    assert_eq!(data["submitted"], false);
    assert_eq!(data["sender"], sender);
    assert_eq!(data["senderWallet"], serde_json::Value::Null);
    assert_eq!(
        data["signerPublicKey"],
        format!(
            "0x{}",
            hex::encode(
                groundhog_signing::derive_default_dev_account_signer(0).verifying_key_bytes()
            )
        )
    );
    assert_eq!(data["recipientInput"], "alice");
    assert_eq!(data["recipient"], recipient);
    assert_eq!(data["tokenAddress"], format!("0x{}01", "00".repeat(31)));
    assert_eq!(data["amount"], "10.5");
    assert_eq!(data["rawAmount"], "105000000");
    assert_eq!(data["expiryHeight"], 1042);
    for absent in [
        "height",
        "status",
        "events",
        "returnData",
        "abortData",
        "confirmation",
        "transactionStatus",
    ] {
        assert!(data.get(absent).is_none(), "{absent} should be omitted");
    }

    let signed_tx = decode_signed_tx(data["signedTransaction"].as_str().unwrap());
    assert_eq!(
        format!(
            "0x{}",
            hex::encode(signed_tx.tx.invocation.invoked_address.as_ref())
        ),
        sender
    );
    assert_eq!(
        signed_tx_entrypoint_from_decoded(&signed_tx),
        "0x646f416374696f6e"
    );
    assert_eq!(*signed_tx.tx.uniquifier.block_expiry_height, 1042);
    assert_eq!(
        data["txHash"],
        format!(
            "0x{}",
            hex::encode(groundhog_signing::transaction_hash(&signed_tx.tx).unwrap())
        )
    );
    let (target, entrypoint, target_calldata) = decode_wallet_action(&signed_tx);
    assert_eq!(target, format!("0x{}01", "00".repeat(31)));
    assert_eq!(entrypoint, "0x63285b3523f9dc3f");
    let (calldata_recipient, raw_amount): (groundhog_xdr::types::ContractAddress, u128) =
        xdrs::cxs::decode(&target_calldata).unwrap();
    assert_eq!(
        format!("0x{}", hex::encode(calldata_recipient.as_ref())),
        recipient
    );
    assert_eq!(raw_amount, 105_000_000);
}

#[test]
fn transfer_dry_run_human_output_identifies_signer_and_submit_capability() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sender = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &sender, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let recipient = format!("0x{}", "ab".repeat(32));
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["transfer", &recipient, "1", "--dry-run"])
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
    let stderr = String::from_utf8(output.stderr).unwrap();
    let public_key = format!(
        "0x{}",
        hex::encode(groundhog_signing::derive_default_dev_account_signer(0).verifying_key_bytes())
    );
    assert!(stderr.contains(&format!("sender: {sender}")));
    assert!(stderr.contains(&format!("signer public key: {public_key}")));
    assert!(stderr.contains("signed transaction can be submitted until its expiry height"));
    assert!(stderr.contains("submitted: no (--dry-run)"));
    assert!(!stderr.contains(groundhog_signing::DEFAULT_DEV_ACCOUNT_SEED));
}

#[test]
fn transfer_dry_run_uses_hosted_entrypoint_but_protocol_hog_constants() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    add_faucet_to_default_profile(&project, &faucet_url);
    let recipient = format!("0x{}", "bc".repeat(32));

    let output = groundhog()
        .args(["--json", "transfer", &recipient, "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    descriptor_server.join().unwrap();
    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let signed_tx = decode_signed_tx(stdout["data"]["signedTransaction"].as_str().unwrap());
    let (target, entrypoint, _) = decode_wallet_action(&signed_tx);
    assert_eq!(
        signed_tx_entrypoint_from_decoded(&signed_tx),
        "0xa0fff94c64fbaf3c"
    );
    assert_eq!(target, format!("0x{}01", "00".repeat(31)));
    assert_eq!(entrypoint, "0x63285b3523f9dc3f");
}

#[test]
fn transfer_dry_run_uses_legacy_wallet_entrypoint() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    let recipient = format!("0x{}", "bc".repeat(32));

    let output = groundhog()
        .args(["--json", "transfer", &recipient, "1", "--dry-run"])
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
    let signed_tx = decode_signed_tx(stdout["data"]["signedTransaction"].as_str().unwrap());
    assert_eq!(
        signed_tx_entrypoint_from_decoded(&signed_tx),
        "0x19846d750fad81d6"
    );
}

#[test]
fn transfer_dry_run_profile_and_cli_target_overrides_select_endpoint() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sender = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &sender, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let recipient = format!("0x{}", "bc".repeat(32));
    std::fs::write(
        project.join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n\n[profile.devnet]\nrpc_url = \"http://127.0.0.1:2\"\ndefault_key = \"dev-0\"\n",
    )
    .unwrap();

    let output = groundhog()
        .args([
            "--json",
            "transfer",
            &recipient,
            "1",
            "--dry-run",
            "--profile",
            "devnet",
            "--rpc-url",
            &rpc_url,
            "--runtime-manifest",
            manifest_path.to_str().unwrap(),
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
    assert_eq!(stdout["data"]["profile"], "devnet");
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["sender"], sender);
}

#[test]
fn transfer_dry_run_rejects_invalid_inputs_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sender = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &sender, None);
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    for (recipient, amount, expected_code) in [
        (
            format!("0x{}", "ab".repeat(32)),
            "1.00000001",
            "invalid_hog_amount",
        ),
        ("missing".to_owned(), "1", "unknown_address_target"),
    ] {
        let output = groundhog()
            .args(["--json", "transfer", &recipient, amount, "--dry-run"])
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["command"], "transfer");
        assert_eq!(stderr["error"]["code"], expected_code);
    }
    rpc_server.join().unwrap();
}

#[test]
fn transfer_dry_run_saved_wallet_errors_follow_selection_mode() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    generate_key_public_key(&config_home, "alice");
    let recipient = format!("0x{}", "bc".repeat(32));

    std::fs::write(
        project.join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"alice\"\n",
    )
    .unwrap();
    let missing = groundhog()
        .args(["--json", "transfer", &recipient, "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let error: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(error["error"]["code"], "saved_wallet_missing");
    assert!(
        error["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("wallet create")
    );

    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"alice\"\n\n[profile.default.wallets.broken-wallet]\naddress = \"0x{}\"\nowner_public_key = \"0x00\"\n",
            "aa".repeat(32),
        ),
    )
    .unwrap();
    let invalid = groundhog()
        .args(["--json", "transfer", &recipient, "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(error["error"]["code"], "saved_wallet_missing");
    assert_eq!(error["error"]["details"]["keyName"], "alice");

    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"alice\"\ndefault_wallet = \"broken-wallet\"\n\n[profile.default.wallets.broken-wallet]\naddress = \"0x{}\"\nowner_public_key = \"0x00\"\n",
            "aa".repeat(32),
        ),
    )
    .unwrap();
    let selected = groundhog()
        .args(["--json", "transfer", &recipient, "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!selected.status.success());
    let error: serde_json::Value = serde_json::from_slice(&selected.stderr).unwrap();
    assert_eq!(
        error["error"]["code"],
        "saved_wallet_owner_public_key_invalid"
    );
    assert_eq!(error["error"]["details"]["keyName"], "broken-wallet");
}

#[test]
fn transfer_rejects_unique_wallet_self_transfer_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let owner = generate_key_public_key(&config_home, "alice");
    let address = format!("0x{}", "ab".repeat(32));
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"alice\"\n\n[profile.default.wallets.wallet-a]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "transfer", "wallet-a", "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "transfer_to_self");
    assert_eq!(error["error"]["details"]["address"], address);
    assert!(error["error"]["hint"].as_str().unwrap().contains("--from"));
}

#[test]
fn transfer_from_selects_named_owner_matched_wallet() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let owner = generate_key_public_key(&config_home, "alice");
    let primary = format!("0x{}", "a1".repeat(32));
    let trading = format!("0x{}", "b2".repeat(32));
    let recipient = format!("0x{}", "c3".repeat(32));
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"alice\"\ndefault_wallet = \"primary\"\n\n[profile.default.wallets.primary]\naddress = \"{primary}\"\nowner_public_key = \"{owner}\"\n\n[profile.default.wallets.trading]\naddress = \"{trading}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args([
            "--json",
            "transfer",
            &recipient,
            "1",
            "--from",
            "trading",
            "--dry-run",
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
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["sender"], trading);
    assert_eq!(stdout["data"]["senderWallet"], "trading");
}

#[test]
fn transfer_live_reports_selected_saved_wallet_for_each_report_shape() {
    let committed = run_saved_wallet_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                status: serde_json::json!({ "result": "ok" }),
                events: serde_json::json!([]),
                return_data: None,
                abort_data: None,
                mismatched_hash: false,
            }),
        },
        None,
    );
    assert!(committed.output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&committed.output.stdout).unwrap();
    assert_eq!(stdout["data"]["senderWallet"], "primary");
    assert_eq!(stdout["data"]["sender"], format!("0x{}", "a1".repeat(32)));
    assert_eq!(stdout["data"]["txHash"], committed.local_hash);

    let unavailable = run_saved_wallet_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::DropResponse,
            poll: Some(TransferPollMock::Status(
                serde_json::json!({ "status": "unknown" }),
            )),
        },
        Some("trading"),
    );
    assert!(!unavailable.output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&unavailable.output.stdout).unwrap();
    assert_eq!(stdout["data"]["confirmation"], "unavailable");
    assert_eq!(stdout["data"]["senderWallet"], "trading");
    assert_eq!(stdout["data"]["sender"], format!("0x{}", "b2".repeat(32)));
    assert_eq!(stdout["data"]["txHash"], unavailable.local_hash);
}

#[test]
fn transfer_live_success_returns_matching_receipt_and_events() {
    let event = serde_json::json!({
        "contractAddress": format!("0x{}", "01".repeat(32)),
        "topics": [format!("0x{}", "02".repeat(32))],
        "data": "0x0304",
    });
    let run = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                status: serde_json::json!({ "result": "ok" }),
                events: serde_json::json!([event.clone()]),
                return_data: Some("0x0506"),
                abort_data: None,
                mismatched_hash: false,
            }),
        },
        true,
    );

    assert!(
        run.output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr)
    );
    assert!(run.output.stderr.is_empty());
    let stdout: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
    let data = &stdout["data"];
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "transfer");
    assert_transfer_live_common(data, &run.local_hash, None);
    assert_eq!(data["height"], 43);
    assert_eq!(data["status"], serde_json::json!({ "result": "ok" }));
    assert_eq!(data["events"], serde_json::json!([event]));
    assert_eq!(data["returnData"], "0x0506");
    for absent in [
        "abortData",
        "confirmation",
        "transactionStatus",
        "expiryHeight",
        "signedTransaction",
    ] {
        assert!(data.get(absent).is_none(), "{absent} should be omitted");
    }
}

#[test]
fn transfer_live_rejects_invalid_persisted_rpc_url() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sender = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &sender, None);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"not a URL\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();
    let recipient = format!("0x{}", "ab".repeat(32));

    let output = groundhog()
        .args(["--json", "transfer", &recipient, "1.5"])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "profile_config_invalid");
}

#[test]
fn transfer_live_committed_failures_preserve_receipt_context() {
    for (status, expected_code) in [
        (
            serde_json::json!({ "result": "reverted", "message": "denied" }),
            "transaction_reverted",
        ),
        (
            serde_json::json!({ "result": "deterministicError", "kind": "outOfGas" }),
            "transaction_deterministic_error",
        ),
    ] {
        let event = serde_json::json!({
            "contractAddress": format!("0x{}", "09".repeat(32)),
            "topics": [],
            "data": "0x0a0b",
        });
        let run = run_transfer_live_mock_with_hash(
            TransferRpcMock {
                submit: TransferSubmitMock::MatchingHash,
                poll: Some(TransferPollMock::Receipt {
                    status: status.clone(),
                    events: serde_json::json!([event.clone()]),
                    return_data: Some("0x0102"),
                    abort_data: Some("0x0304"),
                    mismatched_hash: false,
                }),
            },
            true,
        );

        assert!(!run.output.status.success());
        assert!(run.output.stderr.is_empty());
        let stdout: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
        assert_eq!(stdout["ok"], false);
        assert_eq!(stdout["error"]["code"], expected_code);
        assert_transfer_live_common(&stdout["data"], &run.local_hash, None);
        assert_eq!(stdout["data"]["height"], 43);
        assert_eq!(stdout["data"]["status"], status);
        assert_eq!(stdout["data"]["events"], serde_json::json!([event]));
        assert_eq!(stdout["data"]["returnData"], "0x0102");
        assert_eq!(stdout["data"]["abortData"], "0x0304");
        for absent in [
            "confirmation",
            "transactionStatus",
            "expiryHeight",
            "signedTransaction",
        ] {
            assert!(
                stdout["data"].get(absent).is_none(),
                "{absent} should be omitted"
            );
        }
    }
}

#[test]
fn transfer_live_terminal_invalid_is_conclusive_command_failure() {
    let status = serde_json::json!({ "status": "invalid", "reason": "expired" });
    let run = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Status(status.clone())),
        },
        true,
    );

    assert!(!run.output.status.success());
    assert!(run.output.stderr.is_empty());
    let stdout: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["error"]["code"], "transaction_invalid");
    assert_eq!(stdout["data"]["confirmation"], "invalid");
    assert_eq!(stdout["data"]["transactionStatus"], status);
    assert_transfer_live_common(&stdout["data"], &run.local_hash, None);
    assert_transfer_unconfirmed_omissions(&stdout["data"]);

    let human = run_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Status(serde_json::json!({
                "status": "invalid",
                "reason": "expired",
            }))),
        },
        false,
    );
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(!stderr.contains("may still commit"));
    assert!(!stderr.contains("blindly resubmit"));
}

#[test]
fn transfer_live_poll_failures_are_unavailable_command_failures() {
    let recent = serde_json::json!({
        "status": "recentFailure",
        "attemptHeight": 43,
        "conflict": {
            "address": format!("0x{}", "77".repeat(32)),
            "key": "0x0102",
            "code": "boundsExceeded",
        },
    });
    for (poll, expected_status) in [
        (
            TransferPollMock::Status(recent.clone()),
            Some(recent.clone()),
        ),
        (
            TransferPollMock::Status(serde_json::json!({ "status": "unknown" })),
            None,
        ),
        (TransferPollMock::ReceiptError(-32050), None),
    ] {
        let run = run_transfer_live_mock_with_hash(
            TransferRpcMock {
                submit: TransferSubmitMock::MatchingHash,
                poll: Some(poll),
            },
            true,
        );

        assert!(!run.output.status.success());
        assert!(run.output.stderr.is_empty());
        let stdout: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
        assert_eq!(
            stdout["error"]["code"],
            "transaction_confirmation_unavailable"
        );
        assert_eq!(stdout["data"]["confirmation"], "unavailable");
        assert_transfer_live_common(&stdout["data"], &run.local_hash, None);
        assert_transfer_unconfirmed_omissions(&stdout["data"]);
        match expected_status {
            Some(status) => assert_eq!(stdout["data"]["transactionStatus"], status),
            None => assert!(stdout["data"].get("transactionStatus").is_none()),
        }
    }
}

#[test]
fn transfer_live_lost_submission_response_polls_local_hash() {
    let success = run_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::DropResponse,
            poll: Some(TransferPollMock::Receipt {
                status: serde_json::json!({ "result": "ok" }),
                events: serde_json::json!([]),
                return_data: None,
                abort_data: None,
                mismatched_hash: false,
            }),
        },
        true,
    );
    assert!(
        success.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&success.stderr)
    );

    let unavailable = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::DropResponse,
            poll: Some(TransferPollMock::Status(
                serde_json::json!({ "status": "unknown" }),
            )),
        },
        true,
    );
    assert!(!unavailable.output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&unavailable.output.stdout).unwrap();
    assert_eq!(
        stdout["error"]["code"],
        "transaction_confirmation_unavailable"
    );
    assert_eq!(stdout["data"]["submitted"], true);
    assert_transfer_live_common(&stdout["data"], &unavailable.local_hash, None);
    assert!(stdout["data"].get("transactionStatus").is_none());
    assert_transfer_unconfirmed_omissions(&stdout["data"]);
}

#[test]
fn transfer_live_classifies_send_errors_by_numeric_code() {
    for code in [-32602, -32000, -32001, -32050, -32053] {
        let output = run_transfer_live_mock(
            TransferRpcMock {
                submit: TransferSubmitMock::Error(code),
                poll: None,
            },
            true,
        );
        assert!(!output.status.success(), "code {code} should fail");
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "rpc_method_failed");
    }

    for code in [-32601, -32052] {
        let output = run_transfer_live_mock(
            TransferRpcMock {
                submit: TransferSubmitMock::Error(code),
                poll: None,
            },
            true,
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "rpc_capability_unsupported");
        assert!(
            stderr["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("compatible remote RPC")
        );
    }

    for code in [-32051, -32123] {
        let output = run_transfer_live_mock(
            TransferRpcMock {
                submit: TransferSubmitMock::Error(code),
                poll: Some(TransferPollMock::Status(
                    serde_json::json!({ "status": "unknown" }),
                )),
            },
            true,
        );
        assert!(
            !output.status.success(),
            "code {code} should poll then fail"
        );
        assert!(output.stderr.is_empty());
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            stdout["error"]["code"],
            "transaction_confirmation_unavailable"
        );
    }
}

#[test]
fn transfer_live_hash_mismatches_never_report_commitment() {
    let server_mismatch = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MismatchedHash,
            poll: None,
        },
        true,
    );
    assert!(!server_mismatch.output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&server_mismatch.output.stdout).unwrap();
    assert_eq!(stdout["data"]["confirmation"], "unavailable");
    assert_eq!(stdout["data"]["submitted"], true);
    assert_transfer_live_common(&stdout["data"], &server_mismatch.local_hash, None);
    assert_ne!(stdout["data"]["txHash"], format!("0x{}", "ff".repeat(32)));
    assert!(stdout["data"].get("transactionStatus").is_none());
    assert_transfer_unconfirmed_omissions(&stdout["data"]);

    let receipt_mismatch = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                status: serde_json::json!({ "result": "ok" }),
                events: serde_json::json!([]),
                return_data: None,
                abort_data: None,
                mismatched_hash: true,
            }),
        },
        true,
    );
    assert!(!receipt_mismatch.output.status.success());
    let stdout: serde_json::Value =
        serde_json::from_slice(&receipt_mismatch.output.stdout).unwrap();
    assert_eq!(stdout["data"]["confirmation"], "unavailable");
    assert_transfer_live_common(&stdout["data"], &receipt_mismatch.local_hash, None);
    assert_ne!(stdout["data"]["txHash"], format!("0x{}", "ee".repeat(32)));
    assert!(stdout["data"].get("transactionStatus").is_none());
    assert_transfer_unconfirmed_omissions(&stdout["data"]);
}

#[test]
fn transfer_live_human_unavailable_warns_against_resubmission() {
    let output = run_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::Error(-32051),
            poll: Some(TransferPollMock::Status(
                serde_json::json!({ "status": "unknown" }),
            )),
        },
        false,
    );

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("may still commit"));
    assert!(stderr.contains("do not blindly resubmit"));
    assert!(stderr.contains("groundhog tx show"));
    assert!(stderr.contains("--profile=default"));
    assert!(!stderr.contains("--rpc-url"));
    assert!(!stderr.contains("signed transaction"));
}

#[test]
fn transfer_live_human_unavailable_quotes_cli_rpc_override() {
    let run = run_transfer_live_mock_with_rpc_override(
        TransferRpcMock {
            submit: TransferSubmitMock::Error(-32051),
            poll: Some(TransferPollMock::Status(
                serde_json::json!({ "status": "unknown" }),
            )),
        },
        false,
        "?key=x&network=test",
    );

    assert!(!run.output.status.success());
    let stderr = String::from_utf8(run.output.stderr).unwrap();
    assert!(stderr.contains(&format!("--rpc-url='{}'", run.rpc_url)));
}

#[test]
fn transfer_live_human_success_and_committed_failure_stay_on_stderr() {
    let success = run_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                status: serde_json::json!({ "result": "ok" }),
                events: serde_json::json!([]),
                return_data: None,
                abort_data: None,
                mismatched_hash: false,
            }),
        },
        false,
    );
    assert!(success.status.success());
    assert!(success.stdout.is_empty());
    let stderr = String::from_utf8(success.stderr).unwrap();
    assert!(stderr.contains("Transferred 1.5 HOG."));
    assert!(stderr.contains("recipient: 0x"));
    assert!(stderr.contains("tx: 0x"));
    assert!(stderr.contains("height: 43"));

    let failure = run_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                status: serde_json::json!({
                    "result": "reverted",
                    "message": "denied",
                }),
                events: serde_json::json!([]),
                return_data: None,
                abort_data: Some("0x0102"),
                mismatched_hash: false,
            }),
        },
        false,
    );
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    let stderr = String::from_utf8(failure.stderr).unwrap();
    assert!(stderr.contains("Transfer reverted: denied"));
    assert!(stderr.contains("tx: 0x"));
    assert!(stderr.contains("height: 43"));
    assert!(stderr.contains("abort data: 0x0102"));
}

#[test]
fn transfer_live_human_output_escapes_untrusted_status_text() {
    let output = run_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                status: serde_json::json!({
                    "result": "reverted",
                    "message": "denied\n\u{1b}[31m",
                }),
                events: serde_json::json!([]),
                return_data: None,
                abort_data: None,
                mismatched_hash: false,
            }),
        },
        false,
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r"denied\n\u{1b}[31m"));
    assert!(!stderr.contains('\u{1b}'));
}

#[test]
fn transfer_live_human_output_escapes_conclusive_backend_error() {
    let output = run_transfer_live_mock(
        TransferRpcMock {
            submit: TransferSubmitMock::ErrorWithMessage(-32000, "bad\n\u{1b}[31m"),
            poll: None,
        },
        false,
    );

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(r"bad\n\u{1b}[31m"),
        "unexpected stderr: {stderr:?}"
    );
    assert!(!stderr.contains('\u{1b}'));
}

fn assert_transfer_unconfirmed_omissions(data: &serde_json::Value) {
    for absent in [
        "height",
        "status",
        "events",
        "returnData",
        "abortData",
        "expiryHeight",
        "signedTransaction",
    ] {
        assert!(data.get(absent).is_none(), "{absent} should be omitted");
    }
}

fn assert_transfer_live_common(
    data: &serde_json::Value,
    local_hash: &str,
    sender_wallet: Option<&str>,
) {
    assert_eq!(data["schemaVersion"], 1);
    assert_eq!(data["profile"], "default");
    assert!(
        data["rpcUrl"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(data["dryRun"], false);
    assert_eq!(data["submitted"], true);
    assert_eq!(data["sender"], advertised_dev_wallet_address(0));
    match sender_wallet {
        Some(name) => assert_eq!(data["senderWallet"], name),
        None => assert_eq!(data["senderWallet"], serde_json::Value::Null),
    }
    assert!(
        data["signerPublicKey"]
            .as_str()
            .is_some_and(|value| value.starts_with("0x"))
    );
    assert_eq!(data["recipientInput"], format!("0x{}", "ab".repeat(32)));
    assert_eq!(data["recipient"], format!("0x{}", "ab".repeat(32)));
    assert_eq!(data["tokenAddress"], format!("0x{}01", "00".repeat(31)));
    assert_eq!(data["amount"], "1.5");
    assert_eq!(data["rawAmount"], "15000000");
    assert_eq!(data["txHash"], local_hash);
}

#[test]
fn deploy_dry_run_json_accepts_rv32_artifact() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let rv32_path = tmp.path().join("test.rv32a");
    std::fs::write(&rv32_path, b"rv32-artifact").unwrap();

    let output = groundhog()
        .args([
            "--json",
            "deploy",
            "--dry-run",
            "--artifact",
            rv32_path.to_str().unwrap(),
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
    assert_eq!(
        stdout["data"]["schemaVersion"],
        groundhog_cli::deploy::DEPLOY_JSON_SCHEMA_VERSION
    );
    assert_eq!(stdout["data"]["artifact_kind"], "rv32");
    assert_eq!(
        stdout["data"]["artifact_path"].as_str().unwrap(),
        rv32_path.to_string_lossy()
    );
    assert_eq!(stdout["data"]["artifact_bytes"], 13);
    assert_eq!(
        signed_tx_deploy_contract_kind(stdout["data"]["signed_tx"].as_str().unwrap()),
        "rv32"
    );
}

#[test]
fn deploy_dry_run_auto_resolves_rv32_artifact_by_default() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();

    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let artifact_path = project.join("target/groundhog/demo_contract.rv32a");
    std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    std::fs::write(&artifact_path, b"rv32-auto").unwrap();

    let output = groundhog()
        .args(["--json", "deploy", "--dry-run"])
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
    assert_eq!(
        stdout["data"]["schemaVersion"],
        groundhog_cli::deploy::DEPLOY_JSON_SCHEMA_VERSION
    );
    assert_eq!(stdout["data"]["artifact_kind"], "rv32");
    assert_eq!(
        stdout["data"]["artifact_path"].as_str().unwrap(),
        std::fs::canonicalize(&artifact_path)
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(stdout["data"]["artifact_bytes"], 9);
    assert_eq!(
        signed_tx_deploy_contract_kind(stdout["data"]["signed_tx"].as_str().unwrap()),
        "rv32"
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
            "--artifact",
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
    assert_eq!(signed_tx_entrypoint(signed_tx), "0x6465706c6f794e77");
}

#[test]
fn deploy_dry_run_uses_profile_rpc_and_manifest() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &advertised_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
            "--artifact",
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
            "--artifact",
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
fn deploy_dry_run_cli_target_overrides_profile_rpc_and_manifest() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let profile_wallet = advertised_dev_wallet_address(0);
    let cli_wallet = advertised_dev_wallet_address(1);
    let env_wallet = advertised_dev_wallet_address(2);
    let profile_manifest = write_runtime_manifest(&project, 0, &profile_wallet, None);
    let cli_manifest = write_runtime_manifest(tmp.path(), 0, &cli_wallet, None);
    let env_dir = tmp.path().join("env");
    std::fs::create_dir(&env_dir).unwrap();
    let env_manifest = write_runtime_manifest(&env_dir, 0, &env_wallet, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
            "--rpc-url",
            &rpc_url,
            "--runtime-manifest",
            cli_manifest.to_str().unwrap(),
            "--artifact",
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

    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(signed_tx_invoked_address(signed_tx), cli_wallet);
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
            "--artifact",
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
            "--artifact",
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
            "--artifact",
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
            "--artifact",
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
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    std::fs::write(
        project.join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\n",
    )
    .unwrap();

    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();

    let output = groundhog()
        .args([
            "--json",
            "deploy",
            "--dry-run",
            "--artifact",
            wasm_path.to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
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
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    // Write a minimal WASM file for the deploy to read.
    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();

    let output = groundhog()
        .args([
            "--json",
            "deploy",
            "--artifact",
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
    assert_eq!(
        stdout["data"]["schemaVersion"],
        groundhog_cli::deploy::DEPLOY_JSON_SCHEMA_VERSION
    );

    // The mock returns 0xbb..bb as the deployed address via return_data.
    let expected_address = format!("0x{}", "bb".repeat(32));
    assert_eq!(stdout["data"]["address"], expected_address);
    assert_eq!(stdout["data"]["address_source"], "receipt");
    assert_eq!(stdout["data"]["height"], 42);
    assert_eq!(stdout["data"]["status"]["result"], "ok");
    assert!(stdout["data"].get("return_data").is_none());
    assert!(stdout["data"].get("abort_data").is_none());
    assert!(stdout["data"]["transaction_detail"].is_null());
    assert!(
        stdout["data"]["tx_hash"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
}

#[test]
fn deploy_live_json_fails_with_receipt_context() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_failed_deploy_receipt(42),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();
    let output = groundhog()
        .args([
            "--json",
            "deploy",
            "--artifact",
            wasm_path.to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["command"], "deploy");
    assert_eq!(stdout["error"]["code"], "transaction_deterministic_error");
    assert_eq!(stdout["data"]["height"], 42);
    assert_eq!(stdout["data"]["status"]["result"], "deterministicError");
    assert_eq!(stdout["data"]["return_data"], "0x0102");
    assert_eq!(stdout["data"]["abort_data"], "0x0304");
    assert!(
        stdout["data"]["tx_hash"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
}

#[test]
fn deploy_live_human_fails_after_printing_receipt_context() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_failed_deploy_receipt(42),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();
    let output = groundhog()
        .args(["deploy", "--artifact", wasm_path.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning: Deploy failed with deterministic execution error"));
    assert!(stderr.contains("tx: 0x"));
    assert!(stderr.contains("height: 42"));
    assert!(stderr.contains("return data: 0x0102"), "stderr: {stderr}");
    assert!(stderr.contains("abort data: 0x0304"), "stderr: {stderr}");
}

#[test]
fn deploy_live_json_includes_transaction_detail_when_dev_method_exists() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) =
        spawn_deploy_rpc_with_detail(42, DevDetailRpcResponse::Result(sample_detail_result(42)));
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let wasm_path = tmp.path().join("test.wasm");
    std::fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();

    let output = groundhog()
        .args([
            "--json",
            "deploy",
            "--artifact",
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
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .args(["--json", "call", "0x1234", "transfer"])
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "call");
    assert_eq!(stderr["error"]["code"], "no_active_profile");
}

#[test]
fn storage_json_fails_without_config() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "11".repeat(32));
    let key = format!("0x{}", "42".repeat(32));
    let output = groundhog()
        .args(["--json", "storage", &address, &key])
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "storage");
    assert_eq!(stderr["error"]["code"], "no_active_profile");
}

#[test]
fn view_json_fails_without_config() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["--json", "view", &address, "0x0102030405060708"])
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "view");
    assert_eq!(stderr["error"]["code"], "no_active_profile");
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
    write_project_config_with_dummy_alice_wallet(&project, &rpc_url);

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
    // Abort fields stay absent (not null) on non-abort payloads so the
    // v1 schema shape is unchanged for existing consumers.
    assert!(stdout["data"].get("abortException").is_none());
    assert!(stdout["data"].get("abortValue").is_none());
    assert!(stdout["data"].get("abortDecodeError").is_none());
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.contracts]\ncounter = \"{address}\"\n"
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
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
    write_project_config_with_dummy_alice_wallet(&project, &rpc_url);

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
    let decode_error = stdout["data"]["abortDecodeError"].as_str().unwrap();
    assert!(decode_error.contains("does not match"), "{decode_error}");
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
    write_project_config_with_dummy_alice_wallet(&project, &rpc_url);

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
    write_project_config_with_dummy_alice_wallet(&project, &rpc_url);

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
    assert!(stderr.contains("View succeeded"));
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
    write_project_config_with_dummy_alice_wallet(&project, &rpc_url);

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
fn balance_json_reads_native_hog_balance() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let owner = format!("0x{}", "12".repeat(32));
    let (rpc_url, rpc_server) = spawn_balance_rpc(&address, "123456789");
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.wallets.alice]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "balance", "alice"])
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
    assert_eq!(stdout["command"], "balance");
    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(stdout["data"]["target"], "alice");
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["raw_balance"], "123456789");
    assert_eq!(stdout["data"]["balance_hog"], "12.3456789");
    assert_eq!(stdout["data"]["height"], 34);
}

#[test]
fn balance_json_uses_global_default_profile_when_profile_omitted() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(config_home.join("groundhog")).unwrap();

    let address = format!("0x{}", "77".repeat(32));
    let owner = format!("0x{}", "78".repeat(32));
    let (rpc_url, rpc_server) = spawn_balance_rpc(&address, "70000000");
    std::fs::write(
        config_home.join("groundhog/config.toml"),
        format!(
            "default_profile = \"devnet\"\n\n[profile.devnet]\nrpc_url = \"{rpc_url}\"\n\n[profile.devnet.wallets.alice]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "balance", "alice"])
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
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["balance_hog"], "7");
}

#[test]
fn balance_json_cli_rpc_url_overrides_profile() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "22".repeat(32));
    let (rpc_url, rpc_server) = spawn_balance_rpc(&address, "42");
    std::fs::write(
        project.join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "balance", "--rpc-url", &rpc_url, &address])
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
    assert_eq!(stdout["command"], "balance");
    assert_eq!(stdout["data"]["rpc_url"], rpc_url);
    assert_eq!(stdout["data"]["target"], address);
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["raw_balance"], "42");
}

#[test]
fn balance_json_rejects_invalid_cli_rpc_url_before_rpc() {
    let address = format!("0x{}", "22".repeat(32));
    let output = groundhog()
        .args(["--json", "balance", "--rpc-url", "not-a-url", &address])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "balance");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .expect("bug: error message should be string")
            .contains("--rpc-url must start with http:// or https://")
    );
}

#[test]
fn balance_json_rejects_malformed_address_target_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "balance", "0xdeadbeef"])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["command"], "balance");
    assert_eq!(stderr["error"]["code"], "invalid_address_target");
    assert_eq!(stderr["error"]["details"]["target"], "0xdeadbeef");
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("saved wallet")
    );
}

#[test]
fn balance_json_rejects_unknown_target_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "balance", "missing"])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["command"], "balance");
    assert_eq!(stderr["error"]["code"], "unknown_address_target");
    assert_eq!(stderr["error"]["details"]["target"], "missing");
}

#[test]
fn storage_json_uses_profile_runtime_manifest_with_global_dev_key() {
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
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

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
fn storage_json_cli_rpc_url_overrides_profile_rpc_url() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = format!("0x{}", "11".repeat(32));
    let key = format!("0x{}", "42".repeat(32));
    let manifest_path =
        write_runtime_manifest(tmp.path(), 0, &advertised_dev_wallet_address(0), None);
    let (rpc_url, rpc_server) = spawn_storage_rpc(&address, &key);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "storage", "--rpc-url", &rpc_url, &address, &key])
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.contracts]\ncounter = \"{address}\"\n"
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
fn contract_commands_report_unknown_alias_before_rpc() {
    let key = format!("0x{}", "42".repeat(32));
    for (command, args) in [
        (
            "call",
            vec!["call", "missing", "0x0102030405060708", "0x", "--dry-run"],
        ),
        (
            "view",
            vec!["view", "missing", "0x0102030405060708", "--calldata", "0x"],
        ),
        ("storage", vec!["storage", "missing", key.as_str()]),
    ] {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("groundhog.toml");
        std::fs::write(
            &config_path,
            concat!(
                "default_profile = \"local\"\n\n",
                "[profile.local]\n",
                "[profile.local.contracts]\n",
                "zebra = \"0x1111111111111111111111111111111111111111111111111111111111111111\"\n",
                "alpha = \"0x2222222222222222222222222222222222222222222222222222222222222222\"\n",
            ),
        )
        .unwrap();
        let config_path = std::fs::canonicalize(config_path).unwrap();
        let (rpc_url, rpc_server) = spawn_no_request_rpc();
        let output = groundhog()
            .arg("--json")
            .args(args)
            .args(["--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["command"], command);
        assert_eq!(stderr["error"]["code"], "unknown_contract_alias");
        assert_eq!(stderr["error"]["details"]["alias"], "missing");
        assert_eq!(stderr["error"]["details"]["profile"], "local");
        assert_eq!(
            stderr["error"]["details"]["configSource"],
            config_path.to_string_lossy().as_ref()
        );
        assert_eq!(
            stderr["error"]["details"]["availableAliases"],
            serde_json::json!(["alpha", "zebra"])
        );
    }
}

#[test]
fn contract_aliases_are_isolated_by_profile() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("groundhog.toml");
    std::fs::write(
        &config_path,
        concat!(
            "default_profile = \"local\"\n\n",
            "[profile.local]\n",
            "[profile.local.contracts]\n",
            "counter = \"0x1111111111111111111111111111111111111111111111111111111111111111\"\n\n",
            "[profile.devnet]\n",
            "[profile.devnet.contracts]\n",
            "token = \"0x2222222222222222222222222222222222222222222222222222222222222222\"\n",
        ),
    )
    .unwrap();
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = groundhog()
        .args([
            "--json",
            "storage",
            "counter",
            &key,
            "--profile",
            "devnet",
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "unknown_contract_alias");
    assert_eq!(stderr["error"]["details"]["profile"], "devnet");
    assert_eq!(
        stderr["error"]["details"]["availableAliases"],
        serde_json::json!(["token"])
    );
}

#[test]
fn contract_alias_error_is_actionable_and_terminal_safe() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        "default_profile = \"local\"\n\n[profile.local]\n",
    )
    .unwrap();
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = groundhog()
        .args([
            "storage",
            "missing\n\u{1b}[31m",
            &key,
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unknown contract alias `missing\\n\\u{1b}[31m` in profile `local`"));
    assert!(stderr.contains("groundhog config get local --sources"));
    assert!(!stderr.contains("missing\n"));
    assert!(!stderr.contains('\u{1b}'));
}

#[test]
fn contract_command_preserves_prefixed_address_validation_error() {
    let tmp = TempDir::new().unwrap();
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = groundhog()
        .args([
            "--json",
            "storage",
            "0xdeadbeef",
            &key,
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "invalid_address_target");
    assert_eq!(stderr["error"]["details"]["target"], "0xdeadbeef");
    assert!(
        stderr["error"]["details"]["message"]
            .as_str()
            .unwrap()
            .contains("32 bytes")
    );
}

#[test]
fn storage_json_rejects_short_key_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
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
    let owner = write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    let trading = format!("0x{}", "bb".repeat(32));
    let mut config = std::fs::read_to_string(project.join("groundhog.toml")).unwrap();
    write!(
        config,
        "\n[profile.default.wallets.trading]\naddress = \"{trading}\"\nowner_public_key = \"{owner}\"\n"
    )
    .unwrap();
    std::fs::write(project.join("groundhog.toml"), config).unwrap();

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args([
            "--json",
            "call",
            "--dry-run",
            "--wallet",
            "trading",
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
    let signed_tx = decode_signed_tx(stdout["data"]["signed_tx"].as_str().unwrap());
    assert_eq!(
        format!(
            "0x{}",
            hex::encode(signed_tx.tx.invocation.invoked_address.as_ref())
        ),
        trading
    );
}

#[test]
fn call_dry_run_uses_hosted_descriptor_entrypoint() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    add_faucet_to_default_profile(&project, &faucet_url);
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

    descriptor_server.join().unwrap();
    rpc_server.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["wallet_entrypoint_id"], "0xa0fff94c64fbaf3c");
    assert_eq!(
        stdout["data"]["wallet_entrypoint_source"],
        "network_descriptor"
    );
    assert_eq!(
        signed_tx_entrypoint(stdout["data"]["signed_tx"].as_str().unwrap()),
        "0xa0fff94c64fbaf3c"
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
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
    assert_eq!(signed_tx_entrypoint(signed_tx), "0x646f416374696f6e");
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
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

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
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

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
fn call_live_json_includes_diagnostics_for_failed_local_dev_call() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &advertised_wallet, None);
    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_deterministic_error_call_receipt(42),
        DevDetailRpcResponse::MethodNotFound,
    );
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["--json", "call", &contract, "0x0123456789abcdef", "0x0102"])
        .current_dir(&project)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["error"]["code"], "transaction_deterministic_error");
    assert_eq!(stdout["data"]["status"]["result"], "deterministicError");
    assert_eq!(
        stdout["data"]["diagnostics"]["wallet"]["address"],
        advertised_wallet
    );
    assert_eq!(
        stdout["data"]["diagnostics"]["wallet"]["entrypoint_id"],
        "0x646f416374696f6e"
    );
    assert_eq!(
        stdout["data"]["diagnostics"]["wallet"]["entrypoint_source"],
        "runtime_manifest"
    );
    assert_eq!(
        stdout["data"]["diagnostics"]["target"]["entrypoint_id"],
        "0x0123456789abcdef"
    );
    assert_eq!(
        stdout["data"]["diagnostics"]["transaction_detail"]["rpc_method"],
        "groundhog_dev_getTransactionDetail"
    );
    assert_eq!(
        stdout["data"]["diagnostics"]["transaction_detail"]["available"],
        false
    );
}

#[test]
fn call_live_json_marks_legacy_default_wallet_entrypoint_without_manifest() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_deterministic_error_call_receipt(42),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["--json", "call", &contract, "0x0123456789abcdef", "0x0102"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["status"]["result"], "deterministicError");
    assert_eq!(
        stdout["data"]["diagnostics"]["wallet"]["entrypoint_id"],
        "0x19846d750fad81d6"
    );
    assert_eq!(
        stdout["data"]["diagnostics"]["wallet"]["entrypoint_source"],
        "legacy_default"
    );
}

#[test]
fn call_live_human_fails_after_printing_receipt_context() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_deterministic_error_call_receipt(42),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["call", &contract, "0x0123456789abcdef", "0x0102"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning: Call failed with deterministic execution error"));
    assert!(stderr.contains("contract: 0x"));
    assert!(stderr.contains("tx: 0x"));
    assert!(stderr.contains("height: 42"));
    assert!(stderr.contains("wallet: 0x"));
}

#[test]
fn call_live_json_decodes_library_abort() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let abort_hex = library_abort_hex("NonNegativeIntUnderflow");
    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_reverted_call_receipt(42, &abort_hex),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["--json", "call", &contract, "0x0123456789abcdef", "0x0102"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["error"]["code"], "transaction_reverted");
    assert_eq!(stdout["data"]["status"]["result"], "reverted");
    assert_eq!(stdout["data"]["abort_data"], abort_hex);
    assert_eq!(stdout["data"]["abort_exception"], "LibraryExn");
    assert_eq!(
        stdout["data"]["abort_value"],
        serde_json::json!({"case": "NonNegativeIntUnderflow"})
    );
}

#[test]
fn call_live_human_prints_decoded_abort() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let abort_hex = library_abort_hex("NonNegativeIntUnderflow");
    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_reverted_call_receipt(42, &abort_hex),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["call", &contract, "0x0123456789abcdef", "0x0102"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: Call reverted\n"),
        "stderr: {stderr}"
    );
    // The raw buffer is elided when it decodes, like the success path's
    // return data.
    assert!(!stderr.contains("abort data:"), "stderr: {stderr}");
    assert!(
        stderr.contains("abort type: LibraryExn"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("abort value: {\"case\":\"NonNegativeIntUnderflow\"}"),
        "stderr: {stderr}"
    );
}

#[test]
fn call_live_json_omits_abort_fields_on_success() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_call_receipt(42),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["--json", "call", &contract, "0x0123456789abcdef", "0x0102"])
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
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["data"]["status"]["result"], "ok");
    assert!(stdout["data"].get("abort_exception").is_none());
    assert!(stdout["data"].get("abort_value").is_none());
    assert!(stdout["data"].get("abort_decode_error").is_none());
}

#[test]
fn call_live_json_reports_abort_decode_error() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    // Selector 0 is reserved and matches no exception; the payload byte
    // is parsed off after the selector and reported separately.
    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_reverted_call_receipt(42, "0x00ab"),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["--json", "call", &contract, "0x0123456789abcdef", "0x0102"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["data"]["abort_data"], "0x00ab");
    assert_eq!(stdout["data"]["abort_value"], serde_json::Value::Null);
    let decode_error = stdout["data"]["abort_decode_error"].as_str().unwrap();
    assert!(
        decode_error.contains("0x0000000000000000"),
        "{decode_error}"
    );
    assert!(decode_error.contains("does not match"), "{decode_error}");
    assert!(decode_error.contains("0xab"), "{decode_error}");
}

#[test]
fn call_live_human_reports_abort_decode_error() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(
        42,
        sample_reverted_call_receipt(42, "0x00ab"),
        DevDetailRpcResponse::MethodNotFound,
    );
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = format!("0x{}", "11".repeat(32));
    let output = groundhog()
        .args(["call", &contract, "0x0123456789abcdef", "0x0102"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("abort data: 0x00ab"), "stderr: {stderr}");
    assert!(
        stderr.contains("abort decode error: abort selector 0x0000000000000000"),
        "stderr: {stderr}"
    );
}

#[test]
fn call_fails_without_config() {
    let tmp = TempDir::new().unwrap();
    let output = groundhog()
        .arg("call")
        .arg("0x1234")
        .arg("transfer")
        .env("XDG_CONFIG_HOME", tmp.path())
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

#[test]
fn tx_show_json_returns_committed_status_and_matching_receipt() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        "[profile.devnet]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    let hash = format!("0x{}", "ab".repeat(32));
    let event = serde_json::json!({
        "contractAddress": format!("0x{}", "11".repeat(32)),
        "topics": [format!("0x{}", "22".repeat(32))],
        "data": "0x0304",
    });
    let receipt = serde_json::json!({
        "txHash": hash,
        "height": 42,
        "status": { "result": "ok" },
        "events": [event.clone()],
        "returnData": "0x0506",
        "abortData": "0x0708",
    });
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        TxShowRpcResponse::Result(serde_json::json!({
            "status": "committed",
            "height": 42,
        })),
        Some(TxShowRpcResponse::Result(receipt.clone())),
    );

    let output = groundhog()
        .args([
            "--json",
            "tx",
            "show",
            &hash.to_uppercase(),
            "--profile",
            "devnet",
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
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
    assert_eq!(stdout["command"], "tx.show");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(stdout["data"]["schemaVersion"], 1);
    assert_eq!(stdout["data"]["profile"], "devnet");
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["txHash"], hash);
    assert_eq!(stdout["data"]["status"]["status"], "committed");
    assert_eq!(stdout["data"]["status"]["height"], 42);
    assert_eq!(stdout["data"]["receipt"], receipt);
    assert_eq!(stdout["data"]["receipt"]["events"][0], event);
}

#[test]
fn tx_show_binary_accepts_unprefixed_and_mixed_case_hashes() {
    let canonical = format!("0x{}", "ab".repeat(32));
    for input in ["aB".repeat(32), format!("0x{}", "Ab".repeat(32))] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(
            &canonical,
            TxShowRpcResponse::Result(serde_json::json!({ "status": "pending" })),
            None,
        );
        let output = groundhog()
            .args(["--json", "tx", "show", &input, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(output.status.success());
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(stdout["data"]["txHash"], canonical);
    }
}

#[test]
fn tx_namespace_help_advertises_show_and_list() {
    let output = groundhog().args(["tx", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("show"));
    assert!(stdout.contains("list"));
    assert!(stdout.contains("transactions and address history"));
}

#[test]
fn tx_show_help_describes_one_shot_public_lookup() {
    let output = groundhog().args(["tx", "show", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("one-shot public transaction lookup without waiting"));
    assert!(stdout.contains("public status and receipt fields"));
    assert!(stdout.contains("Raw transaction bodies"));
    assert!(stdout.contains("deep traces are not available"));
    assert!(stdout.contains("--profile"));
    assert!(stdout.contains("--rpc-url"));
    assert!(stdout.contains("--runtime-manifest"));
}

#[test]
fn tx_show_human_output_includes_raw_committed_receipt_data() {
    let tmp = TempDir::new().unwrap();
    let hash = format!("0x{}", "ac".repeat(32));
    let event = serde_json::json!({
        "contractAddress": format!("0x{}", "11".repeat(32)),
        "topics": [],
        "data": "0xaabb",
    });
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        TxShowRpcResponse::Result(serde_json::json!({
            "status": "committed",
            "height": 43,
        })),
        Some(TxShowRpcResponse::Result(serde_json::json!({
            "txHash": hash,
            "height": 43,
            "status": { "result": "reverted", "message": "bad\ncall" },
            "events": [event],
            "returnData": "0xccdd",
            "abortData": "0xeeff",
        }))),
    );

    let output = groundhog()
        .args(["tx", "show", &hash, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("status: committed"));
    assert!(stderr.contains("height: 43"));
    assert!(stderr.contains(&format!("receipt hash: {hash}")));
    assert!(stderr.contains("receipt height: 43"));
    assert!(stderr.contains(r"execution: reverted: bad\ncall"));
    assert!(stderr.contains(r#"event: {"contractAddress""#));
    assert!(stderr.contains("return data: 0xccdd"));
    assert!(stderr.contains("abort data: 0xeeff"));
}

#[test]
fn tx_show_non_committed_statuses_succeed_without_receipt_request() {
    let hash = format!("0x{}", "bc".repeat(32));
    let statuses = [
        serde_json::json!({ "status": "pending" }),
        serde_json::json!({ "status": "invalid", "reason": "bad transaction" }),
        serde_json::json!({
            "status": "recentFailure",
            "attemptHeight": 17,
            "conflict": {
                "address": format!("0x{}", "11".repeat(32)),
                "key": "0xaabb",
                "code": "boundsExceeded",
            },
        }),
        serde_json::json!({ "status": "unknown" }),
        serde_json::json!({ "status": "futureStatus", "futureField": 1 }),
    ];

    for (index, status) in statuses.into_iter().enumerate() {
        let expected_status = if index == 4 {
            serde_json::json!({ "status": "other" })
        } else {
            status.clone()
        };
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) =
            spawn_tx_show_rpc(&hash, TxShowRpcResponse::Result(status), None);
        let output = groundhog()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(
            output.status.success(),
            "status case {index} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(stdout["data"]["receipt"].is_null());
        assert_eq!(stdout["data"]["status"], expected_status);
    }
}

#[test]
fn tx_show_committed_without_receipt_preserves_status_and_null_receipt() {
    let tmp = TempDir::new().unwrap();
    let hash = format!("0x{}", "cd".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        TxShowRpcResponse::Result(serde_json::json!({
            "status": "committed",
            "height": 51,
        })),
        Some(TxShowRpcResponse::Result(serde_json::Value::Null)),
    );

    let output = groundhog()
        .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        stdout["data"],
        serde_json::json!({
            "schemaVersion": 1,
            "profile": "default",
            "rpcUrl": rpc_url,
            "txHash": hash,
            "status": { "status": "committed", "height": 51 },
            "receipt": null,
        })
    );
}

#[test]
fn tx_show_matching_receipt_omits_absent_optional_fields() {
    let tmp = TempDir::new().unwrap();
    let hash = format!("0x{}", "cf".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        TxShowRpcResponse::Result(serde_json::json!({
            "status": "committed",
            "height": 52,
        })),
        Some(TxShowRpcResponse::Result(serde_json::json!({
            "txHash": hash,
            "height": 52,
            "status": { "result": "ok" },
            "events": [],
        }))),
    );

    let output = groundhog()
        .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        stdout["data"]["receipt"],
        serde_json::json!({
            "txHash": hash,
            "height": 52,
            "status": { "result": "ok" },
            "events": [],
        })
    );
    assert!(stdout["data"]["receipt"].get("returnData").is_none());
    assert!(stdout["data"]["receipt"].get("abortData").is_none());
}

#[test]
fn tx_show_human_output_distinguishes_unavailable_receipt_and_unknown_status() {
    let hash = format!("0x{}", "d1".repeat(32));
    for (status, receipt, expected, forbidden) in [
        (
            serde_json::json!({ "status": "committed", "height": 53 }),
            Some(TxShowRpcResponse::Result(serde_json::Value::Null)),
            "receipt: unavailable",
            "status: invalid",
        ),
        (
            serde_json::json!({ "status": "unknown" }),
            None,
            "status: unknown (not found in the mempool or indexer)",
            "status: invalid",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) =
            spawn_tx_show_rpc(&hash, TxShowRpcResponse::Result(status), receipt);
        let output = groundhog()
            .args(["tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(expected));
        assert!(!stderr.contains(forbidden));
        if expected == "receipt: unavailable" {
            assert!(stderr.contains("status: committed"));
            assert!(stderr.contains("height: 53"));
        }
    }
}

#[test]
fn tx_show_uses_profile_rpc_url_without_cli_override() {
    let tmp = TempDir::new().unwrap();
    let hash = format!("0x{}", "ce".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        TxShowRpcResponse::Result(serde_json::json!({ "status": "pending" })),
        None,
    );
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        format!("[profile.devnet]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "tx", "show", &hash, "--profile", "devnet"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["profile"], "devnet");
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
}

#[test]
fn tx_show_rejects_receipt_hash_and_height_mismatches() {
    let hash = format!("0x{}", "de".repeat(32));
    for (name, receipt) in [
        (
            "hash",
            serde_json::json!({
                "txHash": format!("0x{}", "ef".repeat(32)),
                "height": 61,
                "status": { "result": "ok" },
                "events": [],
            }),
        ),
        (
            "height",
            serde_json::json!({
                "txHash": hash,
                "height": 62,
                "status": { "result": "ok" },
                "events": [],
            }),
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(
            &hash,
            TxShowRpcResponse::Result(serde_json::json!({
                "status": "committed",
                "height": 61,
            })),
            Some(TxShowRpcResponse::Result(receipt)),
        );
        let output = groundhog()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "rpc_method_failed");
        assert!(
            stderr["error"]["message"]
                .as_str()
                .unwrap()
                .contains(&format!("receipt {name}"))
        );
    }
}

#[test]
fn tx_show_maps_only_unsupported_rpc_codes_to_capability_error() {
    let hash = format!("0x{}", "ef".repeat(32));
    for code in [-32601, -32052] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(
            &hash,
            TxShowRpcResponse::Error(code, "unsupported\nmethod"),
            None,
        );
        let output = groundhog()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "rpc_capability_unsupported");
        assert_eq!(stderr["error"]["details"]["method"], "getTransactionStatus");
        assert!(
            stderr["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("full local devnode")
        );
        assert!(!stderr["error"]["message"].as_str().unwrap().contains('\n'));
    }

    for code in [-32602, -32050, -32051, -32053, -32099] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(
            &hash,
            TxShowRpcResponse::Error(code, "ordinary RPC error"),
            None,
        );
        let output = groundhog()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "rpc_method_failed");
    }

    let output = groundhog()
        .args([
            "--json",
            "tx",
            "show",
            &hash,
            "--rpc-url",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "rpc_method_failed");
}

#[test]
fn tx_show_human_error_escapes_invalid_rpc_url() {
    let hash = format!("0x{}", "ab".repeat(32));
    let rpc_url = "http://[bad\n\u{1b}[31m";
    let output = groundhog()
        .args(["tx", "show", &hash, "--rpc-url", rpc_url])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains('\u{1b}'));
    assert!(!stderr.contains("http://[bad\n"));
    assert!(stderr.contains(r"http://[bad\n\u{1b}[31m"));
}

#[test]
fn tx_show_receipt_rpc_errors_remain_failures() {
    let tmp = TempDir::new().unwrap();
    let hash = format!("0x{}", "f1".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        TxShowRpcResponse::Result(serde_json::json!({
            "status": "committed",
            "height": 71,
        })),
        Some(TxShowRpcResponse::Error(-32052, "receipt unsupported")),
    );

    let output = groundhog()
        .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "rpc_capability_unsupported");
    assert_eq!(
        stderr["error"]["details"]["method"],
        "getTransactionReceipt"
    );
}

#[test]
fn tx_show_rejects_invalid_hash_before_rpc() {
    for hash in [
        "0xdeadbeef".to_owned(),
        format!("0x{}", "ab".repeat(33)),
        "not-hex".into(),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_no_request_rpc();
        let output = groundhog()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["command"], "tx.show");
        assert_eq!(stderr["error"]["code"], "invalid_transaction_hash");
        assert_eq!(stderr["error"]["details"]["input"], hash);
    }
}

#[test]
fn tx_show_human_error_escapes_invalid_hash_input() {
    let output = groundhog()
        .args([
            "tx",
            "show",
            "bad\n\u{1b}[31m",
            "--rpc-url",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r"bad\n\u{1b}[31m"));
    assert!(!stderr.contains("bad\n"));
    assert!(!stderr.contains('\u{1b}'));
}

#[test]
fn tx_show_human_output_escapes_untrusted_status_text() {
    let tmp = TempDir::new().unwrap();
    let hash = format!("0x{}", "f2".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        TxShowRpcResponse::Result(serde_json::json!({
            "status": "invalid",
            "reason": "bad\nreason\u{001b}[31m\u{202e}spoof",
        })),
        None,
    );

    let output = groundhog()
        .args(["tx", "show", &hash, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("status: invalid"));
    assert!(stderr.contains(r"bad\nreason\u{1b}[31m\u{202e}spoof"));
    assert!(!stderr.contains('\u{1b}'));
    assert!(!stderr.contains('\u{202e}'));
}

#[test]
fn tx_list_help_describes_one_page_root_invocation_history() {
    let output = groundhog().args(["tx", "list", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("exactly one page"));
    assert!(stdout.contains("root invocation"));
    assert!(stdout.contains("Incoming wallet-wrapped transfers may not appear"));
    assert!(stdout.contains("continuation cursor is opaque and reusable"));
    assert!(stdout.contains("Receipts are not fetched"));
    for option in [
        "--limit",
        "--cursor",
        "--profile",
        "--rpc-url",
        "--runtime-manifest",
    ] {
        assert!(stdout.contains(option));
    }
}

#[test]
fn tx_list_json_uses_wallet_first_resolution_and_default_query() {
    let tmp = TempDir::new().unwrap();
    let wallet_address = format!("0x{}", "11".repeat(32));
    let contract_address = format!("0x{}", "22".repeat(32));
    let owner = format!("0x{}", "33".repeat(32));
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        format!(
            "[profile.devnet]\nrpc_url = \"http://127.0.0.1:1\"\n\
             [profile.devnet.wallets.alice]\naddress = \"{wallet_address}\"\nowner_public_key = \"{owner}\"\n\
             [profile.devnet.contracts]\nalice = \"{contract_address}\"\n"
        ),
    )
    .unwrap();
    let first_hash = format!("0x{}", "44".repeat(32));
    let second_hash = format!("0x{}", "55".repeat(32));
    let expected_query = serde_json::json!({
        "address": wallet_address,
        "order": "descending",
        "anchor": { "kind": "atEnd" },
        "cursor": "0x",
        "limit": 50,
    });
    let page = serde_json::json!({
        "refs": [
            { "txHash": first_hash, "height": 90 },
            { "txHash": second_hash, "height": 80 },
        ],
        "cursor": "0xaabb",
        "latestBlock": 100,
        "oldestBlock": 7,
    });
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(expected_query, TxListRpcResponse::Result(page));

    let output = groundhog()
        .args([
            "--json",
            "tx",
            "list",
            "alice",
            "--profile",
            "devnet",
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
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
    assert_eq!(stdout["command"], "tx.list");
    assert_eq!(
        stdout["warnings"],
        serde_json::json!([{
            "level": "warn",
            "code": "root_invocation_history_only",
            "message": "history includes root invocations only; incoming wallet-wrapped transfers may not appear for recipients",
            "hint": "do not treat this page as a complete incoming or outgoing asset ledger",
        }])
    );
    assert_eq!(
        stdout["data"],
        serde_json::json!({
            "schemaVersion": 1,
            "profile": "devnet",
            "rpcUrl": rpc_url,
            "targetInput": "alice",
            "address": wallet_address,
            "refs": [
                { "txHash": first_hash, "height": 90 },
                { "txHash": second_hash, "height": 80 },
            ],
            "cursor": "0xaabb",
            "latestBlock": 100,
            "oldestBlock": 7,
        })
    );
    assert!(stdout["data"].get("order").is_none());
}

#[test]
fn tx_list_continuation_forwards_cursor_and_endpoint_owned_limit() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "66".repeat(32));
    let cursor_input = format!("0X{}", "Ab".repeat(257));
    let canonical_cursor = format!("0x{}", "ab".repeat(257));
    let expected_query = serde_json::json!({
        "address": address,
        "order": "descending",
        "anchor": { "kind": "atEnd" },
        "cursor": canonical_cursor,
        "limit": 201,
    });
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        expected_query,
        TxListRpcResponse::Result(serde_json::json!({
            "refs": [],
            "cursor": "0x0102",
            "latestBlock": 100,
            "oldestBlock": 0,
        })),
    );

    let output = groundhog()
        .args([
            "--json",
            "tx",
            "list",
            &address,
            "--limit",
            "201",
            "--cursor",
            &cursor_input,
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["refs"], serde_json::json!([]));
    assert_eq!(stdout["data"]["cursor"], "0x0102");
}

#[test]
fn tx_list_rejects_page_larger_than_requested_limit() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "65".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        serde_json::json!({
            "address": address,
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 1,
        }),
        TxListRpcResponse::Result(serde_json::json!({
            "refs": [
                { "txHash": format!("0x{}", "66".repeat(32)), "height": 2 },
                { "txHash": format!("0x{}", "67".repeat(32)), "height": 1 },
            ],
            "cursor": "0x",
            "latestBlock": 2,
            "oldestBlock": 0,
        })),
    );

    let output = groundhog()
        .args([
            "--json",
            "tx",
            "list",
            &address,
            "--limit",
            "1",
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "rpc_method_failed");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("returned 2 references for requested limit 1")
    );
}

#[test]
fn tx_list_accepts_empty_exhausted_page() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "67".repeat(32));
    let expected_query = serde_json::json!({
        "address": address,
        "order": "descending",
        "anchor": { "kind": "atEnd" },
        "cursor": "0x",
        "limit": 50,
    });
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        expected_query,
        TxListRpcResponse::Result(serde_json::json!({
            "refs": [],
            "cursor": "0x",
            "latestBlock": 9,
            "oldestBlock": 2,
        })),
    );

    let output = groundhog()
        .args([
            "--json",
            "tx",
            "list",
            &address,
            "--cursor",
            "0x",
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["refs"], serde_json::json!([]));
    assert_eq!(stdout["data"]["cursor"], "0x");
}

#[test]
fn tx_list_human_output_marks_exhausted_empty_page() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "68".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        serde_json::json!({
            "address": address,
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        TxListRpcResponse::Result(serde_json::json!({
            "refs": [],
            "cursor": "0x",
            "latestBlock": 9,
            "oldestBlock": 2,
        })),
    );

    let output = groundhog()
        .args(["tx", "list", &address, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("transactions: none (end of history)"));
    assert!(!stderr.contains("next cursor"));
}

#[test]
fn tx_list_resolves_contract_direct_and_manifest_dev_targets() {
    for target_kind in ["contract", "direct", "dev"] {
        let tmp = TempDir::new().unwrap();
        let address = format!(
            "0x{}",
            match target_kind {
                "contract" => "71",
                "direct" => "72",
                _ => "73",
            }
            .repeat(32)
        );
        let target = match target_kind {
            "contract" => "counter".to_owned(),
            "direct" => address.to_uppercase(),
            _ => "dev-1".to_owned(),
        };
        let (rpc_url, rpc_server) = spawn_tx_list_rpc(
            serde_json::json!({
                "address": address,
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": "0x",
                "limit": 50,
            }),
            TxListRpcResponse::Result(serde_json::json!({
                "refs": [],
                "cursor": "0x",
                "latestBlock": 1,
                "oldestBlock": 0,
            })),
        );
        match target_kind {
            "contract" => std::fs::write(
                tmp.path().join("groundhog.toml"),
                format!("default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n[profile.default.contracts]\ncounter = \"{address}\"\n"),
            )
            .unwrap(),
            "direct" => std::fs::write(
                tmp.path().join("groundhog.toml"),
                format!("default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
            )
            .unwrap(),
            _ => {
                let manifest = write_runtime_manifest(tmp.path(), 1, &address, None);
                std::fs::write(
                    tmp.path().join("groundhog.toml"),
                    format!(
                        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
                        manifest.file_name().unwrap().to_string_lossy()
                    ),
                )
                .unwrap();
            }
        }

        let output = groundhog()
            .args(["--json", "tx", "list", &target])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(
            output.status.success(),
            "{target_kind}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(stdout["data"]["targetInput"], target);
        assert_eq!(stdout["data"]["address"], address);
    }
}

#[test]
fn tx_list_rejects_invalid_limit_and_cursor_before_rpc() {
    let address = format!("0x{}", "81".repeat(32));
    for (flag, value, expected_code) in [
        ("--limit", "0", "invalid_transaction_history_limit"),
        ("--limit", "-1", "invalid_transaction_history_limit"),
        ("--limit", "one", "invalid_transaction_history_limit"),
        ("--limit", "4294967296", "invalid_transaction_history_limit"),
        ("--cursor", "0xabc", "invalid_transaction_cursor"),
        ("--cursor", "0xgg", "invalid_transaction_cursor"),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_no_request_rpc();
        let output = groundhog()
            .args([
                "--json",
                "tx",
                "list",
                &address,
                flag,
                value,
                "--rpc-url",
                &rpc_url,
            ])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["command"], "tx.list");
        assert_eq!(stderr["error"]["code"], expected_code);
        assert_eq!(stderr["error"]["details"]["input"], value);
    }
}

#[test]
fn tx_list_rejects_address_resolution_failures_before_rpc() {
    for (target, expected_code) in [
        ("missing\n\u{1b}[31m", "unknown_address_target"),
        ("0xdeadbeef", "invalid_address_target"),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_no_request_rpc();
        let output = groundhog()
            .args(["tx", "list", target, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(
                expected_code
                    .trim_end_matches("_target")
                    .replace('_', " ")
                    .as_str()
            ) || stderr.contains("address target")
        );
        assert!(!stderr.contains('\u{1b}'));
        assert!(!stderr.contains("missing\n"));
    }

    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n[profile.default.contracts]\nbroken = \"0xdeadbeef\"\n",
    )
    .unwrap();
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = groundhog()
        .args(["--json", "tx", "list", "broken", "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "invalid_address_target");
}

#[test]
fn tx_list_classifies_capability_and_other_rpc_errors() {
    let address = format!("0x{}", "82".repeat(32));
    let query = serde_json::json!({
        "address": address,
        "order": "descending",
        "anchor": { "kind": "atEnd" },
        "cursor": "0x",
        "limit": 50,
    });
    for (code, expected) in [
        (-32601, "rpc_capability_unsupported"),
        (-32052, "rpc_capability_unsupported"),
        (-32050, "rpc_method_failed"),
        (-32051, "rpc_method_failed"),
        (-32053, "rpc_method_failed"),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_list_rpc(
            query.clone(),
            TxListRpcResponse::Error(code, "mock list error"),
        );
        let output = groundhog()
            .args(["--json", "tx", "list", &address, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], expected);
        if expected == "rpc_capability_unsupported" {
            assert_eq!(
                stderr["error"]["details"]["method"],
                "getTransactionsByAddress"
            );
            assert!(
                stderr["error"]["hint"]
                    .as_str()
                    .unwrap()
                    .contains("full local devnode")
            );
        }
    }
}

#[test]
fn tx_list_human_output_marks_sparse_page_and_reusable_cursor() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "83".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        serde_json::json!({
            "address": address,
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        TxListRpcResponse::Result(serde_json::json!({
            "refs": [],
            "cursor": "0xcafe",
            "latestBlock": 20,
            "oldestBlock": 3,
        })),
    );

    let output = groundhog()
        .args(["tx", "list", &address, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("root invocations only"));
    assert!(stderr.contains("incoming wallet-wrapped transfers may not appear"));
    assert!(!stderr.contains("target:"));
    assert!(stderr.contains(&format!("address: {address}")));
    assert!(stderr.contains("transactions: none in this page"));
    assert!(stderr.contains("next cursor: 0xcafe"));
    assert!(
        stderr.contains("transactions: none in this page\n\n  next cursor: 0xcafe"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("latest indexed block: 20"));
    assert!(stderr.contains("oldest retained block: 3"));
    assert!(!stderr.contains("order:"));
}

#[test]
fn tx_list_human_output_shows_each_reference_without_receipts() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "84".repeat(32));
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n[profile.default.wallets.local-wallet]\naddress = \"{address}\"\nowner_public_key = \"0x{}\"\n",
            "86".repeat(32)
        ),
    )
    .unwrap();
    let tx_hash = format!("0x{}", "85".repeat(32));
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        serde_json::json!({
            "address": address,
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        TxListRpcResponse::Result(serde_json::json!({
            "refs": [{ "txHash": tx_hash, "height": 18 }],
            "cursor": "0x",
            "latestBlock": 20,
            "oldestBlock": 3,
        })),
    );

    let output = groundhog()
        .args(["tx", "list", "local-wallet", "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("target: local-wallet"));
    assert!(stderr.contains(&format!("address: {address}")));
    assert!(stderr.contains(&format!("tx: {tx_hash} at height 18")));
    assert!(!stderr.contains("receipt"));
}

fn write_event_spec(tmp: &TempDir) -> std::path::PathBuf {
    use groundhog_contract_spec::{
        ContractEventDef, ContractField, ContractSpec, ContractSpecString, ContractType,
    };
    use xdr_common::VecM;

    let string = |value: &str| ContractSpecString::try_from(value).unwrap();
    let spec = ContractSpec {
        interfaces: VecM(vec![]),
        type_defs: VecM(vec![]),
        events: VecM(vec![ContractEventDef {
            name: string("PingEvent"),
            signature: [0x91; 32].into(),
            topics: VecM(vec![ContractField {
                name: string("tag"),
                ty: ContractType::U64,
            }]),
            fields: VecM(vec![ContractField {
                name: string("value"),
                ty: ContractType::U64,
            }]),
        }]),
    };
    let path = tmp.path().join("counter.spec.xdr");
    std::fs::write(&path, xdrs::txs::encode(&spec).unwrap()).unwrap();
    path
}

#[test]
fn events_list_help_describes_raw_filtered_single_page_query() {
    let output = groundhog()
        .args(["events", "list", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("exactly one newest-first page"));
    assert!(stdout.contains("global event scan is not performed"));
    assert!(stdout.contains("Repeat a topic option for OR within that position"));
    assert!(stdout.contains("Raw topics and data are authoritative"));
    assert!(stdout.contains("cursor is opaque and must be reused"));
    for option in [
        "--topic0",
        "--topic1",
        "--topic2",
        "--limit",
        "--cursor",
        "--spec",
        "--event",
        "--profile",
        "--rpc-url",
        "--runtime-manifest",
    ] {
        assert!(stdout.contains(option));
    }
}

#[test]
fn events_spec_selected_event_injects_filter_and_decodes_payload() {
    let tmp = TempDir::new().unwrap();
    let spec_path = write_event_spec(&tmp);
    let contract = format!("0x{}", "81".repeat(32));
    let signature = format!("0x{}", "91".repeat(32));
    let tag = format!("0x{}", "92".repeat(32));
    let data = format!("0x{}", hex::encode(xdrs::cxs::encode(&7_u64).unwrap()));
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [contract],
                "topics": { "topic0": [signature, signature], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [{
                "txHash": format!("0x{}", "82".repeat(32)),
                "blockHeight": 9,
                "sequenceNumber": 1,
                "event": {
                    "contractAddress": contract,
                    "topics": [signature, tag],
                    "data": data,
                },
            }],
            "cursor": "0x",
            "latestBlock": 9,
            "oldestBlock": 1,
        })),
    );

    let output = groundhog()
        .args([
            "--json",
            "events",
            "list",
            &contract,
            "--spec",
            spec_path.to_str().unwrap(),
            "--event",
            "PingEvent",
            "--topic0",
            &signature,
            "--topic0",
            &signature,
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["schemaVersion"], 1);
    assert_eq!(
        stdout["data"]["filters"]["topic0"],
        serde_json::json!([signature, signature])
    );
    assert_eq!(stdout["data"]["spec"]["source"], "userSupplied");
    assert_eq!(
        stdout["data"]["spec"]["path"],
        spec_path.to_string_lossy().as_ref()
    );
    assert_eq!(stdout["data"]["spec"]["selectedEvent"], "PingEvent");
    let event = &stdout["data"]["events"][0];
    assert_eq!(event["decodeStatus"], "decoded");
    assert_eq!(event["decoded"]["name"], "PingEvent");
    assert_eq!(event["decoded"]["topics"]["tag"], tag);
    assert_eq!(event["decoded"]["fields"]["value"], 7);
    assert_eq!(event["data"], data);
}

#[test]
fn events_spec_human_output_uses_named_topics_without_raw_duplicates() {
    let tmp = TempDir::new().unwrap();
    let spec_path = write_event_spec(&tmp);
    let contract = format!("0x{}", "93".repeat(32));
    let signature = format!("0x{}", "91".repeat(32));
    let tag = format!("0x{}", "92".repeat(32));
    let data = format!("0x{}", hex::encode(xdrs::cxs::encode(&7_u64).unwrap()));
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [contract],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [{
                "txHash": format!("0x{}", "94".repeat(32)),
                "blockHeight": 9,
                "sequenceNumber": 1,
                "event": {
                    "contractAddress": contract,
                    "topics": [signature, tag],
                    "data": data,
                },
            }],
            "cursor": "0x",
            "latestBlock": 9,
            "oldestBlock": 1,
        })),
    );

    let output = groundhog()
        .args([
            "events",
            "list",
            &contract,
            "--spec",
            spec_path.to_str().unwrap(),
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "\nevent\n",
        "height: 9",
        "sequence: 1",
        "txn: 0x",
        "decoded event: PingEvent",
        &format!("signature: {signature}"),
        &format!("tag: {tag}"),
        &format!("data: {data}"),
        "value: 7",
    ] {
        assert!(
            stderr.contains(expected),
            "missing `{expected}` in: {stderr}"
        );
    }
    for duplicate in ["topic 0:", "topic 1:", "decoded topic", "decoded field"] {
        assert!(
            !stderr.contains(duplicate),
            "unexpected `{duplicate}` in: {stderr}"
        );
    }
    assert_eq!(stderr.matches("emitter:").count(), 1, "stderr: {stderr}");
}

#[test]
fn events_spec_rejects_option_and_artifact_failures_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let contract = format!("0x{}", "83".repeat(32));
    std::fs::write(tmp.path().join("malformed.spec.xdr"), [0xff]).unwrap();
    std::fs::write(tmp.path().join("unsupported.spec.json"), b"{}").unwrap();
    std::fs::write(
        tmp.path().join("no-spec.wasm"),
        [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
    )
    .unwrap();
    let cases = [
        (vec!["--event", "PingEvent"], "invalid_event_options"),
        (vec!["--spec", "missing.spec.xdr"], "invalid_event_spec"),
        (vec!["--spec", "malformed.spec.xdr"], "invalid_event_spec"),
        (
            vec!["--spec", "unsupported.spec.json"],
            "invalid_event_spec",
        ),
        (vec!["--spec", "no-spec.wasm"], "invalid_event_spec"),
    ];

    for (options, code) in cases {
        let (rpc_url, rpc_server) = spawn_no_request_rpc();
        let output = groundhog()
            .args(["--json", "events", "list", &contract, "--rpc-url", &rpc_url])
            .args(options)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();
        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], code);
    }

    let spec_path = write_event_spec(&tmp);
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = groundhog()
        .args([
            "--json",
            "events",
            "list",
            &contract,
            "--rpc-url",
            &rpc_url,
            "--spec",
            spec_path.to_str().unwrap(),
            "--event",
            "PingEvent",
            "--topic0",
            &format!("0x{}", "84".repeat(32)),
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();
    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "invalid_event_options");
}

#[test]
fn events_list_json_sends_default_filter_and_reports_complete_raw_schema() {
    let tmp = TempDir::new().unwrap();
    let contract = format!("0x{}", "11".repeat(32));
    let emitter = format!("0x{}", "22".repeat(32));
    let tx_hash = format!("0x{}", "33".repeat(32));
    let topic = format!("0x{}", "44".repeat(32));
    let expected_query = serde_json::json!({
        "filters": [{
            "contractAddresses": [contract],
            "topics": { "topic0": [], "topic1": [], "topic2": [] },
        }],
        "order": "descending",
        "anchor": { "kind": "atEnd" },
        "cursor": "0x",
        "limit": 50,
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        expected_query,
        EventRpcResponse::Result(serde_json::json!({
            "events": [{
                "txHash": tx_hash,
                "blockHeight": 90,
                "sequenceNumber": 3,
                "event": {
                    "contractAddress": emitter,
                    "topics": [topic],
                    "data": "0xdeadbeef",
                },
            }],
            "cursor": "0xaabb",
            "latestBlock": 100,
            "oldestBlock": 7,
        })),
    );

    let output = groundhog()
        .args(["--json", "events", "list", &contract, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
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
    assert_eq!(
        stdout,
        serde_json::json!({
            "ok": true,
            "command": "events.list",
            "data": {
                "schemaVersion": 1,
                "profile": "default",
                "rpcUrl": rpc_url,
                "contractInput": contract,
                "contractAddress": contract,
                "filters": { "topic0": [], "topic1": [], "topic2": [] },
                "spec": null,
                "events": [{
                    "txHash": tx_hash,
                    "blockHeight": 90,
                    "sequenceNumber": 3,
                    "contractAddress": emitter,
                    "topics": [topic],
                    "data": "0xdeadbeef",
                    "decodeStatus": "notRequested",
                    "decoded": null,
                }],
                "cursor": "0xaabb",
                "latestBlock": 100,
                "oldestBlock": 7,
            },
            "warnings": [],
        })
    );
}

#[test]
fn events_list_preserves_duplicate_returned_records_in_order() {
    let tmp = TempDir::new().unwrap();
    let contract = format!("0x{}", "45".repeat(32));
    let first = serde_json::json!({
        "txHash": format!("0x{}", "46".repeat(32)),
        "blockHeight": 9,
        "sequenceNumber": 2,
        "event": { "contractAddress": contract, "topics": [], "data": "0xaaaa" },
    });
    let second = serde_json::json!({
        "txHash": format!("0x{}", "47".repeat(32)),
        "blockHeight": 8,
        "sequenceNumber": 1,
        "event": { "contractAddress": contract, "topics": [], "data": "0xbbbb" },
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [contract],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [first.clone(), second, first],
            "cursor": "0x",
            "latestBlock": 9,
            "oldestBlock": 0,
        })),
    );

    let output = groundhog()
        .args(["--json", "events", "list", &contract, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let events = stdout["data"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], events[2]);
    assert_eq!(events[0]["data"], "0xaaaa");
    assert_eq!(events[1]["data"], "0xbbbb");
}

#[test]
fn events_list_preserves_repeated_topic_order_and_forwards_cursor_and_limit() {
    let tmp = TempDir::new().unwrap();
    let contract = format!("0x{}", "51".repeat(32));
    let topic_a_input = format!("0X{}", "AA".repeat(32));
    let topic_a = format!("0x{}", "aa".repeat(32));
    let topic_b = format!("0x{}", "bb".repeat(32));
    let topic_c = format!("0x{}", "cc".repeat(32));
    let expected_filters = serde_json::json!({
        "topic0": [topic_a, topic_b, topic_a],
        "topic1": [topic_c],
        "topic2": [],
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [contract],
                "topics": expected_filters,
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0xabcd",
            "limit": 17,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [],
            "cursor": "0x",
            "latestBlock": 8,
            "oldestBlock": 1,
        })),
    );

    let output = groundhog()
        .args([
            "--json",
            "events",
            "list",
            &contract,
            "--topic0",
            &topic_a_input,
            "--topic0",
            &topic_b,
            "--topic0",
            &topic_a_input,
            "--topic1",
            &topic_c,
            "--cursor",
            "0XAbCd",
            "--limit",
            "17",
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["filters"], expected_filters);
}

#[test]
fn events_list_uses_selected_profile_rpc_and_explicit_rpc_precedence() {
    let contract = format!("0x{}", "61".repeat(32));
    for explicit_override in [false, true] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "contractAddresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": "0x",
                "limit": 50,
            }),
            EventRpcResponse::Result(serde_json::json!({
                "events": [], "cursor": "0x", "latestBlock": 1, "oldestBlock": 0,
            })),
        );
        let configured_url = if explicit_override {
            "http://127.0.0.1:1"
        } else {
            &rpc_url
        };
        std::fs::write(
            tmp.path().join("groundhog.toml"),
            format!("[profile.archive]\nrpc_url = \"{configured_url}\"\n"),
        )
        .unwrap();
        let mut args = vec![
            "--json",
            "events",
            "list",
            contract.as_str(),
            "--profile",
            "archive",
        ];
        if explicit_override {
            args.extend(["--rpc-url", rpc_url.as_str()]);
        }

        let output = groundhog()
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(output.status.success());
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(stdout["data"]["profile"], "archive");
        assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    }
}

#[test]
fn events_list_resolves_runtime_manifest_dev_account() {
    let tmp = TempDir::new().unwrap();
    let address = format!("0x{}", "71".repeat(32));
    let manifest = write_runtime_manifest(tmp.path(), 7, &address, None);
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [address],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [], "cursor": "0x", "latestBlock": 1, "oldestBlock": 0,
        })),
    );
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            manifest.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["--json", "events", "list", "dev-7"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["contractInput"], "dev-7");
    assert_eq!(stdout["data"]["contractAddress"], address);
}

#[test]
fn events_list_resolves_saved_wallet_before_contract_alias_and_alias_without_wallet() {
    let wallet_address = format!("0x{}", "72".repeat(32));
    let contract_address = format!("0x{}", "73".repeat(32));
    let owner = format!("0x{}", "74".repeat(32));

    for has_wallet in [true, false] {
        let tmp = TempDir::new().unwrap();
        let expected_address = if has_wallet {
            &wallet_address
        } else {
            &contract_address
        };
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "contractAddresses": [expected_address],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": "0x",
                "limit": 50,
            }),
            EventRpcResponse::Result(serde_json::json!({
                "events": [], "cursor": "0x", "latestBlock": 1, "oldestBlock": 0,
            })),
        );
        let wallet = if has_wallet {
            format!(
                "[profile.default.wallets.shared]\naddress = \"{wallet_address}\"\nowner_public_key = \"{owner}\"\n"
            )
        } else {
            String::new()
        };
        std::fs::write(
            tmp.path().join("groundhog.toml"),
            format!(
                "[profile.default]\nrpc_url = \"{rpc_url}\"\n{wallet}[profile.default.contracts]\nshared = \"{contract_address}\"\n"
            ),
        )
        .unwrap();

        let output = groundhog()
            .args(["--json", "events", "list", "shared"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(output.status.success());
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(stdout["data"]["contractAddress"], expected_address.as_str());
    }
}

#[test]
fn events_list_explicit_runtime_manifest_overrides_profile_manifest_for_dev_account() {
    let tmp = TempDir::new().unwrap();
    let explicit_tmp = TempDir::new().unwrap();
    let profile_address = format!("0x{}", "75".repeat(32));
    let explicit_address = format!("0x{}", "76".repeat(32));
    let profile_manifest = write_runtime_manifest(tmp.path(), 7, &profile_address, None);
    let explicit_manifest = write_runtime_manifest(explicit_tmp.path(), 7, &explicit_address, None);
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [explicit_address],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [], "cursor": "0x", "latestBlock": 1, "oldestBlock": 0,
        })),
    );
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            profile_manifest.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = groundhog()
        .args([
            "--json",
            "events",
            "list",
            "dev-7",
            "--runtime-manifest",
            explicit_manifest.to_str().unwrap(),
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["contractAddress"], explicit_address);
}

#[test]
fn events_list_rejects_invalid_inputs_before_rpc_with_structured_details() {
    let contract = format!("0x{}", "81".repeat(32));
    let invalid_cases = [
        (
            vec!["--limit".to_owned(), "0".to_owned()],
            "invalid_event_limit",
            serde_json::json!({ "input": "0", "message": "must be greater than zero" }),
        ),
        (
            vec!["--cursor".to_owned(), "0xabc".to_owned()],
            "invalid_event_cursor",
            serde_json::json!({ "input": "0xabc", "message": "Odd number of digits" }),
        ),
        (
            vec!["--topic1".to_owned(), "0xdeadbeef".to_owned()],
            "invalid_event_topic",
            serde_json::json!({
                "input": "0xdeadbeef",
                "message": "topic must be exactly 32 bytes",
            }),
        ),
        (
            std::iter::repeat_n(
                ["--topic2".to_owned(), format!("0x{}", "ab".repeat(32))],
                17,
            )
            .flatten()
            .collect(),
            "invalid_event_filter",
            serde_json::json!({ "position": "topic2", "count": 17, "max": 16 }),
        ),
    ];

    for (event_args, expected_code, expected_details) in invalid_cases {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_no_request_rpc();
        let mut args = vec![
            "--json".to_owned(),
            "events".to_owned(),
            "list".to_owned(),
            contract.clone(),
        ];
        args.extend(event_args);
        args.extend(["--rpc-url".to_owned(), rpc_url]);

        let output = groundhog()
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["command"], "events.list");
        assert_eq!(stderr["error"]["code"], expected_code);
        assert_eq!(stderr["error"]["details"], expected_details);
        if expected_code == "invalid_event_filter" {
            assert_eq!(
                stderr["error"]["hint"],
                "pass at most 16 values at each event topic position"
            );
        }
    }
}

#[test]
fn events_list_rejects_page_larger_than_requested_limit() {
    let tmp = TempDir::new().unwrap();
    let contract = format!("0x{}", "91".repeat(32));
    let event = serde_json::json!({
        "txHash": format!("0x{}", "92".repeat(32)),
        "blockHeight": 2,
        "sequenceNumber": 0,
        "event": { "contractAddress": contract, "topics": [], "data": "0x" },
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [contract],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 1,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [event.clone(), event],
            "cursor": "0x",
            "latestBlock": 2,
            "oldestBlock": 0,
        })),
    );

    let output = groundhog()
        .args([
            "--json",
            "events",
            "list",
            &contract,
            "--limit",
            "1",
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "rpc_method_failed");
    assert_eq!(
        stderr["error"]["message"],
        "getEvents failed: invalid response: returned 2 events for requested limit 1"
    );
}

#[test]
fn events_list_rejects_oversized_event_shapes() {
    let contract = format!("0x{}", "a1".repeat(32));
    let topic = format!("0x{}", "a2".repeat(32));
    for (topics, data, expected_message) in [
        (
            vec![topic.clone(); 4],
            format!("0x{}", "a3".repeat(1025)),
            "event at height 12 sequence 4 returned 4 topics; maximum is 3".to_owned(),
        ),
        (
            vec![],
            format!("0x{}", "a3".repeat(1025)),
            "event at height 12 sequence 4 is 1025 bytes; maximum is 1024".to_owned(),
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "contractAddresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": "0x",
                "limit": 50,
            }),
            EventRpcResponse::Result(serde_json::json!({
                "events": [{
                    "txHash": format!("0x{}", "a4".repeat(32)),
                    "blockHeight": 12,
                    "sequenceNumber": 4,
                    "event": { "contractAddress": contract, "topics": topics, "data": data },
                }],
                "cursor": "0x",
                "latestBlock": 12,
                "oldestBlock": 0,
            })),
        );

        let output = groundhog()
            .args(["events", "list", &contract, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(&expected_message), "stderr: {stderr}");
    }
}

#[test]
fn events_list_enforces_exact_combined_serialized_event_bound() {
    let contract = format!("0x{}", "a5".repeat(32));
    let topics = ["a6", "a7", "a8"]
        .map(|byte| format!("0x{}", byte.repeat(32)))
        .to_vec();

    for (data_len, succeeds) in [(928, true), (929, false)] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "contractAddresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": "0x",
                "limit": 50,
            }),
            EventRpcResponse::Result(serde_json::json!({
                "events": [{
                    "txHash": format!("0x{}", "a9".repeat(32)),
                    "blockHeight": 12,
                    "sequenceNumber": 4,
                    "event": {
                        "contractAddress": contract,
                        "topics": topics,
                        "data": format!("0x{}", "aa".repeat(data_len)),
                    },
                }],
                "cursor": "0x",
                "latestBlock": 12,
                "oldestBlock": 0,
            })),
        );

        let output = groundhog()
            .args(["events", "list", &contract, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert_eq!(output.status.success(), succeeds);
        if !succeeds {
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(
                stderr.contains("event at height 12 sequence 4 is 1025 bytes; maximum is 1024"),
                "stderr: {stderr}"
            );
        }
    }
}

#[test]
fn events_list_nonempty_human_output_reports_complete_raw_event() {
    let tmp = TempDir::new().unwrap();
    let queried_address = format!("0x{}", "b2".repeat(32));
    let emitter = format!("0x{}", "b3".repeat(32));
    let tx_hash = format!("0x{}", "b4".repeat(32));
    let topics = ["b5", "b6", "b7"].map(|byte| format!("0x{}", byte.repeat(32)));
    let event = serde_json::json!({
        "txHash": tx_hash,
        "blockHeight": 88,
        "sequenceNumber": 6,
        "event": {
            "contractAddress": emitter,
            "topics": topics,
            "data": "0xdeadbeef",
        },
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [queried_address],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [event.clone(), event],
            "cursor": "0xcafe",
            "latestBlock": 100,
            "oldestBlock": 7,
        })),
    );
    std::fs::write(
        tmp.path().join("groundhog.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\n[profile.default.contracts]\ncounter = \"{queried_address}\"\n"
        ),
    )
    .unwrap();

    let output = groundhog()
        .args(["events", "list", "counter"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "contract: counter".to_owned(),
        format!("emitter: {queried_address}"),
        "latest indexed block: 100".to_owned(),
        "oldest retained block: 7".to_owned(),
        "\nevent\n".to_owned(),
        "height: 88".to_owned(),
        "sequence: 6".to_owned(),
        format!("txn: {tx_hash}"),
        format!("event emitter: {emitter}"),
        format!("topic 0: {}", topics[0]),
        format!("topic 1: {}", topics[1]),
        format!("topic 2: {}", topics[2]),
        "data: 0xdeadbeef".to_owned(),
        "next cursor: 0xcafe".to_owned(),
    ] {
        assert!(
            stderr.contains(&expected),
            "missing `{expected}` in: {stderr}"
        );
    }
    assert_eq!(stderr.matches("\n\nevent\n").count(), 2, "stderr: {stderr}");
    assert!(
        stderr.contains("data: 0xdeadbeef\n\n  next cursor: 0xcafe"),
        "stderr: {stderr}"
    );
}

#[test]
fn events_list_connection_error_escapes_malicious_rpc_url() {
    let contract = format!("0x{}", "b8".repeat(32));
    let rpc_url = "http://[bad\n\u{1b}[31m\u{202e}spoof";

    let output = groundhog()
        .args(["events", "list", &contract, "--rpc-url", rpc_url])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(r"bad\n\u{1b}[31m\u{202e}spoof"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("bad\n"));
    assert!(!stderr.contains('\u{1b}'));
    assert!(!stderr.contains('\u{202e}'));
}

#[test]
fn events_list_human_output_distinguishes_empty_page_states() {
    let contract = format!("0x{}", "b1".repeat(32));
    for (request_cursor, response_cursor, expected) in [
        (None, "0x", "events: no events found; stream exhausted"),
        (
            None,
            "0xcafe",
            "events: no displayable events in this page; continue with cursor",
        ),
        (
            Some("0xabcd"),
            "0x",
            "events: no events returned; no continuation cursor",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "contractAddresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": request_cursor.unwrap_or("0x"),
                "limit": 50,
            }),
            EventRpcResponse::Result(serde_json::json!({
                "events": [],
                "cursor": response_cursor,
                "latestBlock": 9,
                "oldestBlock": 2,
            })),
        );
        let mut args = vec!["events", "list", contract.as_str()];
        if let Some(cursor) = request_cursor {
            args.extend(["--cursor", cursor]);
        }
        args.extend(["--rpc-url", rpc_url.as_str()]);

        let output = groundhog()
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(expected), "stderr: {stderr}");
        assert_eq!(
            stderr.contains("next cursor: 0xcafe"),
            response_cursor == "0xcafe"
        );
        if response_cursor == "0xcafe" {
            assert!(
                stderr.contains("\n\n  next cursor: 0xcafe"),
                "stderr: {stderr}"
            );
        }
    }
}

#[test]
fn events_list_classifies_capability_and_ordinary_rpc_errors() {
    let contract = format!("0x{}", "c1".repeat(32));
    for (code, expected_code) in [
        (-32601, "rpc_capability_unsupported"),
        (-32052, "rpc_capability_unsupported"),
        (-32050, "rpc_method_failed"),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "contractAddresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": "0x",
                "limit": 50,
            }),
            EventRpcResponse::Error(code, "mock event error"),
        );

        let output = groundhog()
            .args(["--json", "events", "list", &contract, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], expected_code);
        if expected_code == "rpc_capability_unsupported" {
            assert_eq!(stderr["error"]["details"]["method"], "getEvents");
        } else {
            assert!(stderr["error"]["details"].is_null());
        }
    }
}

fn transfer_signature() -> String {
    format!(
        "0x{}",
        hex::encode(*groundhog_erc20_if::TransferEvent::SIGNATURE)
    )
}

fn token_event_json(
    identity: (&str, u64, u32),
    addresses: [&str; 4],
    data: &str,
) -> serde_json::Value {
    let (tx_byte, block_height, sequence_number) = identity;
    let [token, signature, from, to] = addresses;
    serde_json::json!({
        "txHash": format!("0x{}", tx_byte.repeat(32)),
        "blockHeight": block_height,
        "sequenceNumber": sequence_number,
        "event": {
            "contractAddress": token,
            "topics": [signature, from, to],
            "data": data,
        },
    })
}

#[test]
fn token_transfers_help_describes_canonical_filtered_history() {
    let output = groundhog()
        .args(["token", "transfers", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "exactly one newest-first page",
        "canonical ERC20 TransferEvent",
        "--from",
        "--to",
        "--account",
        "--limit",
        "--cursor",
        "--profile",
        "--rpc-url",
        "--runtime-manifest",
    ] {
        assert!(
            stdout.contains(expected),
            "missing `{expected}` in: {stdout}"
        );
    }
}

#[test]
fn token_transfers_json_queries_signature_and_preserves_decoded_and_malformed_records() {
    let tmp = TempDir::new().unwrap();
    let token = format!("0x{}", "a1".repeat(32));
    let from = format!("0x{}", "a2".repeat(32));
    let to = format!("0x{}", "a3".repeat(32));
    let signature = transfer_signature();
    let amount_data = format!("0x{}", hex::encode(xdrs::cxs::encode(&125_i128).unwrap()));
    let negative_data = format!("0x{}", hex::encode(xdrs::cxs::encode(&-1_i128).unwrap()));
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [token],
                "topics": { "topic0": [signature], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [
                token_event_json(("a4", 12, 1), [&token, &signature, &from, &to], &amount_data),
                token_event_json(("a5", 11, 0), [&token, &signature, &from, &to], &negative_data),
                token_event_json(("a4", 12, 1), [&token, &signature, &from, &to], &amount_data),
            ],
            "cursor": "0xcafe",
            "latestBlock": 12,
            "oldestBlock": 2,
        })),
    );

    let output = groundhog()
        .args([
            "--json",
            "token",
            "transfers",
            &token,
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["command"], "token.transfers");
    assert_eq!(stdout["data"]["schemaVersion"], 1);
    assert_eq!(stdout["data"]["tokenInput"], token);
    assert_eq!(stdout["data"]["tokenAddress"], token);
    assert!(stdout["data"]["participantFilter"].is_null());
    assert_eq!(stdout["data"]["cursor"], "0xcafe");
    assert_eq!(stdout["data"]["latestBlock"], 12);
    assert_eq!(stdout["data"]["oldestBlock"], 2);
    let records = stdout["data"]["records"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["decodeStatus"], "decoded");
    assert_eq!(records[0]["transfer"]["kind"], "transfer");
    assert_eq!(records[0]["transfer"]["from"], from);
    assert_eq!(records[0]["transfer"]["to"], to);
    assert_eq!(records[0]["transfer"]["rawAmount"], "125");
    assert_eq!(records[0]["data"], amount_data);
    assert!(records[0].get("decodeError").is_none());
    assert_eq!(records[1]["decodeStatus"], "malformed");
    assert!(records[1]["transfer"].is_null());
    assert!(
        records[1]["decodeError"]
            .as_str()
            .unwrap()
            .contains("nonnegative")
    );
    assert_eq!(records[1]["data"], negative_data);
    assert_eq!(records[2], records[0]);
}

#[test]
fn token_transfers_account_uses_two_server_side_filters_in_one_request() {
    let tmp = TempDir::new().unwrap();
    let token = format!("0x{}", "b1".repeat(32));
    let account = format!("0x{}", "b2".repeat(32));
    let signature = transfer_signature();
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [
                {
                    "contractAddresses": [token],
                    "topics": { "topic0": [signature], "topic1": [account], "topic2": [] },
                },
                {
                    "contractAddresses": [token],
                    "topics": { "topic0": [signature], "topic1": [], "topic2": [account] },
                },
            ],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 7,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [],
            "cursor": "0x",
            "latestBlock": 12,
            "oldestBlock": 2,
        })),
    );

    let output = groundhog()
        .args([
            "--json",
            "token",
            "transfers",
            &token,
            "--account",
            &account,
            "--limit",
            "7",
            "--rpc-url",
            &rpc_url,
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["participantFilter"]["kind"], "account");
    assert_eq!(stdout["data"]["participantFilter"]["input"], account);
    assert_eq!(stdout["data"]["participantFilter"]["address"], account);
}

#[test]
fn token_transfers_maps_directional_filters_and_cursor_to_one_query() {
    let token = format!("0x{}", "d1".repeat(32));
    let from = format!("0x{}", "d2".repeat(32));
    let to = format!("0x{}", "d3".repeat(32));
    for (flags, topic1, topic2, filter_kind) in [
        (vec!["--from", &from], vec![from.clone()], vec![], "from"),
        (vec!["--to", &to], vec![], vec![to.clone()], "to"),
        (
            vec!["--from", &from, "--to", &to],
            vec![from.clone()],
            vec![to.clone()],
            "fromTo",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let signature = transfer_signature();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "contractAddresses": [token],
                    "topics": { "topic0": [signature], "topic1": topic1, "topic2": topic2 },
                }],
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": "0x1234",
                "limit": 50,
            }),
            EventRpcResponse::Result(serde_json::json!({
                "events": [],
                "cursor": "0x",
                "latestBlock": 12,
                "oldestBlock": 2,
            })),
        );
        let mut args = vec!["--json", "token", "transfers", &token];
        args.extend(flags);
        args.extend(["--cursor", "0x1234", "--rpc-url", &rpc_url]);

        let output = groundhog()
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(output.status.success());
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(stdout["data"]["participantFilter"]["kind"], filter_kind);
    }
}

#[test]
fn token_transfers_human_output_preserves_participants_and_malformed_evidence() {
    let tmp = TempDir::new().unwrap();
    let token = format!("0x{}", "e1".repeat(32));
    let from = format!("0x{}", "e2".repeat(32));
    let to = format!("0x{}", "e3".repeat(32));
    let zero = format!("0x{}", "00".repeat(32));
    let signature = transfer_signature();
    let amount_data = format!("0x{}", hex::encode(xdrs::cxs::encode(&125_i128).unwrap()));
    let malformed_data = "0xff";
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "contractAddresses": [token],
                "topics": { "topic0": [signature], "topic1": [], "topic2": [] },
            }],
            "order": "descending",
            "anchor": { "kind": "atEnd" },
            "cursor": "0x",
            "limit": 50,
        }),
        EventRpcResponse::Result(serde_json::json!({
            "events": [
                {
                    "txHash": format!("0x{}", "e4".repeat(32)),
                    "blockHeight": 12,
                    "sequenceNumber": 1,
                    "event": {
                        "contractAddress": token,
                        "topics": [signature, from, to],
                        "data": amount_data,
                    },
                },
                {
                    "txHash": format!("0x{}", "e5".repeat(32)),
                    "blockHeight": 11,
                    "sequenceNumber": 2,
                    "event": {
                        "contractAddress": token,
                        "topics": [signature, zero, to],
                        "data": amount_data,
                    },
                },
                {
                    "txHash": format!("0x{}", "e6".repeat(32)),
                    "blockHeight": 10,
                    "sequenceNumber": 1,
                    "event": {
                        "contractAddress": token,
                        "topics": [signature, from, zero],
                        "data": amount_data,
                    },
                },
                {
                    "txHash": format!("0x{}", "e7".repeat(32)),
                    "blockHeight": 9,
                    "sequenceNumber": 0,
                    "event": {
                        "contractAddress": token,
                        "topics": [signature, from, to],
                        "data": malformed_data,
                    },
                },
            ],
            "cursor": "0x",
            "latestBlock": 12,
            "oldestBlock": 2,
        })),
    );

    let output = groundhog()
        .args(["token", "transfers", &token, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("\ntransfer\n"), "stderr: {stderr}");
    assert!(stderr.contains("height: 12"), "stderr: {stderr}");
    assert!(stderr.contains("sequence: 1"), "stderr: {stderr}");
    assert!(stderr.contains("txn: 0x"), "stderr: {stderr}");
    assert!(stderr.contains("raw amount: 125"), "stderr: {stderr}");
    assert!(!stderr.contains("\"transfer\""), "stderr: {stderr}");
    assert!(stderr.contains("\nmint\n"), "stderr: {stderr}");
    assert!(stderr.contains("\nburn\n"), "stderr: {stderr}");
    assert!(
        stderr.contains(&format!("from: {zero}")),
        "stderr: {stderr}"
    );
    assert!(stderr.contains(&format!("to: {zero}")), "stderr: {stderr}");
    assert!(
        stderr.contains("warning: malformed transfer event at height 9 sequence 0"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("data: 0xff"), "stderr: {stderr}");
}

#[test]
fn token_transfers_human_output_distinguishes_empty_page_states() {
    let token = format!("0x{}", "f1".repeat(32));
    let signature = transfer_signature();
    for (request_cursor, response_cursor, expected) in [
        (None, "0x", "transfers: no events found; stream exhausted"),
        (
            None,
            "0xcafe",
            "transfers: no displayable events in this page; continue with cursor",
        ),
        (
            Some("0xabcd"),
            "0x",
            "transfers: no events returned; no continuation cursor",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "contractAddresses": [token],
                    "topics": { "topic0": [signature], "topic1": [], "topic2": [] },
                }],
                "order": "descending",
                "anchor": { "kind": "atEnd" },
                "cursor": request_cursor.unwrap_or("0x"),
                "limit": 50,
            }),
            EventRpcResponse::Result(serde_json::json!({
                "events": [],
                "cursor": response_cursor,
                "latestBlock": 9,
                "oldestBlock": 2,
            })),
        );
        let mut args = vec!["token", "transfers", token.as_str()];
        if let Some(cursor) = request_cursor {
            args.extend(["--cursor", cursor]);
        }
        args.extend(["--rpc-url", rpc_url.as_str()]);

        let output = groundhog()
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(expected), "stderr: {stderr}");
        assert_eq!(
            stderr.contains("next cursor: 0xcafe"),
            response_cursor == "0xcafe"
        );
        if response_cursor == "0xcafe" {
            assert!(
                stderr.contains("\n\n  next cursor: 0xcafe"),
                "stderr: {stderr}"
            );
        }
    }
}

#[test]
fn token_transfers_rejects_account_conflicts_before_rpc() {
    let token = format!("0x{}", "c1".repeat(32));
    let account = format!("0x{}", "c2".repeat(32));
    let output = groundhog()
        .args([
            "--json",
            "token",
            "transfers",
            &token,
            "--account",
            &account,
            "--from",
            &account,
            "--rpc-url",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "invalid_event_options");
    assert_eq!(
        stderr["error"]["hint"],
        "use `--account` by itself, or use `--from` and `--to` separately or together"
    );
}

#[test]
fn token_transfers_reports_command_specific_pagination_errors() {
    let token = format!("0x{}", "c1".repeat(32));
    for (option, value, expected_code, expected_hint) in [
        (
            "--limit",
            "0",
            "invalid_event_limit",
            "pass a positive whole-number page size",
        ),
        (
            "--cursor",
            "not-hex",
            "invalid_event_cursor",
            "previous `groundhog token transfers` page",
        ),
    ] {
        let output = groundhog()
            .args([
                "--json",
                "token",
                "transfers",
                &token,
                option,
                value,
                "--rpc-url",
                "http://127.0.0.1:1",
            ])
            .output()
            .unwrap();

        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], expected_code);
        assert_eq!(stderr["error"]["details"]["input"], value);
        assert!(
            stderr["error"]["hint"]
                .as_str()
                .unwrap()
                .contains(expected_hint),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[derive(Clone)]
enum TxListRpcResponse {
    Result(serde_json::Value),
    Error(i32, &'static str),
}

fn spawn_tx_list_rpc(
    expected_query: serde_json::Value,
    response: TxListRpcResponse,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (stream, request) =
            read_jsonrpc_request(&listener, deadline, "getTransactionsByAddress");
        assert_eq!(request["method"], "groundhog_getTransactionsByAddress");
        assert_eq!(request["params"], serde_json::json!([expected_query]));
        match response {
            TxListRpcResponse::Result(result) => {
                send_jsonrpc_response(stream, &request["id"], result);
            }
            TxListRpcResponse::Error(code, message) => {
                send_jsonrpc_error_response(stream, &request["id"], code, message);
            }
        }
        assert_no_follow_up_rpc(&listener);
    });
    (format!("http://{addr}"), handle)
}

#[derive(Clone)]
enum EventRpcResponse {
    Result(serde_json::Value),
    Error(i32, &'static str),
}

fn spawn_events_rpc(
    expected_query: serde_json::Value,
    response: EventRpcResponse,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "getEvents");
        assert_eq!(request["method"], "groundhog_getEvents");
        assert_eq!(request["params"], serde_json::json!([expected_query]));
        match response {
            EventRpcResponse::Result(result) => {
                send_jsonrpc_response(stream, &request["id"], result);
            }
            EventRpcResponse::Error(code, message) => {
                send_jsonrpc_error_response(stream, &request["id"], code, message);
            }
        }
        assert_no_follow_up_rpc(&listener);
    });
    (format!("http://{addr}"), handle)
}

#[derive(Clone)]
enum TxShowRpcResponse {
    Result(serde_json::Value),
    Error(i32, &'static str),
}

fn spawn_tx_show_rpc(
    tx_hash: &str,
    status: TxShowRpcResponse,
    receipt: Option<TxShowRpcResponse>,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let tx_hash = tx_hash.to_owned();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "getTransactionStatus");
        assert_eq!(request["method"], "groundhog_getTransactionStatus");
        assert_eq!(request["params"], serde_json::json!([tx_hash]));
        send_tx_show_response(stream, &request["id"], status);

        if let Some(receipt) = receipt {
            let (stream, request) =
                read_jsonrpc_request(&listener, deadline, "getTransactionReceipt");
            assert_eq!(request["method"], "groundhog_getTransactionReceipt");
            assert_eq!(request["params"], serde_json::json!([tx_hash]));
            send_tx_show_response(stream, &request["id"], receipt);
        }
        assert_no_follow_up_rpc(&listener);
    });
    (format!("http://{addr}"), handle)
}

fn send_tx_show_response(
    stream: std::net::TcpStream,
    request_id: &serde_json::Value,
    response: TxShowRpcResponse,
) {
    match response {
        TxShowRpcResponse::Result(result) => send_jsonrpc_response(stream, request_id, result),
        TxShowRpcResponse::Error(code, message) => {
            send_jsonrpc_error_response(stream, request_id, code, message);
        }
    }
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

fn spawn_network_descriptor(rpc_url: &str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let rpc_url = rpc_url.to_owned();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        assert!(
            request_line.starts_with("GET /v1/network "),
            "unexpected descriptor request: {request_line}"
        );
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": groundhog_json_rpc_types::network_descriptor::NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            "kind": "groundhog-network",
            "networkId": "devnet",
            "protocol": "hogp/test",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "endpoints": { "rpcUrl": rpc_url },
            "nativeHog": {
                "tokenAddress": format!("0x{}", "61".repeat(32)),
                "decimals": 7,
                "transferEntrypoint": "0x63285b3523f9dc3f"
            },
            "faucet": { "walletAddress": format!("0x{}", "71".repeat(32)) },
            "walletCreate": {
                "prefundRawAmount": "100000000",
                "walletContractHash": format!("0x{}", "51".repeat(32))
            },
            "devWallet": {
                "deployEntrypoint": "0xf969542572bd6860",
                "doActionEntrypoint": "0xa0fff94c64fbaf3c"
            }
        }))
        .unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();
    });
    (format!("http://{addr}"), handle)
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

fn spawn_no_request_rpc() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match listener.accept() {
                Ok(_) => panic!("validation failure unexpectedly contacted RPC"),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("failed to inspect mock RPC listener: {error}"),
            }
        }
    });
    (format!("http://{addr}"), handle)
}

#[derive(Clone)]
enum TransferSubmitMock {
    MatchingHash,
    MismatchedHash,
    Error(i32),
    ErrorWithMessage(i32, &'static str),
    DropResponse,
}

#[derive(Clone)]
enum TransferPollMock {
    Receipt {
        status: serde_json::Value,
        events: serde_json::Value,
        return_data: Option<&'static str>,
        abort_data: Option<&'static str>,
        mismatched_hash: bool,
    },
    Status(serde_json::Value),
    ReceiptError(i32),
}

#[derive(Clone)]
struct TransferRpcMock {
    submit: TransferSubmitMock,
    poll: Option<TransferPollMock>,
}

struct TransferMockRun {
    output: std::process::Output,
    local_hash: String,
    rpc_url: String,
}

fn run_transfer_live_mock(mock: TransferRpcMock, json: bool) -> std::process::Output {
    run_transfer_live_mock_with_hash(mock, json).output
}

fn run_transfer_live_mock_with_hash(mock: TransferRpcMock, json: bool) -> TransferMockRun {
    run_transfer_live_mock_with_rpc_override(mock, json, "")
}

fn run_transfer_live_mock_with_rpc_override(
    mock: TransferRpcMock,
    json: bool,
    rpc_url_suffix: &str,
) -> TransferMockRun {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sender = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &sender, None);
    let (rpc_url, rpc_server, local_hash_rx) = spawn_transfer_rpc(mock, sender.clone());
    let effective_rpc_url = format!("{rpc_url}{rpc_url_suffix}");
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();
    let recipient = format!("0x{}", "ab".repeat(32));
    let mut command = groundhog();
    if json {
        command.arg("--json");
    }
    command.args(["transfer", &recipient, "1.5"]);
    if !rpc_url_suffix.is_empty() {
        command.args(["--rpc-url", &effective_rpc_url]);
    }
    let output = command
        .current_dir(&project)
        .env("GROUNDHOG_TEST_RECEIPT_POLL_ATTEMPTS", "1")
        .env("GROUNDHOG_TEST_RECEIPT_POLL_INTERVAL_MS", "0")
        .output()
        .unwrap();
    let local_hash = local_hash_rx.recv().unwrap();
    rpc_server.join().unwrap();
    TransferMockRun {
        output,
        local_hash,
        rpc_url: effective_rpc_url,
    }
}

fn run_saved_wallet_transfer_live_mock(
    mock: TransferRpcMock,
    explicit_wallet: Option<&str>,
) -> TransferMockRun {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let owner = generate_key_public_key(&config_home, "alice");
    let primary = format!("0x{}", "a1".repeat(32));
    let trading = format!("0x{}", "b2".repeat(32));
    let expected_sender = if explicit_wallet.is_some() {
        trading.clone()
    } else {
        primary.clone()
    };
    let (rpc_url, rpc_server, local_hash_rx) = spawn_transfer_rpc(mock, expected_sender);
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"alice\"\ndefault_wallet = \"primary\"\n\n[profile.default.wallets.primary]\naddress = \"{primary}\"\nowner_public_key = \"{owner}\"\n\n[profile.default.wallets.trading]\naddress = \"{trading}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();
    let recipient = format!("0x{}", "ab".repeat(32));
    let mut command = groundhog();
    command.args(["--json", "transfer", &recipient, "1.5"]);
    if let Some(wallet) = explicit_wallet {
        command.args(["--from", wallet]);
    }
    let output = command
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("GROUNDHOG_TEST_RECEIPT_POLL_ATTEMPTS", "1")
        .env("GROUNDHOG_TEST_RECEIPT_POLL_INTERVAL_MS", "0")
        .output()
        .unwrap();
    let local_hash = local_hash_rx.recv().unwrap();
    rpc_server.join().unwrap();
    TransferMockRun {
        output,
        local_hash,
        rpc_url,
    }
}

fn spawn_transfer_rpc(
    mock: TransferRpcMock,
    expected_sender: String,
) -> (String, thread::JoinHandle<()>, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (local_hash_tx, local_hash_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "chainTip");
        assert_eq!(request["method"], "groundhog_chainTip");
        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({
                "height": 42,
                "blockHash": format!("0x{}", "00".repeat(32)),
            }),
        );

        let (stream, request) = read_jsonrpc_request(&listener, deadline, "sendRawTransaction");
        assert_eq!(request["method"], "groundhog_sendRawTransaction");
        let signed_hex = request["params"][0].as_str().unwrap();
        let signed_tx = decode_signed_tx(signed_hex);
        let local_hash = format!(
            "0x{}",
            hex::encode(groundhog_signing::transaction_hash(&signed_tx.tx).unwrap())
        );
        local_hash_tx.send(local_hash.clone()).unwrap();
        assert_eq!(
            format!(
                "0x{}",
                hex::encode(signed_tx.tx.invocation.invoked_address.as_ref())
            ),
            expected_sender
        );
        let (target, entrypoint, calldata) = decode_wallet_action(&signed_tx);
        assert_eq!(target, format!("0x{}01", "00".repeat(31)));
        assert_eq!(entrypoint, "0x63285b3523f9dc3f");
        let (recipient, raw_amount): (groundhog_xdr::types::ContractAddress, u128) =
            xdrs::cxs::decode(&calldata).unwrap();
        assert_eq!(
            format!("0x{}", hex::encode(recipient.as_ref())),
            format!("0x{}", "ab".repeat(32))
        );
        assert_eq!(raw_amount, 15_000_000);

        match mock.submit {
            TransferSubmitMock::MatchingHash => {
                send_jsonrpc_response(stream, &request["id"], serde_json::json!(local_hash));
            }
            TransferSubmitMock::MismatchedHash => {
                send_jsonrpc_response(
                    stream,
                    &request["id"],
                    serde_json::json!(format!("0x{}", "ff".repeat(32))),
                );
            }
            TransferSubmitMock::Error(code) => {
                send_jsonrpc_error_response(stream, &request["id"], code, "mock send error");
            }
            TransferSubmitMock::ErrorWithMessage(code, message) => {
                send_jsonrpc_error_response(stream, &request["id"], code, message);
            }
            TransferSubmitMock::DropResponse => drop(stream),
        }

        if let Some(poll) = mock.poll {
            serve_transfer_poll(&listener, deadline, &local_hash, poll);
            assert_no_follow_up_rpc(&listener);
        } else {
            assert_no_follow_up_rpc(&listener);
        }
    });
    (format!("http://{addr}"), handle, local_hash_rx)
}

fn serve_transfer_poll(
    listener: &TcpListener,
    deadline: Instant,
    local_hash: &str,
    poll: TransferPollMock,
) {
    let (stream, request) = read_jsonrpc_request(listener, deadline, "getTransactionReceipt");
    assert_eq!(request["method"], "groundhog_getTransactionReceipt");
    assert_eq!(request["params"][0], local_hash);
    match poll {
        TransferPollMock::Receipt {
            status,
            events,
            return_data,
            abort_data,
            mismatched_hash,
        } => {
            let mut receipt = serde_json::json!({
                "txHash": if mismatched_hash {
                    format!("0x{}", "ee".repeat(32))
                } else {
                    local_hash.to_owned()
                },
                "height": 43,
                "status": status,
                "events": events,
            });
            if let Some(value) = return_data {
                receipt["returnData"] = serde_json::json!(value);
            }
            if let Some(value) = abort_data {
                receipt["abortData"] = serde_json::json!(value);
            }
            send_jsonrpc_response(stream, &request["id"], receipt);
        }
        TransferPollMock::Status(status) => {
            send_jsonrpc_response(stream, &request["id"], serde_json::Value::Null);
            let (stream, request) =
                read_jsonrpc_request(listener, deadline, "getTransactionStatus");
            assert_eq!(request["method"], "groundhog_getTransactionStatus");
            assert_eq!(request["params"][0], local_hash);
            send_jsonrpc_response(stream, &request["id"], status);
        }
        TransferPollMock::ReceiptError(code) => {
            send_jsonrpc_error_response(stream, &request["id"], code, "mock receipt error");
        }
    }
}

fn assert_no_follow_up_rpc(listener: &TcpListener) {
    let deadline = Instant::now() + Duration::from_millis(200);
    loop {
        match listener.accept() {
            Ok(_) => panic!("unexpected follow-up RPC request"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("failed to inspect RPC listener: {error}"),
        }
    }
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

fn spawn_balance_rpc(address: &str, raw_balance: &str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    let expected_address = address.to_owned();
    let raw_balance = raw_balance.to_owned();

    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (stream, request) = read_jsonrpc_request(&listener, deadline, "getBalance");
        assert_eq!(request["method"], "groundhog_getBalance");
        assert_eq!(request["params"][0], expected_address);

        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({
                "address": expected_address,
                "rawBalance": raw_balance,
                "height": 34,
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

fn assert_split_bootstrap_accounts(manifest_path: &std::path::Path, user_tokens: &str) {
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(manifest_path).expect("bug: runtime manifest should be readable"),
    )
    .expect("bug: runtime manifest should parse");
    assert_eq!(manifest["bootstrapAccounts"][0]["role"], "faucet");
    assert_eq!(
        manifest["bootstrapAccounts"][0]["initialStorageTokens"],
        "10000000000000"
    );
    assert_eq!(manifest["bootstrapAccounts"][1]["role"], "local_user");
    assert_eq!(
        manifest["bootstrapAccounts"][1]["initialStorageTokens"],
        user_tokens
    );
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
        "schemaVersion": 2,
        "kind": "hogd-dev-runtime",
        "chainState": "fresh",
        "networkHash": "00".repeat(32),
        "protocol": "hogp/test",
        "genesisHash": format!("0x{}", "42".repeat(32)),
        "producer": {
            "binary": "hog-devnode",
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
            "role": if index == 0 { "faucet" } else { "local_user" },
            "publicKey": format!("0x{}", hex::encode(public_key)),
            "walletAddress": address,
            "initialStorageTokens": if index == 0 {
                groundhog_node_supervisor::config::LOCAL_FAUCET_STORAGE_TOKENS
            } else {
                groundhog_node_supervisor::config::DEFAULT_HOGD_DEV_ACCOUNT_TOKENS
            }.to_string()
        }],
        "bootstrap": {
            "devWallet": {
                "contentHash": format!("0x{}", "51".repeat(32)),
                "deployedCodeHash": format!("0x{}", "52".repeat(32)),
                "artifactHint": "./dev-wallet.wasm",
                "deployEntrypoint": "0x6465706c6f794e77",
                "createWalletEntrypoint": "0x6372656174655761",
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

fn write_project_config_with_alice_wallet(
    project: &std::path::Path,
    rpc_url: &str,
    config_home: &std::path::Path,
) -> String {
    let owner_public_key = generate_key_public_key(config_home, "alice");
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.wallets.alice]\naddress = \"0x{}\"\nowner_public_key = \"{owner_public_key}\"\n",
            "aa".repeat(32)
        ),
    )
    .unwrap();
    owner_public_key
}

fn add_faucet_to_default_profile(project: &std::path::Path, faucet_url: &str) {
    let path = project.join("groundhog.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        config.replacen(
            "default_profile = \"default\"\n\n[profile.default]\n",
            &format!("default_profile = \"default\"\n\n[profile.default]\nfaucet_url = \"{faucet_url}\"\n"),
            1,
        ),
    )
    .unwrap();
}

fn write_project_config_with_dummy_alice_wallet(project: &std::path::Path, rpc_url: &str) {
    std::fs::write(
        project.join("groundhog.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.wallets.alice]\naddress = \"0x{}\"\nowner_public_key = \"0x{}\"\n",
            "aa".repeat(32),
            "bb".repeat(32),
        ),
    )
    .unwrap();
}

fn generate_key_public_key(config_home: &std::path::Path, name: &str) -> String {
    let output = groundhog()
        .args(["--json", "keys", "generate", name])
        .env("XDG_CONFIG_HOME", config_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    stdout["data"]["key"]["public_key"]
        .as_str()
        .expect("generated key should include public key")
        .to_owned()
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

fn decode_signed_tx(tx_hex: &str) -> groundhog_xdr::transaction::SignedTransaction {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex)).unwrap();
    xdrs::txs::decode(&bytes).unwrap()
}

fn signed_tx_entrypoint_from_decoded(
    signed_tx: &groundhog_xdr::transaction::SignedTransaction,
) -> String {
    format!(
        "0x{}",
        hex::encode(signed_tx.tx.invocation.entrypoint.as_ref())
    )
}

fn decode_wallet_action(
    signed_tx: &groundhog_xdr::transaction::SignedTransaction,
) -> (String, String, Vec<u8>) {
    let inner: Vec<u8> = xdrs::cxs::decode(&signed_tx.tx.invocation.calldata.0).unwrap();
    let (target, entrypoint, gas_limit, calldata): (
        groundhog_xdr::types::ContractAddress,
        groundhog_xdr::contract::ContractEntrypointID,
        u64,
        Vec<u8>,
    ) = xdrs::txs::decode(&inner).unwrap();
    assert_eq!(gas_limit, groundhog_signing::DEFAULT_TX_GAS_LIMIT);
    (
        format!("0x{}", hex::encode(target.as_ref())),
        format!("0x{}", hex::encode(entrypoint.as_ref())),
        calldata,
    )
}

fn signed_tx_entrypoint(tx_hex: &str) -> String {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex)).unwrap();
    let signed_tx: groundhog_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&bytes).unwrap();
    format!(
        "0x{}",
        hex::encode(signed_tx.tx.invocation.entrypoint.as_ref())
    )
}

fn signed_tx_deploy_contract_kind(tx_hex: &str) -> &'static str {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex)).unwrap();
    let signed_tx: groundhog_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&bytes).unwrap();
    let contract = signed_tx
        .tx
        .contracts_to_deploy
        .0
        .first()
        .expect("bug: deploy tx should include contract bytes");
    match contract {
        groundhog_xdr::contract::Contract::kWasmi { .. } => "wasm",
        groundhog_xdr::contract::Contract::kRv32 { .. } => "rv32",
        _ => "other",
    }
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

fn sample_failed_deploy_receipt(height: i64) -> serde_json::Value {
    serde_json::json!({
        "txHash": mock_tx_hash(),
        "height": height,
        "status": {
            "result": "deterministicError",
            "kind": "outOfGas",
        },
        "events": [],
        "returnData": "0x0102",
        "abortData": "0x0304",
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

fn sample_deterministic_error_call_receipt(height: i64) -> serde_json::Value {
    serde_json::json!({
        "txHash": mock_tx_hash(),
        "height": height,
        "status": {
            "result": "deterministicError",
            "kind": "unknown",
        },
        "events": [],
    })
}

fn sample_reverted_call_receipt(height: i64, abort_hex: &str) -> serde_json::Value {
    serde_json::json!({
        "txHash": mock_tx_hash(),
        "height": height,
        "status": { "result": "reverted" },
        "events": [],
        "abortData": abort_hex,
    })
}

/// Wire-encode a no-payload `LibraryExn` abort buffer for `case_name`:
/// cxs varint selector followed by the union case discriminant.
fn library_abort_hex(case_name: &str) -> String {
    let library = groundhog_spec_tool::describe::lookup("LibraryExn")
        .expect("LibraryExn should be registered in the describe registry");
    let described = groundhog_spec_tool::describe::build_describe(library);
    let discriminant = described
        .type_defs
        .iter()
        .find_map(|def| match def {
            groundhog_contract_spec::ContractTypeDef::TypeDefUnion { union_def }
                if AsRef::<[u8]>::as_ref(&union_def.name) == b"LibraryExn" =>
            {
                union_def
                    .cases
                    .iter()
                    .find(|case| AsRef::<[u8]>::as_ref(&case.name) == case_name.as_bytes())
                    .map(|case| case.discriminant)
            }
            _ => None,
        })
        .expect("LibraryExn union case should exist");
    let mut bytes = xdrs::cxs::encode(&library.selector).expect("selector should encode");
    bytes.extend_from_slice(&xdrs::cxs::encode(&discriminant).expect("discriminant should encode"));
    format!("0x{}", hex::encode(bytes))
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
