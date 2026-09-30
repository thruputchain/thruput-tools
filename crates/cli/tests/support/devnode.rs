use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
};

/// Build the supervisor fixture with the current protocol dependencies.
pub fn binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
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
                .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
                .stderr(Stdio::inherit())
                .output()
                .unwrap();
            assert!(output.status.success(), "fake devnode build failed");
            let binary = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find(|message| {
                    message["reason"] == "compiler-artifact"
                        && message["target"]["name"] == "fake_hogd_dev"
                })
                .and_then(|message| message["executable"].as_str().map(PathBuf::from))
                .unwrap();
            assert!(binary.is_file());
            binary
        })
        .clone()
}
