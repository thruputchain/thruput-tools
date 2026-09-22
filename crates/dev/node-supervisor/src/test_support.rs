use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

pub(crate) const FAKE_HOGD_DEV_MODE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_MODE";

/// Path of the `fake_hogd_dev` example binary, built from the current
/// workspace sources.
///
/// `cargo test` compiles that example only as a test harness, so the plain
/// binary the supervisor spawns is built here rather than trusted from disk:
/// a copy left in a cached target directory can predate a `groundhog-xdr`
/// bump and advertise a protocol the supervisor rejects. Cargo re-links the
/// binary on every build, so a fresh build costs one fingerprint check.
pub(crate) fn fake_hogd_dev_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(build_fake_hogd_dev_binary).clone()
}

/// Builds the example with the cargo running these tests and returns the
/// executable path cargo reports, so the lookup follows whatever target
/// directory or profile cargo actually used.
fn build_fake_hogd_dev_binary() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(&cargo)
        .args([
            "build",
            "--quiet",
            "--locked",
            "--message-format=json",
            "-p",
            "groundhog-node-supervisor",
            "--example",
            "fake_hogd_dev",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
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
