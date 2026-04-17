use crate::artifacts::{self, ArtifactOptions};
use crate::{CliError, Print};
use serde_json::json;
use std::env;
use std::process::Command;

/// Build the contract to WASM, then run WASM tests
/// against the mock executor with real abort semantics.
///
/// Unlike `cargo test` (which runs natively with unwind),
/// faults in WASM execution are real aborts — the same
/// as what would happen on-chain.
pub fn run(
    package: Option<&str>,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    artifacts::ensure_no_target_dir_passthrough_conflicts(artifacts, &cwd, cargo_args)?;

    // Build the WASM contract (validates project root,
    // checks for cargo on PATH, etc.)
    let build =
        crate::build::build_project(package, false, &[], artifacts, explicit_profile, print)?;

    // Run only the WASM test target
    let root = build.project_root.clone();
    let profile = build.profile.clone();

    print.status("Running WASM tests (abort semantics)...");

    let host_target = env::consts::ARCH.to_string()
        + "-"
        + match env::consts::OS {
            "macos" => "apple-darwin",
            "linux" => "unknown-linux-gnu",
            os => {
                return Err(CliError::UnsupportedOs(os.to_string()));
            }
        };

    let mut cargo_cmd = Command::new("cargo");
    cargo_cmd
        .current_dir(&root)
        .arg("test")
        .arg("-p")
        .arg(&build.project_name)
        .arg("--test")
        .arg("test_wasm")
        .arg("--target")
        .arg(&host_target)
        .arg("--features")
        .arg("dev")
        .arg("--")
        .arg("--include-ignored");

    artifacts::ensure_no_target_dir_passthrough_conflicts(artifacts, &root, cargo_args)?;
    artifacts.apply_to_command(&mut cargo_cmd);

    for arg in cargo_args {
        cargo_cmd.arg(arg);
    }

    let (cargo_stdout, cargo_stderr) = crate::run_cargo(&mut cargo_cmd, print)?;

    print.json(&crate::result::CommandResult::new(
        cmd,
        json!({
            "project_root": root,
            "profile": profile,
            "wasm": build.wasm,
            "cargo_stdout": cargo_stdout,
            "cargo_stderr": cargo_stderr,
        }),
    ))?;
    Ok(())
}
