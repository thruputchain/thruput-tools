use std::path::PathBuf;

pub(crate) const FAKE_HOGD_DEV_MODE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_MODE";

pub(crate) fn fake_hogd_dev_binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_fake_hogd_dev") {
        return PathBuf::from(path);
    }

    let current_exe = std::env::current_exe().expect("bug: current test binary path is available");
    let deps_dir = current_exe
        .parent()
        .expect("bug: test binary has a parent directory");
    let target_dir = deps_dir
        .parent()
        .expect("bug: test binary lives under target deps");
    let binary_name = if cfg!(windows) {
        "fake_hogd_dev.exe"
    } else {
        "fake_hogd_dev"
    };
    let binary = target_dir.join("examples").join(binary_name);

    assert!(
        binary.is_file(),
        "bug: fake_hogd_dev example binary was not built at {}; run the package tests without --lib",
        binary.display()
    );

    binary
}
