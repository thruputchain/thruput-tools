use std::fmt::Write as _;

#[cfg(unix)]
use std::fs::File;

use std::io::{BufRead, BufReader, Read, Write};

use std::net::TcpListener;

#[cfg(unix)]
use std::os::fd::FromRawFd;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use std::process::Command;

#[cfg(unix)]
use std::process::{ExitStatus, Stdio};

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use std::sync::mpsc;

use std::sync::{Arc, Mutex, Once, OnceLock};

use std::thread;

use std::time::{Duration, Instant};

use jsonrpsee::types::ErrorObjectOwned;

use rpc_recorder::{RecordedCall, RecordingServer, RecordingServerBuilder};

use tempfile::TempDir;

#[cfg(unix)]
use thruput_json_rpc_client::{HttpClientBuilder, native::NativeClient};

#[cfg(unix)]
use thruput_json_rpc_types::{requests::EmptyRequest, responses::DecodedResponse};

#[path = "support/isolation.rs"]
mod isolation;

#[path = "support/rpc_recorder.rs"]
mod rpc_recorder;

mod support;

#[cfg(unix)]
#[path = "support/ui_audit.rs"]
mod ui_audit;

static CONFIG_HOME_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// How long the harness waits for the CLI under test to do something it is
/// expected to do on its own: send a request to a mock endpoint, print its
/// startup line, open a download connection, exit, or reap the process it
/// supervises.
///
/// A passing test never waits anywhere near this long, so the value only
/// bounds how quickly a genuinely broken command fails. It is deliberately far
/// above what any command needs because CI runners pause for seconds at a
/// time: one ~5 s stall failed every `call_*` test at once while the mock
/// endpoints gave up after 5 s, and a 2 s exit budget has failed the forced
/// `config rm` in `removals_require_force_without_an_interactive_prompt` more
/// than once. Checks that the CLI does *not* do something
/// (`assert_no_follow_up_rpc`, recorded mocks) wait for it to exit instead, so
/// a stall cannot make them pass early either.
const CLI_ACTION_TIMEOUT: Duration = Duration::from_mins(1);

/// Write the `.cargo/contract.toml` overlay every contract build requires at
/// the cargo workspace root.
///
/// `thruput build` passes it as `--config`, so its absence refuses the build
/// before cargo runs. Contents are
/// irrelevant here — only the fake cargo on `PATH` ever reads them.
fn write_contract_toml(workspace_root: &std::path::Path) {
    std::fs::create_dir_all(workspace_root.join(".cargo")).unwrap();
    std::fs::write(workspace_root.join(".cargo/contract.toml"), "").unwrap();
}

fn write_opaque_rv32_artifact(root: &std::path::Path) -> std::path::PathBuf {
    let path = root.join("test.rv32a");
    std::fs::write(&path, spec_artifact_fixture()).unwrap();
    path
}

fn spec_artifact_fixture() -> Vec<u8> {
    let spec = thruput_contract_spec::ContractSpec {
        interfaces: vec![thruput_contract_spec::ContractSpecInterface {
            name: "Counter".try_into().unwrap(),
            vers: "0.1.0".try_into().unwrap(),
            throws: vec![].try_into().unwrap(),
            methods: vec![].try_into().unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };
    artifact_with_spec(&spec)
}

fn artifact_with_spec(spec: &thruput_contract_spec::ContractSpec) -> Vec<u8> {
    let bytes = xdrs::txs::encode(spec).unwrap();
    thruput_rv32_artifact::build_artifact_with_spec(
        &[0; 4],
        &[],
        0,
        65536,
        Some(thruput_rv32_artifact::ArtifactSpec {
            schema_version: 1,
            bytes: &bytes,
        }),
    )
    .unwrap()
}

fn spec_elf_fixture() -> Vec<u8> {
    support::minimal_rv32_elf_with_section(
        thruput_spec_tool::RAW_SECTION_NAME,
        &support::minimal_contract_spec_raw(),
    )
}

fn write_bundle_manifest(path: &std::path::Path, version: &str, source_commit: &str) {
    use sha2::{Digest, Sha256};

    let canonical = format!(
        "{{\"bundle\":{{\"version\":\"0.1.25-rc.4\"}},\"clients\":{{\"thruput\":{{\"assetRole\":\"cli\",\"component\":\"groundhog-rpc\",\"sourceCommit\":\"{source_commit}\",\"version\":\"{version}\"}}}},\"components\":{{\"groundhog-rpc\":{{\"active\":true,\"sourceCommit\":\"{source_commit}\",\"version\":\"{version}\"}}}},\"schema\":\"groundhog-bundle-v1\"}}\n"
    );
    let digest = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    );
    let mut manifest: serde_json::Value = serde_json::from_str(&canonical).unwrap();
    manifest["bundle"]["manifestDigest"] = serde_json::Value::String(digest);
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

fn embedded_source_commit() -> Option<&'static str> {
    let commit = thruput_build_info::build_version!().source_commit();
    if let Ok(required) = std::env::var("GROUNDHOG_SOURCE_COMMIT") {
        assert_eq!(commit, Some(required.as_str()));
    }
    commit
}

/// Directory holding the per-invocation config and cache homes `thruput()`
/// hands the CLI.
///
/// Each test process gets a uniquely named directory under cargo's target
/// tmpdir, with a sibling `<name>.lock` it holds locked until it exits. The
/// lock file is locked before it is renamed into place, so a lock that can be
/// acquired always belongs to an exited process: the first call removes those
/// homes, so repeated runs do not accumulate them.
fn test_homes_root() -> &'static std::path::Path {
    static ROOT: OnceLock<(std::path::PathBuf, std::fs::File)> = OnceLock::new();
    let (root, _lock) = ROOT.get_or_init(|| {
        let parent =
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli-integration-homes");
        std::fs::create_dir_all(&parent).expect("bug: test homes parent should be creatable");
        remove_exited_test_homes(&parent);
        let root = tempfile::Builder::new()
            .prefix("homes-")
            .tempdir_in(&parent)
            .expect("bug: test homes directory should be creatable")
            .keep();
        let lock = tempfile::NamedTempFile::new_in(&parent)
            .expect("bug: test homes lock should be creatable");
        lock.as_file()
            .try_lock()
            .expect("bug: a fresh test homes lock should be free");
        let mut lock_path = root.clone().into_os_string();
        lock_path.push(".lock");
        let lock = lock
            .persist(lock_path)
            .expect("bug: test homes lock should be renamable");
        (root, lock)
    });
    root
}

fn remove_exited_test_homes(parent: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let lock_path = entry.path();
        let Some(home) = lock_path
            .to_str()
            .and_then(|path| path.strip_suffix(".lock"))
            .map(std::path::PathBuf::from)
        else {
            continue;
        };
        let Ok(lock) = std::fs::File::options().write(true).open(&lock_path) else {
            continue;
        };
        if lock.try_lock().is_err() {
            continue;
        }
        // Keep the lock while the home remains so a later sweep retries it.
        let removed = match std::fs::remove_dir_all(&home) {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        };
        if removed {
            let _ = std::fs::remove_file(&lock_path);
        }
    }
}

fn thruput() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
    command.env_remove("THRUPUT_BUNDLE_MANIFEST");
    let home = CONFIG_HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
    let config_home = test_homes_root().join(format!("config-{home}"));
    let cache_home = test_homes_root().join(format!("cache-{home}"));
    let thruput_config = config_home.join("thruput");
    std::fs::create_dir_all(&thruput_config).expect("bug: test config directory should exist");
    std::fs::write(
        thruput_config.join("config.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .expect("bug: test config should be writable");
    command.env_remove("THRUPUT_RUNTIME_MANIFEST");
    command.env_remove("THRUPUT_PROFILE");
    command.env_remove("THRUPUT_DEVNODE_BINARY");
    command.env_remove("THRUPUT_OFFLINE");
    command.env_remove("THRUPUT_TOOLCHAIN_BASE_URL");
    isolation::isolate_from_developer_env(&mut command);
    command.env("XDG_CONFIG_HOME", config_home);
    command.env("XDG_CACHE_HOME", cache_home);
    command
}

fn write_global_config(config_home: &std::path::Path, contents: &str) {
    let directory = config_home.join("thruput");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("config.toml"), contents).unwrap();
}

#[cfg(unix)]
// Creates the minimum persistent on-disk state accepted by `thruput chain dump`.
fn write_chain_data_fixture(root: &std::path::Path) {
    // The archive contract includes these database directories but excludes
    // transient runtime state such as endpoints and process identifiers.
    std::fs::create_dir_all(root.join("ramdb")).unwrap();
    std::fs::create_dir_all(root.join("events")).unwrap();
    std::fs::create_dir_all(root.join("indexer")).unwrap();
    std::fs::write(
        root.join("hog-devnode-data.json"),
        br#"{"schemaVersion":5,"kind":"hog-devnode-data"}"#,
    )
    .unwrap();
    std::fs::write(root.join("ramdb/0.log"), b"ramdb-state").unwrap();
    std::fs::write(root.join("events/block.events"), b"event-state").unwrap();
    std::fs::write(root.join("indexer/CURRENT"), b"MANIFEST-1\n").unwrap();
    for relative in [
        thruput_node_supervisor::LOCAL_FAUCET_KEY_FILE,
        thruput_node_supervisor::LOCAL_FUNDING_ATTEMPTS_FILE,
    ] {
        let path = root.join(relative);
        std::fs::write(&path, b"private fixture").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
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
    let dump = thruput()
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
    assert_eq!(dump_json["data"]["schemaVersion"], 2);
    assert_eq!(dump_json["data"]["fileCount"], "6");
    assert!(archive.is_file());

    let original_archive = std::fs::read(&archive).unwrap();
    let refused_overwrite = thruput()
        .args(["--json", "chain", "dump", "--data-dir"])
        .arg(&source)
        .arg("--output")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(!refused_overwrite.status.success());
    let refused_overwrite_json: serde_json::Value =
        serde_json::from_slice(&refused_overwrite.stderr).unwrap();
    assert_eq!(
        refused_overwrite_json["error"]["code"],
        "confirmation_required"
    );
    assert_eq!(
        refused_overwrite_json["error"]["details"]["requiredFlag"],
        "--yes"
    );
    assert_eq!(std::fs::read(&archive).unwrap(), original_archive);

    // A populated destination must remain protected unless the caller opts in
    // to replacement with `--force`.
    write_chain_data_fixture(&destination);
    let refused = thruput()
        .args(["--json", "chain", "restore", "--data-dir"])
        .arg(&destination)
        .arg("--input")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let refused_json: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(refused_json["error"]["code"], "confirmation_required");
    assert_eq!(refused_json["error"]["details"]["requiredFlag"], "--yes");

    // Make the destination observably different so the final assertion proves
    // that forced restore replaced its contents with the archived source state.
    std::fs::write(destination.join("ramdb/0.log"), b"different").unwrap();
    let restored = thruput()
        .args(["--json", "chain", "restore", "-f", "--data-dir"])
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
    assert_eq!(restore_json["data"]["schemaVersion"], 2);
    assert_eq!(
        std::fs::read(destination.join("ramdb/0.log")).unwrap(),
        b"ramdb-state"
    );
}

#[path = "support/devnode.rs"]
mod devnode;

use devnode::binary as fake_hogd_dev_binary;

#[cfg(unix)]
fn install_fake_managed_toolchain(
    cache_home: &std::path::Path,
    source_binary: &std::path::Path,
) -> std::path::PathBuf {
    let catalog = thruput_toolchain::Catalog::embedded().expect("bug: catalog should load");
    let target = thruput_toolchain::HostTarget::current().expect("bug: host should be managed");
    let version = catalog.default_version();
    let artifact = catalog
        .artifact(version, target)
        .expect("bug: default host artifact should exist");
    let root = cache_home
        .join("thruput/toolchain/hog-devnode")
        .join(version)
        .join(target.triple());

    for file in artifact.files() {
        let destination = root.join(file.path());
        std::fs::create_dir_all(destination.parent().expect("bug: file should have parent"))
            .expect("bug: managed fixture parent should create");
        if file.path() == "hog-devnode" {
            std::fs::copy(source_binary, &destination)
                .expect("bug: fake managed binary should copy");
        } else {
            std::fs::write(&destination, b"fixture")
                .expect("bug: managed fixture file should write");
        }
    }
    std::fs::write(
        root.join(".ok"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "version": version,
            "target": target.triple(),
            "archiveSha256": artifact.sha256(),
            "layoutVersion": artifact.layout_version(),
        }))
        .expect("bug: completion fixture should serialize"),
    )
    .expect("bug: completion fixture should write");
    root.join("hog-devnode")
}

/// Write `input` to a child that may refuse without reading it: if it has
/// already exited, the pipe is closed and the write fails with `BrokenPipe`.
fn offer_stdin(child: &mut std::process::Child, input: &[u8]) {
    let stdin = child.stdin.as_mut().expect("bug: stdin should be piped");
    if let Err(error) = stdin.write_all(input) {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe, "{error}");
    }
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
fn spawn_with_terminal(command: Command) -> (std::process::Child, File) {
    spawn_with_terminal_width(command, 0)
}

#[cfg(unix)]
fn spawn_with_terminal_width(command: Command, columns: u16) -> (std::process::Child, File) {
    spawn_with_terminal_size(command, columns, 24)
}

#[cfg(unix)]
fn spawn_with_terminal_size(
    command: Command,
    columns: u16,
    rows: u16,
) -> (std::process::Child, File) {
    spawn_with_terminal_io(command, columns, rows, false)
}

#[cfg(unix)]
fn spawn_with_terminal_outputs(
    command: Command,
    columns: u16,
    terminal_stdout: bool,
) -> (std::process::Child, File) {
    spawn_with_terminal_io(command, columns, 24, terminal_stdout)
}

#[cfg(unix)]
fn spawn_with_terminal_io(
    mut command: Command,
    columns: u16,
    rows: u16,
    terminal_stdout: bool,
) -> (std::process::Child, File) {
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: openpty initializes both file descriptors on success. Each is
    // immediately wrapped in an owned File exactly once.
    let result = unsafe {
        libc::openpty(
            &raw mut master_fd,
            &raw mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    assert_eq!(result, 0, "failed to allocate pseudo-terminal");
    // SAFETY: openpty returned unique, valid descriptors owned by this process.
    let master = unsafe { File::from_raw_fd(master_fd) };
    // SAFETY: openpty returned unique, valid descriptors owned by this process.
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let slave_stdin = slave
        .try_clone()
        .expect("bug: pseudo-terminal descriptor should be cloneable");
    let stdout = if terminal_stdout {
        Stdio::from(slave.try_clone().unwrap())
    } else {
        Stdio::piped()
    };
    command
        .stdin(Stdio::from(slave_stdin))
        .stdout(stdout)
        .stderr(Stdio::from(slave));
    let child = command
        .spawn()
        .expect("bug: CLI should spawn with pseudo-terminal");
    (child, master)
}

#[cfg(unix)]
fn read_terminal_output(mut terminal: File) -> String {
    let mut output = Vec::new();
    match terminal.read_to_end(&mut output) {
        Ok(_) => {}
        // Linux PTYs report EIO after the slave closes; bytes read before that
        // error remain valid output. macOS normally reports EOF instead.
        Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
        Err(error) => panic!("failed to read pseudo-terminal output: {error}"),
    }
    String::from_utf8(output).expect("bug: CLI terminal output should be UTF-8")
}

#[cfg(unix)]
fn strip_terminal_styles(output: &str) -> String {
    let mut chars = output.chars();
    let mut plain = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            let mut escape = String::from(ch);
            for ch in chars.by_ref() {
                escape.push(ch);
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
            if !escape.ends_with('m') {
                plain.push_str(&escape);
            }
        } else if ch != '\r' {
            plain.push(ch);
        }
    }
    plain
}

#[cfg(unix)]
fn terminal_output_reader(terminal: &File) -> thread::JoinHandle<String> {
    let reader = terminal
        .try_clone()
        .expect("bug: pseudo-terminal descriptor should be cloneable");
    thread::spawn(|| read_terminal_output(reader))
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

/// Interrupt first so `thruput node` can stop the hog-devnode it supervises:
/// with `--data-dir` that child outlives a killed parent and would be left
/// running. Kill only if the CLI does not exit in time.
#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.0.as_mut() else {
            return;
        };
        if matches!(child.try_wait(), Ok(None))
            && let Ok(pid) = i32::try_from(child.id())
        {
            // SAFETY: sends SIGINT to the CLI process this guard spawned.
            unsafe { libc::kill(pid, libc::SIGINT) };
            let deadline = Instant::now() + Duration::from_secs(10);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = child.kill();
        let _ = child.wait();
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

/// What a command's `--help` must or must not say.
enum HelpText {
    Has(&'static str),
    Lacks(&'static str),
    /// No line of the command list starts with this removed command.
    LacksCommand(&'static str),
}

/// Each command, its nested subcommands, and what their `--help` must say.
const HELP_CASES: &[(&[&str], &[HelpText])] = {
    use HelpText::{Has, Lacks, LacksCommand};
    &[
        (
            &[],
            &[
                Has("new"),
                Has("init"),
                Has("status"),
                Has("build"),
                Has("test"),
                Has("deploy"),
                Has("call"),
                Has("view"),
                Has("storage"),
                Has("keys"),
                Has("completion"),
                Has("skill"),
                LacksCommand("login"),
                LacksCommand("logout"),
            ],
        ),
        (
            &["build"],
            &[
                Has("--target-dir"),
                Has("--bindings <BINDINGS>"),
                Has("RV32"),
                Lacks("--target <TARGET>"),
                Lacks("--optimize"),
            ],
        ),
        (
            &["test"],
            &[
                Has("--target-dir"),
                Has("rv32"),
                Has("--watch"),
                Has("--integration"),
                Lacks("--target <TARGET>"),
                Lacks("--differential"),
                Has("--hog-devnode-version"),
                Has("-v, --verbose"),
                Has("RUST_LOG"),
                Has("hog-devnode"),
                Has("tests/integration.rs"),
                Lacks("not wired yet"),
            ],
        ),
        (
            &["node"],
            &[
                Has("-v, --verbose"),
                Has("--verbose"),
                Has("--verbose is also honored with --json"),
                Has("--hog-devnode-binary"),
                Has("--contract-artifacts-dir <DIR>"),
                Has("--hog-devnode-version"),
                Has("--data-dir"),
                Has("--init-network-hash <BASE58>"),
                Has("base58 encoding of exactly 32 bytes"),
                Has("default: all zeros for new chains"),
                Has("persistent hog-devnode state"),
                Has("--key <KEY>"),
                Has("--initial-account-tokens <AMOUNT>"),
                Has("Initial native token balance for the selected key"),
                Has("--hogd-protocol"),
                Has("THRUPUT_DEVNODE_BINARY"),
                Has("THRUPUT_OFFLINE"),
                Has("THRUPUT_TOOLCHAIN_BASE_URL"),
                Has("RUST_LOG"),
                Has("hogd_timestamp_ms"),
                Has("hogd_target"),
                Has("hogd_file"),
                Has("hogd_line"),
            ],
        ),
        (
            &["deploy"],
            &[
                Has("--target-dir"),
                Has("--dry-run"),
                Lacks("--target <TARGET>"),
            ],
        ),
        (
            &["call"],
            &[
                Has("--target-dir"),
                Has("--dry-run"),
                Has("0x-prefixed 8-byte entrypoint ID"),
                Has("raw hex calldata"),
            ],
        ),
        (&["storage"], &[Has("getStorage"), Has("--profile")]),
        (&["chain", "restore"], &[Has("-f, --force")]),
        (
            &["config"],
            &[
                Has("Inspect and manage persisted global and project profiles"),
                Has("pass --global or --project"),
                Has("omitted scope means global"),
                Has("get"),
                Has("profiles"),
                Has("new"),
                Has("rm"),
                Has("set"),
                Has("unset"),
                Has("use"),
            ],
        ),
        (
            &["config", "get"],
            &[
                Has("merged from global and project configuration"),
                Has("--sources"),
                Lacks("--global"),
                Lacks("--project"),
            ],
        ),
        (
            &["config", "new"],
            &[
                Has("one atomic config update"),
                Has("omitted scope means global"),
                Has("choose --global or --project explicitly"),
                Has("cannot be used together"),
                Has("--network <NETWORK>"),
                Has("--use"),
            ],
        ),
        (
            &["config", "rm"],
            &[
                Has("contract aliases and wallet bindings"),
                Has("asks for confirmation"),
                Has("-f, --force"),
                Has("Skip the confirmation prompt"),
                Has("cannot be undone"),
                Has("default profile selection is cleared"),
                Has("--global"),
                Has("--project"),
            ],
        ),
        (
            &["config", "set"],
            &[Has("must already exist in that layer")],
        ),
        (
            &["config", "unset"],
            &[Has("Missing fields"), Has("inherited global value")],
        ),
        (
            &["config", "use"],
            &[Has("THRUPUT_PROFILE"), Has("either layer")],
        ),
        (&["keys"], &[Has("export")]),
        (
            &["keys", "export"],
            &[
                Has("written to stdout as unprefixed, case-sensitive base58"),
                Has("Treat it as a secret"),
                Has("logs, CI output, or committed files"),
            ],
        ),
        (
            &["keys", "rm"],
            &[
                Has("-f, --force"),
                Has("Skip the confirmation prompt"),
                Has("cannot be undone"),
            ],
        ),
        (
            &["tx"],
            &[
                Has("show"),
                Has("list"),
                Has("transactions and address history"),
            ],
        ),
        (
            &["tx", "show"],
            &[
                Has("one-shot public transaction lookup without waiting"),
                Has("public status and receipt fields"),
                Has("Raw transaction bodies"),
                Has("deep traces are not available"),
                Has("--profile"),
                Has("--rpc-url"),
                Has("--runtime-manifest"),
            ],
        ),
        (
            &["tx", "list"],
            &[
                Has("exactly one page"),
                Has("root invocation"),
                Has("Incoming wallet-wrapped transfers may not appear"),
                Has("continuation cursor is opaque and reusable"),
                Has("Receipts are not fetched"),
                Has("--limit"),
                Has("--cursor"),
                Has("--profile"),
                Has("--rpc-url"),
                Has("--runtime-manifest"),
            ],
        ),
        (
            &["events", "list"],
            &[
                Has("exactly one newest-first page"),
                Has("global event scan is not performed"),
                Has("Repeat a topic option for OR within that position"),
                Has("Raw topics and data are authoritative"),
                Has("cursor is opaque and must be reused"),
                Has("--topic0"),
                Has("--topic1"),
                Has("--topic2"),
                Has("--limit"),
                Has("--cursor"),
                Has("--spec"),
                Has("--event"),
                Has("--profile"),
                Has("--rpc-url"),
                Has("--runtime-manifest"),
            ],
        ),
        (
            &["token", "transfers"],
            &[
                Has("exactly one newest-first page"),
                Has("canonical ERC20 TransferEvent"),
                Has("MintEvent"),
                Has("BurnEvent"),
                Has("--from"),
                Has("--to"),
                Has("--account"),
                Has("--limit"),
                Has("--cursor"),
                Has("--profile"),
                Has("--rpc-url"),
                Has("--runtime-manifest"),
            ],
        ),
    ]
};

#[test]
fn help_documents_each_command() {
    use HelpText::{Has, Lacks, LacksCommand};

    for (args, expectations) in HELP_CASES {
        let case = ["thruput"]
            .iter()
            .chain(*args)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let output = thruput()
            .args(*args)
            .arg("--help")
            .output()
            .unwrap_or_else(|error| panic!("`{case} --help` did not run: {error}"));
        assert!(
            output.status.success(),
            "`{case} --help` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout)
            .unwrap_or_else(|error| panic!("`{case} --help` printed invalid UTF-8: {error}"));
        for expectation in *expectations {
            match expectation {
                Has(text) => assert!(
                    stdout.contains(text),
                    "`{case} --help` is missing `{text}`:\n{stdout}"
                ),
                Lacks(text) => assert!(
                    !stdout.contains(text),
                    "`{case} --help` still shows `{text}`:\n{stdout}"
                ),
                LacksCommand(command) => assert!(
                    !stdout
                        .lines()
                        .any(|line| line.trim_start().starts_with(command)),
                    "`{case} --help` still lists removed command `{command}`:\n{stdout}"
                ),
            }
        }
    }
}

#[test]
fn skill_show_reads_global_installation_outside_a_project() {
    let directory = TempDir::new().unwrap();
    let installed = directory.path().join("home/.agents/skills/thruput");
    std::fs::create_dir_all(&installed).unwrap();
    let content = "---\nname: thruput\n---\n\nUser-installed guidance\n";
    std::fs::write(installed.join("SKILL.md"), content).unwrap();
    let output = thruput()
        .args(["skill", "show", "--offline"])
        .env("HOME", directory.path().join("home"))
        .env("PATH", "")
        .current_dir(directory.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    assert_eq!(output.stdout, content.as_bytes());
}

#[test]
fn legacy_skill_flag_is_not_supported() {
    let output = thruput().arg("--skill").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
}

#[test]
fn bare_cli_still_requires_an_action() {
    let output = thruput().output().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

#[test]
fn removed_auth_subcommands_are_rejected() {
    for removed_command in ["login", "logout"] {
        let output = thruput().arg(removed_command).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
    }
}

#[test]
fn version_flag_works() {
    let output = thruput().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = thruput_build_info::build_version!();
    let commit = expected
        .source_commit()
        .map_or("unavailable", |commit| &commit[..8]);
    assert_eq!(
        stdout,
        format!("thruput {} (commit {commit})\n", expected.crate_version())
    );
}

#[test]
fn profile_environment_uses_only_the_thruput_name() {
    let old = thruput()
        .env("GROUNDHOG_PROFILE", "missing-profile")
        .args(["config", "get", "--json"])
        .output()
        .unwrap();
    assert!(old.status.success(), "{old:?}");

    let new = thruput()
        .env("THRUPUT_PROFILE", "missing-profile")
        .args(["config", "get", "--json"])
        .output()
        .unwrap();
    assert!(!new.status.success());
    assert!(String::from_utf8_lossy(&new.stderr).contains("missing-profile"));
}

#[test]
fn version_flag_reports_matching_bundle_as_release() {
    let directory = TempDir::new().unwrap();
    let manifest = directory.path().join("bundle-manifest.json");
    let Some(source_commit) = embedded_source_commit() else {
        return;
    };
    write_bundle_manifest(&manifest, env!("CARGO_PKG_VERSION"), source_commit);

    let output = thruput()
        .env("THRUPUT_BUNDLE_MANIFEST", manifest)
        .arg("--version")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "thruput {} (commit {}, release 0.1.25-rc.4)\n",
            env!("CARGO_PKG_VERSION"),
            &source_commit[..8]
        )
    );
}

#[test]
fn version_flag_discovers_executable_relative_bundle_manifest() {
    let directory = TempDir::new().unwrap();
    let bin_dir = directory.path().join("bin");
    let share_dir = directory.path().join("share/thruput");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&share_dir).unwrap();
    let installed_cli = bin_dir.join("thruput");
    std::fs::copy(env!("CARGO_BIN_EXE_thruput"), &installed_cli).unwrap();
    let Some(source_commit) = embedded_source_commit() else {
        return;
    };
    write_bundle_manifest(
        &share_dir.join("bundle-manifest.json"),
        env!("CARGO_PKG_VERSION"),
        source_commit,
    );

    let output = Command::new(installed_cli)
        .env_remove("THRUPUT_BUNDLE_MANIFEST")
        .arg("--version")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "thruput {} (commit {}, release 0.1.25-rc.4)\n",
            env!("CARGO_PKG_VERSION"),
            &source_commit[..8]
        )
    );
}

#[test]
fn version_flag_does_not_claim_mismatched_bundle() {
    let directory = TempDir::new().unwrap();
    let manifest = directory.path().join("bundle-manifest.json");
    write_bundle_manifest(&manifest, env!("CARGO_PKG_VERSION"), "not-this-build");
    let source_commit = embedded_source_commit();

    let output = thruput()
        .env("THRUPUT_BUNDLE_MANIFEST", manifest)
        .arg("--version")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "thruput {} (commit {}, bundle manifest mismatch)\n",
            env!("CARGO_PKG_VERSION"),
            source_commit.map_or("unavailable", |commit| &commit[..8])
        )
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected_reason = source_commit.map_or_else(
        || "binary does not carry a source commit for groundhog-rpc".to_owned(),
        |commit| {
            format!(
                "bundle manifest expects groundhog-rpc source commit not-this-build, binary reports {commit}"
            )
        },
    );
    assert!(
        stderr.contains(&format!(
            "warning: bundle manifest mismatch: {expected_reason}"
        )),
        "missing precise mismatch diagnostic; stderr: {stderr}"
    );
}

#[test]
fn bundle_mismatch_does_not_corrupt_json_error_output() {
    let directory = TempDir::new().unwrap();
    let output = thruput()
        .env(
            "THRUPUT_BUNDLE_MANIFEST",
            directory.path().join("missing-bundle-manifest.json"),
        )
        .args(["--json", "config", "get", "missing"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr)
        .expect("mismatched bundle must not add plain text to JSON stderr");
    assert_eq!(stderr["ok"], false);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("bundle manifest mismatch"));
}

#[test]
fn config_get_rejects_removed_scope_flags() {
    for removed_flag in ["--global", "--project"] {
        let rejected = thruput()
            .args(["config", "get", removed_flag])
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("unexpected argument"));
    }
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
NATIVE_TOKEN = "0x01"
"#,
    );
    std::fs::write(
        project.join("thruput.toml"),
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

    let merged = thruput()
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
    assert!(merged_stdout.contains("contracts.NATIVE_TOKEN"));
    assert!(merged_stdout.contains("global"));

    let plain = thruput()
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

[profile.devnet.network_config]
max_tx_gas = "18446744073709551615"
max_rv32_code_bytes = 262144

[profile.broken]
network = "preview"
"#,
    );
    std::fs::write(
        project.join("thruput.toml"),
        r#"default_profile = "devnet"

[profile.devnet]
rpc_url = "https://rpc.example"
"#,
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "config", "get", "devnet", "--sources"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("THRUPUT_PROFILE", "broken")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(payload["command"], "config.get");
    assert_eq!(payload["data"]["schemaVersion"], 2);
    assert_eq!(payload["data"]["selectionSource"]["kind"], "explicit");
    assert_eq!(payload["data"]["scope"], "resolved");
    assert_eq!(
        payload["data"]["networkConfig"]["max_tx_gas"],
        "18446744073709551615"
    );
    assert_eq!(
        payload["data"]["networkConfig"]["max_rv32_code_bytes"],
        "262144"
    );
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
            .join("thruput/runtime/manifest.json")
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

    let output = thruput()
        .args(["--json", "config", "profiles"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("THRUPUT_PROFILE", "missing")
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

    let output = thruput()
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
    assert!(stdout.contains("expected one of `local`, `futurenet`, `devnet`, `mainnet`"));
    assert!(stdout.contains("valid"));
    assert!(stdout.contains("devnet"));
}

#[test]
fn config_profiles_succeeds_without_an_active_profile() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(&config_home, "[profile.devnet]\nnetwork = \"devnet\"\n");

    let output = thruput()
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

    let output = thruput()
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

    let malformed = thruput()
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
    let missing = thruput()
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

    let output = thruput()
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
    let no_active = thruput()
        .args(["--json", "config", "get"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let no_active_error: serde_json::Value = serde_json::from_slice(&no_active.stderr).unwrap();
    assert_eq!(no_active_error["error"]["code"], "no_active_profile");

    let missing = thruput()
        .args(["--json", "config", "get", "missing"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let missing_error: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(missing_error["error"]["code"], "profile_not_found");

    write_global_config(&config_home, "[executor]\npassword = \"super-secret\n");
    let malformed = thruput()
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

    let malformed_human = thruput()
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
fn config_new_accepts_futurenet_without_pinning_overlay_endpoints() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "[profile.futurenet]\nnetwork = 'futurenet'\nrpc_url = 'https://custom.example'\nfaucet_url = 'https://faucet.example'\n",
    );
    std::fs::write(tmp.path().join("thruput.toml"), "").unwrap();
    let output = thruput()
        .args([
            "config",
            "new",
            "futurenet",
            "--network",
            "futurenet",
            "--project",
            "--use",
            "--json",
        ])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["data"]["createdFields"],
        serde_json::json!(["network"])
    );
    let output = thruput()
        .args(["config", "get", "--json"])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data"]["network"], "futurenet");
    assert_eq!(report["data"]["rpcUrl"], "https://custom.example");
    assert_eq!(report["data"]["faucetUrl"], "https://faucet.example");
}

#[test]
fn config_new_json_creates_complete_profile_then_set_reports_noop() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "# retained\nunknown = true\n\n[executor]\nusername = \"private-user\"\npassword = \"private-password\"\n\n[local_runtime_state]\nnode_id = \"private-node\"\nrpc_url = \"http://127.0.0.1:8545\"\nruntime_manifest = \"/private/runtime.json\"\n",
    );

    let first = thruput()
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
        .env("THRUPUT_PROFILE", "local")
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
            "path": config_home.join("thruput/config.toml"),
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

    let second = thruput()
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

    let contents = std::fs::read_to_string(config_home.join("thruput/config.toml")).unwrap();
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

    let output = thruput()
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

    let duplicate = thruput()
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
            .contains("thruput config set")
    );
    assert_eq!(duplicate_error["error"]["details"]["profile"], "devnet");
    assert_eq!(duplicate_error["error"]["details"]["scope"], "global");

    let invalid = thruput()
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
        "must be one of local, futurenet, devnet (compatibility alias), or mainnet"
    );
    assert!(
        !std::fs::read_to_string(config_home.join("thruput/config.toml"))
            .unwrap()
            .contains("staging")
    );
}

#[cfg(unix)]
#[test]
fn config_rm_confirmation_preserves_profile_on_no_or_empty() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let contents = "default_profile = \"staging\"\n\n[profile.staging]\nnetwork = \"devnet\"\n\n[profile.preview]\nnetwork = \"devnet\"\n";
    write_global_config(&config_home, contents);

    for (profile, answer) in [
        ("staging", b"n\n".as_slice()),
        ("preview", b"\n".as_slice()),
    ] {
        let mut command = thruput();
        command
            .args(["config", "rm", profile])
            .env("XDG_CONFIG_HOME", &config_home)
            .current_dir(tmp.path());
        let (child, mut terminal) = spawn_with_terminal(command);
        let output_reader = terminal_output_reader(&terminal);
        terminal.write_all(answer).unwrap();
        let output = child.wait_with_output().unwrap();
        let terminal_output = output_reader
            .join()
            .expect("bug: terminal output reader should not panic");

        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            terminal_output.contains(&format!("Remove profile `{profile}` from global config?")),
            "terminal output: {terminal_output}"
        );
        assert!(
            terminal_output.contains("Cancelled"),
            "terminal output: {terminal_output}"
        );
        assert_eq!(
            std::fs::read_to_string(config_home.join("thruput/config.toml")).unwrap(),
            contents
        );
    }
}

#[cfg(unix)]
#[test]
fn config_rm_confirmation_removes_all_profile_data_and_clears_default() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "default_profile = \"staging\"\nunknown = true\n\n[profile.staging]\nnetwork = \"devnet\"\n\n[profile.staging.contracts]\ncounter = \"0x1234\"\n",
    );
    let mut command = thruput();
    command
        .args(["config", "rm", "staging"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path());
    let (child, mut terminal) = spawn_with_terminal(command);
    let output_reader = terminal_output_reader(&terminal);
    terminal.write_all(b"y\n").unwrap();

    let output = child.wait_with_output().unwrap();
    let terminal_output = output_reader
        .join()
        .expect("bug: terminal output reader should not panic");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        terminal_output.contains("Remove profile `staging` from global config?"),
        "terminal output: {terminal_output}"
    );
    assert!(
        terminal_output.contains("Removed profile `staging`"),
        "terminal output: {terminal_output}"
    );
    assert!(
        terminal_output.contains("Default profile selection cleared"),
        "terminal output: {terminal_output}"
    );
    assert_eq!(
        std::fs::read_to_string(config_home.join("thruput/config.toml")).unwrap(),
        "unknown = true\n"
    );
}

#[test]
fn config_rm_force_json_is_non_interactive_and_reports_stable_result() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(
        &config_home,
        "default_profile = \"staging\"\n\n[profile.staging]\nnetwork = \"devnet\"\n",
    );

    let output = thruput()
        .args(["--json", "config", "rm", "--force", "staging"])
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
            "path": config_home.join("thruput/config.toml"),
            "changed": true,
            "clearedDefault": true,
        })
    );
}

#[test]
fn config_rm_force_quiet_is_non_interactive() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    write_global_config(&config_home, "[profile.staging]\nnetwork = \"devnet\"\n");

    let output = thruput()
        .args(["--quiet", "config", "rm", "--force", "staging"])
        .env("XDG_CONFIG_HOME", &config_home)
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(
        std::fs::read_to_string(config_home.join("thruput/config.toml")).unwrap(),
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
    std::fs::write(project.join("thruput.toml"), "# project\n").unwrap();

    let ambiguous = thruput()
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

    let conflicting = thruput()
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

    let invalid = thruput()
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

    let missing = thruput()
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

    let unavailable = thruput()
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

    let output = thruput()
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
        project.join("thruput.toml"),
        "[profile.devnet]\nrpc_url = \"https://project.example\"\nunknown = true\n",
    )
    .unwrap();

    let output = thruput()
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
    assert!(stderr.contains(project.join("thruput.toml").to_string_lossy().as_ref()));
    let contents = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
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

    let output = thruput()
        .args(["--json", "config", "use", "devnet", "--global"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("THRUPUT_PROFILE", "local")
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
    let contents = std::fs::read_to_string(config_home.join("thruput/config.toml")).unwrap();
    assert!(contents.starts_with("default_profile = \"devnet\""));
    assert!(contents.contains("[profile.local]"));
    assert!(contents.contains("[profile.devnet]"));
}

#[cfg(unix)]
#[test]
fn node_keeps_an_mldsa65_default_key_selected_and_explains_funding() {
    // Run once without a project and once in a project whose profile selects
    // an Ed25519 saved wallet, which can never belong to the ML-DSA key.
    for with_saved_wallet in [false, true] {
        let config_home = TempDir::new().unwrap();
        let data_root = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        for args in [
            &["--json", "keys", "generate", "pq", "--scheme", "mldsa65"][..],
            &["keys", "use", "pq"][..],
        ] {
            let output = thruput()
                .args(args)
                .env("XDG_CONFIG_HOME", config_home.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if with_saved_wallet {
            let owner = generate_key_public_key(config_home.path(), "alice");
            std::fs::write(
                project.path().join("thruput.toml"),
                format!(
                    "default_profile = \"local\"\n[profile.local]\ndefault_wallet = \"owned\"\n\
                     [profile.local.wallets.owned]\naddress = \"0x{}\"\nowner_public_key = \"{owner}\"\n",
                    "ab".repeat(32)
                ),
            )
            .unwrap();
        }
        let child = thruput()
            .args(["--json", "node", "--port", "0", "--hog-devnode-binary"])
            .arg(fake_hogd_dev_binary())
            .arg("--data-dir")
            .arg(data_root.path().join("node-data"))
            .current_dir(project.path())
            .env("THRUPUT_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
            .env("XDG_CONFIG_HOME", config_home.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bug: thruput node should spawn");
        let mut child = ChildGuard::new(child);
        let stdout = child
            .child_mut()
            .stdout
            .take()
            .expect("bug: stdout should be piped");
        let line = read_line_with_timeout(stdout, CLI_ACTION_TIMEOUT)
            .expect("bug: startup JSON should be readable");
        let payload: serde_json::Value =
            serde_json::from_str(&line).expect("bug: startup line should be JSON");

        // ML-DSA keys get no genesis allocation, but the key is still the
        // selected one: report its account and the airdrop that funds it.
        assert_eq!(payload["ok"], true, "{payload}");
        assert_eq!(payload["data"]["selectedKey"], "pq");
        assert_eq!(payload["data"]["setupHint"], serde_json::Value::Null);
        assert!(payload["data"].get("walletHint").is_none(), "{payload}");
        let wallet = &payload["data"]["selectedWallet"];
        assert_eq!(wallet["name"], "pq", "{payload}");
        assert_eq!(wallet["hasInitialAllocation"], false);
        assert!(
            wallet["fundingHint"]
                .as_str()
                .is_some_and(|hint| hint.contains("thruput airdrop")),
            "{wallet}"
        );
        // The saved Ed25519 default wallet still governs later commands, so
        // startup says it must be cleared before the ML-DSA key is usable.
        let warning = &wallet["defaultWalletWarning"];
        if with_saved_wallet {
            let warning = warning.as_str().expect("bug: warning should be reported");
            for expected in [
                "`owned`",
                "saved_wallet_owner_mismatch",
                "thruput wallet rm owned",
            ] {
                assert!(warning.contains(expected), "{warning}");
            }
        } else {
            assert_eq!(*warning, serde_json::Value::Null, "{wallet}");
        }
    }
}

#[cfg(unix)]
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one CLI lifecycle verifies bind, bootstrap, RPC identity and shutdown"
)]
fn node_hogd_dev_binary_json_binds_public_host_and_starts_supervisor() {
    let fake = fake_hogd_dev_binary();
    let pid_dir = TempDir::new().unwrap();
    let data_root = TempDir::new().unwrap();
    let data_dir = data_root.path().join("node-data");
    let config_home = TempDir::new().unwrap();
    generate_key_public_key(config_home.path(), "alice");
    let pid_path = pid_dir.path().join("hogd.pid");
    let child = thruput()
        .arg("--json")
        .arg("node")
        .arg("--host")
        .arg("0.0.0.0")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-binary")
        .arg(fake)
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--initial-account-tokens")
        .arg("25.0000001")
        .arg("--init-network-hash")
        .arg(thruput_encoding::encode(&[0xab; 32]))
        .env("THRUPUT_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("THRUPUT_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .env("XDG_CONFIG_HOME", config_home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bug: thruput node should spawn");
    let mut child = ChildGuard::new(child);

    let stdout = child
        .child_mut()
        .stdout
        .take()
        .expect("bug: stdout should be piped");
    let line = read_line_with_timeout(stdout, CLI_ACTION_TIMEOUT)
        .expect("bug: startup JSON should be readable");
    let payload: serde_json::Value =
        serde_json::from_str(&line).expect("bug: startup line should be JSON");

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["command"], "node");
    assert_eq!(
        payload["data"]["chain"],
        thruput_encoding::encode(&[0xab; 32])
    );
    assert_eq!(payload["data"]["schemaVersion"], 5);
    assert_eq!(payload["data"]["chainState"], "fresh");
    assert_eq!(payload["data"]["dataDir"], data_dir.display().to_string());
    assert_eq!(payload["data"]["backend"]["kind"], "hogd_dev");
    assert_eq!(payload["data"]["backend"]["stability"], "supported");
    assert!(payload["data"]["backend"].get("limitations").is_none());
    assert_eq!(payload["data"]["backend"]["planes"]["submit"], "scheduler");
    assert_eq!(payload["data"]["backend"]["planes"]["chain"], "db_gateway");
    assert_eq!(payload["data"]["backend"]["planes"]["read"], "indexer");
    assert_eq!(payload["data"]["backend"]["planes"]["view"], "stub");
    assert_eq!(payload["data"]["localRuntimeState"]["ok"], true);
    assert!(
        payload["data"]["localRuntimeState"]["path"]
            .as_str()
            .expect("bug: local_runtime_state path should be a string")
            .ends_with("thruput/config.toml")
    );
    let scheduler = payload["data"]["backend"]["endpoints"]["schedulerCoord"]
        .as_str()
        .expect("bug: SchedulerCoord endpoint should be a string");
    assert!(
        scheduler.starts_with("127.0.0.1:"),
        "SchedulerCoord endpoint should be loopback, got {scheduler}",
    );
    let rpc_url = payload["data"]["rpcUrl"]
        .as_str()
        .expect("bug: rpc_url string")
        .to_string();
    assert!(rpc_url.starts_with("http://127.0.0.1:"));
    let rpc_listen_address = payload["data"]["rpcListenAddress"]
        .as_str()
        .expect("bug: rpc_listen_address string");
    assert!(rpc_listen_address.starts_with("0.0.0.0:"));
    let manifest_path = payload["data"]["runtimeManifest"]["path"]
        .as_str()
        .expect("bug: runtime manifest path should be a string");
    assert_split_bootstrap_accounts(std::path::Path::new(manifest_path), "250000001");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
    assert_eq!(
        manifest["networkHash"],
        thruput_encoding::encode(&[0xab; 32])
    );

    assert_local_runtime_state_rpc_url(config_home.path(), &rpc_url);

    let tip = tokio::runtime::Runtime::new()
        .expect("bug: tokio runtime should start")
        .block_on(async {
            let client = NativeClient::new(
                HttpClientBuilder::default()
                    .build(&rpc_url)
                    .expect("bug: JSON-RPC client should build"),
            );
            assert_eq!(
                match client.get_network_info(EmptyRequest {}).await.unwrap() {
                    DecodedResponse::Known(info) => info.network_hash.into_inner(),
                    other => panic!("unexpected network info: {other:?}"),
                }
                .as_ref(),
                &[0xab; 32]
            );
            match client.get_chain_tip(EmptyRequest {}).await.unwrap() {
                DecodedResponse::Known(tip) => tip,
                other => panic!("unexpected chain tip: {other:?}"),
            }
        });
    assert_eq!(*tip.block_height, 0);

    let pid = read_pid_file(&pid_path);
    let journal_path = data_dir.join("groundhog-funding.json");
    let journal = std::fs::read(&journal_path).unwrap();
    for hash in [0xab, 0xcd, 0x00] {
        let output = thruput()
            .args([
                "node",
                "--port",
                "0",
                "--init-network-hash",
                &thruput_encoding::encode(&[hash; 32]),
            ])
            .arg("--hog-devnode-binary")
            .arg(fake_hogd_dev_binary())
            .arg("--data-dir")
            .arg(&data_dir)
            .env("XDG_CONFIG_HOME", config_home.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("already initialized"), "{error}");
        assert!(error.contains("omit it to restart"), "{error}");
        assert_eq!(std::fs::read(&journal_path).unwrap(), journal);
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let client = NativeClient::new(HttpClientBuilder::default().build(&rpc_url).unwrap());
        assert_eq!(
            match client.get_network_info(EmptyRequest {}).await.unwrap() {
                DecodedResponse::Known(info) => info.network_hash.into_inner(),
                other => panic!("unexpected network info: {other:?}"),
            }
            .as_ref(),
            &[0xab; 32]
        );
    });
    send_sigint(child.child());
    let status = wait_child(child.into_inner(), CLI_ACTION_TIMEOUT);
    assert!(status.success(), "thruput node should stop cleanly");
    let global_config = std::fs::read_to_string(config_home.path().join("thruput/config.toml"))
        .expect("thruput node should leave config readable");
    assert!(!global_config.contains("local_runtime_state"));
    assert!(
        wait_pid_gone(pid, CLI_ACTION_TIMEOUT),
        "fake hogd-dev pid {pid} still alive after CLI exit",
    );
}

fn assert_local_runtime_state_rpc_url(config_home: &std::path::Path, rpc_url: &str) {
    let global_config = std::fs::read_to_string(config_home.join("thruput/config.toml"))
        .expect("thruput node should write local runtime state");
    assert!(global_config.contains(&format!("rpc_url = \"{rpc_url}\"")));
    assert!(!global_config.contains("0.0.0.0"));
}

#[cfg(unix)]
#[test]
fn node_explicit_managed_version_uses_cache_before_path_offline() {
    let fake = fake_hogd_dev_binary();
    let cache_home = TempDir::new().unwrap();
    let managed_binary = install_fake_managed_toolchain(cache_home.path(), &fake);
    assert!(managed_binary.is_file());

    let path_dir = TempDir::new().unwrap();
    let path_binary = path_dir.path().join("hog-devnode");
    std::fs::write(&path_binary, "#!/bin/sh\nexit 73\n").unwrap();
    std::fs::set_permissions(&path_binary, std::fs::Permissions::from_mode(0o755)).unwrap();

    let pid_dir = TempDir::new().unwrap();
    let pid_path = pid_dir.path().join("hogd.pid");
    let child = thruput()
        .arg("--json")
        .arg("--offline")
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-version")
        .arg(
            thruput_toolchain::Catalog::embedded()
                .unwrap()
                .default_version(),
        )
        .env("XDG_CACHE_HOME", cache_home.path())
        .env("PATH", path_dir.path())
        .env("THRUPUT_DEVNODE_BINARY", &path_binary)
        .env("THRUPUT_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("THRUPUT_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bug: thruput node should spawn");
    let mut child = ChildGuard::new(child);

    let stdout = child
        .child_mut()
        .stdout
        .take()
        .expect("bug: stdout should be piped");
    let line = read_line_with_timeout(stdout, CLI_ACTION_TIMEOUT)
        .expect("bug: managed startup JSON should be readable");
    let payload: serde_json::Value =
        serde_json::from_str(&line).expect("bug: startup line should be JSON");
    assert_eq!(payload["ok"], true, "startup payload: {payload}");

    let pid = read_pid_file(&pid_path);
    send_sigint(child.child());
    let status = wait_child(child.into_inner(), CLI_ACTION_TIMEOUT);
    assert!(status.success(), "thruput node should stop cleanly");
    assert!(wait_pid_gone(pid, CLI_ACTION_TIMEOUT));
}

#[cfg(unix)]
#[test]
fn node_explicit_managed_version_does_not_fall_back_to_path_offline() {
    let fake = fake_hogd_dev_binary();
    let cache_home = TempDir::new().unwrap();
    let path_dir = TempDir::new().unwrap();
    std::fs::copy(&fake, path_dir.path().join("hog-devnode")).unwrap();

    let output = thruput()
        .arg("--json")
        .arg("--offline")
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-version")
        .arg(
            thruput_toolchain::Catalog::embedded()
                .unwrap()
                .default_version(),
        )
        .env("XDG_CACHE_HOME", cache_home.path())
        .env("PATH", path_dir.path())
        .output()
        .expect("bug: thruput node should run");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("bug: node error should be JSON");
    assert_eq!(payload["command"], "node");
    assert_eq!(payload["error"]["code"], "toolchain_offline_missing");
}

#[cfg(unix)]
#[test]
fn node_saved_managed_version_does_not_fall_back_to_path_offline() {
    let config_home = TempDir::new().unwrap();
    let cache_home = TempDir::new().unwrap();
    let path_dir = TempDir::new().unwrap();
    support::write_versioned_devnode(path_dir.path());
    let catalog = thruput_toolchain::Catalog::embedded().unwrap();
    write_global_config(
        config_home.path(),
        &format!(
            "[toolchain]\nhog_devnode_version = {:?}\n",
            catalog.default_version()
        ),
    );
    let output = thruput()
        .args(["--json", "--offline", "node"])
        .env("XDG_CONFIG_HOME", config_home.path())
        .env("XDG_CACHE_HOME", cache_home.path())
        .env("PATH", path_dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(payload["error"]["code"], "toolchain_offline_missing");
}

#[cfg(unix)]
#[test]
fn node_managed_download_stops_on_sigint() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let source = format!("http://{}", listener.local_addr().unwrap());
    let cache_home = TempDir::new().unwrap();
    let child = thruput()
        .args(["--json", "node", "--hog-devnode-version"])
        .arg(
            thruput_toolchain::Catalog::embedded()
                .unwrap()
                .default_version(),
        )
        .env("XDG_CACHE_HOME", cache_home.path())
        .env("THRUPUT_TOOLCHAIN_BASE_URL", source)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let child = ChildGuard::new(child);
    let deadline = Instant::now() + CLI_ACTION_TIMEOUT;
    let _connection = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "download request never reached source"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("failed to accept download: {error}"),
        }
    };
    send_sigint(child.child());
    let status = wait_child(child.into_inner(), CLI_ACTION_TIMEOUT);
    assert!(
        !status.success(),
        "cancelled acquisition must report that the node never started"
    );
    let catalog = thruput_toolchain::Catalog::embedded().unwrap();
    let target = thruput_toolchain::HostTarget::current().unwrap();
    let installer = thruput_toolchain::Installer::new(cache_home.path().join("thruput"));
    assert!(
        installer
            .find(&catalog, catalog.default_version(), target)
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn node_offline_default_falls_back_to_path_after_managed_cache_miss() {
    let fake = fake_hogd_dev_binary();
    let cache_home = TempDir::new().unwrap();
    let path_dir = TempDir::new().unwrap();
    std::fs::copy(&fake, path_dir.path().join("hog-devnode")).unwrap();
    let pid_dir = TempDir::new().unwrap();
    let pid_path = pid_dir.path().join("hogd.pid");

    let child = thruput()
        .arg("--json")
        .arg("--offline")
        .arg("node")
        .arg("--port")
        .arg("0")
        .env("XDG_CACHE_HOME", cache_home.path())
        .env("PATH", path_dir.path())
        .env("THRUPUT_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("THRUPUT_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bug: thruput node should spawn");
    let mut child = ChildGuard::new(child);

    let stdout = child
        .child_mut()
        .stdout
        .take()
        .expect("bug: stdout should be piped");
    let line = read_line_with_timeout(stdout, CLI_ACTION_TIMEOUT)
        .expect("bug: PATH fallback startup JSON should be readable");
    let payload: serde_json::Value =
        serde_json::from_str(&line).expect("bug: startup line should be JSON");
    assert_eq!(payload["ok"], true, "startup payload: {payload}");

    let pid = read_pid_file(&pid_path);
    send_sigint(child.child());
    let status = wait_child(child.into_inner(), CLI_ACTION_TIMEOUT);
    assert!(status.success(), "thruput node should stop cleanly");
    assert!(wait_pid_gone(pid, CLI_ACTION_TIMEOUT));
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
    let child = thruput()
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-binary")
        .arg(fake)
        .arg("--data-dir")
        .arg(&data_dir)
        .env("THRUPUT_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("THRUPUT_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("TMPDIR", &temp_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bug: thruput node should spawn");
    let mut child = ChildGuard::new(child);

    let stderr = child
        .child_mut()
        .stderr
        .take()
        .expect("bug: stderr should be piped");
    let output = read_until_with_timeout(stderr, "account manifest", CLI_ACTION_TIMEOUT)
        .expect("bug: human startup output should be readable");

    assert!(output.contains("chain state"), "startup output: {output}");
    assert!(output.contains("fresh"), "startup output: {output}");
    assert!(
        output.contains("export THRUPUT_RUNTIME_MANIFEST='") && output.contains("tmp & root"),
        "startup output: {output}"
    );

    let pid = read_pid_file(&pid_path);
    send_sigint(child.child());
    let status = wait_child(child.into_inner(), CLI_ACTION_TIMEOUT);
    assert!(status.success(), "thruput node should stop cleanly");
    assert!(
        wait_pid_gone(pid, CLI_ACTION_TIMEOUT),
        "fake hogd-dev pid {pid} still alive after CLI exit",
    );
}

#[cfg(unix)]
#[test]
fn node_invalid_key_configuration_fails_before_supervisor() {
    let fake = fake_hogd_dev_binary();
    let pid_dir = TempDir::new().unwrap();
    let config_home = TempDir::new().unwrap();
    let pid_path = pid_dir.path().join("hogd.pid");
    let invalid_config_home = config_home.path().join("not-a-directory");
    std::fs::write(&invalid_config_home, b"not a directory").unwrap();

    let output = thruput()
        .arg("--json")
        .arg("node")
        .arg("--port")
        .arg("0")
        .arg("--hog-devnode-binary")
        .arg(fake)
        .env("THRUPUT_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env("THRUPUT_FAKE_HOGD_DEV_PID_FILE", &pid_path)
        .env("XDG_CONFIG_HOME", &invalid_config_home)
        .output()
        .expect("bug: thruput node should run");

    assert!(
        !output.status.success(),
        "profile write failure should make thruput node fail"
    );
    assert!(
        !pid_path.exists(),
        "invalid configuration must fail before child startup"
    );
}

#[test]
fn node_hogd_dev_missing_binary_fails_without_native_fallback() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("missing-hog-devnode");
    let output = thruput()
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
    let output = thruput()
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
    assert!(dir.join("thruput.toml").exists());
    assert!(dir.join("build.rs").exists());
    assert!(dir.join("src/lib.rs").exists());
    assert!(dir.join("src/implementation.rs").exists());
    assert!(dir.join("tests/integration.rs").exists());
    assert!(dir.join("tests/test.rs").exists());
    assert!(dir.join("tests/test_rv32.rs").exists());
    assert!(!dir.join("tests/test_wasm.rs").exists());
    assert!(dir.join(".cargo/config.toml").exists());
    assert!(dir.join(".cargo/contract.toml").exists());
    assert!(dir.join("rust-toolchain.toml").exists());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Creating project `my-test-project`"));
    assert!(stderr.contains("[ok] Created project `my-test-project`"));
    assert!(stderr.contains("Project Ready"));
    assert!(stderr.contains("Project: my-test-project"));
    assert!(stderr.contains("Template: minimal"));
    assert!(stderr.contains("Config: "));
    assert!(stderr.contains("Next:"));
    assert!(stderr.contains(&format!("cd '{}'", dir.display())));
    assert!(!stderr.contains("Cargo Registry:"));
    assert!(!stderr.contains("cargo login --registry thruput"));
    assert!(stderr.contains("thruput build"));
}

#[test]
fn new_json_outputs_structured_payload() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
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
    assert_eq!(stdout["data"]["schemaVersion"], 2);
    assert_eq!(stdout["data"]["projectName"], "json-project");
    assert_eq!(stdout["data"]["template"], serde_json::Value::Null);
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

    let output = thruput()
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

    let output = thruput()
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
fn new_with_stub_template_explains_minimal_default() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
        .arg("new")
        .arg("stub-proj")
        .arg("--template")
        .arg("stub")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("omit --template"));
    assert!(!tmp.path().join("stub-proj").exists());
}

fn init_command(root: &std::path::Path) -> Command {
    let mut command = thruput();
    command
        .arg("init")
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root)
        .env("CARGO_HOME", root.join("cargo-home"));
    command
}

fn init_report(output: &std::process::Output, complete: bool) -> serde_json::Value {
    assert_eq!(
        output.status.code(),
        Some(i32::from(!complete)),
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    // from_slice rejects trailing records as well as human output on stdout.
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["command"], "init");
    assert_eq!(report["ok"], complete);
    assert_eq!(report["data"]["schemaVersion"], 1);
    assert_eq!(report["data"]["complete"], complete);
    if !complete {
        assert_eq!(report["error"]["code"], "init_incomplete");
    }
    report
}

#[test]
fn init_json_requires_key_consent_and_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let fresh = init_command(tmp.path()).arg("--json").output().unwrap();
    let fresh = init_report(&fresh, false);
    assert_eq!(fresh["data"]["cargoAvailable"], true);
    assert_eq!(fresh["data"]["profilesConfigured"], true);
    assert_eq!(fresh["data"]["selectedKey"], serde_json::Value::Null);
    assert_eq!(fresh["data"]["keyGenerated"], false);
    assert_eq!(fresh["data"]["missing"], serde_json::json!(["signingKey"]));
    assert!(tmp.path().join("thruput/config.toml").exists());
    assert!(!tmp.path().join("thruput/keys/default.json").exists());

    let generated = init_command(tmp.path())
        .args(["--non-interactive", "--generate-key", "--json"])
        .output()
        .unwrap();
    let generated = init_report(&generated, true);
    assert_eq!(generated["data"]["selectedKey"], "default");
    assert_eq!(generated["data"]["keyGenerated"], true);
    assert_eq!(generated["data"]["missing"], serde_json::json!([]));
    let key_path = tmp.path().join("thruput/keys/default.json");
    let key_before = std::fs::read(&key_path).unwrap();
    let config_path = tmp.path().join("thruput/config.toml");
    let config_before = std::fs::read(&config_path).unwrap();
    for flags in [vec!["--json"], vec!["--json", "--generate-key"]] {
        let repeated = init_command(tmp.path()).args(flags).output().unwrap();
        let repeated = init_report(&repeated, true);
        assert_eq!(repeated["data"]["selectedKey"], "default");
        assert_eq!(repeated["data"]["keyGenerated"], false);
        assert!(
            std::fs::read(&key_path).unwrap() == key_before,
            "init must preserve the stored key"
        );
        assert_eq!(std::fs::read(&config_path).unwrap(), config_before);
    }
    assert!(!tmp.path().join("my-contract").exists());
}

#[test]
fn init_quiet_does_not_authorize_generation_and_json_takes_precedence() {
    let tmp = TempDir::new().unwrap();
    let output = init_command(tmp.path()).arg("--quiet").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!tmp.path().join("thruput/keys/default.json").exists());
    let output = init_command(tmp.path())
        .args(["--quiet", "--json"])
        .output()
        .unwrap();
    let report = init_report(&output, false);
    assert_eq!(report["data"]["keyGenerated"], false);
    let output = init_command(tmp.path())
        .args(["--quiet", "--generate-key"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let output = init_command(tmp.path())
        .args(["--quiet", "--json"])
        .output()
        .unwrap();
    init_report(&output, true);
}

#[test]
fn init_does_not_replace_stored_keys_or_missing_selections() {
    for selection in [None, Some("ghost")] {
        let tmp = TempDir::new().unwrap();
        generate_key_public_key(tmp.path(), "alice");
        let key_path = tmp.path().join("thruput/keys/alice.json");
        let before = std::fs::read(&key_path).unwrap();
        write_global_config(
            tmp.path(),
            &selection
                .map(|name| format!("default_key = \"{name}\"\n"))
                .unwrap_or_default(),
        );
        let output = init_command(tmp.path())
            .args(["--generate-key", "--json"])
            .output()
            .unwrap();
        let report = init_report(&output, false);
        assert_eq!(report["data"]["keyGenerated"], false);
        assert_eq!(report["data"]["missing"], serde_json::json!(["signingKey"]));
        assert!(
            std::fs::read(&key_path).unwrap() == before,
            "init must preserve the stored key"
        );
        assert!(!tmp.path().join("thruput/keys/default.json").exists());
    }
}

#[test]
fn init_rejects_unusable_keypairs_and_mismatched_public_summaries() {
    for mismatched_public_key in [false, true] {
        let tmp = TempDir::new().unwrap();
        let output = init_command(tmp.path())
            .args(["--generate-key", "--json"])
            .output()
            .unwrap();
        init_report(&output, true);
        let key_path = tmp.path().join("thruput/keys/default.json");
        let mut key: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&key_path).unwrap()).unwrap();
        if mismatched_public_key {
            generate_key_public_key(tmp.path(), "other");
            let other: serde_json::Value = serde_json::from_slice(
                &std::fs::read(tmp.path().join("thruput/keys/other.json")).unwrap(),
            )
            .unwrap();
            key["public_key_hex"] = other["public_key_hex"].clone();
        } else {
            key["keypair_hex"] = serde_json::json!("invalid");
        }
        let before = serde_json::to_vec(&key).unwrap();
        std::fs::write(&key_path, &before).unwrap();
        let output = init_command(tmp.path())
            .args(["--generate-key", "--json"])
            .output()
            .unwrap();
        let report = init_report(&output, false);
        assert_eq!(report["data"]["selectedKey"], "default");
        assert_eq!(report["data"]["keyGenerated"], false);
        assert_eq!(report["data"]["missing"], serde_json::json!(["signingKey"]));
        assert!(
            std::fs::read(&key_path).unwrap() == before,
            "init must preserve the unusable key file"
        );
    }
}

#[test]
fn init_dangling_selection_prevents_generation_even_without_stored_keys() {
    for profile_selection in [false, true] {
        let tmp = TempDir::new().unwrap();
        let config = if profile_selection {
            "default_profile = \"local\"\n[profile.local]\ndefault_key = \"ghost\"\n"
        } else {
            "default_key = \"ghost\"\n"
        };
        write_global_config(tmp.path(), config);
        let output = init_command(tmp.path())
            .args(["--generate-key", "--json"])
            .output()
            .unwrap();
        let report = init_report(&output, false);
        assert_eq!(report["data"]["keyGenerated"], false);
        assert_eq!(report["data"]["missing"], serde_json::json!(["signingKey"]));
        assert!(!tmp.path().join("thruput/keys/default.json").exists());
    }
}

#[test]
fn init_dangling_project_key_does_not_fall_back_to_global_key() {
    let tmp = TempDir::new().unwrap();
    generate_key_public_key(tmp.path(), "alice");
    std::fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"local\"\n[profile.local]\ndefault_key = \"ghost\"\n",
    )
    .unwrap();
    let output = init_command(tmp.path())
        .args(["--generate-key", "--json"])
        .output()
        .unwrap();
    let report = init_report(&output, false);
    assert_eq!(report["data"]["keyGenerated"], false);
    assert_eq!(report["data"]["missing"], serde_json::json!(["signingKey"]));
    assert!(!tmp.path().join("thruput/keys/default.json").exists());
}

#[test]
fn init_reports_missing_cargo_and_unresolvable_profile() {
    let tmp = TempDir::new().unwrap();
    generate_key_public_key(tmp.path(), "alice");
    let output = init_command(tmp.path())
        .arg("--json")
        .env("PATH", tmp.path().join("empty-path"))
        .output()
        .unwrap();
    let report = init_report(&output, false);
    assert_eq!(report["data"]["cargoAvailable"], false);
    assert_eq!(report["data"]["missing"], serde_json::json!(["cargo"]));
    std::fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"missing\"\n[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();
    let output = init_command(tmp.path()).arg("--json").output().unwrap();
    let report = init_report(&output, false);
    assert!(
        report["data"]["missing"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("profile"))
    );
}

#[cfg(unix)]
#[test]
fn init_non_interactive_never_waits_for_terminal_answers() {
    let tmp = TempDir::new().unwrap();
    for generate in [false, true] {
        let mut command = init_command(tmp.path());
        command.arg("--non-interactive");
        if generate {
            command.arg("--generate-key");
        }
        let (child, terminal) = spawn_with_terminal(command);
        let reader = terminal_output_reader(&terminal);
        let status = wait_child(child, CLI_ACTION_TIMEOUT);
        drop(terminal);
        let output = reader.join().unwrap();
        assert_eq!(status.success(), generate, "{output}");
        assert!(!output.contains("Generate a"), "{output}");
        assert!(!output.contains("Would you"), "{output}");
        assert!(!tmp.path().join("my-contract").exists());
    }
}

#[cfg(unix)]
#[test]
fn global_non_interactive_keeps_terminal_output_plain_and_visible() {
    let tmp = TempDir::new().unwrap();
    let setup = init_command(tmp.path())
        .args(["--non-interactive", "--generate-key"])
        .output()
        .unwrap();
    assert!(setup.status.success());
    for (arguments, expected) in [
        (vec!["--non-interactive", "status"], "default"),
        (
            vec!["new", "plain-project", "--non-interactive"],
            "plain-project",
        ),
        (vec!["config", "get", "--non-interactive"], "local"),
    ] {
        let is_status = arguments.contains(&"status");
        let mut command = thruput();
        command
            .args(arguments)
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path())
            .env("TERM", "xterm")
            .env_remove("NO_COLOR");
        let (child, terminal) = spawn_with_terminal_outputs(command, 120, true);
        let reader = terminal_output_reader(&terminal);
        let status = wait_child(child, CLI_ACTION_TIMEOUT);
        drop(terminal);
        let output = reader.join().unwrap();
        if is_status {
            // Diagnostics may report missing optional local-node tooling.
            assert!(matches!(status.code(), Some(0 | 1)), "{output}");
        } else {
            assert!(status.success(), "{output}");
        }
        assert!(output.contains(expected), "{output}");
        assert!(!output.contains('\u{1b}'), "{output}");
        assert!(!output.contains('╭') && !output.contains('│'), "{output}");
    }
    assert!(tmp.path().join("plain-project/thruput.toml").exists());
}

/// Parse `bytes` as a JSON report, failing with `case` in the message.
fn case_json(case: &str, stream: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|error| {
        panic!(
            "{case}: {stream} is not a JSON report ({error}): {}",
            String::from_utf8_lossy(bytes)
        )
    })
}

/// Run `command` on a terminal and return its exit status and terminal output.
#[cfg(unix)]
fn run_on_terminal(command: Command, case: &str) -> (ExitStatus, String) {
    let (child, terminal) = spawn_with_terminal(command);
    let reader = terminal_output_reader(&terminal);
    let status = wait_child(child, CLI_ACTION_TIMEOUT);
    drop(terminal);
    let output = reader
        .join()
        .unwrap_or_else(|_| panic!("{case}: terminal reader panicked"));
    (status, output)
}

/// What a confirmation-guarded removal deletes.
#[derive(Clone, Copy, Debug)]
enum Removal {
    ConfigProfile,
    Key,
}

/// How a removal runs without someone answering its prompt. The terminal
/// modes, and waiting on the forced removal, need Unix.
#[derive(Clone, Copy, Debug)]
enum RemovalMode {
    /// A terminal is attached, but `--non-interactive` forbids prompting.
    #[cfg(unix)]
    NonInteractiveTerminal {
        json: bool,
    },
    /// `--force` removes on a non-interactive terminal without prompting.
    #[cfg(unix)]
    ForcedNonInteractiveTerminal,
    /// An answer piped to stdin is not consent.
    PipedStdin,
    Json,
    Quiet,
    /// `-f` removes without reading the stdin it was handed.
    #[cfg(unix)]
    ForcedWithPipedStdin,
}

#[test]
fn removals_require_force_without_an_interactive_prompt() {
    #[cfg(unix)]
    use RemovalMode::{ForcedNonInteractiveTerminal, ForcedWithPipedStdin, NonInteractiveTerminal};
    use RemovalMode::{Json, PipedStdin, Quiet};

    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut modes = vec![PipedStdin, Json, Quiet];
    #[cfg(unix)]
    modes.extend([
        NonInteractiveTerminal { json: false },
        NonInteractiveTerminal { json: true },
        ForcedNonInteractiveTerminal,
        ForcedWithPipedStdin,
    ]);
    for target in [Removal::ConfigProfile, Removal::Key] {
        for &mode in &modes {
            let case = format!("{target:?} {mode:?}");
            // Shown with a failing test's output, so a fixture or spawn failure
            // in a shared helper is attributed to its row.
            eprintln!("case: {case}");
            let tmp = TempDir::new().unwrap();
            let config_home = tmp.path().join("config-home");
            let (args, path, prompt, action) = match target {
                Removal::ConfigProfile => {
                    write_global_config(&config_home, "[profile.staging]\nnetwork = \"devnet\"\n");
                    (
                        ["config", "rm", "staging"],
                        config_home.join("thruput/config.toml"),
                        "Remove profile `staging` from global config?",
                        Some("remove a configuration profile"),
                    )
                }
                Removal::Key => {
                    generate_key_public_key(&config_home, "alice");
                    (
                        ["keys", "rm", "alice"],
                        config_home.join("thruput/keys/alice.json"),
                        "Remove key `alice`?",
                        None,
                    )
                }
            };
            let before = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("{case}: fixture missing: {error}"));

            let mut command = thruput();
            command
                .current_dir(tmp.path())
                .env("XDG_CONFIG_HOME", &config_home);
            match mode {
                #[cfg(unix)]
                NonInteractiveTerminal { json } => {
                    command.arg("--non-interactive");
                    if json {
                        command.arg("--json");
                    }
                }
                #[cfg(unix)]
                ForcedNonInteractiveTerminal => {
                    command.arg("--non-interactive");
                }
                Json => {
                    command.arg("--json");
                }
                Quiet => {
                    command.arg("--quiet");
                }
                PipedStdin => {}
                #[cfg(unix)]
                ForcedWithPipedStdin => {}
            }
            command.args(args);

            let forced = run_unprompted_removal(command, mode, &case, prompt, action);

            if forced {
                match target {
                    Removal::ConfigProfile => assert_eq!(
                        std::fs::read_to_string(&path)
                            .unwrap_or_else(|error| panic!("{case}: config unreadable: {error}")),
                        "",
                        "{case}: profile not removed"
                    ),
                    Removal::Key => assert!(!path.exists(), "{case}: key not removed"),
                }
            } else {
                assert_eq!(
                    std::fs::read(&path)
                        .unwrap_or_else(|error| panic!("{case}: target deleted: {error}")),
                    before,
                    "{case}: refusal must preserve the target"
                );
            }
        }
    }
}

/// Run a removal without answering a prompt, check how it reports refusing,
/// and return whether `mode` forced the removal.
#[cfg_attr(not(unix), allow(unused_variables))]
fn run_unprompted_removal(
    mut command: Command,
    mode: RemovalMode,
    case: &str,
    prompt: &str,
    action: Option<&str>,
) -> bool {
    #[cfg(unix)]
    use RemovalMode::{ForcedNonInteractiveTerminal, ForcedWithPipedStdin, NonInteractiveTerminal};
    use RemovalMode::{Json, PipedStdin, Quiet};

    match mode {
        #[cfg(unix)]
        NonInteractiveTerminal { json } => {
            let (status, output) = run_on_terminal(command, case);
            assert!(!status.success(), "{case}: removed without --force");
            if json {
                let report = case_json(case, "terminal output", output.as_bytes());
                assert_eq!(
                    report["error"]["code"], "confirmation_required",
                    "{case}: {output}"
                );
            } else {
                assert!(output.contains("--yes"), "{case}: {output}");
                assert!(!output.contains(prompt), "{case}: {output}");
                assert!(!output.contains("[y/N]"), "{case}: {output}");
            }
            false
        }
        #[cfg(unix)]
        ForcedNonInteractiveTerminal => {
            command.arg("--force");
            let (status, output) = run_on_terminal(command, case);
            assert!(status.success(), "{case}: {output}");
            true
        }
        PipedStdin => {
            let mut child = command
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap_or_else(|error| panic!("{case}: did not start: {error}"));
            offer_stdin(&mut child, b"y\n");
            let output = child
                .wait_with_output()
                .unwrap_or_else(|error| panic!("{case}: did not finish: {error}"));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{case}: removed without --force");
            assert!(stderr.contains("--yes"), "{case}: {stderr}");
            false
        }
        Json => {
            let output = command
                .output()
                .unwrap_or_else(|error| panic!("{case}: did not run: {error}"));
            assert!(!output.status.success(), "{case}: removed without --force");
            let error = case_json(case, "stderr", &output.stderr);
            assert_eq!(
                error["error"]["code"], "confirmation_required",
                "{case}: {error}"
            );
            assert_eq!(
                error["error"]["details"]["requiredFlag"], "--yes",
                "{case}: {error}"
            );
            if let Some(action) = action {
                assert_eq!(
                    error["error"]["details"]["action"], action,
                    "{case}: {error}"
                );
            }
            false
        }
        Quiet => {
            let output = command
                .output()
                .unwrap_or_else(|error| panic!("{case}: did not run: {error}"));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{case}: removed without --force");
            assert!(stderr.contains("--yes"), "{case}: {stderr}");
            false
        }
        #[cfg(unix)]
        ForcedWithPipedStdin => {
            let child = command
                .arg("-f")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let status = wait_child(child, CLI_ACTION_TIMEOUT);
            assert!(status.success(), "{case}: forced removal failed");
            true
        }
    }
}

#[test]
fn status_checks_wallet_owner_without_private_key_bytes() {
    let (_tmp, project, config) = empty_wallet_fixture();
    let owner = generate_key_public_key(&config, "other");
    let public_key = thruput_signing::strkey::parse_public_key(&owner).unwrap();
    let account = thruput_signing::PubkeyAccount::from_public_key_bytes(public_key, 7);
    let original = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "{original}\ndefault_wallet = 'trading'\n[profile.default.wallets.trading]\nkind = 'native'\naccount_nonce = 7\naddress = '{}'\nowner_public_key = '{owner}'\n",
            thruput_encoding::encode(account.address().as_ref())
        ),
    )
    .unwrap();
    make_local_keypair_unavailable(&config, "owner");
    let output = thruput()
        .args(["--json", "status"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| { finding["code"] == "default_wallet_owner_mismatch" })
    );
}

#[test]
fn non_tty_init_ignores_piped_yes_answers_and_shows_setup_sections() {
    let tmp = TempDir::new().unwrap();
    let mut child = thruput()
        .arg("init")
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"y\ny\ny\nshould-not-exist\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Thruput Setup"));
    assert!(stderr.contains("Set up your local development environment."));
    assert!(stderr.contains("[1/4] Environment"));
    assert!(stderr.contains("[2/4] Profiles"));
    assert!(stderr.contains("[3/4] Signing key"));
    assert!(!stderr.contains("download credentials"));
    assert!(stderr.contains("[4/4] Project"));
    assert!(!stderr.contains("Setup Complete"));
    assert!(stderr.contains("Setup Incomplete"));
    assert!(stderr.contains("Config:"));
    assert!(stderr.contains("Default profile:"));
    assert!(stderr.contains("Local RPC:"));
    assert!(stderr.contains("--generate-key"));
    assert!(!tmp.path().join("thruput/keys/default.json").exists());
    assert!(!tmp.path().join("should-not-exist").exists());
}

#[cfg(unix)]
#[test]
fn interactive_init_points_to_created_project() {
    let tmp = TempDir::new().unwrap();
    let mut command = thruput();
    command
        .arg("init")
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path());
    let (child, mut terminal) = spawn_with_terminal(command);
    let output_reader = terminal_output_reader(&terminal);
    terminal.write_all(b"y\ny\ndemo-contract\n").unwrap();
    let output = child.wait_with_output().unwrap();
    let terminal_output = output_reader
        .join()
        .expect("bug: terminal output reader should not panic");
    assert!(
        output.status.success(),
        "terminal output: {terminal_output}"
    );

    assert!(terminal_output.contains("Setup Complete"));
    assert!(terminal_output.contains("Key"));
    assert!(terminal_output.contains("default ("));
    assert!(terminal_output.contains("Project"));
    assert!(terminal_output.contains("demo-contract"));
    assert!(terminal_output.contains("Next:"));
    assert!(terminal_output.contains("cd demo-contract"));
    assert!(terminal_output.contains("thruput build"));
    assert!(!terminal_output.contains("thruput new my-contract"));
    assert!(tmp.path().join("demo-contract/thruput.toml").exists());
}

#[cfg(unix)]
#[test]
fn init_boxes_respect_stderr_terminal_width() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("long-config-directory-".repeat(5));
    let setup = thruput()
        .args(["init", "--quiet", "--generate-key"])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert!(setup.status.success(), "{setup:?}");
    for columns in [0, 24, 40, 60, 80, 120] {
        let mut command = thruput();
        command
            .arg("init")
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", &config)
            .env("TERM", "xterm")
            .env_remove("NO_COLOR");
        let (child, terminal) = spawn_with_terminal_width(command, columns);
        let reader = terminal_output_reader(&terminal);
        assert!(wait_child(child, CLI_ACTION_TIMEOUT).success());
        drop(terminal);
        let output = reader.join().unwrap();
        assert!(output.contains("Thruput Ready"));
        let plain = strip_terminal_styles(&output);
        assert!(
            !plain.contains("Thruput Setup"),
            "already-ready setup needs no wizard: {plain}"
        );
        assert_eq!(plain.contains("╭─ ✓ Ready: Thruput Ready"), columns >= 29);
        assert!(
            !output.contains("\x1b[2J"),
            "already-ready setup must not take over the screen"
        );
        if columns > 0 {
            use unicode_width::UnicodeWidthStr;
            for line in plain
                .lines()
                .filter(|line| line.starts_with(['╭', '│', '╰']))
            {
                assert!(
                    UnicodeWidthStr::width(line) <= usize::from(columns),
                    "{line}"
                );
            }
        }
        let values = plain
            .lines()
            .map(|line| line.trim_start_matches('│').trim_end_matches('│').trim())
            .collect::<String>();
        assert!(
            values.contains(config.to_str().unwrap()),
            "complete path missing: {plain}"
        );
    }
}

#[test]
fn init_human_output_builds_existing_project() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("existing-contract");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("thruput.toml"),
        "[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();
    let mut child = thruput()
        .args(["init", "--generate-key"])
        .current_dir(&project)
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
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
    assert!(stderr.contains("thruput build"));
    assert!(!stderr.contains("thruput new my-contract"));
    assert!(!stderr.contains("cd existing-contract"));
}

#[test]
fn status_json_outputs_structured_payload() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
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
    assert_eq!(
        stdout["data"]["schemaVersion"],
        thruput_cli::status::STATUS_JSON_SCHEMA_VERSION
    );
    assert!(stdout["data"]["environment"]["cargo"].as_bool().unwrap());
    assert!(stdout["data"]["environment"]["rustToolchain"]["available"].is_boolean());
    assert!(stdout["data"]["environment"]["rustToolchain"]["rustSrc"].is_string());
    assert!(stdout["data"]["environment"]["rv32Target"].is_string());
    assert!(stdout["data"]["environment"]["devnode"]["reachable"].is_boolean());
    assert!(stdout["data"]["environment"]["genesisRootArtifact"]["valid"].is_boolean());
    assert!(stdout["data"]["environment"].get("wasm_target").is_none());
    assert!(stdout["data"]["environment"].get("wasm_opt").is_none());
    assert!(stdout["data"].get("credentials").is_none());
    assert!(!stdout["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn status_json_flattens_project_findings_into_warnings() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let output = thruput()
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
fn status_json_reports_project_default_wallet_and_dangling_finding() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\ndefault_wallet = \"missing\"\n",
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "status"])
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let project = &stdout["data"]["project"];
    assert_eq!(project["projectDefaultWallet"], "missing");
    assert_eq!(project["effectiveDefaultWallet"], "missing");
    assert_eq!(project["defaultWalletSource"], "project");
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
    let output = thruput()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("THRUPUT_DEVNODE_BINARY", &devnode)
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = &stdout["data"]["environment"]["devnode"];
    assert_eq!(status["reachable"], true);
    assert_eq!(status["path"], devnode.display().to_string());
    assert_eq!(status["version"], "0.1.12");
    let artifact = &stdout["data"]["environment"]["genesisRootArtifact"];
    assert_eq!(artifact["available"], true);
    assert_eq!(artifact["valid"], true);
    assert!(
        artifact["path"]
            .as_str()
            .unwrap()
            .ends_with("contracts/genesis_root.rv32a")
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

#[cfg(unix)]
#[test]
fn status_json_discovers_managed_devnode_without_mutation() {
    let source = TempDir::new().unwrap();
    let cache_home = TempDir::new().unwrap();
    let devnode = support::write_versioned_devnode(source.path());
    let managed = install_fake_managed_toolchain(cache_home.path(), &devnode);
    let marker = managed.parent().unwrap().join(".ok");
    let before = std::fs::read(&marker).unwrap();
    let modified = std::fs::metadata(&marker).unwrap().modified().unwrap();
    let output = thruput()
        .args(["--json", "status"])
        .env("XDG_CACHE_HOME", cache_home.path())
        .env("PATH", source.path().join("empty"))
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let devnode = &payload["data"]["environment"]["devnode"];
    assert_eq!(devnode["reachable"], true, "{payload}");
    assert_eq!(devnode["path"], managed.display().to_string());
    assert_eq!(std::fs::read(&marker).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&marker).unwrap().modified().unwrap(),
        modified
    );
    assert!(
        !cache_home
            .path()
            .join("thruput/toolchain/hog-devnode/.locks")
            .exists()
    );
}

#[test]
fn quiet_status_exits_one_with_no_output_when_missing_state() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
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
fn status_human_output_shows_sections() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    // Will exit 1 (warnings), but stderr should have
    // human-readable section content.
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Thruput Status"));
    assert!(stderr.contains("Cargo: installed"));
    assert!(stderr.contains("Rust Toolchain:"));
    assert!(stderr.contains("RV32 Target:"));
    assert!(stderr.contains("hog-devnode:"));
    assert!(stderr.contains("Genesis Root:"));
    assert!(!stderr.contains("Wasm Target:"));
    assert!(!stderr.contains("wasm-opt:"));
    assert!(!stderr.contains("Global Registry:"));
    assert!(stderr.contains("Keys: none"));
    assert!(!stderr.contains("Mock Executor Auth"));
}

#[cfg(unix)]
#[test]
fn status_terminal_report_wraps_complete_values_with_and_without_color() {
    use unicode_width::UnicodeWidthStr;

    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("長い-project-directory-".repeat(5));
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("thruput.toml"),
        "default_profile = \"local\"\n[profile.local]\nnetwork = \"local\"\nrpc_url = \"http://127.0.0.1:9545\"\n",
    ).unwrap();
    let mut expected_exit = None;
    for columns in [24, 40, 60, 80, 120] {
        for no_color in [false, true] {
            let mut command = thruput();
            command
                .arg("status")
                .current_dir(&project)
                .env("CARGO_HOME", tmp.path().join("cargo-home"))
                .env("TERM", "xterm")
                .env("LC_ALL", "C.UTF-8")
                .env_remove("NO_COLOR");
            if no_color {
                command.env("NO_COLOR", "1");
            }
            let (child, terminal) = spawn_with_terminal_width(command, columns);
            let reader = terminal_output_reader(&terminal);
            let exit = wait_child(child, CLI_ACTION_TIMEOUT);
            assert_eq!(*expected_exit.get_or_insert(exit.code()), exit.code());
            drop(terminal);
            let output = reader.join().unwrap();
            assert_eq!(output.contains('\x1b'), !no_color);
            let plain = strip_terminal_styles(&output);
            assert!(!plain.contains('\x1b'), "only style escapes are allowed");
            assert!(plain.starts_with("╭─ Thruput Status"));
            let card = plain
                .lines()
                .take_while(|line| !line.starts_with('╰'))
                .collect::<Vec<_>>();
            let card_width = UnicodeWidthStr::width(card[0]);
            assert!(
                card.iter()
                    .all(|line| UnicodeWidthStr::width(*line) == card_width),
                "misaligned borders: {plain}"
            );
            assert!(
                plain
                    .lines()
                    .all(|line| UnicodeWidthStr::width(line) <= usize::from(columns).min(100)),
                "{columns} columns: {plain}"
            );
            for label in ["Thruput Status", "Cargo:", "Keys:", "Project:"] {
                assert!(plain.contains(label), "{plain}");
            }
            let joined = plain
                .lines()
                .map(|line| line.trim_start_matches('│').trim_end_matches('│').trim())
                .collect::<String>();
            assert!(
                joined.contains(project.join("thruput.toml").to_str().unwrap()),
                "full path missing: {plain}"
            );
            assert!(joined.contains("http://127.0.0.1:9545"));
        }
    }
}

#[test]
fn status_dumb_terminal_keeps_plain_report_and_exit_code() {
    let tmp = TempDir::new().unwrap();
    let run = |json: bool| {
        let mut command = thruput();
        command
            .arg("status")
            .current_dir(tmp.path())
            .env("CARGO_HOME", tmp.path().join("cargo-home"))
            .env("TERM", "dumb");
        if json {
            command.arg("--json");
        }
        command.output().unwrap()
    };
    let plain = run(false);
    let json = run(true);
    assert_eq!(plain.status.code(), json.status.code());
    assert!(plain.stdout.is_empty());
    assert!(json.stderr.is_empty());
    assert!(!plain.stderr.contains(&0x1b));
    assert!(String::from_utf8_lossy(&plain.stderr).contains("Cargo: installed"));
    let payload: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(payload["data"]["schemaVersion"], 5);
}

#[cfg(unix)]
#[test]
fn status_no_ui_and_ascii_modes_keep_terminal_output_complete() {
    let tmp = TempDir::new().unwrap();
    for (no_ui, locale) in [(true, "C.UTF-8"), (false, "C")] {
        let mut command = thruput();
        command
            .arg("status")
            .current_dir(tmp.path())
            .env("CARGO_HOME", tmp.path().join("cargo-home"))
            .env("TERM", "xterm")
            .env("LC_ALL", locale)
            .env("NO_COLOR", "1");
        if no_ui {
            command.arg("--no-ui");
        }
        let (child, terminal) = spawn_with_terminal_size(command, 40, 4);
        let reader = terminal_output_reader(&terminal);
        let status = wait_child(child, CLI_ACTION_TIMEOUT);
        drop(terminal);
        let output = reader.join().unwrap();
        assert!(!status.success());
        assert!(!output.contains('\x1b'));
        assert!(!output.contains('╭'));
        assert_eq!(output.starts_with("+- Thruput Status"), !no_ui);
        assert!(
            output.lines().count() > 4,
            "must append the entire report, not clip to terminal height"
        );
        assert!(output.contains("Contracts:"));
        assert!(output.contains(if no_ui { "warning:" } else { "Warning" }));
    }
}

#[test]
fn no_ui_preserves_status_json_quiet_and_raw_data() {
    let tmp = TempDir::new().unwrap();
    let run = |no_ui, json| {
        let mut command = thruput();
        command
            .arg("status")
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path())
            .env("CARGO_HOME", tmp.path().join("cargo-home"));
        if no_ui {
            command.arg("--no-ui");
        }
        if json {
            command.arg("--json");
        } else {
            command.arg("--quiet");
        }
        command.output().unwrap()
    };
    let normal = run(false, true);
    let plain = run(true, true);
    assert_eq!(normal.status.code(), plain.status.code());
    assert_eq!(normal.stdout, plain.stdout);
    assert!(plain.stderr.is_empty());
    let quiet = run(true, false);
    assert!(quiet.stdout.is_empty() && quiet.stderr.is_empty());
    assert_eq!(quiet.status.code(), normal.status.code());
    let raw = thruput().args(["completion", "bash"]).output().unwrap();
    let plain = thruput()
        .args(["--no-ui", "completion", "bash"])
        .output()
        .unwrap();
    assert_eq!(raw.stdout, plain.stdout);
    assert!(plain.stderr.is_empty());
}

#[test]
fn no_ui_does_not_authorize_protected_commands() {
    let tmp = TempDir::new().unwrap();
    let generated = thruput()
        .args(["keys", "generate", "demo", "--quiet"])
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();
    assert!(generated.status.success());
    let output = thruput()
        .args(["--no-ui", "--json", "keys", "rm", "demo"])
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "confirmation_required");
}

#[test]
fn no_ui_disables_parser_styling_even_with_forced_color() {
    for args in [
        vec!["--no-ui", "--help"],
        vec!["--no-ui", "status", "--unknown-option"],
    ] {
        let output = thruput()
            .args(args)
            .env("CLICOLOR_FORCE", "1")
            .output()
            .unwrap();
        assert!(!output.stdout.contains(&0x1b));
        assert!(!output.stderr.contains(&0x1b));
    }
}

#[test]
fn status_json_ignores_legacy_ffi_environment() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("HOGD_FFI_PASSWORD", "secret")
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(stdout["data"].get("credentials").is_none());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
    assert!(
        stdout["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "no_credentials")
    );
}

#[test]
fn status_json_ignores_legacy_ffi_config() {
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join("thruput");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "[executor]\nusername = \"marmot\"\npassword = \"secret\"\n",
    )
    .unwrap();

    let output = thruput()
        .arg("--json")
        .arg("status")
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .output()
        .unwrap();

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(stdout["data"].get("credentials").is_none());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
}

#[test]
fn new_fails_on_existing_dir() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("exists")).unwrap();

    let output = thruput()
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
    let output = thruput()
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
    let output = thruput()
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
fn build_rejects_removed_target_and_optimize_flags() {
    let tmp = TempDir::new().unwrap();
    for args in [
        &["build", "--target", "wasm"][..],
        &["build", "--optimize"][..],
    ] {
        let output = thruput()
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unexpected argument"),
            "expected clap to reject {args:?}, got: {stderr}"
        );
    }
}

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
    let rv32 = target_dir.join("thruput").join("demo_contract_rv32.rv32a");
    let metadata = target_dir
        .join("thruput")
        .join("demo_contract_rv32.rv32.json");
    let bindings = target_dir.join("thruput").join("demo_contract_rv32.ts");
    let elf_fixture = tmp.path().join("fixture.elf");
    std::fs::write(
        &elf_fixture,
        support::minimal_rv32_elf_with_section(
            thruput_spec_tool::RAW_SECTION_NAME,
            &support::minimal_contract_spec_raw(),
        ),
    )
    .unwrap();
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": target_dir,
        "packages": [{
            "name": "demo-contract-rv32",
            "version": "0.1.0",
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

    let output = thruput()
        .args([
            "--json",
            "build",
            "--bindings",
            "ts",
            "-p",
            "demo-contract-rv32",
        ])
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
        format!("{:#018x}", thruput_rv32_lower::FORMAT_FINGERPRINT)
    );

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "build");
    assert_eq!(
        stdout["data"]["schemaVersion"],
        thruput_cli::build::BUILD_JSON_SCHEMA_VERSION
    );
    assert_eq!(stdout["data"]["target"], "rv32");
    assert_eq!(stdout["data"]["elf"], raw_elf.display().to_string());
    assert_eq!(stdout["data"]["rv32"], rv32.display().to_string());
    assert_eq!(stdout["data"]["metadata"], metadata.display().to_string());
    assert_eq!(stdout["data"]["bindingsTs"], bindings.display().to_string());
    assert!(
        std::fs::read_to_string(bindings)
            .unwrap()
            .contains("Counter")
    );
    // v4 stores only the high-water prefix, so the artifact is well under 64 KiB.
    assert!(
        stdout["data"]["sizeBytes"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            < 64 * 1024
    );
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
            "version": "0.1.0",
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

    let output = thruput()
        .args(["--json", "build", "-p", "demo-contract-rv32"])
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
fn node_rejects_ramdb_size_without_a_recognized_unit() {
    let output = thruput()
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
fn codegen_requires_exactly_one_source() {
    let address = thruput_encoding::encode(&[0x11; 32]);
    for args in [
        vec!["codegen", "ts"],
        vec!["codegen", "ts", "counter.spec.xdr", "--address", &address],
        vec![
            "codegen",
            "ts",
            "counter.spec.xdr",
            "--rpc-url",
            "http://localhost:1",
        ],
    ] {
        let output = thruput().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

fn codegen_artifact_response(data: Vec<u8>) -> (serde_json::Value, serde_json::Value) {
    use sha2::{Digest, Sha256};
    use thruput_xdr::contract::{Contract, ContractBlob, ContractType};

    let size = data.len();
    let bytes = xdrs::txs::encode(&Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: data.try_into().unwrap(),
        },
    })
    .unwrap();
    let hash = thruput_encoding::encode(&Sha256::digest(&bytes));
    (
        serde_json::json!({ "hash": hash, "size": size, "contractType": ContractType::kRv32.0 }),
        serde_json::json!({ "hash": hash, "contractXdr": format!("0x{}", hex::encode(bytes)) }),
    )
}

fn codegen_rpc(
    address: &str,
    deployment: serde_json::Value,
    artifact: serde_json::Value,
) -> (String, MockServer<()>) {
    let address = address.to_owned();
    let expected_address = address.clone();
    let hash = deployment["hash"].clone();
    let deployed = !deployment.is_null();
    let server = rpc_mock()
        .method("getContractDeployment", move |_| {
            Ok(serde_json::json!({
                "address": address, "stateBlockHeight": "12", "deployment": deployment,
            }))
        })
        .method("getContractArtifact", move |_| Ok(artifact.clone()))
        .start();
    recorded_mock(server, move |script| {
        assert_eq!(
            script.expect("getContractDeployment")["address"],
            expected_address
        );
        if deployed {
            assert_eq!(script.expect("getContractArtifact")["hash"], hash);
        }
    })
}

#[test]
fn codegen_onchain_matches_local_bindings_for_stdout_json_and_file() {
    let tmp = TempDir::new().unwrap();
    let address = thruput_encoding::encode(&[0x11; 32]);
    let source = tmp.path().join("counter.rv32a");
    let spec = xdrs::txs::decode(include_bytes!(
        "../fixtures/codegen/typed_contract.spec.xdr"
    ))
    .unwrap();
    let data = artifact_with_spec(&spec);
    std::fs::write(&source, &data).unwrap();
    let local = thruput()
        .args(["codegen", "ts"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(local.status.success());
    let expected = String::from_utf8(local.stdout)
        .unwrap()
        .replace("counter.rv32a", &address);
    let output_path = tmp.path().join("bindings.ts");

    for mode in ["stdout", "json", "file"] {
        let (deployment, artifact) = codegen_artifact_response(data.clone());
        let (rpc_url, server) = codegen_rpc(&address, deployment, artifact);
        let mut command = thruput();
        command.args(["codegen", "ts"]).current_dir(tmp.path());
        if mode == "file" {
            std::fs::write(tmp.path().join("thruput.toml"), format!(
                "[profile.bindings]\nrpc_url = '{rpc_url}'\n[profile.bindings.contracts]\ncounter = '{address}'\n"
            )).unwrap();
            command.args(["--address", "counter", "--profile", "bindings"]);
        } else {
            command.args(["--address", &address, "--rpc-url", &rpc_url]);
        }
        if mode != "stdout" {
            command.arg("--json");
        }
        if mode == "file" {
            command.arg("--output").arg(&output_path);
        }
        let output = command.output().unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if mode == "stdout" {
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        } else {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["command"], "codegen.ts");
            assert_eq!(result["data"]["source"], address);
            if mode == "json" {
                assert_eq!(result["data"]["typescript"], expected);
            } else {
                assert_eq!(std::fs::read_to_string(&output_path).unwrap(), expected);
                assert_eq!(result["data"]["output"], output_path.to_str().unwrap());
            }
        }
    }
}

#[test]
fn codegen_onchain_failures_preserve_existing_output() {
    let tmp = TempDir::new().unwrap();
    let address = thruput_encoding::encode(&[0x11; 32]);
    let output_path = tmp.path().join("bindings.ts");
    std::fs::write(&output_path, "existing bindings").unwrap();
    let (deployment, mut corrupted) = codegen_artifact_response(spec_artifact_fixture());
    corrupted["contractXdr"] = serde_json::json!("0x00");
    let (absent_deployment, absent_artifact) = codegen_artifact_response(
        thruput_rv32_artifact::build_artifact(&[0; 4], &[], 0, 16).unwrap(),
    );
    let (unknown_deployment, unknown_artifact) = codegen_artifact_response(
        thruput_rv32_artifact::build_artifact_with_spec(
            &[0; 4],
            &[],
            0,
            16,
            Some(thruput_rv32_artifact::ArtifactSpec {
                schema_version: 99,
                bytes: &[1],
            }),
        )
        .unwrap(),
    );
    for (deployment, artifact, error) in [
        (
            serde_json::Value::Null,
            serde_json::Value::Null,
            "notDeployed",
        ),
        (absent_deployment, absent_artifact, "absent"),
        (unknown_deployment, unknown_artifact, "unsupportedSchema"),
        (deployment, corrupted, "hash mismatch"),
    ] {
        let (rpc_url, server) = codegen_rpc(&address, deployment, artifact);
        let output = thruput()
            .args([
                "--json",
                "codegen",
                "ts",
                "--address",
                &address,
                "--rpc-url",
                &rpc_url,
                "--output",
            ])
            .arg(&output_path)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(
            report["error"]["message"].as_str().unwrap().contains(error),
            "{report}"
        );
        assert_eq!(
            std::fs::read_to_string(&output_path).unwrap(),
            "existing bindings"
        );
    }
}

#[test]
fn codegen_rv32_artifact_reads_embedded_spec() {
    let tmp = TempDir::new().unwrap();
    let artifact = tmp.path().join("counter.rv32a");
    std::fs::write(&artifact, spec_artifact_fixture()).unwrap();

    let output = thruput()
        .args(["codegen", "ts", artifact.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(".spec.xdr"),
        "embedded spec must not require a sidecar: {stderr}"
    );
}

#[test]
fn build_json_prefers_metadata_failure_outside_project() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
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
    let output = thruput()
        .arg("test")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
}

#[test]
fn test_json_failure_is_structured() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
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
    let output = thruput()
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
    let output = thruput()
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
fn test_integration_default_rv32_build_reaches_local_node_startup() {
    let hogd_dev = fake_hogd_dev_binary();
    let tmp = TempDir::new().unwrap();
    let (fake_bin, metadata_json) = prepare_integration_startup_fixture(&tmp);

    let output = thruput()
        .args(["--json", "test", "--integration", "-p", "demo-contract"])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .env("THRUPUT_TEST_METADATA_JSON", metadata_json)
        .env("THRUPUT_DEVNODE_BINARY", &hogd_dev)
        .env("THRUPUT_FAKE_HOGD_DEV_MODE", "exit-7")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        tmp.path()
            .join("target/thruput/demo_contract.rv32a")
            .is_file(),
        "integration should lower an RV32 artifact before node startup by default",
    );
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "test");
    assert_eq!(stderr["error"]["code"], "node_supervisor_error");
}

#[cfg(unix)]
fn prepare_integration_startup_fixture(tmp: &TempDir) -> (std::path::PathBuf, String) {
    use std::os::unix::fs::PermissionsExt;
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
    std::fs::write(&elf_fixture, spec_elf_fixture()).unwrap();
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": target_dir,
        "packages": [{
            "name": "demo-contract",
            "version": "0.1.0",
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
                "  printf '%s' \"$THRUPUT_TEST_METADATA_JSON\"\n",
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

    (fake_bin, metadata_json)
}

#[cfg(unix)]
#[test]
fn integration_managed_startup_cancellation_reports_test_schema() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let tmp = TempDir::new().unwrap();
        let (fake_bin, metadata_json) = prepare_integration_startup_fixture(&tmp);
        let cache = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let catalog = thruput_toolchain::Catalog::embedded().unwrap();
        let child = thruput()
            .args([
                "--json",
                "test",
                "--integration",
                "-p",
                "demo-contract",
                "--hog-devnode-version",
            ])
            .arg(catalog.default_version())
            .current_dir(tmp.path())
            .env("PATH", &fake_bin)
            .env("THRUPUT_TEST_METADATA_JSON", metadata_json)
            .env("THRUPUT_TOOLCHAIN_BASE_URL", origin)
            .env("XDG_CACHE_HOME", cache.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut child = ChildGuard::new(child);
        let deadline = Instant::now() + CLI_ACTION_TIMEOUT;
        let _connection = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(status) = child.child_mut().try_wait().unwrap() {
                        let output = child.into_inner().wait_with_output().unwrap();
                        panic!(
                            "CLI exited {status} before acquisition: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                    assert!(
                        Instant::now() < deadline,
                        "CLI did not reach artifact source"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        // SAFETY: send the requested signal only to the child created by this test.
        assert_eq!(
            unsafe { libc::kill(child.child().id().try_into().unwrap(), signal) },
            0
        );
        let deadline = Instant::now() + CLI_ACTION_TIMEOUT;
        while child.child_mut().try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "integration acquisition did not cancel"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.into_inner().wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["command"], "test");
        assert_eq!(
            payload["error"]["code"],
            "integration_node_startup_interrupted"
        );
        let report: thruput_cli::test_json::TestReport =
            serde_json::from_value(payload["data"].clone()).unwrap();
        assert_eq!(
            report.schema_version,
            thruput_cli::test_json::TEST_JSON_SCHEMA_VERSION
        );
        assert_eq!(report.mode, thruput_cli::test_json::TestMode::Integration);
        assert!(!report.success);
        assert!(report.runs.is_empty());
        assert!(report.integration.is_none());
        assert!(report.artifact.path.is_file());
        let installer = thruput_toolchain::Installer::new(cache.path().join("thruput"));
        assert!(
            installer
                .find(
                    &catalog,
                    catalog.default_version(),
                    thruput_toolchain::HostTarget::current().unwrap()
                )
                .unwrap()
                .is_none()
        );
    }
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
    std::fs::write(&elf_fixture, spec_elf_fixture()).unwrap();
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": tmp.path().join("target"),
        "packages": [{
            "name": "demo-contract",
            "version": "0.1.0",
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

    let output = thruput()
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
    assert_eq!(stdout["data"]["schemaVersion"], 8);
    assert_eq!(stdout["data"]["mode"], "rv32");
    assert_eq!(stdout["data"]["package"], "demo-contract");
    assert_rv32_test_artifact(&stdout["data"]);
    assert!(stdout["data"]["projectRoot"].as_str().is_some());
    assert_eq!(stdout["data"]["success"], false);
    assert_eq!(stdout["data"]["summary"]["source"], "parsed");
    assert_eq!(stdout["data"]["summary"]["passed"], "1");
    assert_eq!(stdout["data"]["summary"]["failed"], "1");
    assert_eq!(stdout["data"]["summary"]["filteredOut"], "2");
    assert_eq!(stdout["data"]["runs"][0]["success"], false);
    assert_eq!(stdout["data"]["runs"][0]["summary"]["source"], "parsed");
    assert_eq!(stdout["data"]["runs"][0]["summary"]["filteredOut"], "2");
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
fn test_rv32_harness_receives_target_dir_and_preserves_legacy_config() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("legacy-config");
    let legacy = "default_profile = \"default\"\n\n[profile.default]\n\n[executor]\nusername = \"private-user\"\npassword = \"private-password\"\n";
    write_global_config(&config_home, legacy);
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
    let raw_dir = target_dir.join("riscv32im-unknown-none-elf/release");
    let raw_elf = raw_dir.join("demo_contract");
    let elf_fixture = tmp.path().join("fixture.elf");
    std::fs::write(&elf_fixture, spec_elf_fixture()).unwrap();
    let metadata_json = serde_json::json!({
        "workspace_root": tmp.path(),
        "target_directory": target_dir,
        "packages": [{
            "name": "demo-contract",
            "version": "0.1.0",
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
            raw_dir.display(),
            elf_fixture.display(),
            raw_elf.display(),
            target_dir.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = thruput()
        .args(["--json", "test", "-p", "demo-contract"])
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(
        std::fs::read_to_string(config_home.join("thruput/config.toml")).unwrap(),
        legacy
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-password"));
    assert_eq!(stdout["data"]["mode"], "rv32");
    assert_eq!(
        stdout["data"]["artifact"]["path"].as_str().unwrap(),
        target_dir
            .join("thruput/demo_contract.rv32a")
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
            .ends_with("target/thruput/demo_contract.rv32a")
    );
}

#[test]
fn test_rejects_removed_target_and_differential_flags() {
    let tmp = TempDir::new().unwrap();
    for args in [
        vec!["test", "--target", "wasm"],
        vec!["test", "--integration", "--differential"],
    ] {
        let output = thruput()
            .args(&args)
            .current_dir(tmp.path())
            .output()
            .unwrap();

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unexpected argument"),
            "expected clap to reject {args:?}, got: {stderr}"
        );
    }
}

#[test]
fn test_json_prefers_metadata_failure_outside_project() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
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

    let output = thruput()
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

/// How a command reports that no profile is configured.
enum NoProfileReport {
    /// A JSON error envelope for this command.
    Json(&'static str),
    /// Human stderr containing any of these.
    Human(&'static [&'static str]),
}

#[test]
fn commands_fail_without_an_active_profile() {
    let address = thruput_encoding::encode(&[0x11; 32]);
    let key = format!("0x{}", "42".repeat(32));
    let cases: [(&str, Vec<&str>, NoProfileReport); 6] = [
        (
            "deploy",
            vec!["deploy"],
            NoProfileReport::Human(&["no active profile"]),
        ),
        (
            "deploy --json",
            vec!["--json", "deploy"],
            NoProfileReport::Json("deploy"),
        ),
        (
            "call",
            vec!["call", "0x1234", "transfer"],
            NoProfileReport::Human(&["rpc_url", "profile"]),
        ),
        (
            "call --json",
            vec!["--json", "call", "0x1234", "transfer"],
            NoProfileReport::Json("call"),
        ),
        (
            "storage --json",
            vec!["--json", "storage", &address, &key],
            NoProfileReport::Json("storage"),
        ),
        (
            "view --json",
            vec!["--json", "view", &address, "0x0102030405060708"],
            NoProfileReport::Json("view"),
        ),
    ];
    for (case, args, report) in cases {
        let tmp = TempDir::new().unwrap();
        let output = thruput()
            .args(args)
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path().join("config-home"))
            .output()
            .unwrap_or_else(|error| panic!("{case}: did not run: {error}"));
        assert!(
            !output.status.success(),
            "{case}: succeeded without a profile"
        );
        match report {
            NoProfileReport::Json(command) => {
                let stderr = case_json(case, "stderr", &output.stderr);
                assert_eq!(stderr["ok"], false, "{case}: {stderr}");
                assert_eq!(stderr["command"], command, "{case}: {stderr}");
                assert_eq!(
                    stderr["error"]["code"], "no_active_profile",
                    "{case}: {stderr}"
                );
            }
            NoProfileReport::Human(any_of) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    any_of.iter().any(|text| stderr.contains(text)),
                    "{case}: expected one of {any_of:?} in: {stderr}"
                );
            }
        }
    }
}

fn write_deploy_workspace(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let member = root.join("counter");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"counter\"]\n",
    )
    .unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"counter\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(member.join("src/lib.rs"), "pub fn counter() {}\n").unwrap();

    let artifact = root.join("target/thruput/counter.rv32a");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, spec_artifact_fixture()).unwrap();
    (member, artifact)
}

#[test]
fn deploy_workspace_root_without_package_recommends_supported_option() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let workspace = tmp.path().join("shared-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_deploy_workspace(&workspace);
    write_project_config_with_dummy_alice_wallet(&workspace, "http://127.0.0.1:1");

    let output = thruput()
        .args(["--json", "deploy", "--dry-run"])
        .current_dir(&workspace)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "package_required");
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("-p <package>"))
    );
}

#[test]
fn deploy_workspace_root_rejects_unknown_package() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let workspace = tmp.path().join("shared-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_deploy_workspace(&workspace);
    write_project_config_with_dummy_alice_wallet(&workspace, "http://127.0.0.1:1");

    let output = thruput()
        .args(["--json", "deploy", "--dry-run", "-p", "missing"])
        .current_dir(&workspace)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "package_not_found");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("missing"))
    );
}

#[test]
fn deploy_workspace_root_accepts_long_package_option() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let workspace = tmp.path().join("shared-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (_, artifact) = write_deploy_workspace(&workspace);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    write_project_config_with_alice_wallet(&workspace, &rpc_url, &config_home);

    let output = thruput()
        .args(["--json", "deploy", "--dry-run", "--package", "counter"])
        .current_dir(&workspace)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    rpc_server.join().unwrap();
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::canonicalize(stdout["data"]["artifactPath"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(artifact).unwrap()
    );
}

#[test]
fn deploy_workspace_member_infers_package_without_option() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let workspace = tmp.path().join("shared-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (member, artifact) = write_deploy_workspace(&workspace);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    write_project_config_with_alice_wallet(&workspace, &rpc_url, &config_home);

    let output = thruput()
        .args(["--json", "deploy", "--dry-run"])
        .current_dir(&member)
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
        std::fs::canonicalize(stdout["data"]["artifactPath"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(artifact).unwrap()
    );
}

#[test]
fn deploy_workspace_package_persists_selected_package_alias() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let workspace = tmp.path().join("shared-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_deploy_workspace(&workspace);
    let (rpc_url, rpc_server) = spawn_deploy_rpc(42);
    write_project_config_with_alice_wallet(&workspace, &rpc_url, &config_home);

    let output = thruput()
        .args(["--json", "deploy", "--force", "-p", "counter"])
        .current_dir(&workspace)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = std::fs::read_to_string(workspace.join("thruput.toml")).unwrap();
    let expected_address = thruput_encoding::encode(&[0xbb; 32]);
    assert!(config.contains(&format!("counter = \"{expected_address}\"")));
    assert!(!config.contains("shared-workspace ="));
}

#[test]
fn deploy_artifact_records_no_alias_without_one_and_never_the_directory() {
    // `--artifact` carries no package name. Deploy used to fall back to the
    // project directory name, so this deploy would have written
    // `shared-workspace = "0x..."` -- a key that matches no package and that
    // `thruput call` would only find by accident.
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let workspace = tmp.path().join("shared-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_deploy_workspace(&workspace);
    let (rpc_url, rpc_server) = spawn_deploy_rpc(42);
    write_project_config_with_alice_wallet(&workspace, &rpc_url, &config_home);

    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
        ])
        .current_dir(&workspace)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let config = std::fs::read_to_string(workspace.join("thruput.toml")).unwrap();
    let deployed_address = thruput_encoding::encode(&[0xbb; 32]);
    assert!(
        !config.contains("shared-workspace ="),
        "the project directory name must never become an alias; config was:\n{config}"
    );
    assert!(
        !config.contains(&deployed_address),
        "no alias should be recorded without --alias; config was:\n{config}"
    );
}

#[test]
fn deploy_artifact_with_alias_records_that_name() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let workspace = tmp.path().join("shared-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_deploy_workspace(&workspace);
    let (rpc_url, rpc_server) = spawn_deploy_rpc(42);
    write_project_config_with_alice_wallet(&workspace, &rpc_url, &config_home);

    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
            "--alias",
            "token",
        ])
        .current_dir(&workspace)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let config = std::fs::read_to_string(workspace.join("thruput.toml")).unwrap();
    let deployed_address = thruput_encoding::encode(&[0xbb; 32]);
    assert!(
        config.contains(&format!("token = \"{deployed_address}\"")),
        "--alias should be recorded verbatim; config was:\n{config}"
    );
    assert!(
        !config.contains("shared-workspace ="),
        "the project directory name must never become an alias; config was:\n{config}"
    );
}

#[test]
fn deploy_alias_outside_a_project_lands_in_the_global_profile() {
    // profile_write_path resolves the nearest thruput.toml, or the global
    // config when there is none, so an alias deployed outside a project is
    // recorded globally rather than dropped. Project aliases override global
    // ones on read, which is the documented layering.
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let loose = tmp.path().join("no-project-here");
    std::fs::create_dir_all(&loose).unwrap();

    let (rpc_url, rpc_server) = spawn_deploy_rpc(42);
    // The profile this deploy runs against lives only in the global config.
    // write_global_config replaces the file `keys generate` just wrote, so
    // default_key has to be set here too or the deploy has no signing key.
    let owner_public_key = generate_key_public_key(&config_home, "alice");
    write_global_config(
        &config_home,
        &format!(
            "default_profile = \"default\"\ndefault_key = \"alice\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nnetwork_hash = \"{TEST_NETWORK_HASH}\"\n\n[profile.default.wallets.alice]\naddress = \"{}\"\nowner_public_key = \"{owner_public_key}\"\n",
            thruput_encoding::encode(&[0xaa; 32])
        ),
    );

    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
            "--alias",
            "token",
        ])
        .current_dir(&loose)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    rpc_server.join().unwrap();

    let global = std::fs::read_to_string(config_home.join("thruput").join("config.toml"))
        .expect("global config should exist");
    let deployed_address = thruput_encoding::encode(&[0xbb; 32]);
    assert!(
        global.contains(&format!("token = \"{deployed_address}\"")),
        "the alias should land in the global profile; config was:\n{global}"
    );
    assert!(
        !loose.join("thruput.toml").exists(),
        "no project config should be invented in the working directory"
    );
}

#[test]
fn deploy_reports_an_alias_save_failure_in_json_warnings() {
    // print.warn is a no-op under --json, so a failed alias write would
    // otherwise leave a script with ok: true and warnings: [] and no way to
    // know the alias is missing. `contracts` as an inline table makes the
    // write fail without touching the deploy itself.
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_deploy_rpc(42);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    // Must sit inside [profile.default], not at the end of the file, or it
    // lands under the wallets table and the save path never sees it.
    let config = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
    let config = config.replace("[profile.default]\n", "[profile.default]\ncontracts = {}\n");
    std::fs::write(project.join("thruput.toml"), config).unwrap();

    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
            "--alias",
            "token",
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();
    assert!(
        output.status.success(),
        "the deploy landed, so the command stays successful; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    let warnings = stdout["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "deploy.alias_not_saved"),
        "a failed alias write must be visible to --json consumers; warnings: {warnings:?}"
    );
}

#[test]
fn deploy_dry_run_json_outputs_prepared_transaction() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let owner = write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    let trading = thruput_encoding::encode(&[0xbb; 32]);
    let mut config = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
    write!(
        config,
        "\n[profile.default.wallets.trading]\naddress = \"{trading}\"\nowner_public_key = \"{owner}\"\n"
    )
    .unwrap();
    std::fs::write(project.join("thruput.toml"), config).unwrap();

    // The deploy path treats the selected RV32 artifact as opaque bytes.
    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--dry-run",
            "--wallet",
            "trading",
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
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(thruput_cli::deploy::DEPLOY_JSON_SCHEMA_VERSION, 9);
    assert_eq!(stdout["data"]["schemaVersion"], 9);
    for old_key in ["dry_run", "rpc_url", "artifact_kind", "signed_tx"] {
        assert!(stdout["data"].get(old_key).is_none(), "found {old_key}");
    }
    assert_eq!(stdout["data"]["dryRun"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["artifactKind"], "rv32");
    assert_eq!(
        stdout["data"]["artifactPath"].as_str().unwrap(),
        rv32_path.to_string_lossy()
    );
    assert_eq!(
        stdout["data"]["artifactBytes"],
        spec_artifact_fixture().len().to_string()
    );
    assert!(
        thruput_encoding::decode_fixed::<32>(stdout["data"]["predictedAddress"].as_str().unwrap())
            .is_ok()
    );
    assert!(
        stdout["data"]["signedTx"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
    assert_eq!(
        signed_tx_deploy_contract_kind(stdout["data"]["signedTx"].as_str().unwrap()),
        "rv32"
    );
    let signed_tx = decode_signed_tx(stdout["data"]["signedTx"].as_str().unwrap());
    assert_eq!(
        thruput_encoding::encode(signed_tx.tx.invocation.invoked_address.as_ref()),
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
    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
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

    descriptor_server.join().unwrap();
    rpc_server.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["walletEntrypointId"], "0xadac498222148e4f");
    assert_eq!(stdout["data"]["walletEntrypointSource"], "legacy_default");
    assert_eq!(
        signed_tx_entrypoint(stdout["data"]["signedTx"].as_str().unwrap()),
        "0xadac498222148e4f"
    );
}

#[test]
fn deploy_rejects_removed_target_flag() {
    let output = thruput()
        .args(["deploy", "--target", "wasm"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--target'"));
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

    let artifact_path = project.join("target/thruput/demo_contract.rv32a");
    std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    std::fs::write(&artifact_path, spec_artifact_fixture()).unwrap();

    let output = thruput()
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
        thruput_cli::deploy::DEPLOY_JSON_SCHEMA_VERSION
    );
    assert_eq!(stdout["data"]["artifactKind"], "rv32");
    assert_eq!(
        stdout["data"]["artifactPath"].as_str().unwrap(),
        std::fs::canonicalize(&artifact_path)
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(
        stdout["data"]["artifactBytes"],
        spec_artifact_fixture().len().to_string()
    );
    assert_eq!(
        signed_tx_deploy_contract_kind(stdout["data"]["signedTx"].as_str().unwrap()),
        "rv32"
    );
}

#[test]
fn deploy_estimate_needs_no_signer_and_checks_network_rules() {
    for (available, code_limit, memory_limit, expected_error) in [
        (true, Some(16), Some(4096), None),
        (
            false,
            None,
            None,
            Some("RPC did not supply authoritative execution rules"),
        ),
        (
            true,
            Some(15),
            Some(4096),
            Some("RV32 code is 16 bytes; network limit is 15"),
        ),
        (
            true,
            Some(16),
            Some(4095),
            Some("RV32 memory is 4096 bytes; network limit is 4095"),
        ),
        (
            true,
            None,
            Some(4096),
            Some("runtime did not report the effective RV32 code limit"),
        ),
        (
            true,
            Some(16),
            None,
            Some("runtime did not report the effective RV32 memory limit"),
        ),
    ] {
        assert_deploy_estimate(available, code_limit, memory_limit, expected_error);
    }
}

fn assert_deploy_estimate(
    available: bool,
    code_limit: Option<u32>,
    memory_limit: Option<u32>,
    expected_error: Option<&str>,
) {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = 'default'\n[profile.default]\n",
    )
    .unwrap();
    let artifact = thruput_rv32_artifact::build_artifact(&[0; 16], &[], 0, 4096).unwrap();
    let path = tmp.path().join("estimate.rv32a");
    std::fs::write(&path, &artifact).unwrap();
    let mut result = if available {
        serde_json::json!({
            "execution": {
                "observedTip": { "blockHeight": "7", "blockHash": thruput_json_rpc_types::NetworkHashText::from([0x11; 32]) },
                "configHash": thruput_json_rpc_types::NetworkHashText::from([0x22; 32]),
                "effectiveLimits": {
                    "maxContractSizeBytes": 1_000_000,
                    "maxRv32CodeBytes": code_limit,
                    "maxRv32MemoryBytes": memory_limit
                },
                "storageCosts": {
                    "baseDataItemCost": "100", "bytestringCostPerByte": "1",
                    "baseContractDeploymentCost": "200", "contractDeploymentCostPerByte": "2",
                    "authPerKeyCost": "1", "rcqCostPerEntry": "1", "replayCacheCostPerEntry": "1",
                    "balanceCost": "30"
                }
            }
        })
    } else {
        serde_json::json!({})
    };
    result["stated"] = serde_json::json!({});
    if let Some(limits) = result
        .pointer_mut("/execution/effectiveLimits")
        .and_then(serde_json::Value::as_object_mut)
    {
        limits.retain(|_, value| !value.is_null());
    }
    let server = rpc_mock()
        .method("getChainConfig", move |_| Ok(result.clone()))
        .start();
    let (rpc_url, server) = recorded_mock(server, |script| {
        script.expect("getChainConfig");
    });
    let output = thruput()
        .args([
            "--json",
            "--non-interactive",
            "deploy",
            "--estimate",
            "--artifact",
        ])
        .arg(&path)
        .args(["--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .output()
        .unwrap();
    match expected_error {
        None => {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(json["data"]["schemaVersion"], 2);
            assert_eq!(json["data"]["artifactBytes"], artifact.len().to_string());
            assert_eq!(
                json["data"]["minimumFundingRaw"],
                (230 + artifact.len() * 2).to_string()
            );
            assert_eq!(json["data"]["execution"]["observedTip"]["blockHeight"], "7");
            assert!(json["data"].get("signedTx").is_none());
            assert!(output.stderr.is_empty());
        }
        Some(message) => {
            assert!(!output.status.success());
            let json: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(json["error"]["code"], "deployment_funding");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(message),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty());
        }
    }
    server.join().unwrap();
}

/// Decode the `0x`-hex signed transaction in `value`, failing with `case`.
fn case_signed_tx(
    case: &str,
    value: &serde_json::Value,
) -> thruput_xdr::transaction::SignedTransaction {
    let hex = value
        .as_str()
        .unwrap_or_else(|| panic!("{case}: no signed transaction in the report: {value}"));
    let bytes = hex::decode(hex.strip_prefix("0x").unwrap_or(hex))
        .unwrap_or_else(|error| panic!("{case}: signed transaction is not hex ({error}): {hex}"));
    xdrs::txs::decode(&bytes)
        .unwrap_or_else(|error| panic!("{case}: signed transaction does not decode: {error:?}"))
}

/// Where a deploy dry run takes its RPC URL and runtime manifest from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeployTargetSource {
    Profile,
    Env,
    Cli,
}

impl DeployTargetSource {
    /// The dev wallet this source's manifest advertises when `expected` should
    /// win: the dev-0 wallet the signing key controls for the winner, and a
    /// different dev wallet for each source that should lose.
    fn wallet(self, expected: Self) -> String {
        advertised_dev_wallet_address(if self == expected {
            0
        } else {
            match self {
                Self::Profile => 1,
                Self::Env => 2,
                Self::Cli => 3,
            }
        })
    }
}

/// What `THRUPUT_RUNTIME_MANIFEST` holds for a deploy precedence case.
#[derive(Clone, Copy)]
enum RuntimeManifestEnv {
    Unset,
    Blank,
    Manifest,
}

struct DeployTargetCase {
    name: &'static str,
    /// The profile names its manifest by absolute path instead of relative to
    /// the project.
    absolute_profile_manifest: bool,
    env: RuntimeManifestEnv,
    /// Pass `--rpc-url` and `--runtime-manifest`; the profile's RPC URL is
    /// then unreachable.
    cli_target: bool,
    expected: DeployTargetSource,
}

const DEPLOY_TARGET_CASES: [DeployTargetCase; 5] = [
    DeployTargetCase {
        name: "profile manifest by relative path",
        absolute_profile_manifest: false,
        env: RuntimeManifestEnv::Unset,
        cli_target: false,
        expected: DeployTargetSource::Profile,
    },
    DeployTargetCase {
        name: "profile manifest by absolute path",
        absolute_profile_manifest: true,
        env: RuntimeManifestEnv::Unset,
        cli_target: false,
        expected: DeployTargetSource::Profile,
    },
    DeployTargetCase {
        name: "env manifest overrides profile",
        absolute_profile_manifest: false,
        env: RuntimeManifestEnv::Manifest,
        cli_target: false,
        expected: DeployTargetSource::Env,
    },
    DeployTargetCase {
        name: "cli target overrides env and profile",
        absolute_profile_manifest: false,
        env: RuntimeManifestEnv::Manifest,
        cli_target: true,
        expected: DeployTargetSource::Cli,
    },
    DeployTargetCase {
        name: "blank env manifest falls back to profile",
        absolute_profile_manifest: false,
        env: RuntimeManifestEnv::Blank,
        cli_target: false,
        expected: DeployTargetSource::Profile,
    },
];

#[test]
fn deploy_dry_run_resolves_rpc_url_and_runtime_manifest_by_precedence() {
    for case in &DEPLOY_TARGET_CASES {
        let name = case.name;
        eprintln!("case: {name}");
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let profile_dir = if case.absolute_profile_manifest {
            tmp.path().to_owned()
        } else {
            project.clone()
        };
        let profile_manifest = write_runtime_manifest(
            &profile_dir,
            0,
            &DeployTargetSource::Profile.wallet(case.expected),
            None,
        );
        let profile_manifest = if case.absolute_profile_manifest {
            profile_manifest.display().to_string()
        } else {
            profile_manifest
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
        let profile_rpc_url = if case.cli_target {
            "http://127.0.0.1:1"
        } else {
            rpc_url.as_str()
        };
        std::fs::write(
            project.join("thruput.toml"),
            format!(
                "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{profile_rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{profile_manifest}\"\n",
            ),
        )
        .unwrap();
        let rv32_path = write_opaque_rv32_artifact(tmp.path());

        let mut command = thruput();
        command.args(["--json", "deploy", "--fund", "0", "--dry-run"]);
        if case.cli_target {
            let cli_dir = tmp.path().join("cli");
            std::fs::create_dir(&cli_dir).unwrap();
            let cli_manifest = write_runtime_manifest(
                &cli_dir,
                0,
                &DeployTargetSource::Cli.wallet(case.expected),
                None,
            );
            command
                .args(["--rpc-url", &rpc_url, "--runtime-manifest"])
                .arg(cli_manifest);
        }
        match case.env {
            RuntimeManifestEnv::Unset => {}
            RuntimeManifestEnv::Blank => {
                command.env("THRUPUT_RUNTIME_MANIFEST", "   ");
            }
            RuntimeManifestEnv::Manifest => {
                let env_dir = tmp.path().join("env");
                std::fs::create_dir(&env_dir).unwrap();
                let env_manifest = write_runtime_manifest(
                    &env_dir,
                    0,
                    &DeployTargetSource::Env.wallet(case.expected),
                    None,
                );
                command.env("THRUPUT_RUNTIME_MANIFEST", env_manifest);
            }
        }
        let output = command
            .args(["--artifact", rv32_path.to_str().unwrap()])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", profile_dir.join("fixture-config"))
            .output()
            .unwrap_or_else(|error| panic!("{name}: deploy did not run: {error}"));

        // The endpoint reports its own mismatch first; this names the row.
        let endpoint =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rpc_server.join().unwrap()));
        assert!(endpoint.is_ok(), "{name}: RPC requests did not match");

        assert!(
            output.status.success(),
            "{name}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = case_json(name, "stdout", &output.stdout);
        let signed = case_signed_tx(name, &stdout["data"]["signedTx"]);

        assert_eq!(stdout["data"]["rpcUrl"], rpc_url, "{name}");
        assert_eq!(
            thruput_encoding::encode(signed.tx.invocation.invoked_address.as_ref()),
            case.expected.wallet(case.expected),
            "{name}"
        );
        assert_eq!(
            signed_tx_entrypoint_from_decoded(&signed),
            "0x6be0cb0f6de0917a",
            "{name}"
        );
    }
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
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            symlink_path.display()
        ),
    )
    .unwrap();

    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--fund",
            "0",
            "--dry-run",
            "--artifact",
            rv32_path.to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", tmp.path().join("fixture-config"))
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "local_runtime_unavailable");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a symlink")
    );
}

#[test]
fn deploy_requires_stored_key_even_for_dev_named_selection() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    std::fs::write(
        project.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\n",
    )
    .unwrap();

    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--fund",
            "0",
            "--dry-run",
            "--artifact",
            rv32_path.to_str().unwrap(),
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
    assert_eq!(stderr["error"]["code"], "key_not_found");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("dev-0")
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

    // The deploy path treats the selected RV32 artifact as opaque bytes.
    let rv32_path = write_opaque_rv32_artifact(tmp.path());

    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    rpc_server.join().unwrap();
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "deploy");
    assert_eq!(stdout["warnings"], serde_json::json!([]));
    assert_eq!(
        stdout["data"]["schemaVersion"],
        thruput_cli::deploy::DEPLOY_JSON_SCHEMA_VERSION
    );

    // Raw return bytes carry an address; the report renders that address as base58.
    let expected_address = thruput_encoding::encode(&[0xbb; 32]);
    assert_eq!(stdout["data"]["address"], expected_address);
    assert_eq!(stdout["data"]["addressSource"], "receipt");
    assert_eq!(stdout["data"]["gasUsed"], "1500");
    assert_eq!(stdout["data"]["blockHeight"], "42");
    assert_eq!(stdout["data"]["execution"]["result"], "return");
    assert!(stdout["data"].get("returnData").is_none());
    assert!(stdout["data"].get("abortData").is_none());
    assert!(stdout["data"].get("transactionDetail").is_none());
    assert!(
        thruput_encoding::decode_fixed::<32>(stdout["data"]["txHash"].as_str().unwrap()).is_ok()
    );
}

#[test]
fn deploy_live_json_fails_with_receipt_context() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(42, sample_failed_deploy_receipt(42));
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let rv32_path = write_opaque_rv32_artifact(tmp.path());
    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
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
    assert_eq!(stdout["data"]["blockHeight"], "42");
    assert_eq!(stdout["data"]["execution"]["result"], "fault");
    assert!(stdout["data"]["execution"].get("returnData").is_none());
    assert!(stdout["data"]["execution"].get("abortData").is_none());
    assert!(
        thruput_encoding::decode_fixed::<32>(stdout["data"]["txHash"].as_str().unwrap()).is_ok()
    );
}

#[test]
fn deploy_live_json_reports_actionable_native_token_overdraft() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) =
        spawn_transaction_rpc(42, sample_insufficient_funding_deploy_receipt(42));
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let rv32_path = write_opaque_rv32_artifact(tmp.path());
    let output = thruput()
        .args([
            "--json",
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
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
    assert_eq!(stdout["data"]["schemaVersion"], 9);
    assert_eq!(stdout["data"]["blockHeight"], "42");
    assert_eq!(stdout["data"]["execution"]["result"], "abort");
    assert_eq!(stdout["error"]["code"], "transaction_insufficient_funds");
    assert_eq!(
        stdout["error"]["details"]["reason"],
        "nativeTokenTransferOverdraft"
    );
    assert!(stdout["error"]["hint"].as_str().is_some_and(|hint| {
        hint.contains("thruput balance --profile=default -- alice")
            && hint.contains("thruput airdrop --profile=default -- alice")
    }));
    assert_ne!(stdout["error"]["code"], "transaction_reverted");
    assert!(
        stdout["data"]["execution"]["abortData"]
            .as_str()
            .is_some_and(|value| value.starts_with("0x"))
    );
}

#[test]
fn deploy_live_human_reports_selected_wallet_funding_commands() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) =
        spawn_transaction_rpc(42, sample_insufficient_funding_deploy_receipt(42));
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let rv32_path = write_opaque_rv32_artifact(tmp.path());
    let output = thruput()
        .args([
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("wallet `alice` has insufficient native tokens"));
    assert!(stderr.contains("thruput balance --profile=default -- alice"));
    assert!(stderr.contains("thruput airdrop --profile=default -- alice"));
    assert!(stderr.contains("retry the original deploy command"));
}

#[test]
fn deploy_live_human_fails_after_printing_receipt_context() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(42, sample_failed_deploy_receipt(42));
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let rv32_path = write_opaque_rv32_artifact(tmp.path());
    let output = thruput()
        .args([
            "deploy",
            "--force",
            "--artifact",
            rv32_path.to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning: Deploy failed with a deterministic execution fault"));
    assert!(stderr.lines().any(|line| {
        line.trim()
            .strip_prefix("tx: ")
            .is_some_and(|hash| thruput_encoding::decode_fixed::<32>(hash).is_ok())
    }));
    assert!(stderr.contains("height: 42"));
    assert!(!stderr.contains("return data:"), "stderr: {stderr}");
    assert!(!stderr.contains("abort data:"), "stderr: {stderr}");
    assert!(stderr.contains("gas used: 4242"), "stderr: {stderr}");
}

fn empty_wallet_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config = tmp.path().join("config");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("thruput.toml"), "default_profile = 'default'\n[profile.default]\ndefault_key = 'owner'\nrpc_url = 'http://127.0.0.1:1'\nfaucet_url = 'http://127.0.0.1:1'\n").unwrap();
    assert!(
        thruput()
            .args(["keys", "generate", "owner"])
            .env("XDG_CONFIG_HOME", &config)
            .current_dir(&project)
            .output()
            .unwrap()
            .status
            .success()
    );
    (tmp, project, config)
}

fn create_empty_wallet(
    project: &std::path::Path,
    config: &std::path::Path,
    extra: &[&str],
) -> std::process::Output {
    thruput()
        .args(["--json", "wallet", "create", "--force"])
        .args(extra)
        .current_dir(project)
        .env("XDG_CONFIG_HOME", config)
        .output()
        .unwrap()
}

#[test]
fn wallet_create_is_offline_idempotent_and_funding_is_explicit() {
    let (_tmp, project, config) = empty_wallet_fixture();
    let first = create_empty_wallet(&project, &config, &[]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let data = &report["data"];
    assert_eq!(data["schemaVersion"], 6);
    assert_eq!(data["kind"], "native");
    assert_eq!(data["accountNonce"], "0");
    assert_eq!(data["created"], true);
    assert_eq!(data["saved"], true);
    assert!(
        data["fundingCommand"]
            .as_str()
            .unwrap()
            .contains(data["wallet"].as_str().unwrap())
    );
    for field in ["txHash", "height", "balance", "prefundTxHash", "signedTx"] {
        assert!(data.get(field).is_none());
    }
    let before = std::fs::read(project.join("thruput.toml")).unwrap();
    let second = create_empty_wallet(&project, &config, &[]);
    assert!(second.status.success());
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["data"]["created"], false);
    assert_eq!(second["data"]["wallet"], data["wallet"]);
    assert_eq!(std::fs::read(project.join("thruput.toml")).unwrap(), before);
    let profile: toml::Value = toml::from_str(std::str::from_utf8(&before).unwrap()).unwrap();
    assert_eq!(
        profile["profile"]["default"]["default_wallet"].as_str(),
        Some("owner")
    );
}

#[test]
fn wallet_create_uses_public_metadata_and_signing_still_checks_ownership() {
    let (_tmp, project, config) = empty_wallet_fixture();
    let first = create_empty_wallet(&project, &config, &["--dry-run"]);
    assert!(first.status.success());
    let original: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let key_path = config.join("thruput/keys/owner.json");
    let mut key: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&key_path).unwrap()).unwrap();
    let public_key = [0xab; 32];
    key["public_key_hex"] = serde_json::Value::String(hex::encode(public_key));
    std::fs::write(&key_path, serde_json::to_vec(&key).unwrap()).unwrap();
    let output = create_empty_wallet(&project, &config, &[]);
    assert!(output.status.success());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let account = thruput_signing::PubkeyAccount::from_public_key_bytes(public_key, 0);
    assert_eq!(
        actual["data"]["wallet"],
        thruput_encoding::encode(account.address().as_ref())
    );
    assert_ne!(actual["data"]["wallet"], original["data"]["wallet"]);
    assert_eq!(
        actual["data"]["ownerPublicKey"],
        thruput_encoding::encode(&public_key)
    );
    let output = thruput()
        .args([
            "--json",
            "transfer",
            "--dry-run",
            &thruput_encoding::encode(&[0x11; 32]),
            "1",
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(report["error"]["code"], "saved_wallet_owner_mismatch");
}

#[test]
fn wallet_create_and_use_work_without_private_key_bytes() {
    let (_tmp, project, config) = empty_wallet_fixture();
    let expected = create_empty_wallet(&project, &config, &["--dry-run"]);
    let expected: serde_json::Value = serde_json::from_slice(&expected.stdout).unwrap();
    let original = std::fs::read(project.join("thruput.toml")).unwrap();
    make_local_keypair_unavailable(&config, "owner");
    let dry_run = create_empty_wallet(&project, &config, &["--name", "primary", "--dry-run"]);
    assert!(
        dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&dry_run.stdout).unwrap();
    assert_eq!(report["data"]["wallet"], expected["data"]["wallet"]);
    assert_eq!(
        std::fs::read(project.join("thruput.toml")).unwrap(),
        original
    );

    let created = create_empty_wallet(&project, &config, &["--name", "primary"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(report["data"]["wallet"], expected["data"]["wallet"]);
    let selected = thruput()
        .args(["--json", "wallet", "use", "primary"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let project_config: toml::Value =
        toml::from_str(&std::fs::read_to_string(project.join("thruput.toml")).unwrap()).unwrap();
    assert_eq!(
        project_config["profile"]["default"]["default_wallet"].as_str(),
        Some("primary")
    );
}

#[test]
fn wallet_use_preserves_bindings_and_checks_owner_without_signer() {
    for native in [false, true] {
        let (_tmp, project, config) = empty_wallet_fixture();
        let keyfile: serde_json::Value =
            serde_json::from_slice(&std::fs::read(config.join("thruput/keys/owner.json")).unwrap())
                .unwrap();
        let public_key: [u8; 32] = hex::decode(keyfile["public_key_hex"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let owner = thruput_encoding::encode(&public_key);
        let address = if native {
            let account = thruput_signing::PubkeyAccount::from_public_key_bytes(public_key, 7);
            thruput_encoding::encode(account.address().as_ref())
        } else {
            thruput_encoding::encode(&[0xab; 32])
        };
        let kind = if native {
            "kind = 'native'\naccount_nonce = 7\n"
        } else {
            ""
        };
        let original = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
        let profile_fragment = format!(
            "{original}\n[profile.default.wallets.trading]\n\
             {kind}address = '{address}'\nowner_public_key = '{owner}'\n"
        );
        std::fs::write(project.join("thruput.toml"), profile_fragment).unwrap();
        make_local_keypair_unavailable(&config, "owner");
        let output = thruput()
            .args(["--json", "wallet", "use", "trading"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let before = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
        let project_config: toml::Value = toml::from_str(&before).unwrap();
        assert_eq!(
            project_config["profile"]["default"]["default_wallet"].as_str(),
            Some("trading")
        );
        assert_eq!(
            project_config["profile"]["default"]["wallets"]["trading"]["address"].as_str(),
            Some(address.as_str())
        );
        generate_key_public_key(&config, "other");
        make_local_keypair_unavailable(&config, "other");
        let mismatched = before.replace("default_key = 'owner'", "default_key = 'other'");
        std::fs::write(project.join("thruput.toml"), &mismatched).unwrap();
        let output = thruput()
            .args(["--json", "wallet", "use", "trading"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(report["error"]["code"], "saved_wallet_owner_mismatch");
        assert_eq!(
            std::fs::read_to_string(project.join("thruput.toml")).unwrap(),
            mismatched
        );
    }
}

#[test]
fn wallet_create_respects_inherited_binding_and_selection() {
    let (_tmp, project, config) = empty_wallet_fixture();
    assert!(create_empty_wallet(&project, &config, &[]).status.success());
    let project_path = project.join("thruput.toml");
    let original = std::fs::read_to_string(&project_path).unwrap();
    std::fs::write(config.join("thruput/config.toml"), &original).unwrap();
    std::fs::write(
        &project_path,
        "default_profile = 'default'\n[profile.default]\n",
    )
    .unwrap();
    let before = std::fs::read(&project_path).unwrap();
    let output = create_empty_wallet(&project, &config, &[]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["created"], false);
    assert_eq!(std::fs::read(&project_path).unwrap(), before);
    assert!(
        create_empty_wallet(&project, &config, &["--name", "second"])
            .status
            .success()
    );
    let project_config: toml::Value =
        toml::from_str(&std::fs::read_to_string(&project_path).unwrap()).unwrap();
    assert!(
        project_config["profile"]["default"]
            .get("default_wallet")
            .is_none()
    );
    assert_eq!(
        std::fs::read_to_string(config.join("thruput/config.toml")).unwrap(),
        original
    );
}

#[test]
fn wallet_create_preserves_global_key_selection_when_adding_another_account() {
    for legacy_wallet in [false, true] {
        let (_tmp, project, config) = empty_wallet_fixture();
        let selected = thruput()
            .args(["keys", "use", "owner"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config)
            .output()
            .unwrap();
        assert!(selected.status.success());
        let mut project_config = "default_profile = 'default'\n[profile.default]\n".to_owned();
        if legacy_wallet {
            let key: serde_json::Value = serde_json::from_slice(
                &std::fs::read(config.join("thruput/keys/owner.json")).unwrap(),
            )
            .unwrap();
            let public_key = thruput_encoding::encode(
                &hex::decode(key["public_key_hex"].as_str().unwrap()).unwrap(),
            );
            write!(project_config, "\n[profile.default.wallets.legacy]\naddress = '{}'\nowner_public_key = '{public_key}'\n", thruput_encoding::encode(&[0xab; 32])).unwrap();
        }
        std::fs::write(project.join("thruput.toml"), project_config).unwrap();
        if !legacy_wallet {
            let first = create_empty_wallet(&project, &config, &[]);
            assert!(
                first.status.success(),
                "{}",
                String::from_utf8_lossy(&first.stderr)
            );
        }
        if !legacy_wallet {
            let path = project.join("thruput.toml");
            let mut doc: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            doc["profile"]["default"]
                .as_table_mut()
                .unwrap()
                .remove("default_wallet");
            std::fs::write(path, toml::to_string(&doc).unwrap()).unwrap();
        }
        let added = create_empty_wallet(&project, &config, &["--name", "trading"]);
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );
        let config: toml::Value =
            toml::from_str(&std::fs::read_to_string(project.join("thruput.toml")).unwrap())
                .unwrap();
        assert_eq!(
            config["profile"]["default"]["default_wallet"].as_str(),
            Some(if legacy_wallet { "legacy" } else { "owner" })
        );
        assert!(config["profile"]["default"].get("default_key").is_none());
    }
}

#[test]
fn wallet_create_dry_run_only_derives_and_prefund_is_rejected() {
    let (_tmp, project, config) = empty_wallet_fixture();
    let before = std::fs::read(project.join("thruput.toml")).unwrap();
    let output = create_empty_wallet(&project, &config, &["--dry-run", "--name", "trading"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data"]["dryRun"], true);
    assert_eq!(report["data"]["saved"], false);
    assert_eq!(report["data"]["name"], "trading");
    assert!(
        thruput_encoding::decode_fixed::<32>(report["data"]["wallet"].as_str().unwrap()).is_ok()
    );
    let output = create_empty_wallet(&project, &config, &["--prefund", "2"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("wallet_create_prefund_unsupported"));
    assert_eq!(std::fs::read(project.join("thruput.toml")).unwrap(), before);
}

#[test]
fn wallet_create_funding_hint_keeps_explicit_target_overrides() {
    let (_tmp, project, config) = empty_wallet_fixture();
    let output = create_empty_wallet(
        &project,
        &config,
        &[
            "--dry-run",
            "--rpc-url",
            "http://127.0.0.1:88",
            "--runtime-manifest",
            "manifest with spaces.json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let command = report["data"]["fundingCommand"].as_str().unwrap();
    assert!(command.contains("--rpc-url=http://127.0.0.1:88"));
    assert!(command.contains("--runtime-manifest='manifest with spaces.json'"));
}

#[test]
fn wallet_funding_hint_cannot_turn_wallet_names_into_options() {
    let (_tmp, project, config) = empty_wallet_fixture();
    let output = create_empty_wallet(&project, &config, &["--name=--force"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &report["data"];
    let command = data["fundingCommand"].as_str().unwrap();
    assert!(command.contains(data["wallet"].as_str().unwrap()));
    assert!(!command.contains("--force"));
    assert!(command.contains("--profile=default"));
}

#[test]
fn wallet_create_preserves_existing_identity_and_default() {
    let (_tmp, project, config) = empty_wallet_fixture();
    assert!(create_empty_wallet(&project, &config, &[]).status.success());
    let path = project.join("thruput.toml");
    let mut doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["profile"]["default"]["wallets"]["owner"]["kind"] =
        toml::Value::String("dev_wallet".into());
    doc["profile"]["default"]["wallets"]["owner"]
        .as_table_mut()
        .unwrap()
        .remove("account_nonce");
    std::fs::write(&path, toml::to_string(&doc).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let output = create_empty_wallet(&project, &config, &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("wallet_binding_conflict"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        create_empty_wallet(&project, &config, &["--name", "native"])
            .status
            .success()
    );
    let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        doc["profile"]["default"]["default_wallet"].as_str(),
        Some("owner")
    );
}

#[test]
fn concurrent_wallet_creates_are_idempotent() {
    let (_tmp, project, config) = empty_wallet_fixture();
    let children: Vec<_> = (0..2)
        .map(|_| {
            thruput()
                .args(["--json", "wallet", "create", "--force"])
                .current_dir(&project)
                .env("XDG_CONFIG_HOME", &config)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let reports: Vec<serde_json::Value> = children
        .into_iter()
        .map(|child| {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice(&output.stdout).unwrap()
        })
        .collect();
    assert_eq!(
        reports
            .iter()
            .filter(|report| report["data"]["created"] == true)
            .count(),
        1
    );
    assert_eq!(reports[0]["data"]["wallet"], reports[1]["data"]["wallet"]);
}

#[test]
fn wallet_ls_json_outputs_saved_wallets() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let alice = thruput_encoding::encode(&[0xab; 32]);
    let bob = thruput_encoding::encode(&[0xcd; 32]);
    let alice_owner = thruput_encoding::encode(&[0x11; 32]);
    let bob_owner = thruput_encoding::encode(&[0x22; 32]);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:9545\"\ndefault_wallet = \"alice\"\n\n[profile.default.wallets.alice]\naddress = \"{alice}\"\nowner_public_key = \"{alice_owner}\"\n\n[profile.default.wallets.bob]\naddress = \"{bob}\"\nowner_public_key = \"{bob_owner}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
    assert_eq!(stdout["data"]["schemaVersion"], 1);
    assert_eq!(
        stdout["data"]["wallets"],
        serde_json::json!([
            { "name": "alice", "address": alice, "ownerPublicKey": alice_owner, "isDefault": true },
            { "name": "bob", "address": bob, "ownerPublicKey": bob_owner, "isDefault": false },
        ])
    );

    let human = thruput()
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
    let address = thruput_encoding::encode(&[0xab; 32]);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\ndefault_key = \"alice\"\n\n[profile.default.wallets.trading]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
    let contents = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
    assert!(
        contents.contains("default_wallet = \"trading\""),
        "config: {contents}"
    );
    let profile = thruput_cli::config::resolve_profile(&project, Some("default")).unwrap();
    assert_eq!(profile.default_wallet.as_deref(), Some("trading"));
}

#[test]
fn wallet_rm_rejects_inherited_default_with_project_only_binding() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(config_home.join("thruput")).unwrap();
    let address = thruput_encoding::encode(&[0xab; 32]);
    let owner = thruput_encoding::encode(&[0x11; 32]);
    std::fs::write(
        config_home.join("thruput/config.toml"),
        "[profile.devnet]\ndefault_wallet = \"alice\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "[profile.devnet]\n\n[profile.devnet.wallets.alice]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "wallet", "rm", "alice", "--profile", "devnet"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "inherited_default_wallet_removal");
    let profile = thruput_cli::config::resolve_profile(&project, Some("devnet")).unwrap();
    assert!(profile.wallets.contains_key("alice"));
}

#[test]
fn wallet_rm_reports_inherited_default_after_clearing_project_default() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(config_home.join("thruput")).unwrap();
    let owner = thruput_encoding::encode(&[0x11; 32]);
    std::fs::write(
        config_home.join("thruput/config.toml"),
        format!(
            "[profile.devnet]\ndefault_wallet = \"global\"\n\n[profile.devnet.wallets.global]\naddress = \"0x{}\"\nowner_public_key = \"{owner}\"\n",
            "aa".repeat(32)
        ),
    )
    .unwrap();
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "[profile.devnet]\ndefault_wallet = \"project\"\n\n[profile.devnet.wallets.project]\naddress = \"0x{}\"\nowner_public_key = \"{owner}\"\n",
            "bb".repeat(32)
        ),
    )
    .unwrap();

    let refused = thruput()
        .args(["--json", "wallet", "rm", "project", "--profile", "devnet"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let refused: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(refused["error"]["code"], "confirmation_required");
    assert_eq!(refused["error"]["details"]["requiredFlag"], "--yes");

    let output = thruput()
        .args([
            "--json",
            "wallet",
            "rm",
            "project",
            "--profile",
            "devnet",
            "--force",
        ])
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
    std::fs::create_dir_all(config_home.join("thruput")).unwrap();
    let owner = thruput_encoding::encode(&[0x11; 32]);
    std::fs::write(
        config_home.join("thruput/config.toml"),
        "[profile.devnet]\ndefault_wallet = \"alice\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "[profile.devnet]\ndefault_wallet = \"alice\"\n\n[profile.devnet.wallets.alice]\naddress = \"0x{}\"\nowner_public_key = \"{owner}\"\n",
            "aa".repeat(32)
        ),
    )
    .unwrap();

    let output = thruput()
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
    let contents = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
    assert!(contents.contains("default_wallet = \"alice\""));
    assert!(contents.contains("[profile.devnet.wallets.alice]"));
}

#[test]
fn airdrop_dry_run_json_uses_default_wallet_when_recipient_is_omitted() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let faucet_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &faucet_wallet, None);
    let (rpc_url, rpc_server) =
        spawn_balance_then_chain_tip_rpc(&faucet_wallet, "100000000000", 42);
    let recipient_wallet = thruput_encoding::encode(&[0xab; 32]);
    let recipient_owner = thruput_encoding::encode(&[0xcd; 32]);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_wallet = \"alice\"\nruntime_manifest = \"{}\"\n\n[profile.default.wallets.alice]\naddress = \"{recipient_wallet}\"\nowner_public_key = \"{recipient_owner}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "airdrop", "--amount", "1.25", "--dry-run"])
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
    assert_eq!(stdout["data"]["schemaVersion"], 6);
    assert_eq!(stdout["data"]["dryRun"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["recipient"], "alice");
    assert_eq!(stdout["data"]["recipientWallet"], recipient_wallet);
    assert_eq!(stdout["data"]["amountTokens"], "1.25");
    assert_eq!(stdout["data"]["rawAmount"], "12500000");
    assert_eq!(stdout["data"]["faucet"]["address"], faucet_wallet);
    assert_eq!(stdout["data"]["faucet"]["role"], "faucet");
    assert!(stdout["data"].get("signedTx").is_none());
}

#[test]
fn airdrop_default_uses_unsaved_native_address_without_resolving_key_name_again() {
    for conflicting_name in [false, true] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config_home = tmp.path().join("config");
        std::fs::create_dir_all(&project).unwrap();
        let owner = generate_key_public_key(&config_home, "alice");
        let account = thruput_signing::PubkeyAccount::from_public_key_bytes(
            thruput_signing::strkey::parse_public_key(&owner).unwrap(),
            0,
        );
        make_local_keypair_unavailable(&config_home, "alice");
        let faucet_wallet = advertised_dev_wallet_address(0);
        let manifest_path = write_runtime_manifest(&project, 0, &faucet_wallet, None);
        let (rpc_url, rpc_server) =
            spawn_balance_then_chain_tip_rpc(&faucet_wallet, "100000000000", 42);
        let unrelated = if conflicting_name {
            format!(
                "\n[profile.default.wallets.alice]\naddress = '{}'\nowner_public_key = '{}'\n",
                thruput_encoding::encode(&[0xab; 32]),
                thruput_encoding::encode(&[0xcd; 32])
            )
        } else {
            String::new()
        };
        std::fs::write(project.join("thruput.toml"), format!(
            "default_profile = 'default'\n[profile.default]\nrpc_url = '{rpc_url}'\ndefault_key = 'alice'\nruntime_manifest = '{}'\n{unrelated}",
            manifest_path.file_name().unwrap().to_string_lossy())).unwrap();
        let output = thruput()
            .args(["--json", "airdrop", "--amount", "1", "--dry-run"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        rpc_server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["data"]["recipient"], "alice");
        assert_eq!(
            report["data"]["recipientWallet"],
            thruput_encoding::encode(account.address().as_ref())
        );
    }
}

#[test]
fn airdrop_default_submits_without_recipient_signer() {
    for hosted in [false, true] {
        for selection in ["unsaved", "legacy", "native"] {
            let tmp = TempDir::new().unwrap();
            let project = tmp.path().join("project");
            let config_home = tmp.path().join("config");
            std::fs::create_dir_all(&project).unwrap();
            let owner = generate_key_public_key(&config_home, "alice");
            let owner_bytes = thruput_signing::strkey::parse_public_key(&owner).unwrap();
            let nonce = if selection == "native" { 7 } else { 0 };
            let account = thruput_signing::PubkeyAccount::from_public_key_bytes(owner_bytes, nonce);
            let recipient = if selection == "legacy" {
                thruput_encoding::encode(&[0xab; 32])
            } else {
                thruput_encoding::encode(account.address().as_ref())
            };
            let binding = match selection {
                "legacy" => format!(
                    "\n[profile.default.wallets.trading]\naddress = '{recipient}'\nowner_public_key = '{owner}'\n"
                ),
                "native" => format!(
                    "\n[profile.default.wallets.trading]\nkind = 'native'\naccount_nonce = 7\naddress = '{recipient}'\nowner_public_key = '{owner}'\n"
                ),
                _ => String::new(),
            };
            make_local_keypair_unavailable(&config_home, "alice");
            let faucet = advertised_dev_wallet_address(0);
            let (rpc_url, rpc_server) = if hosted {
                spawn_airdrop_rpc(&recipient)
            } else {
                spawn_local_airdrop_rpc(&faucet, &recipient)
            };
            let (profile_fields, descriptor_server) = if hosted {
                let (faucet_url, server) = spawn_network_descriptor(&rpc_url);
                (format!("faucet_url = '{faucet_url}'\n"), Some(server))
            } else {
                let manifest = write_runtime_manifest(&project, 0, &faucet, None);
                (
                    format!("runtime_manifest = '{}'\n", manifest.display()),
                    None,
                )
            };
            // Hosted cases use the global default key selected during key generation.
            let default_key = if hosted {
                ""
            } else {
                "default_key = 'alice'\n"
            };
            std::fs::write(project.join("thruput.toml"), format!(
                "default_profile = 'default'\n[profile.default]\nrpc_url = '{rpc_url}'\n{default_key}{profile_fields}{binding}"
            )).unwrap();
            let output = thruput()
                .args(["--json", "airdrop", "--force"])
                .current_dir(&project)
                .env("XDG_CONFIG_HOME", &config_home)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "hosted={hosted}, {selection}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if let Some(server) = descriptor_server {
                server.join().unwrap();
            }
            rpc_server.join().unwrap();
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["data"]["recipientWallet"], recipient);
            assert_eq!(
                report["data"]["recipient"],
                if selection == "unsaved" {
                    "alice"
                } else {
                    "trading"
                }
            );
            assert_eq!(report["ok"], true);
            assert!(report["data"]["txHash"].is_string());
            assert_eq!(
                report["data"]["mode"],
                if hosted { "remote" } else { "local" }
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_airdrop_interrupt_stops_preflight_or_retains_write_uncertainty() {
    for during_write in [false, true] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config_home = tmp.path().join("config");
        let cache_home = tmp.path().join("cache");
        std::fs::create_dir_all(&project).unwrap();
        let recipient = thruput_encoding::encode(&[0xaa; 32]);
        let faucet = advertised_dev_wallet_address(0);
        let listener = MockListener::bind();
        let rpc_url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let manifest = write_runtime_manifest(&project, 0, &faucet, None);
        std::fs::write(project.join("thruput.toml"), format!(
            "default_profile = 'default'\n[profile.default]\nrpc_url = '{rpc_url}'\nruntime_manifest = '{}'\n", manifest.display())).unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = spawn_mock(move || {
            let deadline = Instant::now() + CLI_ACTION_TIMEOUT;
            serve_balance(&listener, deadline, &faucet, "200012006", 41);
            let (mut stream, mut request) =
                read_jsonrpc_request(&listener, deadline, "getNetworkInfo");
            assert_eq!(request["method"], "getNetworkInfo");
            if during_write {
                send_jsonrpc_response(
                    stream,
                    &request["id"],
                    serde_json::json!({"networkHash":TEST_NETWORK_HASH}),
                );
                (stream, request) = read_jsonrpc_request(&listener, deadline, "dev_fundAccount");
                assert_eq!(request["method"], "dev_fundAccount");
            }
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(CLI_ACTION_TIMEOUT);
            drop(stream);
            assert_no_follow_up_rpc(&listener);
        });
        let mut command = thruput();
        command
            .args(["--json", "airdrop", &recipient, "--force"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_CACHE_HOME", &cache_home);
        let child = tokio::process::Command::from(command)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        tokio::time::timeout(Duration::from_secs(30), ready_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            Command::new("/bin/kill")
                .args(["-INT", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        release_tx.send(()).unwrap();
        server.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(report["error"]["details"]["cancelled"], true);
        assert_eq!(
            report["error"]["details"]["fundingRequest"],
            if during_write { "uncertain" } else { "notSent" }
        );
        let path = report["error"]["details"]["attemptPath"].as_str().unwrap();
        assert!(std::path::Path::new(path).exists());
    }
}

#[test]
fn local_airdrop_receipt_failure_preserves_acknowledged_transaction() {
    for json_output in [true, false] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config_home = tmp.path().join("config");
        let cache_home = tmp.path().join("cache");
        std::fs::create_dir_all(&project).unwrap();
        let owner = generate_key_public_key(&config_home, "alice");
        let account = thruput_signing::PubkeyAccount::from_public_key_bytes(
            thruput_signing::strkey::parse_public_key(&owner).unwrap(),
            0,
        );
        let recipient = thruput_encoding::encode(account.address().as_ref());
        let faucet = advertised_dev_wallet_address(0);
        let (rpc_url, server) =
            spawn_local_funding_rpc(&faucet, &recipient, LocalFunding::ReceiptUnavailable);
        let manifest = write_runtime_manifest(&project, 0, &faucet, None);
        std::fs::write(project.join("thruput.toml"), format!(
        "default_profile = 'default'\n[profile.default]\nrpc_url = '{rpc_url}'\ndefault_key = 'alice'\nruntime_manifest = '{}'\n", manifest.display())).unwrap();
        let mut command = thruput();
        if json_output {
            command.arg("--json");
        }
        let output = command
            .args(["airdrop", "--force"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_CACHE_HOME", &cache_home)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let signed = server.join().unwrap();
        let hash = thruput_encoding::encode(
            thruput_signing::transaction_hash(&signed.tx)
                .unwrap()
                .as_ref(),
        );
        if !json_output {
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains(&hash), "{stderr}");
            assert!(stderr.contains("Retained attempt"), "{stderr}");
            assert!(
                stderr.contains("same command with the same amount"),
                "{stderr}"
            );
            continue;
        }
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(report["error"]["code"], "native_rpc_failure");
        assert_eq!(report["error"]["details"]["transactionHash"], hash);
        let path = report["error"]["details"]["attemptPath"].as_str().unwrap();
        let attempt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(attempt["transaction"]["txHash"], hash);
        assert_eq!(
            attempt["transaction"]["signedTransaction"],
            format!("0x{}", hex::encode(xdrs::txs::encode(&signed).unwrap()))
        );
        assert!(attempt["request"]["clientNonce"].is_string());
    }
}

#[test]
fn local_airdrop_invalid_signed_evidence_is_preserved_without_journaling() {
    for corruption in ["xdr", "hash", "error"] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config_home = tmp.path().join("config");
        let cache_home = tmp.path().join("cache");
        std::fs::create_dir_all(&project).unwrap();
        let owner = generate_key_public_key(&config_home, "alice");
        let account = thruput_signing::PubkeyAccount::from_public_key_bytes(
            thruput_signing::strkey::parse_public_key(&owner).unwrap(),
            0,
        );
        let recipient = thruput_encoding::encode(account.address().as_ref());
        let faucet = advertised_dev_wallet_address(0);
        let (rpc_url, server) = spawn_local_funding_rpc(
            &faucet,
            &recipient,
            LocalFunding::InvalidEvidence(corruption),
        );
        let manifest = write_runtime_manifest(&project, 0, &faucet, None);
        std::fs::write(project.join("thruput.toml"), format!(
        "default_profile = 'default'\n[profile.default]\nrpc_url = '{rpc_url}'\ndefault_key = 'alice'\nruntime_manifest = '{}'\n", manifest.display())).unwrap();
        let output = thruput()
            .args(["--json", "airdrop", "--force"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_CACHE_HOME", &cache_home)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let signed = server.join().unwrap();
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        let result = &report["error"]["details"]["result"];
        assert_eq!(report["error"]["code"], "native_rpc_failure");
        if corruption == "xdr" {
            assert_eq!(result["signedTransaction"], "0x00");
        } else if corruption == "error" {
            let details = &report["error"]["details"];
            assert_eq!(details["rpcError"]["code"], -32031);
            assert_eq!(details["rpcError"]["data"]["signedTransaction"], "0x00");
            assert_eq!(
                details["rpcError"]["data"]["transactionHash"],
                thruput_encoding::encode(
                    thruput_signing::transaction_hash(&signed.tx)
                        .unwrap()
                        .as_ref()
                )
            );
            assert!(details["retentionError"].is_string());
        } else {
            assert_eq!(
                result["transactionHash"],
                thruput_encoding::encode(&[0x91; 32])
            );
            assert_eq!(
                result["signedTransaction"],
                format!("0x{}", hex::encode(xdrs::txs::encode(&signed).unwrap()))
            );
        }
        let path = report["error"]["details"]["attemptPath"].as_str().unwrap();
        let journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(journal.get("transaction").is_none());
    }
}

#[test]
fn airdrop_explicit_key_respects_saved_wallet_and_alias_precedence() {
    let bare_address = thruput_encoding::encode(&[0xab; 32]);
    let strkey_address = thruput_signing::strkey::format_contract_address(&[0xab; 32].into());
    for (selection, key_name) in [
        ("unsaved", "alice"),
        ("unsaved", "Charlie"),
        ("unsaved", "0xowner"),
        ("unsaved", "0Xowner"),
        ("literal", bare_address.as_str()),
        ("literal", strkey_address.as_str()),
        ("legacy", "alice"),
        ("native", "alice"),
        ("saved-name", "alice"),
        ("alias", "alice"),
    ] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config_home = tmp.path().join("config");
        std::fs::create_dir_all(&project).unwrap();
        let owner = generate_key_public_key(&config_home, key_name);
        let global_config = config_home.join("thruput/config.toml");
        let before = std::fs::read(&global_config).unwrap();
        let owner_bytes = thruput_signing::strkey::parse_public_key(&owner).unwrap();
        let nonce = if selection == "native" { 7 } else { 0 };
        let native = thruput_signing::PubkeyAccount::from_public_key_bytes(owner_bytes, nonce);
        let native_address = thruput_encoding::encode(native.address().as_ref());
        let legacy_address = thruput_encoding::encode(&[0xab; 32]);
        let expected = match selection {
            "unsaved" | "native" => native_address.as_str(),
            _ => legacy_address.as_str(),
        };
        let binding = match selection {
            "legacy" => format!(
                "\n[profile.default.wallets.legacy]\naddress = '{legacy_address}'\nowner_public_key = '{owner}'\n"
            ),
            "native" => format!(
                "\n[profile.default.wallets.trading]\nkind = 'native'\naccount_nonce = 7\naddress = '{native_address}'\nowner_public_key = '{owner}'\n"
            ),
            "saved-name" => format!(
                "\n[profile.default.wallets.alice]\naddress = '{legacy_address}'\nowner_public_key = '{}'\n\n[profile.default.contracts]\nalice = '{}'\n",
                thruput_encoding::encode(&[0xcd; 32]),
                thruput_encoding::encode(&[0xef; 32])
            ),
            "alias" => format!("\n[profile.default.contracts]\nalice = '{legacy_address}'\n"),
            _ => String::new(),
        };
        // Explicit recipients use public metadata without constructing a signer.
        make_local_keypair_unavailable(&config_home, key_name);
        let faucet_wallet = advertised_dev_wallet_address(0);
        let manifest_path = write_runtime_manifest(&project, 0, &faucet_wallet, None);
        let (rpc_url, rpc_server) =
            spawn_balance_then_chain_tip_rpc(&faucet_wallet, "100000000000", 42);
        std::fs::write(project.join("thruput.toml"), format!("default_profile = 'default'\n[profile.default]\nrpc_url = '{rpc_url}'\nruntime_manifest = '{}'\n{binding}", manifest_path.file_name().unwrap().to_string_lossy())).unwrap();
        let output = thruput()
            .args(["--json", "airdrop", key_name, "--amount", "1", "--dry-run"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{selection}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        rpc_server.join().unwrap();
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["data"]["recipient"], key_name);
        assert_eq!(report["data"]["recipientWallet"], expected, "{selection}");
        assert_eq!(report["data"]["submitted"], false);
        assert_eq!(std::fs::read(&global_config).unwrap(), before);
    }
}

#[test]
fn airdrop_default_rejects_missing_wallet_binding_even_when_key_exists() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config");
    std::fs::create_dir_all(&project).unwrap();
    generate_key_public_key(&config_home, "alice");
    std::fs::write(project.join("thruput.toml"), "default_profile = 'default'\n[profile.default]\ndefault_wallet = 'alice'\nrpc_url = 'http://127.0.0.1:1'\n").unwrap();
    let output = thruput()
        .args(["--json", "airdrop", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(report["error"]["code"], "unknown_address_target");
}

#[test]
fn airdrop_explicit_key_rejects_ambiguous_or_invalid_saved_accounts_before_rpc() {
    for invalid_binding in [false, true] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config_home = tmp.path().join("config");
        std::fs::create_dir_all(&project).unwrap();
        let owner = generate_key_public_key(&config_home, "alice");
        let binding = if invalid_binding {
            format!(
                "[profile.default.wallets.alice]\nkind = 'native'\naccount_nonce = 0\naddress = '{}'\nowner_public_key = '{owner}'\n",
                thruput_encoding::encode(&[0xab; 32])
            )
        } else {
            format!(
                "[profile.default.wallets.primary]\naddress = '{}'\nowner_public_key = '{owner}'\n[profile.default.wallets.trading]\naddress = '{}'\nowner_public_key = '{owner}'\n",
                thruput_encoding::encode(&[0xab; 32]),
                thruput_encoding::encode(&[0xcd; 32])
            )
        };
        let config = format!(
            "default_profile = 'default'\n[profile.default]\nrpc_url = 'http://127.0.0.1:1'\n{binding}"
        );
        std::fs::write(project.join("thruput.toml"), &config).unwrap();
        let output = thruput()
            .args(["--json", "airdrop", "alice", "--dry-run"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            report["error"]["code"],
            if invalid_binding {
                "invalid_wallet_binding"
            } else {
                "saved_wallet_ambiguous"
            }
        );
        assert_eq!(
            std::fs::read_to_string(project.join("thruput.toml")).unwrap(),
            config
        );
    }
}

#[test]
fn airdrop_json_rejects_invalid_native_token_amount_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let faucet_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(&project, 0, &faucet_wallet, None);
    let recipient_wallet = thruput_encoding::encode(&[0xab; 32]);
    let recipient_owner = thruput_encoding::encode(&[0xcd; 32]);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\nruntime_manifest = \"{}\"\n\n[profile.default.wallets.alice]\naddress = \"{recipient_wallet}\"\nowner_public_key = \"{recipient_owner}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = thruput()
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
    assert_eq!(stderr["error"]["code"], "invalid_native_token_amount");
    assert_eq!(stderr["error"]["details"]["input"], "1.00000001");
    assert!(
        stderr["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("seven decimal")
    );
}

#[test]
fn local_airdrop_json_warns_when_projected_faucet_balance_is_below_ten_tokens() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let faucet = advertised_dev_wallet_address(0);
    let recipient = thruput_encoding::encode(&[0xab; 32]);
    let manifest_path = write_runtime_manifest(&project, 0, &faucet, None);
    let (rpc_url, rpc_server) = spawn_local_airdrop_rpc(&faucet, &recipient);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nnetwork = \"local\"\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "airdrop", &recipient, "--force"])
        .current_dir(&project)
        .env("THRUPUT_TEST_RECEIPT_POLL_ATTEMPTS", "1")
        .env("THRUPUT_TEST_RECEIPT_POLL_INTERVAL_MS", "0")
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["warnings"][0]["level"], "warn");
    assert_eq!(payload["warnings"][0]["code"], "low_local_faucet_balance");
    assert_eq!(payload["data"]["schemaVersion"], 6);
}

#[test]
fn hosted_airdrop_uses_request_airdrop_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    let cache_home = tmp.path().join("cache-home");
    std::fs::create_dir_all(&project).unwrap();
    let recipient = thruput_encoding::encode(&[0xaa; 32]);
    let (rpc_url, rpc_server) = spawn_airdrop_rpc(&recipient);
    let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    add_faucet_to_default_profile(&project, &faucet_url);

    let output = thruput()
        .args(["--json", "airdrop", "alice", "--force"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_CACHE_HOME", &cache_home)
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
    assert_eq!(stdout["data"]["mode"], "remote");
    assert_eq!(stdout["data"]["schemaVersion"], 6);
    assert_eq!(stdout["data"]["recipientWallet"], recipient);
    assert_eq!(stdout["data"]["rawAmount"], "25000000");
    assert_eq!(
        stdout["data"]["txHash"],
        thruput_encoding::encode(&[0x22; 32])
    );
}

#[test]
fn hosted_airdrop_pending_only_polls_when_a_hash_is_known() {
    for (has_hash, json_output) in [(true, true), (false, true), (false, false)] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config_home = tmp.path().join("config-home");
        let cache_home = tmp.path().join("cache-home");
        std::fs::create_dir_all(&project).unwrap();
        let recipient = thruput_encoding::encode(&[0xaa; 32]);
        let (rpc_url, rpc_server) = spawn_airdrop_rpc_outcome(&recipient, Some(has_hash));
        let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
        write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
        add_faucet_to_default_profile(&project, &faucet_url);
        let mut command = thruput();
        if json_output {
            command.arg("--json");
        }
        let output = command
            .args(["airdrop", "alice", "--force"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_CACHE_HOME", &cache_home)
            .output()
            .unwrap();
        descriptor_server.join().unwrap();
        rpc_server.join().unwrap();
        assert_eq!(
            output.status.success(),
            has_hash,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !json_output {
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("Retained attempt"), "{stderr}");
            assert!(stderr.contains("Retry after"), "{stderr}");
            assert!(
                stderr.contains("same command with the same amount"),
                "{stderr}"
            );
        } else if has_hash {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["data"]["execution"]["result"], "return");
        } else {
            let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(
                report["error"]["details"]["result"]["attemptStatus"],
                "pending"
            );
        }
    }
}

#[test]
fn hosted_airdrop_unknown_outcomes_retain_attempt_path_and_raw_response() {
    for nested_execution in [false, true] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config_home = tmp.path().join("config-home");
        let cache_home = tmp.path().join("cache-home");
        std::fs::create_dir_all(&project).unwrap();
        let recipient = thruput_encoding::encode(&[0xaa; 32]);
        let response = if nested_execution {
            serde_json::json!({"attemptStatus":"succeeded","recipient":recipient,"rawAmount":"25000000",
                "transactionHash":thruput_encoding::encode(&[0x22;32]),"blockHeight":"42",
                "execution":{"result":"futureExecution","detail":"retained"}})
        } else {
            serde_json::json!({"attemptStatus":"futureOutcome","detail":"retained"})
        };
        let (rpc_url, server) =
            spawn_airdrop_rpc_response(&recipient, None, Some(response.clone()));
        let (faucet_url, descriptor) = spawn_network_descriptor(&rpc_url);
        write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
        add_faucet_to_default_profile(&project, &faucet_url);
        let output = thruput()
            .args(["--json", "airdrop", "alice", "--force"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_CACHE_HOME", &cache_home)
            .output()
            .unwrap();
        descriptor.join().unwrap();
        server.join().unwrap();
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        let details = &report["error"]["details"];
        assert_eq!(details["result"], response);
        let path = details["attemptPath"].as_str().unwrap();
        assert!(std::path::Path::new(path).is_file(), "{report}");
    }
}

#[test]
fn native_wallet_airdrop_uses_saved_default_on_hosted_faucet() {
    let (tmp, project, config) = empty_wallet_fixture();
    let output = create_empty_wallet(&project, &config, &[]);
    assert!(output.status.success());
    let created: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let recipient = created["data"]["wallet"].as_str().unwrap();
    let (rpc_url, rpc_server) = spawn_airdrop_rpc(recipient);
    let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
    let path = project.join("thruput.toml");
    let mut doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["profile"]["default"]["rpc_url"] = toml::Value::String(rpc_url);
    doc["profile"]["default"]["faucet_url"] = toml::Value::String(faucet_url);
    std::fs::write(&path, toml::to_string(&doc).unwrap()).unwrap();
    let output = thruput()
        .args(["--json", "airdrop", "--force"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_CACHE_HOME", tmp.path().join("cache"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    descriptor_server.join().unwrap();
    rpc_server.join().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data"]["recipientWallet"], recipient);
    assert_eq!(report["data"]["mode"], "remote");
}

#[test]
fn transaction_commands_require_force_before_submission_or_attempt_state() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    let cache_home = tmp.path().join("cache-home");
    std::fs::create_dir_all(&project).unwrap();
    let rpc_url = "http://127.0.0.1:9";
    write_project_config_with_alice_wallet(&project, rpc_url, &config_home);
    add_faucet_to_default_profile(&project, "http://127.0.0.1:8");
    let artifact = write_opaque_rv32_artifact(tmp.path());
    let contract = thruput_encoding::encode(&[0x11; 32]);
    let recipient = thruput_encoding::encode(&[0x22; 32]);

    let invocations = [
        (
            vec![
                "deploy".to_owned(),
                "--artifact".to_owned(),
                artifact.display().to_string(),
            ],
            "deploy a contract",
        ),
        (
            vec![
                "call".to_owned(),
                contract,
                "0x0123456789abcdef".to_owned(),
                "0x".to_owned(),
            ],
            "call a contract",
        ),
        (
            vec!["transfer".to_owned(), recipient, "1".to_owned()],
            "transfer native tokens",
        ),
        (
            vec!["airdrop".to_owned(), "alice".to_owned()],
            "request an airdrop",
        ),
        (
            vec![
                "wallet".to_owned(),
                "create".to_owned(),
                "--key".to_owned(),
                "alice".to_owned(),
                "--name".to_owned(),
                "new-wallet".to_owned(),
            ],
            "create a wallet",
        ),
    ];

    for (args, action) in invocations {
        let output = thruput()
            .arg("--json")
            .args(args)
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_CACHE_HOME", &cache_home)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{action} unexpectedly succeeded");
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            error["error"]["code"], "confirmation_required",
            "{action}: {error}"
        );
        assert_eq!(error["error"]["details"]["action"], action);
        assert_eq!(error["error"]["details"]["requiredFlag"], "--yes");
    }

    assert!(!cache_home.join("thruput/airdrop-attempts").exists());
    assert!(!cache_home.join("thruput/wallet-create-attempts").exists());
}

#[test]
fn funding_commands_refuse_without_force_before_reading_the_profile() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    write_project_config_with_alice_wallet(&project, "http://127.0.0.1:9", &config_home);
    let recipient = thruput_encoding::encode(&[0x22; 32]);

    let invocations = [
        (vec!["airdrop".to_owned(), recipient], "request an airdrop"),
        (
            vec![
                "wallet".to_owned(),
                "create".to_owned(),
                "--key".to_owned(),
                "alice".to_owned(),
                "--name".to_owned(),
                "new-wallet".to_owned(),
            ],
            "create a wallet",
        ),
    ];

    for (args, action) in invocations {
        // The preflight runs before profile resolution, so `--force` is the first error.
        let output = thruput()
            .arg("--json")
            .args(&args)
            .args(["--profile", "missing"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{action} unexpectedly succeeded");
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            error["error"]["code"], "confirmation_required",
            "{action}: {error}"
        );
        assert_eq!(error["error"]["details"]["action"], action);

        let forced = thruput()
            .arg("--json")
            .args(&args)
            .args(["--profile", "missing", "--force"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(!forced.status.success(), "{action} unexpectedly succeeded");
        let error: serde_json::Value = serde_json::from_slice(&forced.stderr).unwrap();
        assert_eq!(
            error["error"]["code"], "profile_not_found",
            "{action}: {error}"
        );
    }
}

#[test]
fn initialized_futurenet_profile_discovers_signing_identity_for_all_transaction_commands() {
    use std::fmt::Write as _;

    for command in ["transfer", "call", "deploy"] {
        let tmp = TempDir::new().unwrap();
        let config_home = tmp.path().join("config-home");
        let owner = generate_key_public_key(&config_home, "alice");
        let init = thruput()
            .args(["init", "--non-interactive", "--json"])
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        for args in [
            vec!["config", "use", "futurenet", "--global"],
            vec!["new", "my-counter", "--template", "counter"],
        ] {
            let output = thruput()
                .args(args)
                .current_dir(tmp.path())
                .env("XDG_CONFIG_HOME", &config_home)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let project = tmp.path().join("my-counter");
        let path = config_home.join("thruput/config.toml");
        let mut config = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(config["profile"]["futurenet"].get("network_hash").is_none());
        let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
        let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
        config["profile"]["futurenet"]["rpc_url"] = toml_edit::value(&rpc_url);
        config["profile"]["futurenet"]["faucet_url"] = toml_edit::value(&faucet_url);
        std::fs::write(&path, config.to_string()).unwrap();
        let project_path = project.join("thruput.toml");
        let mut project_config = std::fs::read_to_string(&project_path).unwrap();
        write!(
            project_config,
            "[profile.futurenet.wallets.alice]\naddress = \"{}\"\nowner_public_key = \"{owner}\"\n",
            thruput_encoding::encode(&[0xaa; 32])
        )
        .unwrap();
        std::fs::write(&project_path, project_config).unwrap();
        let target = thruput_encoding::encode(&[0xbc; 32]);
        let artifact = write_opaque_rv32_artifact(&project);
        let mut invocation = thruput();
        invocation
            .args([command, "--json", "--dry-run"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config_home);
        match command {
            "transfer" => {
                invocation.args([&target, "0.01"]);
            }
            "call" => {
                invocation.args([&target, "0x0123456789abcdef", "0x0102"]);
            }
            "deploy" => {
                invocation.arg("--artifact").arg(&artifact);
            }
            _ => unreachable!(),
        }
        let output = invocation.output().unwrap();
        descriptor_server.join().unwrap();
        rpc_server.join().unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["data"]["rpcUrl"], rpc_url);
        if command == "transfer" {
            assert_eq!(result["data"]["profile"], "futurenet");
        }
        let field = if command == "transfer" || command == "call" {
            "signedTransaction"
        } else {
            "signedTx"
        };
        assert!(result["data"][field].as_str().is_some());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), config.to_string());
    }
}

#[test]
fn hosted_signing_without_a_pin_rejects_disagreeing_descriptor_and_rpc() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let (rpc_url, rpc_server) = spawn_network_info_rpc(&thruput_encoding::encode(&[0x42; 32]));
    let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
    write_project_config_with_alice_wallet(tmp.path(), &rpc_url, &config_home);
    add_faucet_to_default_profile(tmp.path(), &faucet_url);
    let path = tmp.path().join("thruput.toml");
    let mut config = std::fs::read_to_string(&path)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    config["profile"]["default"]
        .as_table_mut()
        .unwrap()
        .remove("network_hash");
    std::fs::write(&path, config.to_string()).unwrap();
    let output = thruput()
        .args([
            "transfer",
            &thruput_encoding::encode(&[0xbc; 32]),
            "1",
            "--dry-run",
            "--json",
        ])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    descriptor_server.join().unwrap();
    rpc_server.join().unwrap();
    assert!(!output.status.success() && output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "network_descriptor_mismatch");
    assert_eq!(error["error"]["details"]["field"], "networkHash");
}

#[test]
fn hosted_signing_rejects_profile_network_hash_mismatch() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let live_network_hash = thruput_encoding::encode(&[0x42; 32]);
    let (rpc_url, rpc_server) = spawn_network_info_rpc(&live_network_hash);
    let (faucet_url, descriptor_server) =
        spawn_network_descriptor_with_hash(&rpc_url, &live_network_hash);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    add_faucet_to_default_profile(&project, &faucet_url);
    let recipient = thruput_encoding::encode(&[0xbc; 32]);

    let output = thruput()
        .args(["--json", "transfer", &recipient, "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    descriptor_server.join().unwrap();
    rpc_server.join().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "network_descriptor_mismatch");
    assert_eq!(stderr["error"]["details"]["field"], "networkHash");
}

#[test]
fn transfer_dry_run_signs_with_an_mldsa65_key_and_attaches_the_claim_key() {
    use thruput_signing::Signer as _;

    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = project.join("fixture-config");
    std::fs::create_dir_all(&project).unwrap();
    let generate = thruput()
        .args(["--json", "keys", "generate", "pq", "--scheme", "mldsa65"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(generate.status.success());
    let keyfile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_home.join("thruput/keys/pq.json")).unwrap())
            .unwrap();
    let seed: [u8; 32] = hex::decode(keyfile["seed_hex"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let signer = thruput_signing::MlDsa65Signer::from_seed(&seed);
    let account = thruput_signing::PubkeyAccount::new(signer.public_key(), 0);
    let sender = thruput_encoding::encode(account.address().as_ref());
    let recipient = thruput_encoding::encode(&[0xab; 32]);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\
             network_hash = \"{TEST_NETWORK_HASH}\"\ndefault_key = \"pq\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
    let data = &stdout["data"];
    assert_eq!(data["sender"], sender);
    assert_eq!(data["signerScheme"], "mldsa65");
    assert_eq!(
        data["signerPublicKey"],
        thruput_encoding::encode(signer.public_key_bytes())
    );

    let signed_tx = decode_signed_tx(data["signedTransaction"].as_str().unwrap());
    assert_eq!(signed_tx.tx.auth_claim, account.auth_claim());
    assert_eq!(signed_tx.pubkey_claim_key, Some(signer.public_key()));
    let [signature] = signed_tx.signatures.0.as_slice() else {
        panic!("pubkey claims carry exactly one signature");
    };
    assert_eq!(signature.public_key_id, 0);
    let thruput_xdr::types::Signature::MlDsa65 { mldsa65 } = &signature.signature else {
        panic!("expected an ML-DSA-65 signature");
    };
    let network_hash = thruput_xdr::types::NetworkHash::new(thruput_xdr::types::SHA256Hash::new(
        thruput_encoding::decode_fixed::<32>(TEST_NETWORK_HASH).unwrap(),
    ));
    let preimage =
        thruput_domain_separator::transaction_signature_preimage(&network_hash, &signed_tx.tx)
            .unwrap();
    assert!(thruput_signing::verify_mldsa65(
        signer.public_key_bytes(),
        &preimage,
        mldsa65
    ));
    assert_eq!(
        data["txHash"],
        thruput_encoding::encode(
            thruput_signing::transaction_hash(&signed_tx.tx)
                .unwrap()
                .as_ref()
        )
    );
}

#[test]
fn transfer_dry_run_json_outputs_signed_wallet_wrapped_native_token_transfer() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sender = advertised_dev_wallet_address(0);
    let recipient = thruput_encoding::encode(&[0xab; 32]);
    let manifest_path = write_runtime_manifest(&project, 0, &sender, None);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n\n[profile.default.wallets.alice]\naddress = \"{recipient}\"\nowner_public_key = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy(),
            thruput_encoding::encode(&[0xcd; 32]),
        ),
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "transfer", "alice", "10.5000000", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", project.join("fixture-config"))
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
    assert_eq!(data["schemaVersion"], 5);
    assert_eq!(data["profile"], "default");
    assert_eq!(data["rpcUrl"], rpc_url);
    assert_eq!(data["dryRun"], true);
    assert_eq!(data["submitted"], false);
    assert_eq!(data["sender"], sender);
    assert_eq!(data["senderWallet"], serde_json::Value::Null);
    assert_eq!(data["signerScheme"], "ed25519");
    assert_eq!(
        data["signerPublicKey"],
        thruput_encoding::encode(
            &thruput_signing::derive_default_dev_account_signer(0).verifying_key_bytes()
        )
    );
    assert_eq!(data["recipientInput"], "alice");
    assert_eq!(data["recipient"], recipient);
    assert_eq!(
        data["tokenAddress"],
        thruput_encoding::encode(thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN.as_ref())
    );
    assert_eq!(data["amount"], "10.5");
    assert_eq!(data["rawAmount"], "105000000");
    assert_eq!(data["expiryHeight"], "1042");
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
        thruput_encoding::encode(signed_tx.tx.invocation.invoked_address.as_ref()),
        sender
    );
    assert_eq!(
        signed_tx_entrypoint_from_decoded(&signed_tx),
        "0xc3edb11fdd6d5d58"
    );
    assert_eq!(*signed_tx.tx.uniquifier.block_expiry_height, 1042);
    assert_eq!(
        data["txHash"],
        thruput_encoding::encode(
            thruput_signing::transaction_hash(&signed_tx.tx)
                .unwrap()
                .as_ref()
        )
    );
    let (target, entrypoint, target_calldata) = decode_wallet_action(&signed_tx);
    assert_eq!(
        target,
        thruput_encoding::encode(thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN.as_ref())
    );
    assert_eq!(entrypoint, "0x63285b3523f9dc3f");
    let (calldata_recipient, raw_amount): (thruput_xdr::types::ContractAddress, u128) =
        xdrs::cxs::decode(&target_calldata).unwrap();
    assert_eq!(
        thruput_encoding::encode(calldata_recipient.as_ref()),
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
    let recipient = thruput_encoding::encode(&[0xab; 32]);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = thruput()
        .args(["transfer", &recipient, "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", project.join("fixture-config"))
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
    let public_key = thruput_encoding::encode(
        &thruput_signing::derive_default_dev_account_signer(0).verifying_key_bytes(),
    );
    assert!(stderr.contains(&format!("sender: {sender}")));
    assert!(stderr.contains("signer scheme: ed25519"));
    assert!(stderr.contains(&format!("signer public key: {public_key}")));
    assert!(stderr.contains("signed transaction can be submitted until its expiry height"));
    assert!(stderr.contains("submitted: no (--dry-run)"));
    assert!(!stderr.contains(thruput_signing::DEFAULT_DEV_ACCOUNT_SEED));
}

#[test]
fn transfer_dry_run_uses_hosted_entrypoint_but_protocol_native_token_constants() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    add_faucet_to_default_profile(&project, &faucet_url);
    let recipient = thruput_encoding::encode(&[0xbc; 32]);

    let output = thruput()
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
        "0x19846d750fad81d6"
    );
    assert_eq!(
        target,
        thruput_encoding::encode(thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN.as_ref())
    );
    assert_eq!(entrypoint, "0x63285b3523f9dc3f");
}

#[test]
fn native_wallet_transfer_discovers_unconfigured_hosted_network_identity() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config");
    let owner = generate_key_public_key(&config_home, "alice");
    let owner = thruput_signing::strkey::parse_public_key(&owner).unwrap();
    let account = thruput_signing::PubkeyAccount::from_public_key_bytes(owner, 0);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    let (faucet_url, descriptor_server) = spawn_network_descriptor(&rpc_url);
    std::fs::write(tmp.path().join("thruput.toml"), format!(
        "default_profile = 'devnet'\n[profile.devnet]\nnetwork = 'devnet'\nrpc_url = '{rpc_url}'\nfaucet_url = '{faucet_url}'\n"
    )).unwrap();
    let output = thruput()
        .args([
            "transfer",
            &thruput_encoding::encode(&[0xbc; 32]),
            "0.01",
            "--dry-run",
            "--json",
        ])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    rpc_server.join().unwrap();
    descriptor_server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["data"]["sender"],
        thruput_encoding::encode(account.address().as_ref())
    );
    assert_eq!(report["data"]["submitted"], false);
    assert!(report["data"]["signedTransaction"].as_str().is_some());
}

#[test]
fn transfer_dry_run_uses_legacy_wallet_entrypoint() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(&project).unwrap();
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);
    let recipient = thruput_encoding::encode(&[0xbc; 32]);

    let output = thruput()
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
    let recipient = thruput_encoding::encode(&[0xbc; 32]);
    std::fs::write(
        project.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n\n[profile.devnet]\nrpc_url = \"http://127.0.0.1:2\"\ndefault_key = \"dev-0\"\n",
    )
    .unwrap();

    let output = thruput()
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
        .env("XDG_CONFIG_HOME", tmp.path().join("fixture-config"))
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
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    for (recipient, amount, expected_code) in [
        (
            thruput_encoding::encode(&[0xab; 32]),
            "1.00000001",
            "invalid_native_token_amount",
        ),
        ("missing".to_owned(), "1", "unknown_address_target"),
    ] {
        let output = thruput()
            .args(["--json", "transfer", &recipient, amount, "--dry-run"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", project.join("fixture-config"))
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
    let recipient = thruput_encoding::encode(&[0xbc; 32]);

    std::fs::write(
        project.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"alice\"\n",
    )
    .unwrap();
    let missing = thruput()
        .args(["--json", "transfer", &recipient, "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let error: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(error["error"]["code"], "missing_network_hash");

    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"alice\"\n\n[profile.default.wallets.broken-wallet]\naddress = \"0x{}\"\nowner_public_key = \"0x00\"\n",
            "aa".repeat(32),
        ),
    )
    .unwrap();
    let invalid = thruput()
        .args(["--json", "transfer", &recipient, "1", "--dry-run"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(
        error["error"]["code"],
        "saved_wallet_owner_public_key_invalid"
    );
    assert_eq!(error["error"]["details"]["keyName"], "broken-wallet");

    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"alice\"\ndefault_wallet = \"broken-wallet\"\n\n[profile.default.wallets.broken-wallet]\naddress = \"0x{}\"\nowner_public_key = \"0x00\"\n",
            "aa".repeat(32),
        ),
    )
    .unwrap();
    let selected = thruput()
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
    let address = thruput_encoding::encode(&[0xab; 32]);
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nnetwork_hash = \"{TEST_NETWORK_HASH}\"\ndefault_key = \"alice\"\n\n[profile.default.wallets.wallet-a]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
    let primary = thruput_encoding::encode(&[0xa1; 32]);
    let trading = thruput_encoding::encode(&[0xb2; 32]);
    let recipient = thruput_encoding::encode(&[0xc3; 32]);
    let (rpc_url, rpc_server) = spawn_chain_tip_rpc(42);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nnetwork_hash = \"{TEST_NETWORK_HASH}\"\ndefault_key = \"alice\"\ndefault_wallet = \"primary\"\n\n[profile.default.wallets.primary]\naddress = \"{primary}\"\nowner_public_key = \"{owner}\"\n\n[profile.default.wallets.trading]\naddress = \"{trading}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
                execution: serde_json::json!({ "result": "return", "returnData": "0x" }),
                events: serde_json::json!([]),
                mismatched_hash: false,
            }),
        },
        None,
    );
    assert!(committed.output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&committed.output.stdout).unwrap();
    assert_eq!(stdout["data"]["senderWallet"], "primary");
    assert_eq!(
        stdout["data"]["sender"],
        thruput_encoding::encode(&[0xa1; 32])
    );
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
    assert_eq!(stdout["data"]["txHash"], unavailable.local_hash);
    assert_eq!(stdout["data"]["evidence"]["submission"], "uncertain");
}

#[test]
fn transfer_live_success_returns_matching_receipt_and_events() {
    let event = serde_json::json!({
        "address": thruput_encoding::encode(&[0x01; 32]),
        "topics": [format!("0x{}", "02".repeat(32))],
        "data": "0x0304",
    });
    let run = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                execution: serde_json::json!({ "result": "return", "returnData": "0x0506" }),
                events: serde_json::json!([event.clone()]),
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
    assert_eq!(data["blockHeight"], "43");
    assert_eq!(
        data["execution"],
        serde_json::json!({"result":"return","returnData":"0x0506"})
    );
    assert_eq!(data["gasUsed"], "1500");
    assert_eq!(data["events"], serde_json::json!([event]));
    assert_eq!(data["execution"]["returnData"], "0x0506");
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
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"not a URL\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();
    let recipient = thruput_encoding::encode(&[0xab; 32]);

    let output = thruput()
        .args(["--json", "transfer", &recipient, "1.5"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", project.join("fixture-config"))
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "profile_config_invalid");
}

#[test]
fn transfer_live_committed_failures_preserve_receipt_context() {
    for (execution, expected_code) in [
        (
            serde_json::json!({ "result": "abort", "abortData": "0x0304" }),
            "transaction_reverted",
        ),
        (
            serde_json::json!({ "result": "fault" }),
            "transaction_deterministic_error",
        ),
    ] {
        let event = serde_json::json!({
            "address": thruput_encoding::encode(&[0x09; 32]),
            "topics": [],
            "data": "0x0a0b",
        });
        let run = run_transfer_live_mock_with_hash(
            TransferRpcMock {
                submit: TransferSubmitMock::MatchingHash,
                poll: Some(TransferPollMock::Receipt {
                    execution: execution.clone(),
                    events: serde_json::json!([event.clone()]),
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
        assert_eq!(stdout["data"]["blockHeight"], "43");
        assert_eq!(stdout["data"]["events"], serde_json::json!([event]));
        assert_eq!(stdout["data"]["execution"], execution);
        if execution["result"] == "abort" {
            assert!(
                stdout["data"]["abortDecodeError"]
                    .as_str()
                    .is_some_and(|error| error.contains("selector"))
            );
        } else {
            assert!(stdout["data"].get("abortDecodeError").is_none());
        }
        for absent in [
            "abortException",
            "abortValue",
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
fn transfer_live_decodes_standard_exception_abort() {
    let abort_data = xdrs::cxs::encode(&(
        <thruput_erc20_if::ERC20Error as thruput_sdk::exn::Exn>::SELECTOR,
        thruput_erc20_if::ERC20Error::TooLarge,
    ))
    .expect("bug: ERC20 exception should encode");
    let abort_hex = format!("0x{}", hex::encode(&abort_data));

    let run = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                execution: serde_json::json!({ "result": "abort", "abortData": abort_hex }),
                events: serde_json::json!([]),
                mismatched_hash: false,
            }),
        },
        true,
    );

    assert!(!run.output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["data"]["execution"]["abortData"], abort_hex);
    assert_eq!(stdout["data"]["abortException"], "ERC20Error");
    assert_eq!(stdout["data"]["abortExceptionCrate"], "thruput-erc20-if");
    assert_eq!(stdout["data"]["abortExceptionVersion"], "0.1");
    assert_eq!(stdout["data"]["abortValue"], "TooLarge");
    assert!(stdout["data"].get("abortDecodeError").is_none());
}

#[test]
fn transfer_live_unknown_status_preserves_raw_evidence() {
    let status = serde_json::json!({"status":"futureStatus","reason":"expired"});
    let run = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Status(status.clone())),
        },
        true,
    );
    assert!(!run.output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
    assert_eq!(report["error"]["code"], "native_rpc_failure");
    assert_eq!(report["data"]["txHash"], run.local_hash);
    assert_eq!(report["data"]["profile"], "default");
    assert_eq!(report["data"]["sender"], advertised_dev_wallet_address(0));
    assert_eq!(
        report["data"]["recipient"],
        thruput_encoding::encode(&[0xab; 32])
    );
    assert_eq!(report["data"]["rawAmount"], "15000000");
    assert_eq!(report["data"]["submitted"], true);
    assert_eq!(report["data"]["evidence"]["submission"], "acknowledged");
    assert_eq!(report["data"]["evidence"]["cause"]["result"], status);
}

#[test]
fn transfer_live_poll_failures_preserve_native_evidence() {
    for poll in [
        TransferPollMock::Status(serde_json::json!({"status":"unknown"})),
        TransferPollMock::ReceiptError(-32050),
    ] {
        let run = run_transfer_live_mock_with_hash(
            TransferRpcMock {
                submit: TransferSubmitMock::MatchingHash,
                poll: Some(poll),
            },
            true,
        );
        assert!(!run.output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
        assert_eq!(report["error"]["code"], "native_rpc_failure");
        assert_eq!(report["data"]["txHash"], run.local_hash);
        assert_eq!(report["data"]["evidence"]["submission"], "acknowledged");
        assert!(report["data"]["evidence"]["cause"].is_object());
    }
}

#[test]
fn transfer_live_lost_submission_response_retains_hash_without_retry_or_poll() {
    let run = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::DropResponse,
            poll: None,
        },
        true,
    );
    assert!(!run.output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
    assert_eq!(report["error"]["code"], "native_rpc_failure");
    assert_eq!(report["data"]["txHash"], run.local_hash);
    assert_eq!(report["data"]["signedTransaction"], run.signed_transaction);
    let signed = decode_signed_tx(report["data"]["signedTransaction"].as_str().unwrap());
    assert_eq!(
        thruput_encoding::encode(
            thruput_signing::transaction_hash(&signed.tx)
                .unwrap()
                .as_ref()
        ),
        run.local_hash
    );
    assert!(!signed.signatures.is_empty());
    assert_eq!(report["data"]["evidence"]["submission"], "uncertain");
    assert!(report["data"]["evidence"]["cause"]["transportError"].is_string());
}

#[test]
fn transfer_live_preserves_send_errors_without_inferring_admission() {
    for code in [
        -32602, -32601, -32000, -32001, -32050, -32051, -32052, -32053, -32123,
    ] {
        let run = run_transfer_live_mock_with_hash(
            TransferRpcMock {
                submit: TransferSubmitMock::Error(code),
                poll: None,
            },
            true,
        );
        assert!(!run.output.status.success());
        assert!(run.output.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
        assert_eq!(report["error"]["code"], "native_rpc_failure");
        assert_eq!(report["data"]["txHash"], run.local_hash);
        let evidence = &report["data"]["evidence"];
        assert_eq!(evidence["cause"]["rpcError"]["code"], code);
        assert_eq!(evidence["submission"], "uncertain");
        assert!(evidence.get("admission").is_none());
    }
}

#[test]
fn transfer_live_hash_mismatches_never_report_commitment() {
    for (submit, poll, phase) in [
        (TransferSubmitMock::MismatchedHash, None, "uncertain"),
        (
            TransferSubmitMock::MatchingHash,
            Some(TransferPollMock::Receipt {
                execution: serde_json::json!({ "result": "return", "returnData": "0x" }),
                events: serde_json::json!([]),
                mismatched_hash: true,
            }),
            "acknowledged",
        ),
    ] {
        let run = run_transfer_live_mock_with_hash(TransferRpcMock { submit, poll }, true);
        assert!(!run.output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&run.output.stdout).unwrap();
        assert_eq!(report["data"]["txHash"], run.local_hash);
        assert_eq!(report["data"]["evidence"]["submission"], phase);
        assert!(report["data"].get("blockHeight").is_none());
        assert!(report["data"].get("execution").is_none());
    }
}

#[test]
fn transfer_live_human_submission_error_retains_hash() {
    let run = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::Error(-32051),
            poll: None,
        },
        false,
    );
    assert!(!run.output.status.success());
    assert!(run.output.stdout.is_empty());
    let stderr = String::from_utf8(run.output.stderr).unwrap();
    assert!(stderr.contains(&run.local_hash), "{stderr}");
    assert!(stderr.contains("mock send error"), "{stderr}");
}

#[test]
fn transfer_live_human_error_does_not_echo_rpc_query_credentials() {
    let run = run_transfer_live_mock_with_rpc_override(
        TransferRpcMock {
            submit: TransferSubmitMock::Error(-32051),
            poll: None,
        },
        false,
        "?key=x&network=test",
    );
    assert!(!run.output.status.success());
    let stderr = String::from_utf8(run.output.stderr).unwrap();
    assert!(!stderr.contains("key=x"), "{stderr}");
}

#[test]
fn transfer_live_human_success_and_committed_failure_stay_on_stderr() {
    let success = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                execution: serde_json::json!({ "result": "return", "returnData": "0x" }),
                events: serde_json::json!([]),
                mismatched_hash: false,
            }),
        },
        false,
    );
    assert!(success.output.status.success());
    assert!(success.output.stdout.is_empty());
    let stderr = String::from_utf8(success.output.stderr).unwrap();
    assert!(stderr.contains("Transferred 1.5 native tokens."));
    assert!(stderr.contains(&format!(
        "recipient: {}",
        thruput_encoding::encode(&[0xab; 32])
    )));
    assert!(stderr.contains(&format!("tx: {}", success.local_hash)));
    assert!(stderr.contains("height: 43"));

    let failure = run_transfer_live_mock_with_hash(
        TransferRpcMock {
            submit: TransferSubmitMock::MatchingHash,
            poll: Some(TransferPollMock::Receipt {
                execution: serde_json::json!({ "result": "abort", "abortData": "0x0102" }),
                events: serde_json::json!([]),
                mismatched_hash: false,
            }),
        },
        false,
    );
    assert!(!failure.output.status.success());
    assert!(failure.output.stdout.is_empty());
    let stderr = String::from_utf8(failure.output.stderr).unwrap();
    assert!(stderr.contains("Transfer aborted"));
    assert!(stderr.contains(&format!("tx: {}", failure.local_hash)));
    assert!(stderr.contains("height: 43"));
    assert!(stderr.contains("abort data: 0x0102"));
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

#[test]
fn transfer_live_scheduler_failures_preserve_json_outcome() {
    for tag in ["retry", "dropped"] {
        let status =
            serde_json::json!({ "status": tag, "error": { "source": "scheduler", "code": 1 } });
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
        assert_eq!(stdout["error"]["code"], "native_rpc_failure");
        assert_eq!(stdout["data"]["evidence"]["submission"], "acknowledged");
        assert_eq!(stdout["data"]["evidence"]["lastStatus"], status);
    }
}

fn assert_transfer_live_common(
    data: &serde_json::Value,
    local_hash: &str,
    sender_wallet: Option<&str>,
) {
    assert_eq!(data["schemaVersion"], 5);
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
            .is_some_and(|value| thruput_encoding::decode_fixed::<32>(value).is_ok())
    );
    assert_eq!(
        data["recipientInput"],
        thruput_encoding::encode(&[0xab; 32])
    );
    assert_eq!(data["recipient"], thruput_encoding::encode(&[0xab; 32]));
    assert_eq!(
        data["tokenAddress"],
        thruput_encoding::encode(thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN.as_ref())
    );
    assert_eq!(data["amount"], "1.5");
    assert_eq!(data["rawAmount"], "15000000");
    assert_eq!(data["txHash"], local_hash);
}

#[test]
fn view_json_returns_raw_output() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
    let entrypoint = "0x0102030405060708";
    let calldata = "0xdeadbeef";
    let (rpc_url, rpc_server) = spawn_view_rpc(&address, entrypoint, calldata);
    write_project_config_with_dummy_alice_wallet(&project, &rpc_url);

    let output = thruput()
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
    assert_eq!(stdout["data"]["schemaVersion"], 4);
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["entrypoint"], entrypoint);
    assert_eq!(stdout["data"]["calldata"], calldata);
    assert_eq!(stdout["data"]["caller"], serde_json::Value::Null);
    assert_eq!(stdout["data"]["stateBlockHeight"], "12");
    assert_eq!(stdout["data"]["output"]["result"], "return");
    assert_eq!(stdout["data"]["output"]["returnData"], "0x0102");
    assert_eq!(stdout["data"]["gasUsed"], "4321");
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

    let address = thruput_encoding::encode(&[0x11; 32]);
    let caller = thruput_encoding::encode(&[0x22; 32]);
    let entrypoint = "0x0102030405060708";
    let calldata = "0x";
    let (rpc_url, rpc_server) = spawn_view_rpc_with_caller(&address, entrypoint, calldata, &caller);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.contracts]\ncounter = \"{address}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["caller"], caller);
}

#[test]
fn view_json_resolves_named_callers_without_private_key_bytes() {
    #[derive(Debug)]
    enum Caller {
        UnsavedKey,
        NativeWallet,
        LegacyWallet,
        KeyWithNativeWallet,
        ContractAlias,
    }
    for selection in [
        Caller::UnsavedKey,
        Caller::NativeWallet,
        Caller::LegacyWallet,
        Caller::KeyWithNativeWallet,
        Caller::ContractAlias,
    ] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let config = tmp.path().join("config");
        std::fs::create_dir_all(&project).unwrap();
        let owner = generate_key_public_key(&config, "owner");
        make_local_keypair_unavailable(&config, "owner");
        let public_key = thruput_signing::strkey::parse_public_key(&owner).unwrap();
        let native_address = |nonce| {
            let account = thruput_signing::PubkeyAccount::from_public_key_bytes(public_key, nonce);
            thruput_encoding::encode(account.address().as_ref())
        };
        let alias_address = thruput_encoding::encode(&[0x33; 32]);
        let native_kind = "kind = 'native'\naccount_nonce = 7\n";
        let (from, caller, binding_kind, alias_name) = match selection {
            Caller::UnsavedKey => ("owner", native_address(0), None, None),
            Caller::NativeWallet => (
                "primary",
                native_address(7),
                Some(native_kind),
                Some("primary"),
            ),
            Caller::LegacyWallet => (
                "primary",
                thruput_encoding::encode(&[0x22; 32]),
                Some(""),
                Some("primary"),
            ),
            Caller::KeyWithNativeWallet => ("owner", native_address(7), Some(native_kind), None),
            Caller::ContractAlias => ("viewer", alias_address.clone(), None, Some("viewer")),
        };
        let binding = if let Some(kind) = binding_kind {
            format!(
                "[profile.default.wallets.primary]\n{kind}address = '{caller}'\nowner_public_key = '{owner}'\n"
            )
        } else {
            String::new()
        };
        let alias = if let Some(name) = alias_name {
            format!("[profile.default.contracts]\n{name} = '{alias_address}'\n")
        } else {
            String::new()
        };
        let address = thruput_encoding::encode(&[0x11; 32]);
        let entrypoint = "0x0102030405060708";
        let (rpc_url, server) = spawn_view_rpc_with_caller(&address, entrypoint, "0x", &caller);
        let project_config = format!(
            "default_profile = 'default'\n[profile.default]\n\
             rpc_url = '{rpc_url}'\n{binding}{alias}"
        );
        std::fs::write(project.join("thruput.toml"), project_config).unwrap();
        let output = thruput()
            .args(["--json", "view", &address, entrypoint, "--from", from])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", &config)
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{selection:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["data"]["caller"], caller);
    }
}

#[test]
fn view_json_omitted_caller_does_not_use_the_default_wallet() {
    let (_tmp, project, config) = empty_wallet_fixture();
    assert!(create_empty_wallet(&project, &config, &[]).status.success());
    make_local_keypair_unavailable(&config, "owner");
    let address = thruput_encoding::encode(&[0x11; 32]);
    let entrypoint = "0x0102030405060708";
    let (rpc_url, server) = spawn_view_rpc(&address, entrypoint, "0x");
    let profile = project.join("thruput.toml");
    let contents = std::fs::read_to_string(&profile).unwrap();
    std::fs::write(&profile, contents.replace("http://127.0.0.1:1", &rpc_url)).unwrap();
    let output = thruput()
        .args(["--json", "view", &address, entrypoint])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data"]["caller"], serde_json::Value::Null);
}

#[test]
fn view_json_uses_manifest_dev_account_as_caller_without_dev_rpc() {
    for collision in ["none", "alias", "wallet", "both"] {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        let address = thruput_encoding::encode(&[0x11; 32]);
        let caller = advertised_dev_wallet_address(0);
        let entrypoint = "0x0102030405060708";
        let manifest_path = write_runtime_manifest(tmp.path(), 0, &caller, None);
        let shadow_address = thruput_encoding::encode(&[0x22; 32]);
        let mut bindings = String::new();
        if matches!(collision, "alias" | "both") {
            write!(
                bindings,
                "\n[profile.default.contracts]\ndev-0 = '{shadow_address}'\n"
            )
            .unwrap();
        }
        if matches!(collision, "wallet" | "both") {
            let owner = "33".repeat(32);
            write!(
                bindings,
                "\n[profile.default.wallets.dev-0]\naddress = '{shadow_address}'\n\
                 owner_public_key = '0x{owner}'\n"
            )
            .unwrap();
        }
        let expected_caller = if collision == "none" {
            &caller
        } else {
            &shadow_address
        };
        let (rpc_url, rpc_server) =
            spawn_view_rpc_with_caller(&address, entrypoint, "0x", expected_caller);
        let profile_config = format!(
            "default_profile = 'default'\n[profile.default]\n\
             rpc_url = '{rpc_url}'\nruntime_manifest = '{}'\n{bindings}",
            manifest_path.display()
        );
        std::fs::write(project.join("thruput.toml"), profile_config).unwrap();

        let output = thruput()
            .args(["--json", "view", &address, entrypoint, "--from", "dev-0"])
            .current_dir(&project)
            .env("XDG_CONFIG_HOME", tmp.path().join("fixture-config"))
            .env_remove("THRUPUT_RUNTIME_MANIFEST")
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
        assert_eq!(stdout["data"]["caller"], *expected_caller, "{collision}");
    }
}

#[test]
fn view_json_surfaces_abort_as_failed_command_with_result() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
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

    let output = thruput()
        .args(["--json", "view", &address, entrypoint])
        .current_dir(&project)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        !output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["data"]["output"]["result"], "abort");
    assert_eq!(stdout["data"]["output"]["abortData"], "0x0a0b");
    assert_eq!(stdout["data"]["returnValue"], serde_json::Value::Null);
    let decode_error = stdout["data"]["abortDecodeError"].as_str().unwrap();
    assert!(decode_error.contains("does not match"), "{decode_error}");
}

#[test]
fn view_json_surfaces_fault_as_failed_command_with_result() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
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

    let output = thruput()
        .args(["--json", "view", &address, entrypoint])
        .current_dir(&project)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        !output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["data"]["output"]["result"], "fault");
    assert_eq!(stdout["data"]["returnValue"], serde_json::Value::Null);
}

#[test]
fn view_human_prints_event_details() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
    let entrypoint = "0x0102030405060708";
    let topic = format!("0x{}", "22".repeat(32));
    let event_data = "0x1234";
    let (rpc_url, rpc_server) = spawn_view_rpc_with_events(
        &address,
        entrypoint,
        "0x",
        serde_json::json!([{
            "address": address,
            "topics": [topic],
            "data": event_data,
        }]),
    );
    write_project_config_with_dummy_alice_wallet(&project, &rpc_url);

    let output = thruput()
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
    assert!(stderr.contains("Simulation succeeded"));
    assert!(stderr.contains("Gas used: 4321"), "{stderr}");
    assert!(stderr.contains("Event 0"));
    assert!(stderr.contains(&format!("{address} topic0={topic} data={event_data}")));
}

#[test]
fn storage_json_reads_latest_item() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_storage_rpc(&address, &key);
    write_project_config_with_dummy_alice_wallet(&project, &rpc_url);

    let output = thruput()
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
    assert_eq!(stdout["data"]["schemaVersion"], 3);
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["key"], key);
    assert_eq!(stdout["data"]["stateBlockHeight"], "12");
    assert_eq!(stdout["data"]["item"]["kind"], "bytestring");
    assert_eq!(
        stdout["data"]["item"]["bytes"],
        "0x6c6f63616c2d6e6f64652d73746f72616765"
    );
}

#[test]
fn inspect_json_reports_absent_deployment_without_artifact_lookup() {
    let tmp = TempDir::new().unwrap();
    let address = thruput_encoding::encode(&[0x11; 32]);
    let response_address = address.clone();
    let server = rpc_mock()
        .method("getContractArtifact", |_| {
            panic!("absent deployment must not fetch an artifact")
        })
        .method("getContractDeployment", move |_| {
            Ok(serde_json::json!({
                "address": response_address,
                "stateBlockHeight": "12",
                "deployment": null,
            }))
        })
        .start();
    let expected_address = address.clone();
    let (rpc_url, server) = recorded_mock(server, move |script| {
        assert_eq!(
            script.expect("getContractDeployment")["address"],
            expected_address
        );
    });
    write_project_config_with_dummy_alice_wallet(tmp.path(), &rpc_url);
    let output = thruput()
        .args(["--json", "inspect", &address])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["command"], "inspect");
    assert_eq!(report["data"]["specStatus"], "notDeployed");
    assert_eq!(report["data"]["stateBlockHeight"], "12");
    assert!(report["data"]["spec"].is_null());
}

#[test]
fn balance_json_uses_default_wallet_when_target_is_omitted() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
    let owner = thruput_encoding::encode(&[0x12; 32]);
    let (rpc_url, rpc_server) = spawn_balance_rpc(&address, "123456789");
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_wallet = \"alice\"\n\n[profile.default.wallets.alice]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "balance"])
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
    assert_eq!(stdout["data"]["schemaVersion"], 4);
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["target"], "alice");
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["rawBalance"], "123456789");
    assert_eq!(stdout["data"]["balanceTokens"], "12.3456789");
    assert_eq!(stdout["data"]["stateBlockHeight"], "34");
}

#[test]
fn balance_json_uses_global_default_profile_when_profile_omitted() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    std::fs::create_dir_all(config_home.join("thruput")).unwrap();

    let address = thruput_encoding::encode(&[0x77; 32]);
    let owner = thruput_encoding::encode(&[0x78; 32]);
    let (rpc_url, rpc_server) = spawn_balance_rpc(&address, "70000000");
    std::fs::write(
        config_home.join("thruput/config.toml"),
        format!(
            "default_profile = \"devnet\"\n\n[profile.devnet]\nrpc_url = \"{rpc_url}\"\n\n[profile.devnet.wallets.alice]\naddress = \"{address}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["balanceTokens"], "7");
}

#[test]
fn balance_json_cli_rpc_url_overrides_profile() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address_hex = thruput_encoding::encode(&[0x22; 32]);
    let address = rpc_address_from_hex_fixture(&address_hex);
    let (rpc_url, rpc_server) = spawn_balance_rpc(&address_hex, "42");
    std::fs::write(
        project.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "balance", "--rpc-url", &rpc_url, &address])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    rpc_server.join().unwrap();
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "balance");
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["target"], address);
    assert_eq!(stdout["data"]["address"], address);
    assert_eq!(stdout["data"]["rawBalance"], "42");
}

#[test]
fn balance_json_rejects_invalid_cli_rpc_url_before_rpc() {
    let address = thruput_encoding::encode(&[0x22; 32]);
    let output = thruput()
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
        project.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let output = thruput()
        .args([
            "--json",
            "balance",
            "ZAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAZ4A",
        ])
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["command"], "balance");
    assert_eq!(stderr["error"]["code"], "invalid_address_target");
    assert_eq!(
        stderr["error"]["details"]["target"],
        "ZAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAZ4A"
    );
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
        project.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let output = thruput()
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
    std::fs::create_dir_all(config_home.join("thruput")).unwrap();
    std::fs::write(
        config_home.join("thruput").join("config.toml"),
        "default_key = \"dev-0\"\n",
    )
    .unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
    let key = format!("0x{}", "42".repeat(32));
    let manifest_path =
        write_runtime_manifest(tmp.path(), 0, &advertised_dev_wallet_address(0), None);
    let (rpc_url, rpc_server) = spawn_storage_rpc(&address, &key);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

    let output = thruput()
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
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["stateBlockHeight"], "12");
}

#[test]
fn storage_json_cli_rpc_url_overrides_profile_rpc_url() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
    let key = format!("0x{}", "42".repeat(32));
    let manifest_path =
        write_runtime_manifest(tmp.path(), 0, &advertised_dev_wallet_address(0), None);
    let (rpc_url, rpc_server) = spawn_storage_rpc(&address, &key);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

    let output = thruput()
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
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["stateBlockHeight"], "12");
}

#[test]
fn storage_json_resolves_contract_alias() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_storage_rpc(&address, &key);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.contracts]\ncounter = \"{address}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
        let config_path = tmp.path().join("thruput.toml");
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
        let output = thruput()
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
    let config_path = tmp.path().join("thruput.toml");
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
    let output = thruput()
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
        tmp.path().join("thruput.toml"),
        "default_profile = \"local\"\n\n[profile.local]\n",
    )
    .unwrap();
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = thruput()
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
    assert!(stderr.contains("thruput config get local --sources"));
    assert!(!stderr.contains("missing\n"));
    assert!(!stderr.contains('\u{1b}'));
}

#[test]
fn contract_command_preserves_prefixed_address_validation_error() {
    let tmp = TempDir::new().unwrap();
    let key = format!("0x{}", "42".repeat(32));
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = thruput()
        .args([
            "--json",
            "storage",
            "ZAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAZ4A",
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
    assert_eq!(
        stderr["error"]["details"]["target"],
        "ZAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAZ4A"
    );
    assert!(
        stderr["error"]["details"]["message"]
            .as_str()
            .unwrap()
            .contains("Z StrKey or base58 encoding of exactly 32 bytes")
    );
}

#[test]
fn storage_json_rejects_short_key_before_rpc() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let address = thruput_encoding::encode(&[0x11; 32]);
    let output = thruput()
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
    let trading = thruput_encoding::encode(&[0xbb; 32]);
    let mut config = std::fs::read_to_string(project.join("thruput.toml")).unwrap();
    write!(
        config,
        "\n[profile.default.wallets.trading]\naddress = \"{trading}\"\nowner_public_key = \"{owner}\"\n"
    )
    .unwrap();
    std::fs::write(project.join("thruput.toml"), config).unwrap();

    let contract = thruput_encoding::encode(&[0x11; 32]);
    let output = thruput()
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
    assert_eq!(thruput_cli::call::CALL_JSON_SCHEMA_VERSION, 4);
    assert_eq!(stdout["data"]["schemaVersion"], 4);
    for old_key in ["dry_run", "rpc_url", "entrypoint_id", "signed_tx"] {
        assert!(stdout["data"].get(old_key).is_none(), "found {old_key}");
    }
    assert_eq!(stdout["data"]["dryRun"], true);
    assert_eq!(stdout["data"]["submitted"], false);
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["address"], contract);
    assert_eq!(stdout["data"]["method"], "0x0123456789abcdef");
    assert_eq!(stdout["data"]["entrypoint"], "0x0123456789abcdef");
    assert_eq!(stdout["data"]["calldata"], "0x0102");
    assert_eq!(stdout["data"]["selector"]["source"], "explicit_entrypoint");
    assert!(stdout["data"]["selector"]["interface"].is_null());
    assert!(
        stdout["data"]["signedTransaction"]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
    let signed_tx = decode_signed_tx(stdout["data"]["signedTransaction"].as_str().unwrap());
    assert_eq!(signed_tx.tx.invocation.gas_limit, 25_000_000);
    let inner: Vec<u8> = xdrs::cxs::decode(&signed_tx.tx.invocation.calldata.0).unwrap();
    let (_, _, target_gas, _): (
        thruput_xdr::types::ContractAddress,
        thruput_xdr::contract::ContractEntrypointID,
        u64,
        Vec<u8>,
    ) = xdrs::txs::decode(&inner).unwrap();
    assert_eq!(target_gas, 25_000_000);
    assert_eq!(
        thruput_encoding::encode(signed_tx.tx.invocation.invoked_address.as_ref()),
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
    let contract = thruput_encoding::encode(&[0x11; 32]);

    let output = thruput()
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
    assert_eq!(stdout["data"]["walletEntrypoint"], "0x19846d750fad81d6");
    assert_eq!(stdout["data"]["walletEntrypointSource"], "legacy_default");
    assert_eq!(
        signed_tx_entrypoint(stdout["data"]["signedTransaction"].as_str().unwrap()),
        "0x19846d750fad81d6"
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
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

    let contract = thruput_encoding::encode(&[0x11; 32]);
    let output = thruput()
        .args([
            "--json",
            "call",
            "--dry-run",
            &contract,
            "0x0123456789abcdef",
            "0x0102",
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", tmp.path().join("fixture-config"))
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let signed_tx = stdout["data"]["signedTransaction"]
        .as_str()
        .expect("bug: dry-run output includes signed tx");

    assert_eq!(signed_tx_invoked_address(signed_tx), advertised_wallet);
    assert_eq!(signed_tx_entrypoint(signed_tx), "0xc3edb11fdd6d5d58");
}

#[test]
fn call_with_insufficient_gas_balance_fails_before_submission() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let server = rpc_mock()
        .method("getNetworkInfo", |_| Ok(network_info(TEST_NETWORK_HASH)))
        .method("getChainTip", |_| Ok(chain_tip(42)))
        .method("getBalance", |_| {
            Ok(serde_json::json!({
                "address": thruput_encoding::encode(&[0xaa; 32]),
                "rawBalance": "10000000",
                "stateBlockHeight": "42",
            }))
        })
        .start();
    let (rpc_url, rpc_server) = recorded_mock(server, |script| {
        script.network_check();
        script.expect("getChainTip");
        assert_eq!(
            script.expect("getBalance")["address"],
            thruput_encoding::encode(&[0xaa; 32])
        );
    });
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = thruput_encoding::encode(&[0x11; 32]);
    let output = thruput()
        .args([
            "--json",
            "call",
            &contract,
            "0x0123456789abcdef",
            "0x0102",
            "--force",
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "native_rpc_failure");
    assert_eq!(
        result["error"]["details"]["preflightReason"],
        "insufficientCapacity"
    );
    assert_eq!(result["error"]["details"]["availableRaw"], "10000000");
    assert_eq!(result["error"]["details"]["requiredRaw"], "25000000");
}

#[test]
fn call_human_raw_return_uses_child_bytes_for_native_and_legacy_wallets() {
    for native in [false, true] {
        for child in [vec![0xde_u8, 0xad, 0xbe, 0xef], vec![]] {
            let tmp = TempDir::new().unwrap();
            let config = tmp.path().join("config");
            let mut receipt = sample_call_receipt(42);
            let returned = if native {
                xdrs::cxs::encode(&vec![child.clone()]).unwrap()
            } else {
                child.clone()
            };
            receipt["execution"]["returnData"] =
                serde_json::json!(format!("0x{}", hex::encode(returned)));
            let (rpc, server) = spawn_transaction_rpc(42, receipt);
            let owner = write_project_config_with_alice_wallet(tmp.path(), &rpc, &config);
            if native {
                let account = thruput_signing::PubkeyAccount::from_public_key_bytes(
                    thruput_signing::strkey::parse_public_key(&owner).unwrap(),
                    0,
                );
                let path = tmp.path().join("thruput.toml");
                let mut document = std::fs::read_to_string(&path)
                    .unwrap()
                    .parse::<toml_edit::DocumentMut>()
                    .unwrap();
                let wallet = &mut document["profile"]["default"]["wallets"]["alice"];
                wallet["kind"] = toml_edit::value("native");
                wallet["account_nonce"] = toml_edit::value(0);
                wallet["address"] =
                    toml_edit::value(thruput_encoding::encode(account.address().as_ref()));
                std::fs::write(path, document.to_string()).unwrap();
            }
            let output = thruput()
                .args([
                    "call",
                    &thruput_encoding::encode(&[0x11; 32]),
                    "0x0123456789abcdef",
                    "0x",
                    "--force",
                ])
                .current_dir(tmp.path())
                .env("XDG_CONFIG_HOME", &config)
                .output()
                .unwrap();
            server.join().unwrap();
            let text = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{text}");
            let expected = format!("0x{}", hex::encode(child));
            assert!(text.contains("Child return"), "{text}");
            assert!(text.contains(&expected), "{text}");
        }
    }
}

#[test]
fn call_live_human_reports_native_receipt_without_optional_legacy_requests() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) = spawn_transaction_rpc(42, sample_call_receipt(42));
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = thruput_encoding::encode(&[0x11; 32]);
    let output = thruput()
        .args(["call", &contract, "0x0123456789abcdef", "0x0102", "--force"])
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
    assert!(stderr.contains("Event 0"));
    assert!(stderr.contains("data=0x1234"));
    assert!(
        stderr.contains("Gas used") && stderr.contains("1500"),
        "{stderr}"
    );
    assert!(!stderr.contains("transaction detail"));
}

#[test]
fn call_live_json_includes_diagnostics_for_failed_local_dev_call() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let advertised_wallet = advertised_dev_wallet_address(0);
    let manifest_path = write_runtime_manifest(tmp.path(), 0, &advertised_wallet, None);
    let (rpc_url, rpc_server) =
        spawn_transaction_rpc(42, sample_deterministic_error_call_receipt(42));
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.display()
        ),
    )
    .unwrap();

    let contract = thruput_encoding::encode(&[0x11; 32]);
    let output = thruput()
        .args([
            "--json",
            "call",
            &contract,
            "0x0123456789abcdef",
            "0x0102",
            "--force",
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", tmp.path().join("fixture-config"))
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["error"]["code"], "transaction_deterministic_error");
    assert_eq!(stdout["data"]["execution"]["result"], "fault");
    assert_eq!(stdout["data"]["gasUsed"], "1500");
    assert_eq!(
        stdout["data"]["walletAddress"],
        rpc_address_from_hex_fixture(&advertised_wallet)
    );
    assert_eq!(stdout["data"]["walletEntrypoint"], "0xc3edb11fdd6d5d58");
    assert_eq!(
        stdout["data"]["walletEntrypointSource"],
        "native_default_wallet"
    );
    assert_eq!(stdout["data"]["entrypoint"], "0x0123456789abcdef");
}

#[test]
fn call_live_json_marks_legacy_default_wallet_entrypoint_without_manifest() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) =
        spawn_transaction_rpc(42, sample_deterministic_error_call_receipt(42));
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = thruput_encoding::encode(&[0x11; 32]);
    let output = thruput()
        .args([
            "--json",
            "call",
            &contract,
            "0x0123456789abcdef",
            "0x0102",
            "--force",
        ])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["execution"]["result"], "fault");
    assert_eq!(stdout["data"]["walletEntrypoint"], "0x19846d750fad81d6");
    assert_eq!(stdout["data"]["walletEntrypointSource"], "legacy_default");
}

#[test]
fn call_live_human_fails_after_printing_receipt_context() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let (rpc_url, rpc_server) =
        spawn_transaction_rpc(42, sample_deterministic_error_call_receipt(42));
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = thruput_encoding::encode(&[0x11; 32]);
    let output = thruput()
        .args(["call", &contract, "0x0123456789abcdef", "0x0102", "--force"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Call failed"), "{stderr}");
    assert!(stderr.contains(&contract), "{stderr}");
    assert!(stderr.contains("Transaction"), "{stderr}");
    assert!(stderr.contains("Block height"), "{stderr}");
    assert!(stderr.contains("42"), "{stderr}");
    assert!(stderr.contains("1500"), "{stderr}");
    assert!(
        stderr.contains(&thruput_encoding::encode(&[0xaa; 32])),
        "{stderr}"
    );
}

fn write_counter_with_exception_spec(project: &std::path::Path, selector: u64) {
    use thruput_contract_spec::{
        ContractExceptionType, ContractSpec, ContractSpecInterface, ContractSpecMethod,
        ContractType,
    };
    std::fs::write(project.join("Cargo.toml"), "[package]\nname = 'counter'\nversion = '0.1.0'\nedition = '2024'\n[lib]\npath = 'lib.rs'\n").unwrap();
    std::fs::write(project.join("lib.rs"), "").unwrap();
    let artifacts = project.join("target/thruput");
    std::fs::create_dir_all(&artifacts).unwrap();
    let spec = ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: "Counter".try_into().unwrap(),
            vers: "0.1.0".try_into().unwrap(),
            throws: vec![ContractExceptionType {
                selector,
                ty: ContractType::U64,
            }]
            .try_into()
            .unwrap(),
            methods: vec![ContractSpecMethod {
                name: "get".try_into().unwrap(),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::U64,
                effect: None,
            }]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };
    std::fs::write(artifacts.join("counter.rv32a"), artifact_with_spec(&spec)).unwrap();
}

#[test]
fn call_native_decodes_forwarded_aborts_and_preserves_unknown_bytes() {
    let selector = 42u64;
    let custom_abort = format!(
        "0x{}",
        hex::encode(xdrs::cxs::encode(&(selector, 7u64)).unwrap())
    );
    let wallet_abort = format!(
        "0x{}",
        hex::encode(xdrs::cxs::encode(&(0x931d_3297_d37d_1bdf_u64, 5i32)).unwrap())
    );
    let library_abort = library_abort_hex("NonNegativeIntUnderflow");
    for (abort, expected) in [
        (custom_abort.as_str(), Some(serde_json::json!("7"))),
        (
            library_abort.as_str(),
            Some(serde_json::json!({"case": "NonNegativeIntUnderflow"})),
        ),
        (wallet_abort.as_str(), None),
        ("0x2bab", None), // Unknown selector.
        ("0xff", None),   // Truncated selector.
    ] {
        for json in [true, false] {
            let tmp = TempDir::new().unwrap();
            let project = tmp.path();
            let config_home = project.join("config-home");
            generate_key_public_key(&config_home, "alice");
            let (rpc_url, rpc_server) =
                spawn_transaction_rpc(42, sample_reverted_call_receipt(42, abort));
            // An unsaved single-key identity uses the native wallet path.
            std::fs::write(project.join("thruput.toml"), format!(
                "default_profile = 'default'\n[profile.default]\ndefault_key = 'alice'\nrpc_url = '{rpc_url}'\nnetwork_hash = '{TEST_NETWORK_HASH}'\n"
            )).unwrap();
            write_counter_with_exception_spec(project, selector);
            let contract = thruput_encoding::encode(&[0x11; 32]);
            let mut command = thruput();
            if json {
                command.arg("--json");
            }
            let output = command
                .args(["call", &contract, "get", "--force"])
                .current_dir(project)
                .env("XDG_CONFIG_HOME", &config_home)
                .env_remove("CARGO_TARGET_DIR")
                .output()
                .unwrap();
            rpc_server.join().unwrap();
            assert!(!output.status.success());
            if json {
                assert!(
                    output.stderr.is_empty(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(report["error"]["code"], "transaction_reverted");
                assert_eq!(
                    report["data"]["walletEntrypointSource"],
                    "native_default_wallet"
                );
                assert_eq!(report["data"]["execution"]["abortData"], abort);
                if let Some(value) = &expected {
                    assert_eq!(&report["data"]["abortValue"], value);
                    assert!(report["data"].get("abortDecodeError").is_none());
                } else {
                    assert!(report["data"]["abortDecodeError"].is_string());
                    assert!(report["data"].get("abortValue").is_none());
                }
            } else {
                assert!(output.stdout.is_empty());
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains("Call failed"), "{stderr}");
                if let Some(value) = &expected {
                    assert!(stderr.contains(&format!("Abort: {value}")), "{stderr}");
                    assert!(!stderr.contains("abort data:"), "{stderr}");
                } else {
                    assert!(stderr.contains(abort), "{stderr}");
                    assert!(stderr.contains("warning:"), "{stderr}");
                }
            }
        }
    }
}

/// Run `thruput call` with the alice wallet against a node that commits the
/// call with `receipt`, and return the CLI's output once the endpoint has
/// checked the submission flow.
fn run_live_call(receipt: serde_json::Value, json: bool) -> std::process::Output {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (rpc_url, rpc_server) = spawn_transaction_rpc(42, receipt);
    write_project_config_with_alice_wallet(&project, &rpc_url, &config_home);

    let contract = thruput_encoding::encode(&[0x11; 32]);
    let mut command = thruput();
    if json {
        command.arg("--json");
    }
    let output = command
        .args(["call", &contract, "0x0123456789abcdef", "0x0102", "--force"])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    rpc_server.join().unwrap();
    output
}

#[test]
fn call_live_json_decodes_library_abort() {
    let abort_hex = library_abort_hex("NonNegativeIntUnderflow");
    let output = run_live_call(sample_reverted_call_receipt(42, &abort_hex), true);

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["error"]["code"], "transaction_reverted");
    assert_eq!(stdout["data"]["schemaVersion"], 4);
    assert!(stdout["data"].get("abort_exception").is_none());
    assert_eq!(stdout["data"]["execution"]["result"], "abort");
    assert_eq!(stdout["data"]["execution"]["abortData"], abort_hex);
    assert_eq!(stdout["data"]["abortException"], "LibraryExn");
    assert_eq!(stdout["data"]["abortExceptionCrate"], "thruput-sdk");
    assert_eq!(stdout["data"]["abortExceptionVersion"], "1");
    assert_eq!(
        stdout["data"]["abortValue"],
        serde_json::json!({"case": "NonNegativeIntUnderflow"})
    );
}

#[test]
fn call_live_json_decodes_erc20_permit_abort() {
    // No local metadata declares the exception, so it resolves through the
    // same standard-exception registry an ERC20 abort does.
    let abort_data = xdrs::cxs::encode(&(
        <thruput_erc20_permit_if::ERC20PermitError as thruput_sdk::exn::Exn>::SELECTOR,
        thruput_erc20_permit_if::ERC20PermitError::ExceedsGrant,
    ))
    .expect("bug: ERC20 permit exception should encode");
    let abort_hex = format!("0x{}", hex::encode(&abort_data));
    let output = run_live_call(sample_reverted_call_receipt(42, &abort_hex), true);

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());

    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["error"]["code"], "transaction_reverted");
    assert_eq!(stdout["data"]["execution"]["result"], "abort");
    assert_eq!(stdout["data"]["execution"]["abortData"], abort_hex);
    assert_eq!(stdout["data"]["abortException"], "ERC20PermitError");
    assert_eq!(
        stdout["data"]["abortExceptionCrate"],
        "thruput-erc20-permit-if"
    );
    assert_eq!(stdout["data"]["abortExceptionVersion"], "0.1");
    assert_eq!(stdout["data"]["abortValue"], "ExceedsGrant");
    assert!(stdout["data"].get("abortDecodeError").is_none());
}

#[test]
fn call_live_human_prints_decoded_abort() {
    let abort_hex = library_abort_hex("NonNegativeIntUnderflow");
    let output = run_live_call(sample_reverted_call_receipt(42, &abort_hex), false);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Call failed"), "stderr: {stderr}");
    // The raw buffer is elided when it decodes, like the success path's
    // return data.
    assert!(!stderr.contains("abort data:"), "stderr: {stderr}");
    assert!(
        stderr.contains("Abort: {\"case\":\"NonNegativeIntUnderflow\"}"),
        "stderr: {stderr}"
    );
}

#[test]
fn call_live_json_omits_abort_fields_on_success() {
    let output = run_live_call(sample_call_receipt(42), true);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["data"]["execution"]["result"], "return");
    assert!(stdout["data"].get("abortException").is_none());
    assert!(stdout["data"].get("abortValue").is_none());
    assert!(stdout["data"].get("abortDecodeError").is_none());
}

#[test]
fn call_live_json_reports_abort_decode_error() {
    // Selector 0 is reserved and matches no exception; the payload byte
    // is parsed off after the selector and reported separately.
    let output = run_live_call(sample_reverted_call_receipt(42, "0x00ab"), true);

    assert!(!output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], false);
    assert_eq!(stdout["data"]["execution"]["abortData"], "0x00ab");
    assert_eq!(stdout["data"]["abortValue"], serde_json::Value::Null);
    let decode_error = stdout["data"]["abortDecodeError"].as_str().unwrap();
    assert!(
        decode_error.contains("0x0000000000000000"),
        "{decode_error}"
    );
    assert!(decode_error.contains("does not match"), "{decode_error}");
    assert!(decode_error.contains("0xab"), "{decode_error}");
}

#[test]
fn call_live_human_reports_abort_decode_error() {
    let output = run_live_call(sample_reverted_call_receipt(42, "0x00ab"), false);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("0x00ab"), "stderr: {stderr}");
    assert!(
        stderr.contains("abort selector 0x0000000000000000"),
        "stderr: {stderr}"
    );
}

#[test]
fn invalid_template_rejected() {
    let tmp = TempDir::new().unwrap();
    let output = thruput()
        .arg("new")
        .arg("bad")
        .arg("--template")
        .arg("nope")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown template"));
    assert!(stderr.contains("templates list"));
    assert!(!tmp.path().join("bad").exists());
}

#[test]
fn completion_json_outputs_structured_payload() {
    let output = thruput()
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
    let script = stdout["data"]["script"].as_str().unwrap();
    assert!(script.contains("thruput"));
    assert!(script.contains("skill"));
    assert!(script.contains("--version"));
}

#[test]
fn keys_json_flow_works() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");

    let generate = thruput()
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
    assert_eq!(generated["data"]["key"]["isDefault"], true);

    let listed = thruput()
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

    let used = thruput()
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
    assert_eq!(used_json["data"]["key"]["isDefault"], true);

    let removed = thruput()
        .arg("--json")
        .arg("keys")
        .arg("rm")
        .arg("--force")
        .arg("alice")
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(removed.status.success());
    let removed_json: serde_json::Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(removed_json["command"], "keys.rm");
    assert_eq!(removed_json["data"]["clearedDefault"], true);
}

#[test]
fn keys_export_outputs_client_compatible_private_key() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");

    let generated = thruput()
        .args(["--json", "keys", "generate", "alice"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(generated.status.success());
    let generated: serde_json::Value = serde_json::from_slice(&generated.stdout).unwrap();

    let exported = thruput()
        .args(["keys", "export", "alice", "--force"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(exported.status.success());
    assert!(exported.stderr.is_empty());
    let plain = String::from_utf8(exported.stdout).unwrap();
    assert!(plain.ends_with('\n'));

    let private_key = plain.trim_end();
    let stored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(config_home.join("thruput/keys/alice.json")).unwrap(),
    )
    .unwrap();
    let stored_keypair = stored["keypair_hex"].as_str().unwrap();
    assert_eq!(
        private_key,
        thruput_encoding::encode(&hex::decode(&stored_keypair[..64]).unwrap())
    );

    let private_key_bytes = thruput_encoding::decode_fixed::<32>(private_key).unwrap();
    let signer = thruput_signing::LocalSigner::from_secret_key(private_key_bytes);
    assert_eq!(
        thruput_encoding::encode(&signer.verifying_key_bytes()),
        generated["data"]["key"]["publicKey"]
    );

    let quiet = thruput()
        .args(["--quiet", "keys", "export", "alice", "--force"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(quiet.status.success());
    assert_eq!(quiet.stdout, plain.as_bytes());
    assert!(quiet.stderr.is_empty());

    let json = thruput()
        .args(["--json", "keys", "export", "alice", "--force"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(json.status.success());
    assert!(json.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "keys.export");
    assert_eq!(json["warnings"], serde_json::json!([]));
    assert_eq!(json["data"]["schemaVersion"], 1);
    assert_eq!(json["data"]["key"]["name"], "alice");
    assert_eq!(json["data"]["key"]["scheme"], "ed25519");
    assert_eq!(json["data"]["key"]["privateKey"], private_key);
    assert_eq!(json["data"].as_object().unwrap().len(), 2);
    assert_eq!(json["data"]["key"].as_object().unwrap().len(), 3);
}

#[test]
fn keys_export_rejects_missing_and_invalid_names() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");

    for name in ["ghost", "dev-0"] {
        let output = thruput()
            .args(["--json", "keys", "export", name, "--force"])
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["command"], "keys.export");
        assert_eq!(error["error"]["code"], "key_not_found");
    }

    let invalid = thruput()
        .args(["--json", "keys", "export", "../alice"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(error["error"]["code"], "key_name_invalid");
}

#[test]
fn mldsa65_keys_store_a_scheme_tagged_seed_and_derive_the_account() {
    use thruput_signing::Signer as _;

    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let output = thruput()
        .args(["--json", "keys", "generate", "pq", "--scheme", "mldsa65"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let key = &report["data"]["key"];
    assert_eq!(report["command"], "keys.generate");
    assert_eq!(key["scheme"], "mldsa65");
    assert!(key.get("public_key_strkey").is_none());

    // The file holds only the FIPS 204 seed and cached public key, owner-only.
    let key_path = config_home.join("thruput/keys/pq.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let keyfile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&key_path).unwrap()).unwrap();
    assert_eq!(keyfile["scheme"], "mldsa65");
    assert!(keyfile.get("keypair_hex").is_none());
    let seed: [u8; 32] = hex::decode(keyfile["seed_hex"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let signer = thruput_signing::MlDsa65Signer::from_seed(&seed);
    assert_eq!(
        keyfile["public_key_hex"],
        hex::encode(signer.public_key_bytes())
    );
    let account = thruput_signing::PubkeyAccount::new(signer.public_key(), 0);
    assert_eq!(
        key["address"],
        thruput_encoding::encode(account.address().as_ref())
    );
    assert_eq!(
        key["publicKeyHash"],
        thruput_encoding::encode(account.public_key_hash().as_ref())
    );

    let export = thruput()
        .args(["--json", "keys", "export", "pq", "--force"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(export.status.success());
    let export: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
    assert_eq!(export["data"]["key"]["scheme"], "mldsa65");
    assert_eq!(
        export["data"]["key"]["privateKey"],
        thruput_encoding::encode(&seed)
    );

    // S StrKeys are Ed25519-only.
    let strkey = thruput()
        .args([
            "--json", "keys", "export", "pq", "--force", "--format", "strkey",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(!strkey.status.success());
    assert!(strkey.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&strkey.stderr).unwrap();
    assert_eq!(error["error"]["code"], "key_scheme_unsupported");
    assert_eq!(error["error"]["details"]["scheme"], "mldsa65");
}

#[test]
fn keys_export_rejects_invalid_stored_keys() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let keys_dir = config_home.join("thruput/keys");
    std::fs::create_dir_all(&keys_dir).unwrap();

    let cases = [
        (
            "bad-scheme",
            serde_json::json!({
                "version": 1,
                "scheme": "rsa",
                "keypair_hex": "00".repeat(64),
            }),
            "unsupported key scheme",
        ),
        (
            "bad-hex",
            serde_json::json!({
                "version": 1,
                "scheme": "ed25519",
                "keypair_hex": "zz",
            }),
            "Invalid character",
        ),
        (
            "bad-keypair",
            serde_json::json!({
                "version": 1,
                "scheme": "ed25519",
                "keypair_hex": "00".repeat(64),
            }),
            "invalid keypair",
        ),
    ];

    for (name, keyfile, message) in cases {
        std::fs::write(
            keys_dir.join(format!("{name}.json")),
            serde_json::to_vec(&keyfile).unwrap(),
        )
        .unwrap();
        let output = thruput()
            .args(["--json", "keys", "export", name, "--force"])
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(message),
            "expected {message:?} for {name}, stderr: {stderr}"
        );
    }
}

#[test]
fn keys_export_requires_confirmation_before_loading_secret_material() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    let keys_dir = config_home.join("thruput/keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    std::fs::write(
        keys_dir.join("broken.json"),
        br#"{"version":1,"scheme":"ed25519","keypair_hex":"zz"}"#,
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "keys", "export", "broken"])
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "confirmation_required");
    assert_eq!(error["error"]["details"]["requiredFlag"], "--yes");
}

#[cfg(unix)]
#[test]
fn keys_rm_confirmation_accepts_yes_and_preserves_on_no_or_empty() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");
    for name in ["alice", "bob", "carol"] {
        let generated = thruput()
            .args(["keys", "generate", name])
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(generated.status.success());
    }

    let mut command = thruput();
    command
        .args(["keys", "rm", "alice"])
        .env("XDG_CONFIG_HOME", &config_home);
    let (confirmed, mut terminal) = spawn_with_terminal(command);
    terminal.write_all(b"y\n").unwrap();
    let confirmed = confirmed.wait_with_output().unwrap();
    assert!(confirmed.status.success());
    assert!(!config_home.join("thruput/keys/alice.json").exists());

    for (name, answer) in [("bob", b"n\n".as_slice()), ("carol", b"\n".as_slice())] {
        let mut command = thruput();
        command
            .args(["keys", "rm", name])
            .env("XDG_CONFIG_HOME", &config_home);
        let (declined, mut terminal) = spawn_with_terminal(command);
        terminal.write_all(answer).unwrap();
        let declined = declined.wait_with_output().unwrap();
        assert!(declined.status.success());
        assert!(
            config_home
                .join(format!("thruput/keys/{name}.json"))
                .exists()
        );
    }
}

#[test]
fn keys_ls_human_output_uses_stdout() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config-home");

    let generate = thruput()
        .arg("keys")
        .arg("generate")
        .arg("alice")
        .env("XDG_CONFIG_HOME", &config_home)
        .output()
        .unwrap();
    assert!(generate.status.success());

    let listed = thruput()
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

    let first = thruput()
        .args(["keys", "generate", "dup"])
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert!(first.status.success());

    let second = thruput()
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
    let output = thruput()
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
    let output = thruput()
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

#[cfg(unix)]
#[test]
fn key_import_refuses_terminal_input_in_non_interactive_modes() {
    for flags in [vec!["--non-interactive"], vec!["--quiet"], vec!["--json"]] {
        let tmp = TempDir::new().unwrap();
        let mut command = thruput();
        command
            .args(["keys", "import", "alice"])
            .args(&flags)
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path());
        let (child, terminal) = spawn_with_terminal(command);
        let reader = terminal_output_reader(&terminal);
        let status = wait_child(child, CLI_ACTION_TIMEOUT);
        drop(terminal);
        let output = reader.join().unwrap();
        assert!(!status.success());
        assert!(!output.contains("Ed25519 secret seed"));
        assert!(!tmp.path().join("thruput/keys/alice.json").exists());
        if flags == ["--json"] {
            let report: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(report["error"]["code"], "interactive_input_required");
        }
    }
}

#[test]
fn global_non_interactive_key_import_accepts_piped_seed() {
    let tmp = TempDir::new().unwrap();
    let mut child = thruput()
        .args(["--non-interactive", "keys", "import", "alice", "--json"])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let seed = thruput_encoding::encode(&[0x42; 32]);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(seed.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&seed));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["command"], "keys.import");
    assert_eq!(report["ok"], true);
    assert_eq!(report["data"]["key"]["name"], "alice");
    assert!(tmp.path().join("thruput/keys/alice.json").exists());
}

#[test]
fn key_generation_stores_dev_named_keys_without_implicit_derivation() {
    for name in ["dev-0", "dev-1", "dev-01"] {
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("thruput/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let before = "default_key = 'existing'\n[profile.default]\n";
        std::fs::write(&config, before).unwrap();
        let output = thruput()
            .args(["--json", "keys", "generate", name])
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["data"]["key"]["name"], name);
        assert!(
            tmp.path()
                .join(format!("thruput/keys/{name}.json"))
                .exists()
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
    }
}

#[test]
fn tx_show_json_returns_committed_status_and_matching_receipt() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.devnet]\nrpc_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    let hash = thruput_encoding::encode(&[0xab; 32]);
    let event = serde_json::json!({
        "address": thruput_encoding::encode(&[0x11; 32]),
        "topics": [format!("0x{}", "22".repeat(32))],
        "data": "0x0304",
    });
    let receipt = serde_json::json!({
        "transactionHash": hash,
        "blockHeight": "42",
        "execution": { "result": "return", "returnData": "0x0506" },
        "events": [event.clone()],
        "gasUsed": "1500",
    });
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        RpcReply::Result(serde_json::json!({
            "status": "committed",
            "blockHeight": "42",
        })),
        Some(RpcReply::Result(receipt.clone())),
    );

    let output = thruput()
        .args([
            "--json",
            "tx",
            "show",
            &hash,
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
    assert_eq!(stdout["data"]["schemaVersion"], 5);
    assert_eq!(stdout["data"]["profile"], "devnet");
    assert_eq!(stdout["data"]["rpcUrl"], rpc_url);
    assert_eq!(stdout["data"]["txHash"], hash);
    assert_eq!(stdout["data"]["status"]["status"], "committed");
    assert_eq!(stdout["data"]["status"]["blockHeight"], "42");
    assert_eq!(stdout["data"]["receipt"], receipt);
    assert_eq!(stdout["data"]["receipt"]["events"][0], event);
}

#[test]
fn tx_show_committed_height_handles_indexer_catching_up() {
    let hash = thruput_encoding::encode(&[0xab; 32]);
    for json_output in [false, true] {
        for receipt_available in [false, true] {
            let tmp = TempDir::new().unwrap();
            let receipt = if receipt_available {
                serde_json::json!({
                    "transactionHash": hash,
                    "blockHeight": "42",
                    "execution": { "result": "return", "returnData": "0x" },
                    "events": [],
                    "gasUsed": "1500",
                })
            } else {
                serde_json::Value::Null
            };
            let (rpc_url, rpc_server) = spawn_tx_show_rpc(
                &hash,
                RpcReply::Result(serde_json::json!({"status": "committed", "blockHeight": "42"})),
                Some(RpcReply::Result(receipt.clone())),
            );
            let mut command = thruput();
            if json_output {
                command.arg("--json");
            }
            let output = command
                .args(["tx", "show", &hash, "--rpc-url", &rpc_url])
                .current_dir(tmp.path())
                .output()
                .unwrap();
            rpc_server.join().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            if json_output {
                let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(
                    stdout["data"]["status"],
                    serde_json::json!({"status": "committed", "blockHeight": "42"})
                );
                assert_eq!(stdout["data"]["receipt"], receipt);
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains(r#""status":"committed""#));
                assert!(stderr.contains(r#""blockHeight":"42""#));
                if receipt_available {
                    assert!(stderr.contains("receipt block height: 42"));
                } else {
                    assert!(stderr.contains("receipt: unavailable"));
                }
            }
        }
    }
}

#[test]
fn tx_show_binary_preserves_case_sensitive_base58_hashes() {
    for input in [
        thruput_encoding::encode(&[0xab; 32]),
        thruput_encoding::encode(&[0xcd; 32]),
    ] {
        let canonical = input.clone();
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(
            &canonical,
            RpcReply::Result(serde_json::json!({ "status": "pending" })),
            None,
        );
        let output = thruput()
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
fn tx_show_human_output_includes_raw_committed_receipt_data() {
    let tmp = TempDir::new().unwrap();
    let hash = thruput_encoding::encode(&[0xac; 32]);
    let event = serde_json::json!({
        "address": thruput_encoding::encode(&[0x11; 32]),
        "topics": [],
        "data": "0xaabb",
    });
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        RpcReply::Result(serde_json::json!({
            "status": "committed",
            "blockHeight": "43",
        })),
        Some(RpcReply::Result(serde_json::json!({
            "transactionHash": hash,
            "blockHeight": "43",
            "execution": { "result": "abort", "abortData": "0xeeff" },
            "events": [event],
            "gasUsed": "1500",
        }))),
    );

    let output = thruput()
        .args(["tx", "show", &hash, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""status":"committed""#));
    assert!(stderr.contains(&format!("hash: {hash}")));
    assert!(stderr.contains("receipt block height: 43"));
    assert!(stderr.contains(r#""result":"abort""#));
    assert!(stderr.contains(r#"event: {"address""#));
    assert!(stderr.contains("abort data: 0xeeff"));
    assert!(!stderr.contains("return data:"));
}

#[test]
fn tx_show_known_statuses_still_read_receipts() {
    let hash = thruput_encoding::encode(&[0xbc; 32]);
    let statuses = [
        serde_json::json!({ "status": "pending" }),
        serde_json::json!({
            "status": "recentFailure",
            "attemptBlockHeight": "17",
            "cause": { "kind": "dbConflict", "conflict": {
                "address": thruput_encoding::encode(&[0x11; 32]),
                "key": "0xaabb",
                "code": "boundsExceeded",
            } },
        }),
        serde_json::json!({ "status": "retry", "error": { "source": "scheduler", "code": 1 } }),
        serde_json::json!({ "status": "dropped", "error": { "source": "auth", "code": 2 } }),
        serde_json::json!({ "status": "unknown" }),
    ];

    for (index, status) in statuses.into_iter().enumerate() {
        let expected_status = status.clone();
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(&hash, RpcReply::Result(status), None);
        let output = thruput()
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
    let hash = thruput_encoding::encode(&[0xcd; 32]);
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        RpcReply::Result(serde_json::json!({
            "status": "committed",
            "blockHeight": "51",
        })),
        Some(RpcReply::Result(serde_json::Value::Null)),
    );

    let output = thruput()
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
            "schemaVersion": 5,
            "profile": "default",
            "rpcUrl": rpc_url,
            "txHash": hash,
            "status": { "status": "committed", "blockHeight": "51" },
            "receipt": null,
        })
    );
}

#[test]
fn tx_show_matching_receipt_omits_absent_optional_fields() {
    let tmp = TempDir::new().unwrap();
    let hash = thruput_encoding::encode(&[0xcf; 32]);
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        RpcReply::Result(serde_json::json!({
            "status": "committed",
            "blockHeight": "52",
        })),
        Some(RpcReply::Result(serde_json::json!({
            "transactionHash": hash,
            "blockHeight": "52",
            "execution": { "result": "return", "returnData": "0x" },
            "events": [],
            "gasUsed": "1500",
        }))),
    );

    let output = thruput()
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
            "transactionHash":hash, "blockHeight":"52", "gasUsed":"1500",
            "execution":{"result":"return","returnData":"0x"}, "events":[],
        })
    );
    assert!(stdout["data"]["receipt"].get("returnData").is_none());
    assert!(stdout["data"]["receipt"].get("abortData").is_none());
}

#[test]
fn tx_show_human_output_distinguishes_unavailable_receipt_and_unknown_status() {
    let hash = thruput_encoding::encode(&[0xd1; 32]);
    for (status, receipt, expected, forbidden) in [
        (
            serde_json::json!({ "status": "committed", "blockHeight": "53" }),
            Some(RpcReply::Result(serde_json::Value::Null)),
            "receipt: unavailable",
            "status: invalid",
        ),
        (
            serde_json::json!({ "status": "unknown" }),
            None,
            r#""status":"unknown""#,
            "status: invalid",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(&hash, RpcReply::Result(status), receipt);
        let output = thruput()
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
            assert!(stderr.contains(r#""status":"committed""#));
            assert!(stderr.contains(r#""blockHeight":"53""#));
        }
    }
}

#[test]
fn tx_show_uses_profile_rpc_url_without_cli_override() {
    let tmp = TempDir::new().unwrap();
    let hash = thruput_encoding::encode(&[0xce; 32]);
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        RpcReply::Result(serde_json::json!({ "status": "pending" })),
        None,
    );
    std::fs::write(
        tmp.path().join("thruput.toml"),
        format!("[profile.devnet]\nrpc_url = \"{rpc_url}\"\n"),
    )
    .unwrap();

    let output = thruput()
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
    let hash = thruput_encoding::encode(&[0xde; 32]);
    for (_name, receipt) in [
        (
            "hash",
            serde_json::json!({
                "transactionHash": thruput_encoding::encode(&[0xef; 32]),
                "blockHeight": "61",
                "execution": { "result": "return", "returnData": "0x" },
                "events": [],
                "gasUsed": "1500",
            }),
        ),
        (
            "height",
            serde_json::json!({
                "transactionHash": hash,
                "blockHeight": "62",
                "execution": { "result": "return", "returnData": "0x" },
                "events": [],
                "gasUsed": "1500",
            }),
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(
            &hash,
            RpcReply::Result(serde_json::json!({
                "status": "committed",
                "blockHeight": "61",
            })),
            Some(RpcReply::Result(receipt)),
        );
        let output = thruput()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "native_rpc_failure");
        assert!(
            stderr["error"]["message"]
                .as_str()
                .unwrap()
                .contains("inconsistent transaction receipt")
        );
    }
}

#[test]
fn tx_show_preserves_rpc_error_codes_without_recovery_classification() {
    let hash = thruput_encoding::encode(&[0xef; 32]);
    for code in [-32601, -32052] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) =
            spawn_tx_show_rpc(&hash, RpcReply::Error(code, "unsupported\nmethod"), None);
        let output = thruput()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "native_rpc_failure");
        assert_eq!(stderr["error"]["details"]["rpcError"]["code"], code);
        assert_eq!(
            stderr["error"]["details"]["rpcError"]["message"],
            "unsupported\nmethod"
        );
    }

    for code in [-32602, -32050, -32051, -32053, -32099] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) =
            spawn_tx_show_rpc(&hash, RpcReply::Error(code, "ordinary RPC error"), None);
        let output = thruput()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "native_rpc_failure");
    }

    let output = thruput()
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
    assert_eq!(stderr["error"]["code"], "native_rpc_failure");
}

#[test]
fn tx_show_human_error_escapes_invalid_rpc_url() {
    let hash = thruput_encoding::encode(&[0xab; 32]);
    let rpc_url = "http://[bad\n\u{1b}[31m";
    let output = thruput()
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
fn proxy_errors_never_print_credentials() {
    let hash = thruput_encoding::encode(&[0xab; 32]);
    let unused_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let unused_address = unused_listener.local_addr().unwrap();
    drop(unused_listener);
    let proxies = [
        ("http://proxy-user:supersecret@".to_owned(), true),
        (
            format!("http://proxy-user:supersecret@{unused_address}"),
            false,
        ),
    ];

    for (proxy, invalid) in proxies {
        for json in [false, true] {
            let mut command = thruput();
            if json {
                command.arg("--json");
            }
            let output = command
                .args(["tx", "show", &hash, "--rpc-url", "http://127.0.0.1:1"])
                .env("HTTP_PROXY", &proxy)
                .env_remove("NO_PROXY")
                .env_remove("no_proxy")
                .output()
                .unwrap();

            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(!stderr.contains("proxy-user"));
            assert!(!stderr.contains("supersecret"));
            assert!(!stderr.contains(&proxy));
            if invalid {
                assert!(stderr.contains("HTTP_PROXY"));
                if json {
                    let error: serde_json::Value = serde_json::from_str(&stderr).unwrap();
                    assert_eq!(error["error"]["code"], "proxy_configuration_invalid");
                }
            } else {
                assert!(stderr.contains("outbound HTTP connection failed"));
            }
        }
    }
}

#[test]
fn tx_show_receipt_rpc_errors_remain_failures() {
    let tmp = TempDir::new().unwrap();
    let hash = thruput_encoding::encode(&[0xf1; 32]);
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        RpcReply::Result(serde_json::json!({
            "status": "committed",
            "blockHeight": "71",
        })),
        Some(RpcReply::Error(-32052, "receipt unsupported")),
    );

    let output = thruput()
        .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "native_rpc_failure");
    assert_eq!(stderr["error"]["details"]["rpcError"]["code"], -32052);
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
        let output = thruput()
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
    let output = thruput()
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
    let hash = thruput_encoding::encode(&[0xf2; 32]);
    let (rpc_url, rpc_server) = spawn_tx_show_rpc(
        &hash,
        RpcReply::Result(serde_json::json!({
            "status": "invalid",
            "reason": "bad\nreason\u{001b}[31m\u{202e}spoof",
        })),
        None,
    );

    let output = thruput()
        .args(["tx", "show", &hash, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unsupported getTransactionStatus"));
    assert!(!stderr.contains('\u{1b}'));
    assert!(!stderr.contains('\u{202e}'));
}

#[test]
fn tx_show_scheduler_failures_preserve_diagnostics_without_recovery_advice() {
    let hash = thruput_encoding::encode(&[0xbc; 32]);
    for tag in ["retry", "dropped"] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_tx_show_rpc(
            &hash,
            RpcReply::Result(serde_json::json!({
                "status":tag, "error":{"source":"scheduler", "code":3}
            })),
            None,
        );
        let output = thruput()
            .args(["tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();
        assert!(output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(&format!("\"status\":\"{tag}\"")),
            "{stderr}"
        );
        assert!(stderr.contains(r#""source":"scheduler""#), "{stderr}");
        assert!(stderr.contains(r#""code":3"#), "{stderr}");
        assert!(!stderr.contains("resubmit"), "{stderr}");
    }
}

#[test]
fn tx_show_receipt_survives_failed_or_unsupported_status_lookup() {
    for (status, expected_evidence) in [
        (
            RpcReply::Error(-32050, "scheduler offline"),
            serde_json::json!({"rpcError":{"code":-32050,"message":"scheduler offline"}}),
        ),
        (
            RpcReply::Result(serde_json::json!({"status":"futureStatus","detail":"retained"})),
            serde_json::json!({"method":"getTransactionStatus","result":{"status":"futureStatus","detail":"retained"}}),
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let hash = thruput_encoding::encode(&[0xab; 32]);
        let receipt = serde_json::json!({"transactionHash":hash,"blockHeight":"42","gasUsed":"1","events":[],"execution":{"result":"return","returnData":"0x0102"}});
        let (rpc_url, server) =
            spawn_tx_show_rpc(&hash, status, Some(RpcReply::Result(receipt.clone())));
        let output = thruput()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["data"]["status"],
            serde_json::json!({"status":"committed","blockHeight":"42"})
        );
        assert_eq!(report["data"]["receipt"], receipt);
        assert_eq!(report["data"]["statusError"]["evidence"], expected_evidence);
    }
}

#[test]
fn tx_show_receipt_failure_preserves_known_commitment_and_original_error() {
    let hash = thruput_encoding::encode(&[0xab; 32]);
    let invalid = serde_json::json!({"transactionHash":hash,"blockHeight":42});
    for receipt in [
        RpcReply::Error(-32050, "indexer offline"),
        RpcReply::Result(invalid.clone()),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, server) = spawn_tx_show_rpc(
            &hash,
            RpcReply::Result(serde_json::json!({"status":"committed","blockHeight":"42"})),
            Some(receipt),
        );
        let output = thruput()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        let evidence = &report["error"]["details"];
        assert_eq!(
            evidence["status"],
            serde_json::json!({"status":"committed","blockHeight":"42"})
        );
        if evidence.get("rpcError").is_some() {
            assert_eq!(evidence["rpcError"]["code"], -32050);
            assert_eq!(evidence["rpcError"]["message"], "indexer offline");
        } else {
            assert_eq!(evidence["result"], invalid);
            assert!(evidence["decodeError"].is_string());
        }
    }
}

#[test]
fn tx_show_keeps_observed_advisory_status_with_an_authoritative_receipt() {
    for status in [
        serde_json::json!({"status":"pending"}),
        serde_json::json!({"status":"unknown"}),
        serde_json::json!({"status":"dropped","error":{"source":"scheduler","code":1}}),
    ] {
        let tmp = TempDir::new().unwrap();
        let hash = thruput_encoding::encode(&[0xab; 32]);
        let receipt = serde_json::json!({"transactionHash":hash,"blockHeight":"42","gasUsed":"1","events":[],"execution":{"result":"return","returnData":"0x"}});
        let (url, server) = spawn_tx_show_rpc(
            &hash,
            RpcReply::Result(status.clone()),
            Some(RpcReply::Result(receipt.clone())),
        );
        let output = thruput()
            .args(["--json", "tx", "show", &hash, "--rpc-url", &url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["data"]["status"], status);
        assert_eq!(report["data"]["receipt"], receipt);
        assert!(report["data"].get("statusError").is_none());
    }
}

#[test]
fn tx_show_human_discloses_receipt_derived_status_after_lookup_failure() {
    let tmp = TempDir::new().unwrap();
    let hash = thruput_encoding::encode(&[0xab; 32]);
    let receipt = serde_json::json!({"transactionHash":hash,"blockHeight":"42","gasUsed":"1","events":[],"execution":{"result":"return","returnData":"0x"}});
    let (url, server) = spawn_tx_show_rpc(
        &hash,
        RpcReply::Error(-32050, "scheduler offline"),
        Some(RpcReply::Result(receipt)),
    );
    let output = thruput()
        .args(["tx", "show", &hash, "--rpc-url", &url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(text.contains("status (from receipt)"), "{text}");
    assert!(text.contains("scheduler offline"), "{text}");
    assert!(text.contains("receipt block height: 42"), "{text}");
}

#[test]
fn tx_show_preserves_recent_attempt_diagnostics_in_json_and_terminal_output() {
    let hash = thruput_encoding::encode(&[0xab; 32]);
    let address = thruput_encoding::encode(&[0xcd; 32]);
    let status = serde_json::json!({
        "status":"retry", "error":{"source":"executor", "code":3},
        "recentFailure":{"attemptBlockHeight":"41", "cause":{
            "kind":"dbConflict", "conflict":{"address":address,"key":"0x0102","code":"insufficientFunds"}
        }}
    });
    for json_output in [true, false] {
        let tmp = TempDir::new().unwrap();
        let (url, server) = spawn_tx_show_rpc(
            &hash,
            RpcReply::Result(status.clone()),
            Some(RpcReply::Result(serde_json::Value::Null)),
        );
        let mut command = thruput();
        if json_output {
            command.arg("--json");
        }
        let output = command
            .args(["tx", "show", &hash, "--rpc-url", &url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if json_output {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["data"]["status"], status);
            assert!(report["data"]["receipt"].is_null());
        } else {
            let text = String::from_utf8(output.stderr).unwrap();
            let observed = text
                .lines()
                .find_map(|line| line.split_once("status: ").map(|(_, value)| value))
                .expect("human status line");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(observed).unwrap(),
                status
            );
        }
    }
}

#[test]
fn tx_list_json_uses_default_wallet_when_target_is_omitted() {
    let tmp = TempDir::new().unwrap();
    let wallet_address = thruput_encoding::encode(&[0x11; 32]);
    let contract_address = thruput_encoding::encode(&[0x22; 32]);
    let owner = thruput_encoding::encode(&[0x33; 32]);
    std::fs::write(
        tmp.path().join("thruput.toml"),
        format!(
            "[profile.devnet]\nrpc_url = \"http://127.0.0.1:1\"\ndefault_wallet = \"alice\"\n\
             [profile.devnet.wallets.alice]\naddress = \"{wallet_address}\"\nowner_public_key = \"{owner}\"\n\
             [profile.devnet.contracts]\nalice = \"{contract_address}\"\n"
        ),
    )
    .unwrap();
    let first_hash = thruput_encoding::encode(&[0x44; 32]);
    let second_hash = thruput_encoding::encode(&[0x55; 32]);
    let expected_query = serde_json::json!({
        "address": wallet_address,
        "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
        "limit": 50,
    });
    let page = serde_json::json!({
        "transactions": [
            { "transactionHash": first_hash, "blockHeight": "90" },
            { "transactionHash": second_hash, "blockHeight": "80" },
        ],
        "nextCursor": "0xaabb",
        "latestIndexedBlockHeight": "100",
        "oldestIndexedBlockHeight": "7",
    });
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(expected_query, RpcReply::Result(page));

    let output = thruput()
        .args([
            "--json",
            "tx",
            "list",
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
            "schemaVersion": 2,
            "profile": "devnet",
            "rpcUrl": rpc_url,
            "targetInput": "alice",
            "address": wallet_address,
            "refs": [
                { "txHash": first_hash, "height": "90" },
                { "txHash": second_hash, "height": "80" },
            ],
            "cursor": "0xaabb",
            "latestBlock": "100",
            "oldestBlock": "7",
        })
    );
    assert!(stdout["data"].get("order").is_none());
}

#[test]
fn tx_list_continuation_forwards_cursor_and_endpoint_owned_limit() {
    let tmp = TempDir::new().unwrap();
    let address = thruput_encoding::encode(&[0x66; 32]);
    let cursor_input = format!("0X{}", "Ab".repeat(257));
    let canonical_cursor = format!("0x{}", "ab".repeat(257));
    let expected_query = serde_json::json!({
        "address": address,
        "cursor": canonical_cursor,
        "limit": 201,
    });
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        expected_query,
        RpcReply::Result(serde_json::json!({
            "transactions": [],
            "nextCursor": "0x0102",
            "latestIndexedBlockHeight": "100",
            "oldestIndexedBlockHeight": "0",
        })),
    );

    let output = thruput()
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
    let address = thruput_encoding::encode(&[0x65; 32]);
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        serde_json::json!({
            "address": address,
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 1,
        }),
        RpcReply::Result(serde_json::json!({
            "transactions": [
                { "transactionHash": thruput_encoding::encode(&[0x66; 32]), "blockHeight": "2" },
                { "transactionHash": thruput_encoding::encode(&[0x67; 32]), "blockHeight": "1" },
            ],
            "nextCursor": null,
            "latestIndexedBlockHeight": "2",
            "oldestIndexedBlockHeight": "0",
        })),
    );

    let output = thruput()
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
    assert_eq!(stderr["error"]["code"], "native_rpc_failure");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("native root history exceeded the requested page limit")
    );
    assert_eq!(stderr["error"]["details"]["requestedLimit"], 1);
}

#[test]
fn tx_list_accepts_empty_exhausted_page() {
    let tmp = TempDir::new().unwrap();
    let address = thruput_encoding::encode(&[0x67; 32]);
    let expected_query = serde_json::json!({
        "address": address,
        "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
        "limit": 50,
    });
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        expected_query,
        RpcReply::Result(serde_json::json!({
            "transactions": [],
            "nextCursor": null,
            "latestIndexedBlockHeight": "9",
            "oldestIndexedBlockHeight": "2",
        })),
    );

    let output = thruput()
        .args(["--json", "tx", "list", &address, "--rpc-url", &rpc_url])
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
    let address = thruput_encoding::encode(&[0x68; 32]);
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        serde_json::json!({
            "address": address,
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "transactions": [],
            "nextCursor": null,
            "latestIndexedBlockHeight": "9",
            "oldestIndexedBlockHeight": "2",
        })),
    );

    let output = thruput()
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
        let address = match target_kind {
            "contract" => thruput_encoding::encode(&[0x71; 32]),
            "direct" => thruput_encoding::encode(&[0x72; 32]),
            _ => advertised_dev_wallet_address(1),
        };
        let target = match target_kind {
            "contract" => "counter".to_owned(),
            "direct" => address.clone(),
            _ => "dev-1".to_owned(),
        };
        let (rpc_url, rpc_server) = spawn_tx_list_rpc(
            serde_json::json!({
                "address": address,
                "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
                "limit": 50,
            }),
            RpcReply::Result(serde_json::json!({
                "transactions": [],
                "nextCursor": null,
                "latestIndexedBlockHeight": "1",
                "oldestIndexedBlockHeight": "0",
            })),
        );
        match target_kind {
            "contract" => std::fs::write(
                tmp.path().join("thruput.toml"),
                format!("default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n[profile.default.contracts]\ncounter = \"{address}\"\n"),
            )
            .unwrap(),
            "direct" => std::fs::write(
                tmp.path().join("thruput.toml"),
                format!("default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n"),
            )
            .unwrap(),
            _ => {
                let manifest = write_runtime_manifest(tmp.path(), 1, &address, None);
                std::fs::write(
                    tmp.path().join("thruput.toml"),
                    format!(
                        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
                        manifest.file_name().unwrap().to_string_lossy()
                    ),
                )
                .unwrap();
            }
        }

        let output = thruput()
            .args(["--json", "tx", "list", &target])
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path().join("fixture-config"))
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
    let address = thruput_encoding::encode(&[0x81; 32]);
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
        let output = thruput()
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
        (
            "ZAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAZ4A",
            "invalid_address_target",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_no_request_rpc();
        let output = thruput()
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
        tmp.path().join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n[profile.default.contracts]\nbroken = \"0xdeadbeef\"\n",
    )
    .unwrap();
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = thruput()
        .args(["--json", "tx", "list", "broken", "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    rpc_server.join().unwrap();
    let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "invalid_address_target");
}

#[test]
fn tx_list_preserves_rpc_errors() {
    let address = thruput_encoding::encode(&[0x82; 32]);
    let query = serde_json::json!({
        "address": address,
        "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
        "limit": 50,
    });
    for code in [-32601, -32052, -32050, -32051, -32053] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) =
            spawn_tx_list_rpc(query.clone(), RpcReply::Error(code, "mock list error"));
        let output = thruput()
            .args(["--json", "tx", "list", &address, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "native_rpc_failure");
        assert_eq!(stderr["error"]["details"]["rpcError"]["code"], code);
        assert_eq!(
            stderr["error"]["details"]["rpcError"]["message"],
            "mock list error"
        );
    }
}

#[test]
fn tx_list_human_output_marks_sparse_page_and_reusable_cursor() {
    let tmp = TempDir::new().unwrap();
    let address = thruput_encoding::encode(&[0x83; 32]);
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        serde_json::json!({
            "address": address,
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "transactions": [],
            "nextCursor": "0xcafe",
            "latestIndexedBlockHeight": "20",
            "oldestIndexedBlockHeight": "3",
        })),
    );

    let output = thruput()
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
    let address = thruput_encoding::encode(&[0x84; 32]);
    std::fs::write(
        tmp.path().join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://127.0.0.1:1\"\n[profile.default.wallets.local-wallet]\naddress = \"{address}\"\nowner_public_key = \"0x{}\"\n",
            "86".repeat(32)
        ),
    )
    .unwrap();
    let tx_hash = thruput_encoding::encode(&[0x85; 32]);
    let (rpc_url, rpc_server) = spawn_tx_list_rpc(
        serde_json::json!({
            "address": address,
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "transactions": [{ "transactionHash": tx_hash, "blockHeight": "18" }],
            "nextCursor": null,
            "latestIndexedBlockHeight": "20",
            "oldestIndexedBlockHeight": "3",
        })),
    );

    let output = thruput()
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
    use thruput_contract_spec::{
        ContractEventDef, ContractField, ContractSpec, ContractSpecId, ContractSpecString,
        ContractType,
    };
    use xdr_common::VecM;

    let string = |value: &str| ContractSpecString::try_from(value).unwrap();
    // One crate, one module: the declared name is what this fixture turns on.
    let id = |declared: &str| ContractSpecId {
        crate_name: string("demo"),
        major: string("1"),
        module_path: string("demo"),
        declared: string(declared),
    };
    let spec = ContractSpec {
        interfaces: VecM(vec![]),
        type_defs: VecM(vec![]),
        events: VecM(vec![ContractEventDef {
            name: id("PingEvent"),
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
fn events_spec_selected_event_injects_filter_and_decodes_payload() {
    let tmp = TempDir::new().unwrap();
    let spec_path = write_event_spec(&tmp);
    let contract = thruput_encoding::encode(&[0x81; 32]);
    let signature = format!("0x{}", "91".repeat(32));
    let tag = format!("0x{}", "92".repeat(32));
    let data = format!("0x{}", hex::encode(xdrs::cxs::encode(&7_u64).unwrap()));
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "addresses": [contract],
                "topics": { "topic0": [signature, signature], "topic1": [], "topic2": [] },
            }],
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [{
                "transactionHash": thruput_encoding::encode(&[0x82; 32]),
                "blockHeight": "9",
                "sequenceNumber": 1,
                "event": {
                    "address": contract,
                    "topics": [signature, tag],
                    "data": data,
                },
            }],
            "nextCursor": null,
            "latestIndexedBlockHeight": "9",
            "oldestIndexedBlockHeight": "1",
        })),
    );

    let output = thruput()
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
    assert_eq!(stdout["data"]["schemaVersion"], 2);
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
    assert_eq!(event["decoded"]["fields"]["value"], "7");
    assert_eq!(event["data"], data);
}

#[test]
fn events_spec_human_output_uses_named_topics_without_raw_duplicates() {
    let tmp = TempDir::new().unwrap();
    let spec_path = write_event_spec(&tmp);
    let contract = thruput_encoding::encode(&[0x93; 32]);
    let signature = format!("0x{}", "91".repeat(32));
    let tag = format!("0x{}", "92".repeat(32));
    let data = format!("0x{}", hex::encode(xdrs::cxs::encode(&7_u64).unwrap()));
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "addresses": [contract],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [{
                "transactionHash": thruput_encoding::encode(&[0x94; 32]),
                "blockHeight": "9",
                "sequenceNumber": 1,
                "event": {
                    "address": contract,
                    "topics": [signature, tag],
                    "data": data,
                },
            }],
            "nextCursor": null,
            "latestIndexedBlockHeight": "9",
            "oldestIndexedBlockHeight": "1",
        })),
    );

    let output = thruput()
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
        &format!("txn: {}", thruput_encoding::encode(&[0x94; 32])),
        "decoded event: PingEvent",
        &format!("signature: {signature}"),
        &format!("tag: {tag}"),
        &format!("data: {data}"),
        "value: \"7\"",
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
    let contract = thruput_encoding::encode(&[0x83; 32]);
    std::fs::write(tmp.path().join("malformed.spec.xdr"), [0xff]).unwrap();
    std::fs::write(tmp.path().join("unsupported.spec.json"), b"{}").unwrap();
    std::fs::write(tmp.path().join("no-spec.rv32a"), b"rv32-artifact").unwrap();
    let cases = [
        (vec!["--event", "PingEvent"], "invalid_event_options", None),
        (
            vec!["--spec", "missing.spec.xdr"],
            "invalid_event_spec",
            None,
        ),
        (
            vec!["--spec", "malformed.spec.xdr"],
            "invalid_event_spec",
            None,
        ),
        (
            vec!["--spec", "unsupported.spec.json"],
            "invalid_event_spec",
            None,
        ),
        (
            vec!["--spec", "no-spec.rv32a"],
            "invalid_event_spec",
            Some("no-spec.spec.xdr"),
        ),
    ];

    for (options, code, expected_message) in cases {
        let (rpc_url, rpc_server) = spawn_no_request_rpc();
        let output = thruput()
            .args(["--json", "events", "list", &contract, "--rpc-url", &rpc_url])
            .args(options)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();
        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], code);
        if let Some(expected) = expected_message {
            assert!(
                stderr["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(expected)),
                "expected `{expected}` in: {stderr}"
            );
        }
    }

    let spec_path = write_event_spec(&tmp);
    let (rpc_url, rpc_server) = spawn_no_request_rpc();
    let output = thruput()
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
    let contract = thruput_encoding::encode(&[0x11; 32]);
    let emitter = thruput_encoding::encode(&[0x22; 32]);
    let tx_hash = thruput_encoding::encode(&[0x33; 32]);
    let topic = format!("0x{}", "44".repeat(32));
    let expected_query = serde_json::json!({
        "filters": [{
            "addresses": [contract],
            "topics": { "topic0": [], "topic1": [], "topic2": [] },
        }],
        "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
        "limit": 50,
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        expected_query,
        RpcReply::Result(serde_json::json!({
            "events": [{
                "transactionHash": tx_hash,
                "blockHeight": "90",
                "sequenceNumber": 3,
                "event": {
                    "address": emitter,
                    "topics": [topic],
                    "data": "0xdeadbeef",
                },
            }],
            "nextCursor": "0xaabb",
            "latestIndexedBlockHeight": "100",
            "oldestIndexedBlockHeight": "7",
        })),
    );

    let output = thruput()
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
                "schemaVersion": 2,
                "profile": "default",
                "rpcUrl": rpc_url,
                "contractInput": contract,
                "contractAddress": contract,
                "filters": { "topic0": [], "topic1": [], "topic2": [] },
                "spec": null,
                "events": [{
                    "txHash": tx_hash,
                    "blockHeight": "90",
                    "sequenceNumber": 3,
                    "contractAddress": emitter,
                    "topics": [topic],
                    "data": "0xdeadbeef",
                    "decodeStatus": "notRequested",
                    "decoded": null,
                }],
                "cursor": "0xaabb",
                "latestBlock": "100",
                "oldestBlock": "7",
            },
            "warnings": [],
        })
    );
}

#[test]
fn events_list_preserves_duplicate_returned_records_in_order() {
    let tmp = TempDir::new().unwrap();
    let contract = thruput_encoding::encode(&[0x45; 32]);
    let first = serde_json::json!({
        "transactionHash": thruput_encoding::encode(&[0x46; 32]),
        "blockHeight": "9",
        "sequenceNumber": 2,
        "event": { "address": contract, "topics": [], "data": "0xaaaa" },
    });
    let second = serde_json::json!({
        "transactionHash": thruput_encoding::encode(&[0x47; 32]),
        "blockHeight": "8",
        "sequenceNumber": 1,
        "event": { "address": contract, "topics": [], "data": "0xbbbb" },
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "addresses": [contract],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [first.clone(), second, first],
            "nextCursor": null,
            "latestIndexedBlockHeight": "9",
            "oldestIndexedBlockHeight": "0",
        })),
    );

    let output = thruput()
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
    let contract = thruput_encoding::encode(&[0x51; 32]);
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
                "addresses": [contract],
                "topics": expected_filters,
            }],
            "cursor": "0xabcd",
            "limit": 17,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [],
            "nextCursor": null,
            "latestIndexedBlockHeight": "8",
            "oldestIndexedBlockHeight": "1",
        })),
    );

    let output = thruput()
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
    let contract = thruput_encoding::encode(&[0x61; 32]);
    for explicit_override in [false, true] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "addresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
                "limit": 50,
            }),
            RpcReply::Result(serde_json::json!({
                "events": [],
                "nextCursor": null,
                "latestIndexedBlockHeight": "1",
                "oldestIndexedBlockHeight": "0",
            })),
        );
        let configured_url = if explicit_override {
            "http://127.0.0.1:1"
        } else {
            &rpc_url
        };
        std::fs::write(
            tmp.path().join("thruput.toml"),
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

        let output = thruput()
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
    let address = advertised_dev_wallet_address(7);
    let manifest = write_runtime_manifest(tmp.path(), 7, &address, None);
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "addresses": [address],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [],
            "nextCursor": null,
            "latestIndexedBlockHeight": "1",
            "oldestIndexedBlockHeight": "0",
        })),
    );
    std::fs::write(
        tmp.path().join("thruput.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            manifest.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = thruput()
        .args(["--json", "events", "list", "dev-7"])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("fixture-config"))
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
    let wallet_address = thruput_encoding::encode(&[0x72; 32]);
    let contract_address = thruput_encoding::encode(&[0x73; 32]);
    let owner = thruput_encoding::encode(&[0x74; 32]);

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
                    "addresses": [expected_address],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
                "limit": 50,
            }),
            RpcReply::Result(serde_json::json!({
                "events": [],
                "nextCursor": null,
                "latestIndexedBlockHeight": "1",
                "oldestIndexedBlockHeight": "0",
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
            tmp.path().join("thruput.toml"),
            format!(
                "[profile.default]\nrpc_url = \"{rpc_url}\"\n{wallet}[profile.default.contracts]\nshared = \"{contract_address}\"\n"
            ),
        )
        .unwrap();

        let output = thruput()
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
    let profile_address = advertised_dev_wallet_address(7);
    let explicit_signer = thruput_signing::derive_dev_account_signer("alternate-event-seed", 7);
    let explicit_address = thruput_encoding::encode(
        thruput_signing::PubkeyAccount::from_public_key_bytes(
            explicit_signer.verifying_key_bytes(),
            0,
        )
        .address()
        .as_ref(),
    );
    let profile_manifest = write_runtime_manifest(tmp.path(), 7, &profile_address, None);
    let explicit_manifest = write_runtime_manifest_with_seed(
        explicit_tmp.path(),
        7,
        &explicit_address,
        "alternate-event-seed",
        None,
    );
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "addresses": [explicit_address],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [],
            "nextCursor": null,
            "latestIndexedBlockHeight": "1",
            "oldestIndexedBlockHeight": "0",
        })),
    );
    std::fs::write(
        tmp.path().join("thruput.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\nruntime_manifest = \"{}\"\n",
            profile_manifest.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = thruput()
        .args([
            "--json",
            "events",
            "list",
            "dev-7",
            "--runtime-manifest",
            explicit_manifest.to_str().unwrap(),
        ])
        .current_dir(tmp.path())
        .env(
            "XDG_CONFIG_HOME",
            explicit_tmp.path().join("fixture-config"),
        )
        .output()
        .unwrap();
    rpc_server.join().unwrap();

    assert!(output.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["data"]["contractAddress"], explicit_address);
}

#[test]
fn events_list_rejects_invalid_inputs_before_rpc_with_structured_details() {
    let contract = thruput_encoding::encode(&[0x81; 32]);
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

        let output = thruput()
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
    let contract = thruput_encoding::encode(&[0x91; 32]);
    let event = serde_json::json!({
        "transactionHash": thruput_encoding::encode(&[0x92; 32]),
        "blockHeight": "2",
        "sequenceNumber": 0,
        "event": { "address": contract, "topics": [], "data": "0x" },
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "addresses": [contract],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 1,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [event.clone(), event],
            "nextCursor": null,
            "latestIndexedBlockHeight": "2",
            "oldestIndexedBlockHeight": "0",
        })),
    );

    let output = thruput()
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
    assert_eq!(stderr["error"]["code"], "native_rpc_failure");
    assert_eq!(
        stderr["error"]["message"],
        "native RPC: getEvents failed: invalid response: returned 2 events for requested limit 1"
    );
}

#[test]
fn events_list_rejects_oversized_event_shapes() {
    let contract = thruput_encoding::encode(&[0xa1; 32]);
    let topic = format!("0x{}", "a2".repeat(32));
    for (topics, data, expected_message) in [
        (
            vec![topic.clone(); 4],
            format!("0x{}", "a3".repeat(1025)),
            "at most three event topics".to_owned(),
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
                    "addresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
                "limit": 50,
            }),
            RpcReply::Result(serde_json::json!({
                "events": [{
                    "transactionHash": thruput_encoding::encode(&[0xa4; 32]),
                    "blockHeight": "12",
                    "sequenceNumber": 4,
                    "event": { "address": contract, "topics": topics, "data": data },
                }],
                "nextCursor": null,
                "latestIndexedBlockHeight": "12",
                "oldestIndexedBlockHeight": "0",
            })),
        );

        let output = thruput()
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
    let contract = thruput_encoding::encode(&[0xa5; 32]);
    let topics = ["a6", "a7", "a8"]
        .map(|byte| format!("0x{}", byte.repeat(32)))
        .to_vec();

    for (data_len, succeeds) in [(928, true), (929, false)] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "addresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
                "limit": 50,
            }),
            RpcReply::Result(serde_json::json!({
                "events": [{
                    "transactionHash": thruput_encoding::encode(&[0xa9; 32]),
                    "blockHeight": "12",
                    "sequenceNumber": 4,
                    "event": {
                        "address": contract,
                        "topics": topics,
                        "data": format!("0x{}", "aa".repeat(data_len)),
                    },
                }],
                "nextCursor": null,
                "latestIndexedBlockHeight": "12",
                "oldestIndexedBlockHeight": "0",
            })),
        );

        let output = thruput()
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
    let queried_address = thruput_encoding::encode(&[0xb2; 32]);
    let emitter = thruput_encoding::encode(&[0xb3; 32]);
    let tx_hash = thruput_encoding::encode(&[0xb4; 32]);
    let topics = ["b5", "b6", "b7"].map(|byte| format!("0x{}", byte.repeat(32)));
    let event = serde_json::json!({
        "transactionHash": tx_hash,
        "blockHeight": "88",
        "sequenceNumber": 6,
        "event": {
            "address": emitter,
            "topics": topics,
            "data": "0xdeadbeef",
        },
    });
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [{
                "addresses": [queried_address],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [event.clone(), event],
            "nextCursor": "0xcafe",
            "latestIndexedBlockHeight": "100",
            "oldestIndexedBlockHeight": "7",
        })),
    );
    std::fs::write(
        tmp.path().join("thruput.toml"),
        format!(
            "[profile.default]\nrpc_url = \"{rpc_url}\"\n[profile.default.contracts]\ncounter = \"{queried_address}\"\n"
        ),
    )
    .unwrap();

    let output = thruput()
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
    let contract = thruput_encoding::encode(&[0xb8; 32]);
    let rpc_url = "http://[bad\n\u{1b}[31m\u{202e}spoof";

    let output = thruput()
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
    let contract = thruput_encoding::encode(&[0xb1; 32]);
    for (request_cursor, response_cursor, expected) in [
        (None, None, "events: no events found; stream exhausted"),
        (
            None,
            Some("0xcafe"),
            "events: no displayable events in this page; continue with cursor",
        ),
        (
            Some("0xabcd"),
            None,
            "events: no events returned; no continuation cursor",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let mut query = serde_json::json!({
            "filters": [{
                "addresses": [contract],
                "topics": { "topic0": [], "topic1": [], "topic2": [] },
            }],
            "limit": 50,
        });
        match request_cursor {
            Some(cursor) => query["cursor"] = serde_json::json!(cursor),
            None => {
                query["start"] =
                    serde_json::json!({ "order": "descending", "anchor": { "kind": "atEnd" } });
            }
        }
        let (rpc_url, rpc_server) = spawn_events_rpc(
            query,
            RpcReply::Result(serde_json::json!({
                "events": [],
                "nextCursor": response_cursor,
                "latestIndexedBlockHeight": "9",
                "oldestIndexedBlockHeight": "2",
            })),
        );
        let mut args = vec!["events", "list", contract.as_str()];
        if let Some(cursor) = request_cursor {
            args.extend(["--cursor", cursor]);
        }
        args.extend(["--rpc-url", rpc_url.as_str()]);

        let output = thruput()
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
            response_cursor == Some("0xcafe")
        );
        if response_cursor == Some("0xcafe") {
            assert!(
                stderr.contains("\n\n  next cursor: 0xcafe"),
                "stderr: {stderr}"
            );
        }
    }
}

#[test]
fn events_list_preserves_rpc_errors() {
    let contract = thruput_encoding::encode(&[0xc1; 32]);
    for code in [-32601, -32052, -32050] {
        let tmp = TempDir::new().unwrap();
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": [{
                    "addresses": [contract],
                    "topics": { "topic0": [], "topic1": [], "topic2": [] },
                }],
                "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
                "limit": 50,
            }),
            RpcReply::Error(code, "mock event error"),
        );

        let output = thruput()
            .args(["--json", "events", "list", &contract, "--rpc-url", &rpc_url])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(!output.status.success());
        let stderr: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(stderr["error"]["code"], "native_rpc_failure");
        assert_eq!(stderr["error"]["details"]["rpcError"]["code"], code);
        assert_eq!(
            stderr["error"]["details"]["rpcError"]["message"],
            "mock event error"
        );
    }
}

fn transfer_signature() -> String {
    format!(
        "0x{}",
        hex::encode(*thruput_erc20_if::TransferEvent::SIGNATURE)
    )
}

fn mint_signature() -> String {
    format!("0x{}", hex::encode(*thruput_erc20_if::MintEvent::SIGNATURE))
}

fn burn_signature() -> String {
    format!("0x{}", hex::encode(*thruput_erc20_if::BurnEvent::SIGNATURE))
}

fn all_token_event_filters(token: &str) -> serde_json::Value {
    serde_json::json!(
        [transfer_signature(), mint_signature(), burn_signature()].map(
            |signature| serde_json::json!({
                "addresses": [token],
                "topics": { "topic0": [signature], "topic1": [], "topic2": [] },
            })
        )
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
        "transactionHash": thruput_encoding::encode(&hex::decode(tx_byte.repeat(32)).unwrap()),
        "blockHeight": block_height.to_string(),
        "sequenceNumber": sequence_number,
        "event": {
            "address": token,
            "topics": [signature, &address_topic(from), &address_topic(to)],
            "data": data,
        },
    })
}

fn token_supply_event_json(
    identity: (&str, u64, u32),
    token: &str,
    signature: &str,
    participant: &str,
    data: &str,
) -> serde_json::Value {
    let mut event = token_event_json(identity, [token, signature, participant, participant], data);
    event["event"]["topics"] = serde_json::json!([signature, address_topic(participant)]);
    event
}

fn address_topic(address: &str) -> String {
    format!(
        "0x{}",
        hex::encode(thruput_encoding::decode_fixed::<32>(address).unwrap())
    )
}

#[test]
fn token_transfers_json_queries_signature_and_preserves_decoded_and_malformed_records() {
    let tmp = TempDir::new().unwrap();
    let token = thruput_encoding::encode(&[0xa1; 32]);
    let from = thruput_encoding::encode(&[0xa2; 32]);
    let to = thruput_encoding::encode(&[0xa3; 32]);
    let signature = transfer_signature();
    let amount_data = format!("0x{}", hex::encode(xdrs::cxs::encode(&125_i128).unwrap()));
    let negative_data = format!("0x{}", hex::encode(xdrs::cxs::encode(&-1_i128).unwrap()));
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": all_token_event_filters(&token),
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [
                token_event_json(("a4", 12, 1), [&token, &signature, &from, &to], &amount_data),
                token_event_json(("a5", 11, 0), [&token, &signature, &from, &to], &negative_data),
                token_event_json(("a4", 12, 1), [&token, &signature, &from, &to], &amount_data),
                token_supply_event_json(("a6", 10, 0), &token, &mint_signature(), &to, &amount_data),
                token_supply_event_json(("a7", 9, 0), &token, &burn_signature(), &from, &amount_data),
            ],
            "nextCursor": "0xcafe",
            "latestIndexedBlockHeight": "12",
            "oldestIndexedBlockHeight": "2",
        })),
    );

    let output = thruput()
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
    assert_eq!(stdout["data"]["schemaVersion"], 4);
    assert_eq!(stdout["data"]["tokenInput"], token);
    assert_eq!(stdout["data"]["tokenAddress"], token);
    assert!(stdout["data"]["participantFilter"].is_null());
    assert_eq!(stdout["data"]["cursor"], "0xcafe");
    assert_eq!(stdout["data"]["latestBlock"], "12");
    assert_eq!(stdout["data"]["oldestBlock"], "2");
    let records = stdout["data"]["records"].as_array().unwrap();
    assert_eq!(records.len(), 5);
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
    assert_eq!(records[3]["transfer"]["kind"], "mint");
    assert_eq!(records[3]["transfer"]["from"], serde_json::Value::Null);
    assert_eq!(records[3]["transfer"]["to"], to);
    assert_eq!(records[4]["transfer"]["kind"], "burn");
    assert_eq!(records[4]["transfer"]["from"], from);
    assert_eq!(records[4]["transfer"]["to"], serde_json::Value::Null);
}

#[test]
fn token_transfers_account_uses_four_server_side_filters_in_one_request() {
    let tmp = TempDir::new().unwrap();
    let token = thruput_encoding::encode(&[0xb1; 32]);
    let account = thruput_encoding::encode(&[0xb2; 32]);
    let signature = transfer_signature();
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": [
                {
                    "addresses": [token],
                    "topics": { "topic0": [signature], "topic1": [address_topic(&account)], "topic2": [] },
                },
                {
                    "addresses": [token],
                    "topics": { "topic0": [signature], "topic1": [], "topic2": [address_topic(&account)] },
                },
                {
                    "addresses": [token],
                    "topics": { "topic0": [mint_signature()], "topic1": [address_topic(&account)], "topic2": [] },
                },
                {
                    "addresses": [token],
                    "topics": { "topic0": [burn_signature()], "topic1": [address_topic(&account)], "topic2": [] },
                },
            ],
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 7,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [],
            "nextCursor": null,
            "latestIndexedBlockHeight": "12",
            "oldestIndexedBlockHeight": "2",
        })),
    );

    let output = thruput()
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
    let token = thruput_encoding::encode(&[0xd1; 32]);
    let from = thruput_encoding::encode(&[0xd2; 32]);
    let to = thruput_encoding::encode(&[0xd3; 32]);
    for (flags, topic1, topic2, filter_kind) in [
        (
            vec!["--from", &from],
            vec![address_topic(&from)],
            vec![],
            "from",
        ),
        (vec!["--to", &to], vec![], vec![address_topic(&to)], "to"),
        (
            vec!["--from", &from, "--to", &to],
            vec![address_topic(&from)],
            vec![address_topic(&to)],
            "fromTo",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let signature = transfer_signature();
        let mut filters = vec![serde_json::json!({
            "addresses": [token],
            "topics": { "topic0": [signature], "topic1": topic1, "topic2": topic2 },
        })];
        if filter_kind != "fromTo" {
            let (supply_signature, participant) = if filter_kind == "from" {
                (burn_signature(), &from)
            } else {
                (mint_signature(), &to)
            };
            filters.push(serde_json::json!({
                "addresses": [token],
                "topics": { "topic0": [supply_signature], "topic1": [address_topic(participant)], "topic2": [] },
            }));
        }
        let (rpc_url, rpc_server) = spawn_events_rpc(
            serde_json::json!({
                "filters": filters,
                "cursor": "0x1234",
                "limit": 50,
            }),
            RpcReply::Result(serde_json::json!({
                "events": [],
                "nextCursor": null,
                "latestIndexedBlockHeight": "12",
                "oldestIndexedBlockHeight": "2",
            })),
        );
        let mut args = vec!["--json", "token", "transfers", &token];
        args.extend(flags);
        args.extend(["--cursor", "0x1234", "--rpc-url", &rpc_url]);

        let output = thruput()
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        rpc_server.join().unwrap();

        assert!(output.status.success());
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let filter = &stdout["data"]["participantFilter"];
        assert_eq!(filter["kind"], filter_kind);
        if filter_kind == "fromTo" {
            assert_eq!(filter["fromInput"], from);
            assert_eq!(filter["from"], from);
            assert_eq!(filter["toInput"], to);
            assert_eq!(filter["to"], to);
            assert!(filter.get("from_input").is_none(), "{filter}");
        }
    }
}

#[test]
fn token_transfers_human_output_preserves_participants_and_malformed_evidence() {
    let tmp = TempDir::new().unwrap();
    let token = thruput_encoding::encode(&[0xe1; 32]);
    let from = thruput_encoding::encode(&[0xe2; 32]);
    let to = thruput_encoding::encode(&[0xe3; 32]);
    let signature = transfer_signature();
    let amount_data = format!("0x{}", hex::encode(xdrs::cxs::encode(&125_i128).unwrap()));
    let malformed_data = "0xff";
    let (rpc_url, rpc_server) = spawn_events_rpc(
        serde_json::json!({
            "filters": all_token_event_filters(&token),
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": [
                {
                    "transactionHash": thruput_encoding::encode(&[0xe4; 32]),
                    "blockHeight": "12",
                    "sequenceNumber": 1,
                    "event": {
                        "address": token,
                        "topics": [signature, address_topic(&from), address_topic(&to)],
                        "data": amount_data,
                    },
                },
                {
                    "transactionHash": thruput_encoding::encode(&[0xe5; 32]),
                    "blockHeight": "11",
                    "sequenceNumber": 2,
                    "event": {
                        "address": token,
                        "topics": [mint_signature(), address_topic(&to)],
                        "data": amount_data,
                    },
                },
                {
                    "transactionHash": thruput_encoding::encode(&[0xe6; 32]),
                    "blockHeight": "10",
                    "sequenceNumber": 1,
                    "event": {
                        "address": token,
                        "topics": [burn_signature(), address_topic(&from)],
                        "data": amount_data,
                    },
                },
                {
                    "transactionHash": thruput_encoding::encode(&[0xe7; 32]),
                    "blockHeight": "9",
                    "sequenceNumber": 0,
                    "event": {
                        "address": token,
                        "topics": [signature, address_topic(&from), address_topic(&to)],
                        "data": malformed_data,
                    },
                },
            ],
            "nextCursor": null,
            "latestIndexedBlockHeight": "12",
            "oldestIndexedBlockHeight": "2",
        })),
    );

    let output = thruput()
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
    assert!(
        stderr.contains(&format!("txn: {}", thruput_encoding::encode(&[0xe4; 32]))),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("raw amount: 125"), "stderr: {stderr}");
    assert!(!stderr.contains("\"transfer\""), "stderr: {stderr}");
    assert!(stderr.contains("\nmint\n"), "stderr: {stderr}");
    assert!(stderr.contains("\nburn\n"), "stderr: {stderr}");
    assert!(
        stderr.contains(&format!("from: {from}")),
        "stderr: {stderr}"
    );
    assert!(stderr.contains(&format!("to: {to}")), "stderr: {stderr}");
    assert!(
        !stderr.contains(&format!("0x{}", "00".repeat(32))),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("warning: malformed transfer event at height 9 sequence 0"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("data: 0xff"), "stderr: {stderr}");
}

#[test]
fn token_transfers_human_output_distinguishes_empty_page_states() {
    let token = thruput_encoding::encode(&[0xf1; 32]);
    for (request_cursor, response_cursor, expected) in [
        (None, None, "transfers: no events found; stream exhausted"),
        (
            None,
            Some("0xcafe"),
            "transfers: no displayable events in this page; continue with cursor",
        ),
        (
            Some("0xabcd"),
            None,
            "transfers: no events returned; no continuation cursor",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let mut query = serde_json::json!({
            "filters": all_token_event_filters(&token),
            "limit": 50,
        });
        match request_cursor {
            Some(cursor) => query["cursor"] = serde_json::json!(cursor),
            None => {
                query["start"] =
                    serde_json::json!({ "order": "descending", "anchor": { "kind": "atEnd" } });
            }
        }
        let (rpc_url, rpc_server) = spawn_events_rpc(
            query,
            RpcReply::Result(serde_json::json!({
                "events": [],
                "nextCursor": response_cursor,
                "latestIndexedBlockHeight": "9",
                "oldestIndexedBlockHeight": "2",
            })),
        );
        let mut args = vec!["token", "transfers", token.as_str()];
        if let Some(cursor) = request_cursor {
            args.extend(["--cursor", cursor]);
        }
        args.extend(["--rpc-url", rpc_url.as_str()]);

        let output = thruput()
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
            response_cursor == Some("0xcafe")
        );
        if response_cursor == Some("0xcafe") {
            assert!(
                stderr.contains("\n\n  next cursor: 0xcafe"),
                "stderr: {stderr}"
            );
        }
    }
}

#[test]
fn token_transfers_rejects_account_conflicts_before_rpc() {
    let token = thruput_encoding::encode(&[0xc1; 32]);
    let account = thruput_encoding::encode(&[0xc2; 32]);
    let output = thruput()
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
    let token = thruput_encoding::encode(&[0xc1; 32]);
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
            "previous `thruput token transfers` page",
        ),
    ] {
        let output = thruput()
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

/// A scripted JSON-RPC answer.
#[derive(Clone)]
enum RpcReply {
    Result(serde_json::Value),
    Error(i32, &'static str),
}

impl RpcReply {
    fn into_result(self) -> Result<serde_json::Value, ErrorObjectOwned> {
        match self {
            Self::Result(value) => Ok(value),
            Self::Error(code, message) => Err(rpc_error(code, message)),
        }
    }
}

fn spawn_tx_list_rpc(
    expected_query: serde_json::Value,
    response: RpcReply,
) -> (String, MockServer) {
    let server = rpc_mock()
        .method("getTransactionsByRootAddress", move |_| {
            response.clone().into_result()
        })
        .start();
    recorded_mock(server, move |script| {
        assert_eq!(
            script.expect("getTransactionsByRootAddress"),
            expected_query
        );
    })
}

fn spawn_events_rpc(expected_query: serde_json::Value, response: RpcReply) -> (String, MockServer) {
    let server = rpc_mock()
        .method("getEvents", move |_| response.clone().into_result())
        .start();
    recorded_mock(server, move |script| {
        assert_eq!(script.expect("getEvents"), expected_query);
    })
}

fn spawn_tx_show_rpc(
    tx_hash: &str,
    status: RpcReply,
    receipt: Option<RpcReply>,
) -> (String, MockServer) {
    let receipt = receipt.unwrap_or(RpcReply::Result(serde_json::Value::Null));
    let server = rpc_mock()
        .method("getTransactionStatus", move |_| {
            status.clone().into_result()
        })
        .method("getTransactionReceipt", move |_| {
            receipt.clone().into_result()
        })
        .start();
    let expected = serde_json::json!({"transactionHash":tx_hash});
    recorded_mock(server, move |script| {
        assert_eq!(script.expect("getTransactionStatus"), expected);
        assert_eq!(script.expect("getTransactionReceipt"), expected);
    })
}

/// Network hash the test profiles declare in `thruput.toml`.
const TEST_NETWORK_HASH: &str = "4Ss5JMkXAD9Z7cktFEdrqeMuT6jGMF1pVozTyPHZ6zT4";

/// Read one HTTP POST request from a TCP stream and return the parsed JSON body,
/// answering any version probe that precedes it.
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
    if request["method"] == "getVersionInfo" {
        assert_eq!(request["params"], serde_json::json!({}));
        send_jsonrpc_response(stream, &request["id"], version_info());
        return read_jsonrpc_request(listener, deadline, label);
    }
    (stream, request)
}

fn spawn_network_descriptor(rpc_url: &str) -> (String, MockServer) {
    spawn_network_descriptor_with_hash(rpc_url, TEST_NETWORK_HASH)
}

fn spawn_network_descriptor_with_hash(rpc_url: &str, network_hash: &str) -> (String, MockServer) {
    let descriptor = serde_json::json!({
        "schemaVersion": thruput_json_rpc_types::network_descriptor::NETWORK_DESCRIPTOR_SCHEMA_VERSION,
        "kind": "groundhog-network",
        "networkId": "devnet",
        "networkHash": network_hash,
        "protocol": "hogp/test00000000000",
        "genesisHash": thruput_json_rpc_types::NetworkHashText::from([0x42; 32]),
        "endpoints": { "rpcUrl": rpc_url },
        "nativeToken": {
            "tokenAddress": thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from([0x61; 32])),
            "decimals": 7,
            "transferEntrypoint": "0x63285b3523f9dc3f"
        },
        "faucet": { "walletAddress": thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from([0x71; 32])) },
        "airdrop": {
            "rawAmount": "25000000",
            "recipientCooldownSeconds": "60",
            "clientQuotaWindowSeconds": "3600",
            "clientQuotaMax": 5,
            "maxPendingPerRecipient": 1
        },
    });
    let server = RecordingServer::builder()
        .method("networkDescriptor", move |_| Ok(descriptor.clone()))
        .get_route("/v1/network", "networkDescriptor")
        .start();
    recorded_mock(server, |script| {
        script.expect("networkDescriptor");
    })
}

fn spawn_network_info_rpc(network_hash: &str) -> (String, MockServer) {
    let network_hash = network_hash.to_owned();
    let server = rpc_mock()
        .method("getNetworkInfo", move |_| Ok(network_info(&network_hash)))
        .start();
    recorded_mock(server, |script| {
        script.expect("getNetworkInfo");
    })
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

fn spawn_chain_tip_rpc(height: i64) -> (String, MockServer) {
    let server = rpc_mock()
        .method("getNetworkInfo", |_| Ok(network_info(TEST_NETWORK_HASH)))
        .method("getChainTip", move |_| Ok(chain_tip(height)))
        .start();
    recorded_mock(server, |script| {
        while script.accept("getNetworkInfo").is_some() {}
        // Nothing may follow: dry-run must not broadcast.
        script.expect("getChainTip");
    })
}

fn spawn_airdrop_rpc(recipient: &str) -> (String, MockServer) {
    spawn_airdrop_rpc_outcome(recipient, None)
}

// None is an immediate success; Some selects pending with/without a known hash.
fn spawn_airdrop_rpc_outcome(recipient: &str, pending_hash: Option<bool>) -> (String, MockServer) {
    spawn_airdrop_rpc_response(recipient, pending_hash, None)
}

fn spawn_airdrop_rpc_response(
    recipient: &str,
    pending_hash: Option<bool>,
    override_response: Option<serde_json::Value>,
) -> (String, MockServer) {
    let recipient = recipient.to_owned();
    let hash = thruput_encoding::encode(&[0x22; 32]);
    let response = override_response.unwrap_or_else(|| match pending_hash {
        None => serde_json::json!({"attemptStatus":"succeeded","recipient":recipient,
            "rawAmount":"25000000","transactionHash":hash,"blockHeight":"42",
            "execution":{"result":"return","returnData":"0x"}}),
        Some(true) => serde_json::json!({"attemptStatus":"pending","recipient":recipient,
            "rawAmount":"25000000","transactionHash":hash,"retryAfterSeconds":"2"}),
        Some(false) => serde_json::json!({"attemptStatus":"pending","recipient":recipient,
            "rawAmount":"25000000","retryAfterSeconds":"2"}),
    });
    let receipt = serde_json::json!({
        "transactionHash":hash, "blockHeight":"42", "gasUsed":"1", "events":[],
        "execution":{"result":"return","returnData":"0x"},
    });
    let server = rpc_mock()
        .method("getNetworkInfo", |_| Ok(network_info(TEST_NETWORK_HASH)))
        .method("faucet_requestAirdrop", move |_| Ok(response.clone()))
        .method("getTransactionReceipt", move |_| Ok(receipt.clone()))
        .start();
    recorded_mock(server, move |script| {
        script.expect("getNetworkInfo");
        script.expect("getNetworkInfo");
        let params = script.expect("faucet_requestAirdrop");
        let mut fields = params
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        fields.sort_unstable();
        assert_eq!(fields, ["clientNonce", "recipient"]);
        assert_eq!(params["recipient"], recipient);
        assert_eq!(params["clientNonce"].as_str().map(str::len), Some(34));
        if pending_hash == Some(true) {
            assert_eq!(
                script.expect("getTransactionReceipt")["transactionHash"],
                hash
            );
        }
    })
}

/// How the local node answers the CLI's `dev_fundAccount` request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalFunding {
    /// Funds the recipient; the CLI reads the receipt and both balances.
    Confirmed,
    /// Acknowledges the funding transaction but cannot read its receipt.
    ReceiptUnavailable,
    /// Returns evidence the CLI must not journal: undecodable signed XDR
    /// (`"xdr"`), a hash that does not match it (`"hash"`), or a retained
    /// funding error (`"error"`).
    InvalidEvidence(&'static str),
}

/// Local airdrop endpoint that funds `recipient` successfully.
fn spawn_local_airdrop_rpc(faucet: &str, recipient: &str) -> (String, MockServer) {
    let (server, verify) = local_funding_rpc(faucet, recipient, LocalFunding::Confirmed);
    recorded_mock(server, move |script| {
        verify(script);
    })
}

/// Local airdrop endpoint whose funding ends as `outcome` describes. Joining
/// returns the transaction the node signed.
fn spawn_local_funding_rpc(
    faucet: &str,
    recipient: &str,
    outcome: LocalFunding,
) -> (
    String,
    MockServer<thruput_xdr::transaction::SignedTransaction>,
) {
    let (server, verify) = local_funding_rpc(faucet, recipient, outcome);
    recorded_mock(server, verify)
}

/// The faucet balance preflight, `dev_fundAccount`, and whatever `outcome`
/// leads the CLI to read next, with the check of those requests.
fn local_funding_rpc(
    faucet: &str,
    recipient: &str,
    outcome: LocalFunding,
) -> (
    RecordingServer,
    impl FnOnce(&mut RpcScript) -> thruput_xdr::transaction::SignedTransaction + Send + 'static,
) {
    let signed_tx = local_funding_transaction(recipient);
    let tx_hash = thruput_encoding::encode(
        thruput_signing::transaction_hash(&signed_tx.tx)
            .unwrap()
            .as_ref(),
    );
    let funding = local_funding_response(outcome, &signed_tx, &tx_hash, recipient);
    let receipt = if outcome == LocalFunding::ReceiptUnavailable {
        Err(rpc_error(-32050, "receipt read unavailable"))
    } else {
        Ok(
            serde_json::json!({ "transactionHash":tx_hash,"blockHeight":"43","gasUsed":"1500","execution":{"result":"return","returnData":"0x"},"events":[] }),
        )
    };
    let faucet_rpc = rpc_address_from_hex_fixture(faucet);
    let recipient_rpc = rpc_address_from_hex_fixture(recipient);
    let balances = Mutex::new(std::collections::VecDeque::from([
        (faucet_rpc.clone(), "200012006", 41),
        (faucet_rpc.clone(), "99999999", 43),
        (recipient_rpc.clone(), "100000000", 43),
    ]));
    let server = rpc_mock()
        .method("getBalance", move |_| {
            let (address, raw_balance, height) = balances
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| rpc_error(-32603, "no balance read scripted"))?;
            Ok(serde_json::json!({
                "address": address,
                "rawBalance": raw_balance,
                "stateBlockHeight": height.to_string(),
            }))
        })
        .method("getNetworkInfo", |_| Ok(network_info(TEST_NETWORK_HASH)))
        .method("dev_fundAccount", move |_| funding.clone())
        .method("getTransactionReceipt", move |_| receipt.clone())
        .start();
    let faucet = faucet.to_owned();
    let recipient = recipient.to_owned();
    let verify = move |script: &mut RpcScript| {
        assert_eq!(script.expect("getBalance")["address"], faucet_rpc);
        script.expect("getNetworkInfo");
        let params = script.expect("dev_fundAccount");
        assert_eq!(params["faucet"], faucet);
        assert_eq!(params["recipient"], recipient);
        assert_eq!(params["rawAmount"], "100000000");
        if !matches!(outcome, LocalFunding::InvalidEvidence(_)) {
            assert_eq!(
                script.expect("getTransactionReceipt")["transactionHash"],
                tx_hash
            );
        }
        if outcome == LocalFunding::Confirmed {
            assert_local_funding_transaction(&signed_tx, &faucet, &recipient);
            for address in [&faucet_rpc, &recipient_rpc] {
                assert_eq!(script.expect("getBalance")["address"], *address);
            }
        }
        signed_tx
    };
    (server, verify)
}

/// The node's `dev_fundAccount` answer for `outcome`.
fn local_funding_response(
    outcome: LocalFunding,
    signed_tx: &thruput_xdr::transaction::SignedTransaction,
    tx_hash: &str,
    recipient: &str,
) -> Result<serde_json::Value, ErrorObjectOwned> {
    if outcome == LocalFunding::InvalidEvidence("error") {
        return Err(ErrorObjectOwned::owned(
            -32031,
            "retained funding",
            Some(serde_json::json!({"kind":"localFundingUnresolved",
                "transactionHash":tx_hash, "signedTransaction":"0x00"})),
        ));
    }
    let response_hash = if outcome == LocalFunding::InvalidEvidence("hash") {
        thruput_encoding::encode(&[0x91; 32])
    } else {
        tx_hash.to_owned()
    };
    let signed_bytes = if outcome == LocalFunding::InvalidEvidence("xdr") {
        "0x00".into()
    } else {
        format!("0x{}", hex::encode(xdrs::txs::encode(signed_tx).unwrap()))
    };
    Ok(serde_json::json!({
        "recipient":recipient, "rawAmount":"100000000", "transactionHash":response_hash,
        "signedTransaction": signed_bytes,
    }))
}

/// Check the faucet's funding transfer moves 10 native tokens to `recipient`
/// under the faucet's own signature.
fn assert_local_funding_transaction(
    signed_tx: &thruput_xdr::transaction::SignedTransaction,
    faucet: &str,
    recipient: &str,
) {
    assert_eq!(
        signed_tx.tx.invocation.entrypoint,
        thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT
    );
    let (target, entrypoint, calldata) = decode_wallet_action(signed_tx);
    assert_eq!(
        target,
        thruput_encoding::encode(thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN.as_ref())
    );
    assert_eq!(
        entrypoint,
        format!(
            "0x{}",
            hex::encode(thruput_signing::NATIVE_TOKEN_TRANSFER_ENTRYPOINT.as_ref())
        )
    );
    let (actual_recipient, amount): (thruput_xdr::types::ContractAddress, u128) =
        xdrs::cxs::decode(&calldata).unwrap();
    assert_eq!(
        thruput_encoding::encode(actual_recipient.as_ref()),
        recipient
    );
    assert_eq!(amount, 100_000_000);
    assert_eq!(
        thruput_encoding::encode(signed_tx.tx.invocation.invoked_address.as_ref()),
        faucet
    );
    let sponsor = thruput_signing::derive_default_dev_account_signer(0);
    let network_hash = thruput_json_rpc_types::NetworkHashText::try_from(TEST_NETWORK_HASH)
        .unwrap()
        .to_network_hash();
    let expected = thruput_signing::sign_transaction(
        signed_tx.tx.clone(),
        &sponsor,
        thruput_signing::SignTransactionParams::default(),
        &network_hash,
    )
    .unwrap();
    assert_eq!(signed_tx.signatures, expected.signatures);
}

/// The default-wallet transfer the local faucet signs to fund `recipient`.
fn local_funding_transaction(recipient: &str) -> thruput_xdr::transaction::SignedTransaction {
    let sponsor = thruput_signing::derive_default_dev_account_signer(0);
    let network = thruput_json_rpc_types::NetworkHashText::try_from(TEST_NETWORK_HASH)
        .unwrap()
        .to_network_hash();
    let account =
        thruput_signing::PubkeyAccount::from_public_key_bytes(sponsor.verifying_key_bytes(), 0);
    let recipient_address = thruput_signing::strkey::parse_contract_address(recipient).unwrap();
    let calldata = xdrs::cxs::encode(&(recipient_address, 100_000_000u128)).unwrap();
    let tx = thruput_signing::default_wallet::build_default_wallet_call_transaction(
        thruput_signing::default_wallet::BuildDefaultWalletCallTransaction {
            account: &account,
            target_address: thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN,
            target_entrypoint: thruput_signing::NATIVE_TOKEN_TRANSFER_ENTRYPOINT,
            target_calldata: &calldata,
            gas_limit: thruput_signing::DEFAULT_TX_GAS_LIMIT * 2,
            target_gas_limit: thruput_signing::DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: 100,
            nonce: 42,
        },
    )
    .unwrap();
    thruput_signing::sign_transaction(
        tx,
        &sponsor,
        thruput_signing::SignTransactionParams::default(),
        &network,
    )
    .unwrap()
}

fn serve_balance(
    listener: &TcpListener,
    deadline: Instant,
    address: &str,
    raw_balance: &str,
    height: i64,
) {
    let address = rpc_address_from_hex_fixture(address);
    let (stream, request) = read_jsonrpc_request(listener, deadline, "getBalance");
    assert_eq!(request["method"], "getBalance");
    assert_eq!(request["params"]["address"], address);
    send_jsonrpc_response(
        stream,
        &request["id"],
        serde_json::json!({
            "address": address,
            "rawBalance": raw_balance,
            "stateBlockHeight": height.to_string(),
        }),
    );
}

fn serve_chain_tip(listener: &TcpListener, deadline: Instant, height: i64) {
    let (mut stream, mut request) = read_jsonrpc_request(listener, deadline, "chainTip");
    if request["method"] == "getNetworkInfo" {
        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({"networkHash":TEST_NETWORK_HASH,"genesisHash":thruput_json_rpc_types::NetworkHashText::from([0x42; 32])}),
        );
        (stream, request) = read_jsonrpc_request(listener, deadline, "chainTip");
    }
    assert_eq!(request["method"], "getChainTip");
    send_jsonrpc_response(
        stream,
        &request["id"],
        serde_json::json!({
            "blockHeight": height.to_string(),
            "blockHash": "1".repeat(32),
        }),
    );
}

fn spawn_balance_then_chain_tip_rpc(
    address: &str,
    raw_balance: &str,
    height: i64,
) -> (String, MockServer) {
    let address = rpc_address_from_hex_fixture(address);
    let response = serde_json::json!({
        "address": address,
        "rawBalance": raw_balance,
        "stateBlockHeight": height.to_string(),
    });
    let server = rpc_mock()
        .method("getBalance", move |_| Ok(response.clone()))
        .start();
    recorded_mock(server, move |script| {
        assert_eq!(script.expect("getBalance")["address"], address);
    })
}

fn spawn_no_request_rpc() -> (String, MockServer) {
    recorded_mock(RecordingServer::builder().start(), |_| {})
}

#[derive(Clone, Copy)]
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
        execution: serde_json::Value,
        events: serde_json::Value,
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
    signed_transaction: String,
}

struct TransferSubmission {
    hash: String,
    signed_transaction: String,
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
    let (rpc_url, rpc_server) = spawn_transfer_rpc(mock, sender.clone());
    let effective_rpc_url = format!("{rpc_url}{rpc_url_suffix}");
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n",
            manifest_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();
    let recipient = thruput_encoding::encode(&[0xab; 32]);
    let mut command = thruput();
    if json {
        command.arg("--json");
    }
    command.args(["transfer", &recipient, "1.5", "--force"]);
    if !rpc_url_suffix.is_empty() {
        command.args(["--rpc-url", &effective_rpc_url]);
    }
    let output = command
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", project.join("fixture-config"))
        .env("THRUPUT_TEST_RECEIPT_POLL_ATTEMPTS", "1")
        .env("THRUPUT_TEST_RECEIPT_POLL_INTERVAL_MS", "100")
        .output()
        .unwrap();
    let submission = rpc_server.join().unwrap();
    TransferMockRun {
        output,
        local_hash: submission.hash,
        signed_transaction: submission.signed_transaction,
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
    let primary = thruput_encoding::encode(&[0xa1; 32]);
    let trading = thruput_encoding::encode(&[0xb2; 32]);
    let expected_sender = if explicit_wallet.is_some() {
        trading.clone()
    } else {
        primary.clone()
    };
    let (rpc_url, rpc_server) = spawn_transfer_rpc(mock, expected_sender);
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nnetwork_hash = \"{TEST_NETWORK_HASH}\"\ndefault_key = \"alice\"\ndefault_wallet = \"primary\"\n\n[profile.default.wallets.primary]\naddress = \"{primary}\"\nowner_public_key = \"{owner}\"\n\n[profile.default.wallets.trading]\naddress = \"{trading}\"\nowner_public_key = \"{owner}\"\n"
        ),
    )
    .unwrap();
    let recipient = thruput_encoding::encode(&[0xab; 32]);
    let mut command = thruput();
    command.args(["--json", "transfer", &recipient, "1.5", "--force"]);
    if let Some(wallet) = explicit_wallet {
        command.args(["--from", wallet]);
    }
    let output = command
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("THRUPUT_TEST_RECEIPT_POLL_ATTEMPTS", "1")
        .env("THRUPUT_TEST_RECEIPT_POLL_INTERVAL_MS", "100")
        .output()
        .unwrap();
    let submission = rpc_server.join().unwrap();
    TransferMockRun {
        output,
        local_hash: submission.hash,
        signed_transaction: submission.signed_transaction,
    }
}

fn read_after_network_check(
    listener: &TcpListener,
    deadline: Instant,
    label: &str,
) -> (std::net::TcpStream, serde_json::Value) {
    let (stream, request) = read_jsonrpc_request(listener, deadline, label);
    if request["method"] == "getNetworkInfo" {
        assert_eq!(request["params"], serde_json::json!({}));
        send_jsonrpc_response(
            stream,
            &request["id"],
            serde_json::json!({"networkHash":TEST_NETWORK_HASH}),
        );
        read_jsonrpc_request(listener, deadline, label)
    } else {
        (stream, request)
    }
}

fn spawn_transfer_rpc(
    mock: TransferRpcMock,
    expected_sender: String,
) -> (String, MockServer<TransferSubmission>) {
    let accepted = match mock.submit {
        TransferSubmitMock::DropResponse => return spawn_dropped_transfer_rpc(expected_sender),
        TransferSubmitMock::MatchingHash => true,
        _ => false,
    };
    let poll = mock.poll.filter(|_| accepted);
    let submitted = Arc::new(Mutex::new(String::new()));
    let mut server = rpc_mock()
        .method("getNetworkInfo", |_| Ok(network_info(TEST_NETWORK_HASH)))
        .method("getChainTip", |_| Ok(chain_tip(42)));
    server = server.method("sendTransaction", {
        let submitted = Arc::clone(&submitted);
        let submit = mock.submit;
        move |params| {
            let local_hash = submitted_transaction_hash(params)?;
            submitted.lock().unwrap().clone_from(&local_hash);
            match submit {
                TransferSubmitMock::MatchingHash => Ok(
                    serde_json::json!({"transactionHash":local_hash,"admission":"accepted"}),
                ),
                TransferSubmitMock::MismatchedHash => Ok(
                    serde_json::json!({"transactionHash":thruput_encoding::encode(&[0xff;32]),"admission":"accepted"}),
                ),
                TransferSubmitMock::Error(code) => Err(rpc_error(code, "mock send error")),
                TransferSubmitMock::ErrorWithMessage(code, message) => {
                    Err(rpc_error(code, message))
                }
                TransferSubmitMock::DropResponse => unreachable!("served on a raw socket"),
            }
        }
    });
    if let Some(poll) = poll.clone() {
        let status = match &poll {
            TransferPollMock::Status(status) => Some(status.clone()),
            _ => None,
        };
        server = server.method("getTransactionReceipt", move |_| {
            let local_hash = submitted.lock().unwrap().clone();
            match &poll {
                TransferPollMock::Receipt {
                    execution,
                    events,
                    mismatched_hash,
                } => Ok(serde_json::json!({
                    "transactionHash": if *mismatched_hash {
                        thruput_encoding::encode(&[0xee; 32])
                    } else {
                        local_hash
                    },
                    "blockHeight": "43",
                    "execution": execution,
                    "events": events,
                    "gasUsed": "1500",
                })),
                TransferPollMock::Status(_) => Ok(serde_json::Value::Null),
                TransferPollMock::ReceiptError(code) => Err(rpc_error(*code, "mock receipt error")),
            }
        });
        if let Some(status) = status {
            server = server.method("getTransactionStatus", move |_| Ok(status.clone()));
        }
    }
    recorded_mock(server.start(), move |script| {
        script.accept("getNetworkInfo");
        script.expect("getChainTip");
        script.network_check();
        let params = script.expect("sendTransaction");
        let submission = assert_transfer_submission(
            params["signedTransaction"].as_str().unwrap(),
            &expected_sender,
        );
        if let Some(poll) = poll {
            assert_eq!(
                script.expect("getTransactionReceipt")["transactionHash"],
                submission.hash
            );
            if let TransferPollMock::Status(_) = poll {
                assert_eq!(
                    script.expect("getTransactionStatus")["transactionHash"],
                    submission.hash
                );
            }
        }
        submission
    })
}

/// Transfer endpoint that drops the connection instead of answering
/// `sendTransaction`, leaving the CLI unable to tell whether the node admitted
/// the transaction. A JSON-RPC server always answers, so this stays on a raw
/// socket.
fn spawn_dropped_transfer_rpc(expected_sender: String) -> (String, MockServer<TransferSubmission>) {
    let listener = MockListener::bind();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let handle = spawn_mock(move || {
        let deadline = Instant::now() + CLI_ACTION_TIMEOUT;
        serve_chain_tip(&listener, deadline, 42);
        let (stream, request) = read_after_network_check(&listener, deadline, "sendTransaction");
        assert_eq!(request["method"], "sendTransaction");
        let submission = assert_transfer_submission(
            request["params"]["signedTransaction"].as_str().unwrap(),
            &expected_sender,
        );
        drop(stream);
        assert_no_follow_up_rpc(&listener);
        submission
    });
    (format!("http://{addr}"), handle)
}

/// Check that a submitted transfer sends 1.5 native tokens from
/// `expected_sender` to the fixture recipient, and return its local hash.
fn assert_transfer_submission(signed_hex: &str, expected_sender: &str) -> TransferSubmission {
    let signed_tx = decode_signed_tx(signed_hex);
    let local_hash = thruput_encoding::encode(
        thruput_signing::transaction_hash(&signed_tx.tx)
            .unwrap()
            .as_ref(),
    );
    assert_eq!(
        thruput_encoding::encode(signed_tx.tx.invocation.invoked_address.as_ref()),
        expected_sender
    );
    let (target, entrypoint, calldata) = decode_wallet_action(&signed_tx);
    assert_eq!(
        target,
        thruput_encoding::encode(thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN.as_ref())
    );
    assert_eq!(entrypoint, "0x63285b3523f9dc3f");
    let (recipient, raw_amount): (thruput_xdr::types::ContractAddress, u128) =
        xdrs::cxs::decode(&calldata).unwrap();
    assert_eq!(
        format!("0x{}", hex::encode(recipient.as_ref())),
        format!("0x{}", "ab".repeat(32))
    );
    assert_eq!(raw_amount, 15_000_000);
    TransferSubmission {
        hash: local_hash,
        signed_transaction: signed_hex.to_owned(),
    }
}

/// A mock-server listener whose port is never handed back to the OS.
///
/// The OS recycles an ephemeral port as soon as its listener is dropped, and
/// these integration tests run concurrently against a `thruput` child
/// process each. When one test's mock finishes, its port can immediately be
/// reassigned to another test's mock while the first test's child is still
/// retrying — that stray request then arrives as the second mock's "first"
/// request and fails a test that never made it.
///
/// Retiring the listener instead of closing it keeps the port out of
/// circulation for the rest of the run. A reaper thread accepts and
/// immediately closes anything that still arrives, so a straggler sees the
/// connection close rather than hanging on an unanswered socket.
struct MockListener(Option<TcpListener>);

impl MockListener {
    fn bind() -> Self {
        Self(Some(TcpListener::bind("127.0.0.1:0").unwrap()))
    }
}

impl std::ops::Deref for MockListener {
    type Target = TcpListener;

    fn deref(&self) -> &TcpListener {
        self.0
            .as_ref()
            .expect("bug: listener retired while still in use")
    }
}

impl Drop for MockListener {
    fn drop(&mut self) {
        if let Some(listener) = self.0.take() {
            retire_listener(listener);
        }
    }
}

fn retired_listeners() -> &'static Mutex<Vec<TcpListener>> {
    static RETIRED: OnceLock<Mutex<Vec<TcpListener>>> = OnceLock::new();
    RETIRED.get_or_init(|| Mutex::new(Vec::new()))
}

fn retire_listener(listener: TcpListener) {
    static REAPER: Once = Once::new();
    REAPER.call_once(|| {
        thread::spawn(|| {
            loop {
                for retired in retired_listeners().lock().unwrap().iter() {
                    // Retired listeners stay non-blocking, so this drains
                    // whatever is pending and then yields.
                    while let Ok((stream, _)) = retired.accept() {
                        drop(stream);
                    }
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
    });
    retired_listeners().lock().unwrap().push(listener);
}

#[test]
fn a_retired_mock_listener_keeps_its_port_out_of_circulation() {
    let addr = {
        let listener = MockListener::bind();
        listener.set_nonblocking(true).unwrap();
        listener.local_addr().unwrap()
    };

    // The guard has been dropped. If the port had gone back to the OS another
    // test's MockListener::bind() could be handed it while a child process is
    // still retrying against it, which is the race this guards against.
    assert!(
        TcpListener::bind(addr).is_err(),
        "retired port {addr} was returned to the OS"
    );

    // Stragglers are accepted and closed rather than left hanging.
    let straggler = std::net::TcpStream::connect(addr);
    assert!(
        straggler.is_ok(),
        "retired port {addr} refused a late connection: {straggler:?}"
    );
}

/// Send one raw HTTP/1.1 request to `url`, with `body` as `(content type,
/// bytes)`, and return the response once the server closes the connection.
fn send_raw_http(url: &str, method: &str, path: &str, body: Option<(&str, &str)>) -> String {
    let address = url.strip_prefix("http://").unwrap();
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(CLI_ACTION_TIMEOUT)).unwrap();
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some((content_type, body)) = body {
        write!(
            request,
            "Content-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    } else {
        request.push_str("\r\n");
    }
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn recording_server_records_requests_rejected_before_rpc_dispatch() {
    let server = rpc_mock().start();
    let url = server.url().to_owned();
    for (method, path, body) in [
        ("GET", "/stray", None),
        ("POST", "/", Some(("application/json", "{not json"))),
        ("POST", "/", Some(("text/plain", "{}"))),
        (
            "POST",
            "/",
            Some((
                "application/json",
                r#"{"jsonrpc":"2.0","id":1,"method":"getVersionInfo"}"#,
            )),
        ),
    ] {
        let response = send_raw_http(&url, method, path, body);
        assert!(response.starts_with("HTTP/1.1 "), "{response}");
    }
    let calls = server.stop();
    let methods: Vec<&str> = calls.iter().map(|(method, _)| method.as_str()).collect();
    // The dispatched call is recorded once, under its RPC method.
    assert_eq!(
        methods,
        [
            "HTTP GET /stray",
            "HTTP POST /",
            "HTTP POST /",
            "getVersionInfo"
        ],
        "{calls:?}"
    );
    for (_, params) in &calls[..3] {
        assert!(params["status"].is_u64(), "{calls:?}");
    }
}

#[test]
fn no_request_mock_fails_on_any_http_request() {
    let (url, server) = spawn_no_request_rpc();
    send_raw_http(&url, "GET", "/", None);
    let joined = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.join()));
    assert!(
        joined.is_err(),
        "a stray HTTP request must fail the no-request mock"
    );
}

/// A mock RPC endpoint. Joining it, which tests do once the CLI has exited,
/// checks every request it received against its script.
struct MockServer<T = ()> {
    kind: Option<MockKind<T>>,
    client_exited: Arc<AtomicBool>,
}

type VerifyCalls<T> = Box<dyn FnOnce(&mut RpcScript) -> T + Send>;

enum MockKind<T> {
    /// A recording server whose requests the closure checks on join.
    Recorded(RecordingServer, VerifyCalls<T>),
    /// A script served from raw sockets on its own thread, for transport
    /// behavior a JSON-RPC server cannot produce, such as dropping or holding
    /// a response.
    Socket(thread::JoinHandle<T>),
}

impl<T> MockServer<T> {
    fn join(mut self) -> thread::Result<T> {
        self.client_exited.store(true, Ordering::SeqCst);
        match self.kind.take().expect("bug: a mock server is joined once") {
            MockKind::Recorded(server, verify) => {
                let mut script = RpcScript::new(server.stop());
                let result = verify(&mut script);
                script.finish();
                Ok(result)
            }
            MockKind::Socket(thread) => thread.join(),
        }
    }
}

impl<T> Drop for MockServer<T> {
    fn drop(&mut self) {
        self.client_exited.store(true, Ordering::SeqCst);
    }
}

thread_local! {
    static CLIENT_EXITED: std::cell::RefCell<Option<Arc<AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

fn spawn_mock<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> MockServer<T> {
    let client_exited = Arc::new(AtomicBool::new(false));
    let watched = Arc::clone(&client_exited);
    let thread = thread::spawn(move || {
        CLIENT_EXITED.set(Some(watched));
        body()
    });
    MockServer {
        kind: Some(MockKind::Socket(thread)),
        client_exited,
    }
}

/// Serve `server` as a mock whose requests `verify` checks on join.
fn recorded_mock<T>(
    server: RecordingServer,
    verify: impl FnOnce(&mut RpcScript) -> T + Send + 'static,
) -> (String, MockServer<T>) {
    let url = server.url().to_owned();
    let mock = MockServer {
        kind: Some(MockKind::Recorded(server, Box::new(verify))),
        client_exited: Arc::new(AtomicBool::new(false)),
    };
    (url, mock)
}

/// The requests a recorded mock received, consumed in order by its script.
///
/// Version probes are dropped where the CLI may make them, before a scripted
/// request, once their params are checked.
struct RpcScript(std::iter::Peekable<std::vec::IntoIter<RecordedCall>>);

impl RpcScript {
    fn new(calls: Vec<RecordedCall>) -> Self {
        let mut scripted = Vec::with_capacity(calls.len());
        let mut trailing_probe = false;
        for (method, params) in calls {
            trailing_probe = method == "getVersionInfo";
            if trailing_probe {
                assert_eq!(params, serde_json::json!({}), "getVersionInfo params");
            } else {
                scripted.push((method, params));
            }
        }
        assert!(
            !trailing_probe,
            "unexpected getVersionInfo request beyond the mock's script"
        );
        Self(scripted.into_iter().peekable())
    }

    /// The next request, which must call `method`; returns its params.
    fn expect(&mut self, method: &str) -> serde_json::Value {
        let Some((actual, params)) = self.0.next() else {
            panic!("the CLI never sent the {method} request");
        };
        assert_eq!(
            actual, method,
            "unexpected request in place of {method}: {params}"
        );
        params
    }

    /// Consume the next request if it calls `method`.
    fn accept(&mut self, method: &str) -> Option<serde_json::Value> {
        self.0
            .next_if(|(actual, _)| actual == method)
            .map(|(_, params)| params)
    }

    /// Consume the network check the CLI may make before a request.
    fn network_check(&mut self) {
        if let Some(params) = self.accept("getNetworkInfo") {
            assert_eq!(params, serde_json::json!({}));
        }
    }

    fn finish(mut self) {
        if let Some((method, params)) = self.0.next() {
            panic!("unexpected RPC request beyond the mock's script: {method} {params}");
        }
    }
}

/// A recording mock that answers the CLI's version probe.
fn rpc_mock() -> RecordingServerBuilder {
    RecordingServer::builder().method("getVersionInfo", |_| Ok(version_info()))
}

fn version_info() -> serde_json::Value {
    serde_json::json!({
        "wireFormat":"14.0.0", "implementation":"fixture",
        "supportedMethods":["getVersionInfo","getNetworkInfo","getChainTip","getChainConfig",
            "getBalance","getStorage","getContractDeployment","getContractArtifact","simulateInvocation","sendTransaction",
            "getTransactionStatus","getTransactionReceipt","getTransactionsByRootAddress",
            "getTransactionsByBlock","getEvents","faucet_requestAirdrop","dev_fundAccount"]
    })
}

fn network_info(network_hash: &str) -> serde_json::Value {
    serde_json::json!({
        "networkHash": network_hash,
        "genesisHash": thruput_encoding::encode(&[0x42; 32]),
    })
}

fn chain_tip(height: i64) -> serde_json::Value {
    serde_json::json!({
        "blockHeight": height.to_string(),
        "blockHash": "1".repeat(32),
    })
}

fn rpc_error(code: i32, message: &str) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(code, message.to_owned(), None::<()>)
}

/// Local hash of the transaction in `sendTransaction` params.
fn submitted_transaction_hash(params: &serde_json::Value) -> Result<String, ErrorObjectOwned> {
    let invalid = || rpc_error(-32602, "undecodable signedTransaction");
    let hex = params["signedTransaction"].as_str().ok_or_else(invalid)?;
    let bytes = hex::decode(hex.strip_prefix("0x").unwrap_or(hex)).map_err(|_| invalid())?;
    let signed_tx: thruput_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&bytes).map_err(|_| invalid())?;
    let hash = thruput_signing::transaction_hash(&signed_tx.tx).map_err(|_| invalid())?;
    Ok(thruput_encoding::encode(hash.as_ref()))
}

/// Fail on any request after the scripted ones, for as long as the CLI runs.
///
/// A fixed window would pass a request that arrives after it, which the
/// retired-listener reaper then drops silently. Instead this watches until the
/// test joins the endpoint after the CLI exits; any request the CLI made is
/// queued on the listener by then.
fn assert_no_follow_up_rpc(listener: &TcpListener) {
    let client_exited = CLIENT_EXITED
        .with_borrow(Clone::clone)
        .expect("bug: assert_no_follow_up_rpc runs on a spawn_mock thread");
    loop {
        // Read the flag before polling, so the last poll sees every request
        // made before the CLI exited.
        let exited = client_exited.load(Ordering::SeqCst);
        match listener.accept() {
            Ok(_) => panic!("unexpected RPC request beyond the mock's script"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if exited {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("failed to inspect RPC listener: {error}"),
        }
    }
}

fn spawn_storage_rpc(address: &str, key: &str) -> (String, MockServer) {
    let server = rpc_mock()
        .method("getStorage", |_| {
            Ok(serde_json::json!({
                "stateBlockHeight": "12",
                "item": {
                    "kind": "bytestring",
                    "bytes": "0x6c6f63616c2d6e6f64652d73746f72616765",
                },
            }))
        })
        .start();
    let expected_address = address.to_owned();
    let expected_key = key.to_owned();
    recorded_mock(server, move |script| {
        let params = script.expect("getStorage");
        assert_eq!(params["address"], expected_address);
        assert_eq!(params["key"], expected_key);
    })
}

// CLI presentation/config fixtures are migrated separately from the RPC boundary.
fn rpc_address_from_hex_fixture(address: &str) -> String {
    if !address.starts_with("0x") {
        thruput_encoding::parse_address(address).unwrap();
        return address.to_owned();
    }
    thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from(
        thruput_json_rpc_types::hex::decode_fixed_hex::<32>(address).unwrap(),
    ))
    .to_string()
}

fn spawn_balance_rpc(address: &str, raw_balance: &str) -> (String, MockServer) {
    let expected_address = rpc_address_from_hex_fixture(address);
    let response = serde_json::json!({
        "address": expected_address,
        "rawBalance": raw_balance,
        "stateBlockHeight": "34",
    });
    let server = rpc_mock()
        .method("getBalance", move |_| Ok(response.clone()))
        .start();
    recorded_mock(server, move |script| {
        assert_eq!(script.expect("getBalance")["address"], expected_address);
    })
}

fn spawn_view_rpc(address: &str, entrypoint: &str, calldata: &str) -> (String, MockServer) {
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
) -> (String, MockServer) {
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
) -> (String, MockServer) {
    spawn_view_rpc_inner(address, entrypoint, calldata, None, output)
}

fn spawn_view_rpc_with_events(
    address: &str,
    entrypoint: &str,
    calldata: &str,
    events: serde_json::Value,
) -> (String, MockServer) {
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
) -> (String, MockServer) {
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
) -> (String, MockServer) {
    let mut result = serde_json::json!({
        "stateBlockHeight": "12",
        "gasUsed": "4321",
    });
    result["output"] = output;
    result["events"] = events;
    let server = rpc_mock()
        .method("getNetworkInfo", |_| Ok(network_info(TEST_NETWORK_HASH)))
        .method("simulateInvocation", move |_| Ok(result.clone()))
        .start();
    let expected_address = address.to_owned();
    let expected_entrypoint = entrypoint.to_owned();
    let expected_calldata = calldata.to_owned();
    let expected_caller = caller.map(ToOwned::to_owned);
    recorded_mock(server, move |script| {
        script.network_check();
        let params = script.expect("simulateInvocation");
        assert_eq!(params["address"], expected_address);
        assert_eq!(params["entrypoint"], expected_entrypoint);
        assert_eq!(params["calldata"], expected_calldata);
        match expected_caller {
            Some(caller) => assert_eq!(params["caller"], caller),
            None => assert!(params.get("caller").is_none()),
        }
    })
}

fn advertised_dev_wallet_address(index: u8) -> String {
    let signer = thruput_signing::derive_default_dev_account_signer(index as usize);
    let address =
        thruput_signing::PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0)
            .address();
    thruput_encoding::encode(address.as_ref())
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
        thruput_signing::DEFAULT_DEV_ACCOUNT_SEED,
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
    let signer = thruput_signing::dev_account::derive_dev_account_signer(seed, index);
    let public_key = public_key_override.unwrap_or_else(|| signer.verifying_key_bytes());
    let path = dir.join(format!("runtime-manifest-{index}.json"));
    let manifest = serde_json::json!({
        "schemaVersion": 5,
        "kind": "hogd-dev-runtime",
        "chainState": "fresh",
        "networkHash": TEST_NETWORK_HASH,
        "protocol": "hogp/test00000000000",
        "genesisHash": thruput_encoding::encode(&[0x42; 32]),
        "producer": {
            "binary": "hog-devnode",
            "version": "0.1.0"
        },
        "endpoints": {
            "scheduler": "127.0.0.1:1000"
        },
        "bootstrapAccounts": [{
            "role": if index == 0 { "faucet" } else { "local_user" },
            "publicKey": thruput_json_rpc_types::Ed25519PublicKey::from(thruput_xdr::types::Ed25519PublicKey::from(public_key)),
            "walletAddress": rpc_address_from_hex_fixture(address),
            "initialStorageTokens": if index == 0 {
                thruput_node_supervisor::config::LOCAL_FAUCET_STORAGE_TOKENS
            } else {
                thruput_node_supervisor::config::DEFAULT_LOCAL_ACCOUNT_TOKENS
            }.to_string()
        }],
        "bootstrap": {
            "storageCostModel": {
                "hash": thruput_json_rpc_types::NetworkHashText::from([0x53; 32]),
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
    std::fs::create_dir_all(dir.join("fixture-config/thruput")).unwrap();
    std::fs::write(
        dir.join("fixture-config/thruput/config.toml"),
        "default_profile = 'default'\n[profile.default]\n",
    )
    .unwrap();
    let key_dir = dir.join("fixture-config/thruput/keys");
    std::fs::create_dir_all(&key_dir).unwrap();
    let key_path = key_dir.join(format!("dev-{index}.json"));
    std::fs::write(&key_path, serde_json::to_vec(&serde_json::json!({"version":1,"scheme":"ed25519","keypair_hex":hex::encode(signer.to_keypair_bytes()),"public_key_hex":hex::encode(signer.verifying_key_bytes())})).unwrap()).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn write_project_config_with_alice_wallet(
    project: &std::path::Path,
    rpc_url: &str,
    config_home: &std::path::Path,
) -> String {
    let owner_public_key = generate_key_public_key(config_home, "alice");
    std::fs::write(
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\nnetwork_hash = \"{TEST_NETWORK_HASH}\"\n\n[profile.default.wallets.alice]\naddress = \"{}\"\nowner_public_key = \"{owner_public_key}\"\n",
            thruput_encoding::encode(&[0xaa; 32])
        ),
    )
    .unwrap();
    owner_public_key
}

fn add_faucet_to_default_profile(project: &std::path::Path, faucet_url: &str) {
    let path = project.join("thruput.toml");
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
        project.join("thruput.toml"),
        format!(
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"{rpc_url}\"\n\n[profile.default.wallets.alice]\naddress = \"{}\"\nowner_public_key = \"{}\"\n",
            thruput_encoding::encode(&[0xaa; 32]),
            thruput_encoding::encode(&[0xbb; 32]),
        ),
    )
    .unwrap();
}

fn make_local_keypair_unavailable(config_home: &std::path::Path, name: &str) {
    let key_path = config_home.join(format!("thruput/keys/{name}.json"));
    let mut key: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&key_path).unwrap()).unwrap();
    key["keypair_hex"] = serde_json::Value::String("unavailable".into());
    std::fs::write(&key_path, serde_json::to_vec(&key).unwrap()).unwrap();
}

fn generate_key_public_key(config_home: &std::path::Path, name: &str) -> String {
    let output = thruput()
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
    assert_eq!(stdout["data"]["schemaVersion"], 1);
    stdout["data"]["key"]["publicKey"]
        .as_str()
        .expect("generated key should include public key")
        .to_owned()
}

fn signed_tx_invoked_address(tx_hex: &str) -> String {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex)).unwrap();
    let signed_tx: thruput_xdr::transaction::SignedTransaction = xdrs::txs::decode(&bytes).unwrap();
    thruput_encoding::encode(signed_tx.tx.invocation.invoked_address.as_ref())
}

fn decode_signed_tx(tx_hex: &str) -> thruput_xdr::transaction::SignedTransaction {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex)).unwrap();
    xdrs::txs::decode(&bytes).unwrap()
}

fn signed_tx_entrypoint_from_decoded(
    signed_tx: &thruput_xdr::transaction::SignedTransaction,
) -> String {
    format!(
        "0x{}",
        hex::encode(signed_tx.tx.invocation.entrypoint.as_ref())
    )
}

fn decode_wallet_action(
    signed_tx: &thruput_xdr::transaction::SignedTransaction,
) -> (String, String, Vec<u8>) {
    if signed_tx.tx.invocation.entrypoint
        == thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT
    {
        type Call = (
            thruput_xdr::types::ContractAddress,
            thruput_xdr::contract::ContractEntrypointID,
            u64,
            Vec<u8>,
        );
        let calls: Vec<Call> = xdrs::cxs::decode(&signed_tx.tx.invocation.calldata).unwrap();
        assert_eq!(calls.len(), 1);
        let (address, entrypoint, gas, calldata) = &calls[0];
        assert_eq!(*gas, thruput_signing::DEFAULT_TX_GAS_LIMIT);
        return (
            thruput_encoding::encode(address.as_ref()),
            format!("0x{}", hex::encode(entrypoint.as_ref())),
            calldata.clone(),
        );
    }
    let inner: Vec<u8> = xdrs::cxs::decode(&signed_tx.tx.invocation.calldata.0).unwrap();
    let (target, entrypoint, gas_limit, calldata): (
        thruput_xdr::types::ContractAddress,
        thruput_xdr::contract::ContractEntrypointID,
        u64,
        Vec<u8>,
    ) = xdrs::txs::decode(&inner).unwrap();
    assert_eq!(gas_limit, thruput_signing::DEFAULT_TX_GAS_LIMIT);
    (
        thruput_encoding::encode(target.as_ref()),
        format!("0x{}", hex::encode(entrypoint.as_ref())),
        calldata,
    )
}

fn signed_tx_entrypoint(tx_hex: &str) -> String {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex)).unwrap();
    let signed_tx: thruput_xdr::transaction::SignedTransaction = xdrs::txs::decode(&bytes).unwrap();
    format!(
        "0x{}",
        hex::encode(signed_tx.tx.invocation.entrypoint.as_ref())
    )
}

fn signed_tx_deploy_contract_kind(tx_hex: &str) -> &'static str {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex)).unwrap();
    let signed_tx: thruput_xdr::transaction::SignedTransaction = xdrs::txs::decode(&bytes).unwrap();
    let contract = signed_tx
        .tx
        .contracts_to_deploy
        .0
        .first()
        .expect("bug: deploy tx should include contract bytes");
    match contract {
        thruput_xdr::contract::Contract::kRv32 { .. } => "rv32",
        _ => "other",
    }
}

fn mock_tx_hash() -> String {
    thruput_json_rpc_types::NetworkHashText::from([0xaa; 32]).to_string()
}

/// Mock RPC server that handles the full deploy flow:
/// Native preflight, send once, then read the receipt.
///
/// Returns a 32-byte dummy address (`0xbb..bb`) as the deploy `return_data` so the live
/// path exercises the receipt-based address extraction.
fn spawn_deploy_rpc(height: i64) -> (String, MockServer) {
    spawn_transaction_rpc(height, sample_deploy_receipt(height))
}

fn spawn_transaction_rpc(height: i64, receipt: serde_json::Value) -> (String, MockServer) {
    let submitted = Arc::new(Mutex::new(None::<String>));
    let server = rpc_mock()
        .method("getNetworkInfo", |_| Ok(network_info(TEST_NETWORK_HASH)))
        .method("getChainTip", move |_| Ok(chain_tip(height)))
        .method("getBalance", move |params| {
            Ok(serde_json::json!({
                "address": params["address"],
                "rawBalance": "100000000",
                "stateBlockHeight": height.to_string(),
            }))
        })
        .method("sendTransaction", {
            let submitted = Arc::clone(&submitted);
            move |params| {
                let tx_hash = submitted_transaction_hash(params)?;
                *submitted.lock().unwrap() = Some(tx_hash.clone());
                Ok(serde_json::json!({"transactionHash":tx_hash,"admission":"accepted"}))
            }
        })
        .method("getTransactionReceipt", move |_| {
            let mut receipt = receipt.clone();
            if let Some(tx_hash) = submitted.lock().unwrap().clone() {
                receipt["transactionHash"] = serde_json::json!(tx_hash);
            }
            Ok(receipt)
        })
        .start();
    recorded_mock(server, |script| {
        script.accept("getNetworkInfo");
        script.expect("getChainTip");
        // Contract calls check the sender balance before submission; deploys do not.
        script.network_check();
        if script.accept("getBalance").is_some() {
            script.network_check();
        }
        let params = script.expect("sendTransaction");
        let signed_hex = params["signedTransaction"].as_str().unwrap();
        assert!(
            signed_hex.starts_with("0x"),
            "sendTransaction should receive 0x-prefixed tx hex"
        );
        let tx_hash = submitted_transaction_hash(&params).unwrap();
        assert_eq!(
            script.expect("getTransactionReceipt")["transactionHash"],
            tx_hash
        );
    })
}

fn sample_deploy_receipt(height: i64) -> serde_json::Value {
    serde_json::json!({
        "transactionHash": mock_tx_hash(),
        "blockHeight": height.to_string(),
        "execution": { "result": "return", "returnData": format!("0x{}", "bb".repeat(32)) },
        "events": [],
        "gasUsed": "1500",
    })
}

fn sample_failed_deploy_receipt(height: i64) -> serde_json::Value {
    serde_json::json!({
        "transactionHash": mock_tx_hash(),
        "blockHeight": height.to_string(),
        "execution": { "result": "fault" },
        "events": [],
        "gasUsed": "4242",
    })
}

fn sample_insufficient_funding_deploy_receipt(height: i64) -> serde_json::Value {
    let abort_data = xdrs::cxs::encode(&(
        <thruput_erc20_if::ERC20Error as thruput_sdk::exn::Exn>::SELECTOR,
        thruput_erc20_if::ERC20Error::TooLarge,
    ))
    .expect("bug: ERC20 exception should encode");
    serde_json::json!({
        "transactionHash": mock_tx_hash(),
        "blockHeight": height.to_string(),
        "execution": { "result": "abort", "abortData": format!("0x{}", hex::encode(abort_data)) },
        "events": [],
        "gasUsed": "1500",
    })
}

fn sample_call_receipt(height: i64) -> serde_json::Value {
    let contract_address = thruput_json_rpc_types::ContractAddress::from(
        thruput_xdr::types::ContractAddress::from([0x11; 32]),
    );
    serde_json::json!({
        "transactionHash": mock_tx_hash(),
        "blockHeight": height.to_string(),
        "execution": { "result": "return", "returnData": "0x0102" },
        "events": [{
            "address": contract_address,
            "topics": [format!("0x{}", "22".repeat(32))],
            "data": "0x1234",
        }],
        "gasUsed": "1500",
    })
}

fn sample_deterministic_error_call_receipt(height: i64) -> serde_json::Value {
    serde_json::json!({
        "transactionHash": mock_tx_hash(),
        "blockHeight": height.to_string(),
        "execution": { "result": "fault" },
        "events": [],
        "gasUsed": "1500",
    })
}

fn sample_reverted_call_receipt(height: i64, abort_hex: &str) -> serde_json::Value {
    serde_json::json!({
        "transactionHash": mock_tx_hash(),
        "blockHeight": height.to_string(),
        "execution": { "result": "abort", "abortData": abort_hex },
        "events": [],
        "gasUsed": "1500",
    })
}

/// Wire-encode a no-payload `LibraryExn` abort buffer for `case_name`:
/// cxs varint selector followed by the union case discriminant.
fn library_abort_hex(case_name: &str) -> String {
    let library = thruput_spec_tool::describe::lookup("LibraryExn")
        .expect("LibraryExn should be registered in the describe registry");
    let described = thruput_spec_tool::describe::build_describe(library);
    let discriminant = described
        .type_defs
        .iter()
        .find_map(|def| match def {
            thruput_contract_spec::ContractTypeDef::TypeDefUnion { union_def }
                if AsRef::<[u8]>::as_ref(&union_def.name.declared) == b"LibraryExn" =>
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

fn issuer_event_fixture() -> Vec<serde_json::Value> {
    let mut records: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/issuer-events.json")).unwrap();
    // The captured fixture names the emitter `contractAddress` in hex; the RPC wire carries
    // it as the typed base58 `address` of an `EmittedEvent`.
    for record in &mut records {
        let event = record["event"].as_object_mut().unwrap();
        let emitter = event.remove("contractAddress").unwrap();
        let address = rpc_address_from_hex_fixture(emitter.as_str().unwrap());
        event.insert("address".into(), address.into());
    }
    records
}

fn run_issuer_event_command(
    fixture: &[serde_json::Value],
    command: [&str; 2],
    filters: &serde_json::Value,
) -> serde_json::Value {
    let tmp = TempDir::new().unwrap();
    let token = fixture[0]["event"]["address"].as_str().unwrap();
    let events: Vec<_> = fixture
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let hash = hex::decode(format!("{:064x}", index + 1)).unwrap();
            serde_json::json!({
                "transactionHash": thruput_encoding::encode(&hash),
                "blockHeight": (20 - index).to_string(),
                "sequenceNumber": 0,
                "event": record["event"],
            })
        })
        .collect();
    let (rpc_url, server) = spawn_events_rpc(
        serde_json::json!({
            "filters": filters,
            "start": { "order": "descending", "anchor": { "kind": "atEnd" } },
            "limit": 50,
        }),
        RpcReply::Result(serde_json::json!({
            "events": events,
            "nextCursor": null,
            "latestIndexedBlockHeight": "20",
            "oldestIndexedBlockHeight": "0",
        })),
    );
    let output = thruput()
        .arg("--json")
        .args(command)
        .args([token, "--rpc-url", &rpc_url])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn issuer_raw_events_survive_the_cli_http_boundary() {
    let fixture = issuer_event_fixture();
    let token = fixture[0]["event"]["address"].as_str().unwrap();
    let report = run_issuer_event_command(
        &fixture,
        ["events", "list"],
        &serde_json::json!([{
            "addresses": [token],
            "topics": {
                "topic0": [],
                "topic1": [],
                "topic2": [],
            },
        }]),
    );
    let events = report["data"]["events"].as_array().unwrap();
    assert_eq!(fixture.len(), 9);
    assert_eq!(events.len(), fixture.len());
    for (event, original) in events.iter().zip(&fixture) {
        assert_eq!(event["contractAddress"], original["event"]["address"]);
        for field in ["topics", "data"] {
            assert_eq!(event[field], original["event"][field]);
        }
        assert_eq!(event["decodeStatus"], "notRequested");
    }
}

#[test]
fn issuer_token_movements_count_clawback_once() {
    let movements: Vec<_> = issuer_event_fixture()
        .into_iter()
        .filter(|record| {
            [transfer_signature(), mint_signature(), burn_signature()]
                .iter()
                .any(|signature| record["event"]["topics"][0] == *signature)
        })
        .collect();
    let token = movements[0]["event"]["address"].as_str().unwrap();
    let report = run_issuer_event_command(
        &movements,
        ["token", "transfers"],
        &all_token_event_filters(token),
    );
    let records = report["data"]["records"].as_array().unwrap();
    assert_eq!(movements.len(), 4);
    assert_eq!(records.len(), movements.len());
    assert_eq!(report["data"]["schemaVersion"], 4);
    for ((record, original), amount) in records.iter().zip(&movements).zip(["100", "25", "5", "7"])
    {
        assert_eq!(record["contractAddress"], original["event"]["address"]);
        for field in ["topics", "data"] {
            assert_eq!(record[field], original["event"][field]);
        }
        assert_eq!(record["decodeStatus"], "decoded");
        let expected = if original["operation"] == "clawback" {
            "burn"
        } else {
            original["operation"].as_str().unwrap()
        };
        assert_eq!(record["transfer"]["kind"], expected);
        assert_eq!(record["transfer"]["rawAmount"], amount);
    }
    let clawback_burn = &records[3];
    let alice = thruput_encoding::encode(&[0x13; 32]);
    assert_eq!(clawback_burn["transfer"]["from"], alice);
    assert!(clawback_burn["transfer"]["to"].is_null());
}
