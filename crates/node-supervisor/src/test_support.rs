use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

pub(crate) const FAKE_HOGD_DEV_MODE_ENV: &str = "THRUPUT_FAKE_HOGD_DEV_MODE";

/// A loopback address that refuses connections. Port 1 is privileged, so no
/// test can be listening on it; a freshly closed ephemeral port is not dead,
/// because a test running in parallel can bind it first.
pub(crate) const DEAD_LOOPBACK_ADDR: std::net::SocketAddr = std::net::SocketAddr::V4(
    std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 1),
);

/// Builds and returns the `fake_hogd_dev` example binary; `cargo test` builds it
/// only as a test harness, so a copy on disk may predate a `thruput-xdr` bump.
///
/// The build runs once per test process, or once per nextest run when nextest
/// gives every test its own process.
pub(crate) fn fake_hogd_dev_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(build_once_per_run).clone()
}

/// Shares one build between the processes of a nextest run. Without this,
/// every test process runs its own `cargo build`, and they queue on cargo's
/// build-directory lock.
///
/// The record names the run, so a new run always rebuilds. Two checkouts
/// running nextest concurrently against one target directory can still swap
/// this executable, as they can every test executable nextest re-launches.
fn build_once_per_run() -> PathBuf {
    let target_dir = outer_target_dir();
    let (Some(target_dir), Some(run_id)) = (&target_dir, std::env::var_os("NEXTEST_RUN_ID")) else {
        return build_fake_hogd_dev_binary(target_dir.as_deref());
    };
    let lock = std::fs::File::create(target_dir.join("fake_hogd_dev.build.lock"))
        .expect("bug: fake_hogd_dev build lock should be creatable");
    lock.lock()
        .expect("bug: fake_hogd_dev build lock should be acquirable");
    let record = target_dir.join("fake_hogd_dev.build.run");
    let run_id = run_id.to_string_lossy().into_owned();
    if let Some(binary) = std::fs::read_to_string(&record)
        .ok()
        .and_then(|contents| {
            let (recorded, binary) = contents.split_once('\n')?;
            (recorded == run_id).then(|| PathBuf::from(binary))
        })
        .filter(|binary| binary.is_file())
    {
        return binary;
    }
    let binary = build_fake_hogd_dev_binary(Some(target_dir));
    std::fs::write(&record, format!("{run_id}\n{}", binary.display()))
        .expect("bug: fake_hogd_dev build record should be writable");
    binary
}

/// The target directory this test executable was built into, so the nested
/// build reuses it instead of falling back to the workspace default. Cargo
/// marks the root of every target directory with `CACHEDIR.TAG`; executables
/// sit below it at `<profile>/deps` or `<triple>/<profile>/deps`.
fn outer_target_dir() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    executable
        .ancestors()
        .skip(1)
        .find(|directory| directory.join("CACHEDIR.TAG").is_file())
        .map(Path::to_path_buf)
}

/// Builds the example with the current cargo and returns the path it reports.
fn build_fake_hogd_dev_binary(target_dir: Option<&Path>) -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(&cargo);
    command.args([
        "build",
        "--quiet",
        "--locked",
        "--message-format=json",
        "-p",
        "thruput-node-supervisor",
        "--example",
        "fake_hogd_dev",
    ]);
    if let Some(target_dir) = target_dir {
        command.arg("--target-dir").arg(target_dir);
    }
    let output = command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        // PATH-resolution tests hide the toolchain while other tests build this helper.
        .env("PATH", env!("PATH"))
        .stderr(Stdio::inherit())
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "failed to run {} to build the fake_hogd_dev example: {err}",
                cargo.to_string_lossy()
            )
        });
    assert!(
        output.status.success(),
        "failed to build fake_hogd_dev example; status: {}",
        output.status
    );
    let binary = fake_hogd_dev_executable(&output.stdout);
    assert!(
        binary.is_file(),
        "fake_hogd_dev example missing after build at {}",
        binary.display()
    );
    binary
}

/// Finds the `fake_hogd_dev` example artifact in cargo's JSON build messages.
fn fake_hogd_dev_executable(messages: &[u8]) -> PathBuf {
    String::from_utf8_lossy(messages)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|message| {
            message["reason"] == "compiler-artifact"
                && message["target"]["name"] == "fake_hogd_dev"
                && message["target"]["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "example"))
        })
        .and_then(|message| message["executable"].as_str().map(PathBuf::from))
        .expect("bug: cargo build output should report the fake_hogd_dev example executable")
}
