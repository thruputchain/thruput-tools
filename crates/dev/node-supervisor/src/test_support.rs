use std::path::PathBuf;
use std::process::Command;
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

fn build_fake_hogd_dev_binary() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args([
            "build",
            "--quiet",
            "--locked",
            "-p",
            "groundhog-node-supervisor",
            "--example",
            "fake_hogd_dev",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("bug: cargo should run to build the fake_hogd_dev example");
    assert!(
        status.success(),
        "failed to build fake_hogd_dev example; status: {status}"
    );

    let current_exe = std::env::current_exe().expect("bug: current test binary path is available");
    let deps_dir = current_exe
        .parent()
        .expect("bug: test binary has a parent directory");
    let target_dir = deps_dir
        .parent()
        .expect("bug: test binary lives under target deps");
    let binary = target_dir
        .join("examples")
        .join(format!("fake_hogd_dev{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.is_file(),
        "fake_hogd_dev example missing after build at {}",
        binary.display()
    );
    binary
}
