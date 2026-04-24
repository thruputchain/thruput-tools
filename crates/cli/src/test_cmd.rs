use crate::artifacts::{self, ArtifactOptions};
use crate::{CliError, Print};
use serde_json::json;
use std::env;
use std::process::Command;

/// Explicit controls for the `test_wasm` harness invocation.
#[derive(Debug, Clone, Default)]
pub struct TestOptions {
    /// Run only tests whose names contain this substring.
    pub match_test: Option<String>,
    /// Do not start new tests after the first failure.
    pub fail_fast: bool,
    /// Show captured stdout for successful tests.
    pub show_output: bool,
    /// Do not capture stdout/stderr from tests.
    pub no_capture: bool,
    /// Number of test threads to use.
    pub test_threads: Option<usize>,
    /// Skip tests whose names contain these substrings.
    pub skip: Vec<String>,
}

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
    run_with_options(
        package,
        &TestOptions::default(),
        cargo_args,
        artifacts,
        explicit_profile,
        cmd,
        print,
    )
}

/// Build the contract to WASM, then run WASM tests with explicit
/// harness controls.
pub fn run_with_options(
    package: Option<&str>,
    options: &TestOptions,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
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

    apply_harness_options(&mut cargo_cmd, options);

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

fn apply_harness_options(cargo_cmd: &mut Command, options: &TestOptions) {
    if options.fail_fast {
        cargo_cmd.arg("--fail-fast");
    }
    if options.show_output {
        cargo_cmd.arg("--show-output");
    }
    if options.no_capture {
        cargo_cmd.arg("--no-capture");
    }
    if let Some(threads) = options.test_threads {
        cargo_cmd.arg("--test-threads").arg(threads.to_string());
    }
    for skip in &options.skip {
        cargo_cmd.arg("--skip").arg(skip);
    }
    if let Some(filter) = &options.match_test {
        cargo_cmd.arg(filter);
    }
}

#[cfg(test)]
mod tests {
    use super::{TestOptions, apply_harness_options};
    use std::process::Command;

    #[test]
    fn default_harness_options_add_no_args() {
        let mut command = Command::new("cargo");

        apply_harness_options(&mut command, &TestOptions::default());

        assert_eq!(command.get_args().count(), 0);
    }

    #[test]
    fn harness_options_map_to_libtest_args() {
        let mut command = Command::new("cargo");
        let options = TestOptions {
            match_test: Some("counter".into()),
            fail_fast: true,
            show_output: true,
            no_capture: true,
            test_threads: Some(1),
            skip: vec!["slow".into(), "flaky".into()],
        };

        apply_harness_options(&mut command, &options);

        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--fail-fast",
                "--show-output",
                "--no-capture",
                "--test-threads",
                "1",
                "--skip",
                "slow",
                "--skip",
                "flaky",
                "counter",
            ]
        );
    }
}
