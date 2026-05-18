use crate::artifacts::{self, ArtifactOptions};
use crate::result::CommandResult;
use crate::test_json::{
    TEST_JSON_SCHEMA_VERSION, TestCase, TestCaseStatus, TestReport, TestRerunInfo, TestRun,
    TestSummary, TestSummarySource, TestWatchInfo,
};
use crate::{CliError, Print};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime};

const FAILURE_CACHE_VERSION: u8 = 1;
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(350);
const WASM_TEST_TARGET: &str = "test_wasm";
#[cfg(any(feature = "dev-node", test))]
const INTEGRATION_TEST_TARGET: &str = "integration";

/// Explicit controls for the `test_wasm` harness invocation.
#[derive(Debug, Clone, Default)]
pub struct TestOptions {
    /// Run only tests whose names contain this substring.
    pub match_test: Option<String>,
    /// Number of WASM test harness threads to use.
    pub test_threads: Option<usize>,
    /// Skip tests whose names contain these substrings.
    pub skip: Vec<String>,
    /// Rerun tests that failed in the previous WASM test run.
    pub rerun: bool,
    /// Keep rerunning WASM tests when project files change.
    pub watch: bool,
    /// Boolean controls forwarded to the WASM test harness.
    pub harness: TestHarnessOptions,
}

/// Options for the `groundhog test --integration` orchestration path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrationTestOptions {}

/// Boolean controls forwarded to the selected libtest harness.
#[derive(Debug, Clone, Default)]
pub struct TestHarnessOptions {
    /// Stop the WASM test harness after the first test failure.
    pub fail_fast: bool,
    /// Show captured stdout for successful tests.
    pub show_output: bool,
    /// Do not capture stdout/stderr from tests.
    pub no_capture: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FailureCache {
    version: u8,
    package: String,
    tests: Vec<String>,
}

#[derive(Debug, Clone)]
struct TestRunOutput {
    filter: Option<String>,
    exact_filter: bool,
    success: bool,
    code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, Copy)]
struct SelectedFilter<'a> {
    name: Option<&'a str>,
    exact: bool,
}

struct TestCommandContext<'a> {
    root: &'a Path,
    project_name: &'a str,
    host_target: &'a str,
    test_target: &'a str,
    env: &'a [(String, String)],
    artifacts: &'a ArtifactOptions,
    cargo_args: &'a [String],
    print: &'a Print,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchSnapshot {
    files: BTreeMap<PathBuf, WatchFileFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchFileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
struct WatchContext {
    root: PathBuf,
    ignored_dirs: Vec<PathBuf>,
}

#[cfg(feature = "dev-node")]
struct IntegrationRun {
    report: TestReport,
    failed_code: Option<i32>,
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
    if options.watch {
        return run_watch(
            package,
            options,
            cargo_args,
            artifacts,
            explicit_profile,
            cmd,
            print,
        );
    }

    run_once_with_options(
        package,
        options,
        cargo_args,
        artifacts,
        explicit_profile,
        cmd,
        print,
    )
}

/// Run integration tests with explicit orchestration controls.
#[allow(clippy::too_many_arguments)]
pub fn run_integration_with_options(
    package: Option<&str>,
    options: &TestOptions,
    _integration: &IntegrationTestOptions,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if options.watch {
        return Err(CliError::InvalidTestOptions(
            "`--integration` cannot be combined with `--watch` yet".into(),
        ));
    }
    if options.rerun {
        return Err(CliError::InvalidTestOptions(
            "`--integration` cannot be combined with `--rerun` yet".into(),
        ));
    }

    #[cfg(feature = "dev-node")]
    {
        run_integration_with_dev_node(
            package,
            options,
            cargo_args,
            artifacts,
            explicit_profile,
            cmd,
            print,
        )
    }

    #[cfg(not(feature = "dev-node"))]
    {
        let _ = (package, cargo_args, artifacts, explicit_profile, cmd, print);
        Err(CliError::IntegrationTestsUnavailable)
    }
}

#[cfg(feature = "dev-node")]
#[allow(clippy::too_many_arguments)]
fn run_integration_with_dev_node(
    package: Option<&str>,
    options: &TestOptions,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let build =
        crate::build::build_project(package, false, &[], artifacts, explicit_profile, &[], print)?;
    let host_target = host_target()?;
    artifacts::ensure_no_target_dir_passthrough_conflicts(
        artifacts,
        &build.project_root,
        cargo_args,
    )?;
    ensure_integration_test_target_exists(&build.project_root)?;

    print.status("Starting ephemeral local node for integration tests...");
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let started = rt.block_on(crate::node::start_local_node(
        crate::node::LocalNodeOptions {
            port: 0,
            pid_file: crate::node::LocalNodePidFile::Disabled,
            ..crate::node::LocalNodeOptions::default()
        },
    ))?;

    let rpc_url = started.rpc_url();
    let result = run_integration_against_node(
        &build,
        &host_target,
        &rpc_url,
        options,
        cargo_args,
        artifacts,
        print,
    );

    let stop_result = rt.block_on(started.stop());
    handle_integration_outcome(result, stop_result.map(|_| ()), cmd, print)
}

#[cfg(feature = "dev-node")]
fn handle_integration_outcome(
    result: Result<IntegrationRun, CliError>,
    stop_result: Result<(), CliError>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    match (result, stop_result) {
        (Ok(run), Ok(())) => finish_integration_run(&run, cmd, print, Vec::new()),
        (Ok(run), Err(error)) => {
            let warning = local_node_shutdown_warning(
                if run.failed_code.is_some() {
                    "integration test failure"
                } else {
                    "successful integration test run"
                },
                &error,
            );
            print.warn(&warning.message);
            finish_integration_run(&run, cmd, print, vec![warning])
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(stop_error)) => {
            print.warn(format_args!(
                "local node shutdown failed after integration test error: {stop_error}"
            ));
            Err(error)
        }
    }
}

#[cfg(feature = "dev-node")]
fn local_node_shutdown_warning(context: &'static str, error: &CliError) -> crate::checks::Finding {
    crate::checks::Finding {
        level: crate::checks::FindingLevel::Warn,
        code: "local_node_shutdown_failed",
        message: format!("local node shutdown failed after {context}: {error}"),
        hint: Some(std::borrow::Cow::Borrowed(
            "check for a lingering hogd-dev process if the next local-node command fails",
        )),
    }
}

#[cfg(feature = "dev-node")]
#[allow(clippy::too_many_arguments)]
fn run_integration_against_node(
    build: &crate::build::BuildResult,
    host_target: &str,
    rpc_url: &str,
    options: &TestOptions,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    print: &Print,
) -> Result<IntegrationRun, CliError> {
    print.status(format_args!(
        "Deploying {} to {rpc_url} for integration tests...",
        build.project_name
    ));
    let prepared = crate::deploy::prepare_deploy_for_rpc(&build.wasm, rpc_url, "dev-0")?;
    let submitted = crate::deploy::submit_prepared_deploy_and_resolve(&prepared)?;
    let contract_address = submitted.contract_address.ok_or_else(|| {
        CliError::Other(format!(
            "integration deploy did not produce a contract address: {:?}",
            submitted.receipt.status
        ))
    })?;
    print.status(format_args!(
        "Running RPC integration tests against {contract_address}..."
    ));

    let env = integration_test_environment(rpc_url, &contract_address);
    let filter = SelectedFilter {
        name: options.match_test.as_deref(),
        exact: false,
    };
    let context = TestCommandContext {
        root: &build.project_root,
        project_name: &build.project_name,
        host_target,
        test_target: INTEGRATION_TEST_TARGET,
        env: &env,
        artifacts,
        cargo_args,
        print,
    };
    let outputs = run_test_commands(&context, options, &[filter])?;

    let mut profile = build.profile.clone();
    profile.name = "integration".into();
    profile.rpc_url = Some(rpc_url.to_owned());
    profile.default_key = Some("dev-0".into());
    profile.source = None;
    profile
        .contracts
        .insert(build.project_name.clone(), contract_address);

    let report = build_test_report(
        &build.project_root,
        &profile,
        &build.wasm,
        &outputs,
        false,
        &failure_cache_path(&build.target_directory, &build.project_name),
        &[],
        &[],
        &executed_tests(&outputs),
        &dedup_tests(
            outputs
                .iter()
                .flat_map(|output| extract_failed_tests(&output.stdout, &output.stderr)),
        ),
        false,
    );

    let failed_code = outputs
        .iter()
        .find(|output| !output.success)
        .map(|output| output.code);

    Ok(IntegrationRun {
        report,
        failed_code,
    })
}

#[cfg(feature = "dev-node")]
fn finish_integration_run(
    run: &IntegrationRun,
    cmd: &'static str,
    print: &Print,
    warnings: Vec<crate::checks::Finding>,
) -> Result<(), CliError> {
    if let Some(code) = run.failed_code {
        if print.is_json() {
            print.json(&CommandResult::failure_with_warnings(
                cmd,
                warnings,
                &run.report,
            ))?;
            return Err(CliError::CommandReportedFailure);
        }
        return Err(CliError::CargoFailed(code));
    }

    if warnings.is_empty() {
        print.json(&CommandResult::new(cmd, &run.report))?;
    } else {
        print.json(&CommandResult::with_warnings(cmd, warnings, &run.report))?;
    }
    Ok(())
}

#[cfg(any(feature = "dev-node", test))]
fn integration_test_environment(rpc_url: &str, contract_address: &str) -> Vec<(String, String)> {
    vec![
        ("GROUNDHOG_RPC_URL".into(), rpc_url.into()),
        ("GROUNDHOG_CONTRACT_ADDRESS".into(), contract_address.into()),
        ("GROUNDHOG_PROFILE".into(), "integration".into()),
        ("GROUNDHOG_DEFAULT_KEY".into(), "dev-0".into()),
    ]
}

#[cfg(feature = "dev-node")]
fn ensure_integration_test_target_exists(project_root: &Path) -> Result<(), CliError> {
    let path = project_root.join("tests").join("integration.rs");
    if path.try_exists()? {
        return Ok(());
    }
    Err(CliError::IntegrationTestNotFound(
        path.display().to_string(),
    ))
}

fn run_watch(
    package: Option<&str>,
    options: &TestOptions,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let watch = initial_watch_context(artifacts)?;

    print.status(format_args!(
        "Watching {} for changes. Press Ctrl-C to stop.",
        watch.root.display()
    ));

    loop {
        if let Err(error) = run_once_with_options(
            package,
            options,
            cargo_args,
            artifacts,
            explicit_profile,
            cmd,
            print,
        ) && !matches!(error, CliError::CommandReportedFailure)
        {
            print.command_error(cmd, &error)?;
        }
        let mut snapshot = watch_snapshot(&watch)?;

        print.status("Waiting for changes...");
        wait_for_watch_change(&watch, &mut snapshot, WATCH_POLL_INTERVAL, WATCH_DEBOUNCE)?;
        print.status("Change detected; rerunning WASM tests...");
    }
}

fn run_once_with_options(
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
        crate::build::build_project(package, false, &[], artifacts, explicit_profile, &[], print)?;

    // Run only the WASM test target
    let root = build.project_root.clone();
    let profile = build.profile.clone();
    let cache_path = failure_cache_path(&build.target_directory, &build.project_name);
    let cached_tests = read_failure_cache(&cache_path, &build.project_name)?;
    let rerun_filters = filter_rerun_tests(options, &cached_tests);

    if options.rerun && rerun_filters.is_empty() {
        return finish_empty_rerun(
            cmd,
            &build,
            &profile,
            &cache_path,
            &cached_tests,
            options.watch,
            print,
        );
    }
    print_test_status(options, rerun_filters.len(), print);

    let host_target = host_target()?;

    artifacts::ensure_no_target_dir_passthrough_conflicts(artifacts, &root, cargo_args)?;

    let filters = selected_filters(options, &rerun_filters);
    let context = TestCommandContext {
        root: &root,
        project_name: &build.project_name,
        host_target: &host_target,
        test_target: WASM_TEST_TARGET,
        env: &[],
        artifacts,
        cargo_args,
        print,
    };
    let outputs = run_test_commands(&context, options, &filters)?;

    let failed_tests = dedup_tests(
        outputs
            .iter()
            .flat_map(|output| extract_failed_tests(&output.stdout, &output.stderr)),
    );
    let executed_tests = executed_tests(&outputs);
    let cached_tests_after = update_failure_cache(
        &cache_path,
        &build.project_name,
        options,
        &cached_tests,
        &executed_tests,
        &failed_tests,
        &outputs,
    )?;

    let report = build_test_report(
        &build.project_root,
        &profile,
        &build.wasm,
        &outputs,
        options.rerun,
        &cache_path,
        &cached_tests,
        &cached_tests_after,
        &executed_tests,
        &failed_tests,
        options.watch,
    );

    if let Some(failed) = outputs.iter().find(|output| !output.success) {
        if print.is_json() {
            print.json(&CommandResult::failure(cmd, &report))?;
            return Err(CliError::CommandReportedFailure);
        }
        return Err(CliError::CargoFailed(failed.code));
    }

    print.json(&CommandResult::new(cmd, &report))?;
    Ok(())
}

fn initial_watch_context(artifacts: &ArtifactOptions) -> Result<WatchContext, CliError> {
    let cwd = std::env::current_dir()?;
    let metadata = crate::build::cargo_metadata_with_artifacts(artifacts);
    let root = match &metadata {
        Ok(metadata) => metadata.workspace_root.clone(),
        Err(_) => cwd,
    };
    let mut ignored_dirs = vec![root.join("target")];
    if let Ok(metadata) = metadata {
        push_ignored_dir(&root, &mut ignored_dirs, metadata.target_directory);
    }
    if let Some(target_dir) = artifacts.target_dir() {
        push_ignored_dir(&root, &mut ignored_dirs, target_dir.to_path_buf());
    }
    Ok(WatchContext { root, ignored_dirs })
}

fn push_ignored_dir(root: &Path, ignored_dirs: &mut Vec<PathBuf>, path: PathBuf) {
    let absolute = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if !ignored_dirs.contains(&absolute) {
        ignored_dirs.push(absolute);
    }
}

fn wait_for_watch_change(
    watch: &WatchContext,
    previous: &mut WatchSnapshot,
    poll_interval: Duration,
    debounce: Duration,
) -> Result<(), CliError> {
    loop {
        thread::sleep(poll_interval);
        let current = watch_snapshot(watch)?;
        if current == *previous {
            continue;
        }

        let mut stable = current;
        loop {
            thread::sleep(debounce);
            let next = watch_snapshot(watch)?;
            if next == stable {
                *previous = next;
                return Ok(());
            }
            stable = next;
        }
    }
}

fn watch_snapshot(watch: &WatchContext) -> Result<WatchSnapshot, CliError> {
    let mut snapshot = WatchSnapshot {
        files: BTreeMap::new(),
    };
    collect_watch_files(watch, &watch.root, &mut snapshot)?;
    Ok(snapshot)
}

fn collect_watch_files(
    watch: &WatchContext,
    dir: &Path,
    snapshot: &mut WatchSnapshot,
) -> Result<(), CliError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if !is_ignored_watch_dir(&entry.file_name()) && !is_ignored_watch_path(watch, &path) {
                collect_watch_files(watch, &path, snapshot)?;
            }
        } else if file_type.is_file() && should_watch_file(&watch.root, &path) {
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let relative = path
                .strip_prefix(&watch.root)
                .unwrap_or(&path)
                .to_path_buf();
            snapshot.files.insert(
                relative,
                WatchFileFingerprint {
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                },
            );
        }
    }
    Ok(())
}

fn is_ignored_watch_dir(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("target" | ".git" | ".hg" | ".svn" | ".direnv" | "node_modules")
    )
}

fn is_ignored_watch_path(watch: &WatchContext, path: &Path) -> bool {
    watch.ignored_dirs.iter().any(|ignored| path == ignored)
}

fn should_watch_file(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if starts_with_dir(relative, ".cargo") {
        return extension_is(relative, "toml");
    }
    if starts_with_dir(relative, "src")
        || starts_with_dir(relative, "tests")
        || starts_with_dir(relative, "benches")
        || starts_with_dir(relative, "examples")
    {
        return true;
    }
    if matches!(
        relative.file_name().and_then(OsStr::to_str),
        Some(
            "Cargo.toml"
                | "Cargo.lock"
                | "groundhog.toml"
                | "rust-toolchain"
                | "rust-toolchain.toml"
        )
    ) {
        return true;
    }
    extension_is(relative, "rs")
        || extension_is(relative, "toml")
        || extension_is(relative, "lock")
        || extension_is(relative, "json")
}

fn starts_with_dir(path: &Path, name: &str) -> bool {
    path.components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .is_some_and(|component| component == name)
}

fn extension_is(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension == expected)
}

fn filter_rerun_tests(options: &TestOptions, cached_tests: &[String]) -> Vec<String> {
    if options.rerun {
        filter_cached_tests(cached_tests, options.match_test.as_deref())
    } else {
        Vec::new()
    }
}

fn finish_empty_rerun(
    cmd: &'static str,
    build: &crate::build::BuildResult,
    profile: &crate::config::ResolvedProfile,
    cache_path: &Path,
    cached_tests: &[String],
    watch_enabled: bool,
    print: &Print,
) -> Result<(), CliError> {
    if cached_tests.is_empty() {
        clear_failure_cache(cache_path)?;
        print.warn("no cached failed WASM tests to rerun");
    } else {
        print.warn("no cached failed WASM tests match --match-test");
    }
    let report = build_test_report(
        &build.project_root,
        profile,
        &build.wasm,
        &[],
        true,
        cache_path,
        cached_tests,
        cached_tests,
        &[],
        &[],
        watch_enabled,
    );
    print.json(&CommandResult::new(cmd, &report))?;
    Ok(())
}

fn print_test_status(options: &TestOptions, rerun_count: usize, print: &Print) {
    if options.rerun {
        print.status(format_args!(
            "Rerunning {rerun_count} failed WASM test(s) (abort semantics)..."
        ));
    } else {
        print.status("Running WASM tests (abort semantics)...");
    }
}

fn host_target() -> Result<String, CliError> {
    let os = match env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        os => return Err(CliError::UnsupportedOs(os.to_string())),
    };
    Ok(format!("{}-{os}", env::consts::ARCH))
}

fn selected_filters<'a>(
    options: &'a TestOptions,
    rerun_filters: &'a [String],
) -> Vec<SelectedFilter<'a>> {
    if options.rerun {
        rerun_filters
            .iter()
            .map(|filter| SelectedFilter {
                name: Some(filter.as_str()),
                exact: true,
            })
            .collect()
    } else {
        vec![SelectedFilter {
            name: options.match_test.as_deref(),
            exact: false,
        }]
    }
}

fn run_test_commands(
    context: &TestCommandContext<'_>,
    options: &TestOptions,
    filters: &[SelectedFilter<'_>],
) -> Result<Vec<TestRunOutput>, CliError> {
    let mut outputs = Vec::new();
    for filter in filters {
        if options.rerun
            && let Some(name) = filter.name
        {
            context.print.status(format_args!("Rerunning {name}..."));
        }

        let mut cargo_cmd = test_command(
            context.root,
            context.project_name,
            context.host_target,
            context.test_target,
        );
        context.artifacts.apply_to_command(&mut cargo_cmd);
        apply_test_environment(&mut cargo_cmd, context.env);
        apply_harness_options(&mut cargo_cmd, options, *filter);
        for arg in context.cargo_args {
            cargo_cmd.arg(arg);
        }

        outputs.push(run_test_command(&mut cargo_cmd, context.print, *filter)?);
        if should_stop_rerun(options, &outputs) {
            break;
        }
    }
    Ok(outputs)
}

fn should_stop_rerun(options: &TestOptions, outputs: &[TestRunOutput]) -> bool {
    options.rerun && options.harness.fail_fast && outputs.last().is_some_and(|o| !o.success)
}

#[allow(clippy::too_many_arguments)]
fn build_test_report(
    project_root: &Path,
    profile: &crate::config::ResolvedProfile,
    wasm: &Path,
    outputs: &[TestRunOutput],
    rerun_enabled: bool,
    cache_path: &Path,
    cached_tests: &[String],
    cached_tests_after: &[String],
    executed_tests: &[String],
    failed_tests: &[String],
    watch_enabled: bool,
) -> TestReport {
    let runs = outputs.iter().map(parse_test_run).collect::<Vec<_>>();
    let summary = aggregate_summary(&runs);
    let success = outputs.iter().all(|output| output.success);

    TestReport {
        schema_version: TEST_JSON_SCHEMA_VERSION,
        project_root: project_root.to_path_buf(),
        profile: profile.clone(),
        wasm: wasm.to_path_buf(),
        success,
        summary,
        runs,
        rerun: TestRerunInfo {
            enabled: rerun_enabled,
            cache_path: cache_path.to_path_buf(),
            cached_tests: cached_tests.to_vec(),
            cached_tests_after: cached_tests_after.to_vec(),
            executed_tests: executed_tests.to_vec(),
            failed_tests: failed_tests.to_vec(),
        },
        watch: TestWatchInfo {
            enabled: watch_enabled,
        },
    }
}

fn parse_test_run(output: &TestRunOutput) -> TestRun {
    let tests = parse_test_cases(&output.stdout, &output.stderr);
    let summary = parse_test_summary(&output.stdout, &output.stderr, &tests);

    TestRun {
        filter: output.filter.clone(),
        exact_filter: output.exact_filter,
        success: output.success,
        exit_code: output.code,
        summary,
        tests,
        stdout: output.stdout.clone(),
        stderr: output.stderr.clone(),
    }
}

fn parse_test_cases(stdout: &str, stderr: &str) -> Vec<TestCase> {
    let mut tests: Vec<TestCase> = Vec::new();
    for line in stdout.lines().chain(stderr.lines()) {
        let Some((name, status)) = parse_test_case_line(line) else {
            continue;
        };
        if let Some(existing) = tests.iter_mut().find(|test| test.name == name) {
            existing.status = status;
        } else {
            tests.push(TestCase { name, status });
        }
    }
    tests
}

fn parse_test_case_line(line: &str) -> Option<(String, TestCaseStatus)> {
    let line = line.trim();
    let rest = line.strip_prefix("test ")?;
    let (name, status_text) = rest.split_once(" ... ")?;
    let status_token = status_text
        .split(|c: char| c == ',' || c.is_whitespace())
        .find(|token| !token.is_empty())?;
    let status = match status_token {
        "ok" => TestCaseStatus::Passed,
        "FAILED" => TestCaseStatus::Failed,
        "ignored" => TestCaseStatus::Ignored,
        "measured" => TestCaseStatus::Measured,
        _ => return None,
    };
    Some((name.to_string(), status))
}

fn parse_test_summary(stdout: &str, stderr: &str, tests: &[TestCase]) -> TestSummary {
    stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(parse_summary_line)
        .next_back()
        .unwrap_or_else(|| derive_summary_from_cases(tests))
}

fn parse_summary_line(line: &str) -> Option<TestSummary> {
    let line = line.trim();
    let rest = line.strip_prefix("test result: ")?;
    let (_, metrics) = rest.split_once(". ")?;

    let mut summary = TestSummary {
        source: TestSummarySource::Parsed,
        total: 0,
        passed: 0,
        failed: 0,
        ignored: 0,
        measured: 0,
        filtered_out: 0,
        duration_seconds: None,
    };

    for segment in metrics.split(';') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if let Some(count) = parse_summary_count(segment, "passed") {
            summary.passed = count;
            continue;
        }
        if let Some(count) = parse_summary_count(segment, "failed") {
            summary.failed = count;
            continue;
        }
        if let Some(count) = parse_summary_count(segment, "ignored") {
            summary.ignored = count;
            continue;
        }
        if let Some(count) = parse_summary_count(segment, "measured") {
            summary.measured = count;
            continue;
        }
        if let Some(count) = parse_summary_count(segment, "filtered out") {
            summary.filtered_out = count;
            continue;
        }
        if let Some(duration) = segment
            .strip_prefix("finished in ")
            .and_then(|value| value.strip_suffix('s'))
            .and_then(|value| value.parse::<f64>().ok())
        {
            summary.duration_seconds = Some(duration);
        }
    }

    summary.total = summary.passed + summary.failed + summary.ignored + summary.measured;
    if summary.total == 0 && summary.filtered_out == 0 && summary.duration_seconds.is_none() {
        return None;
    }
    Some(summary)
}

fn parse_summary_count(segment: &str, suffix: &str) -> Option<u64> {
    let number = segment.strip_suffix(suffix)?.trim_end();
    number.parse().ok()
}

fn derive_summary_from_cases(tests: &[TestCase]) -> TestSummary {
    if tests.is_empty() {
        return TestSummary {
            source: TestSummarySource::Empty,
            total: 0,
            passed: 0,
            failed: 0,
            ignored: 0,
            measured: 0,
            filtered_out: 0,
            duration_seconds: None,
        };
    }

    let mut summary = TestSummary {
        source: TestSummarySource::Derived,
        total: tests.len() as u64,
        passed: 0,
        failed: 0,
        ignored: 0,
        measured: 0,
        filtered_out: 0,
        duration_seconds: None,
    };
    for test in tests {
        match test.status {
            TestCaseStatus::Passed => summary.passed += 1,
            TestCaseStatus::Failed => summary.failed += 1,
            TestCaseStatus::Ignored => summary.ignored += 1,
            TestCaseStatus::Measured => summary.measured += 1,
        }
    }
    summary
}

fn aggregate_summary(runs: &[TestRun]) -> TestSummary {
    if runs.is_empty() {
        return derive_summary_from_cases(&[]);
    }

    let mut summary = runs[0].summary.clone();
    if runs.len() > 1 {
        summary.filtered_out = 0;
    }

    for run in &runs[1..] {
        summary.total += run.summary.total;
        summary.passed += run.summary.passed;
        summary.failed += run.summary.failed;
        summary.ignored += run.summary.ignored;
        summary.measured += run.summary.measured;

        match (summary.duration_seconds, run.summary.duration_seconds) {
            (Some(total), Some(duration)) => summary.duration_seconds = Some(total + duration),
            _ => summary.duration_seconds = None,
        }

        summary.source = match (summary.source, run.summary.source) {
            (TestSummarySource::Parsed, TestSummarySource::Parsed) => TestSummarySource::Parsed,
            (TestSummarySource::Empty, TestSummarySource::Empty) => TestSummarySource::Empty,
            (TestSummarySource::Empty, other) | (other, TestSummarySource::Empty) => other,
            _ => TestSummarySource::Derived,
        };
    }

    summary
}

#[cfg(test)]
fn combined_output(outputs: &[TestRunOutput], select: impl Fn(&TestRunOutput) -> &str) -> String {
    if outputs.len() <= 1 {
        return outputs.iter().map(select).collect();
    }

    let mut combined = String::new();
    for output in outputs {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str("--- rerun");
        if let Some(filter) = &output.filter {
            combined.push_str(": ");
            combined.push_str(filter);
        }
        combined.push_str(" ---\n");
        let chunk = select(output);
        combined.push_str(chunk);
        if !chunk.ends_with('\n') {
            combined.push('\n');
        }
    }
    combined
}

fn executed_tests(outputs: &[TestRunOutput]) -> Vec<String> {
    outputs
        .iter()
        .filter_map(|output| output.filter.clone())
        .collect()
}

fn test_command(root: &Path, project_name: &str, host_target: &str, test_target: &str) -> Command {
    let mut cargo_cmd = Command::new("cargo");
    cargo_cmd
        .current_dir(root)
        .arg("test")
        .arg("-p")
        .arg(project_name)
        .arg("--test")
        .arg(test_target)
        .arg("--target")
        .arg(host_target)
        .arg("--features")
        .arg("dev")
        .arg("--");
    if test_target == WASM_TEST_TARGET {
        cargo_cmd.arg("--include-ignored");
    }
    cargo_cmd
}

fn apply_test_environment(cargo_cmd: &mut Command, env: &[(String, String)]) {
    for (key, value) in env {
        cargo_cmd.env(key, value);
    }
}

fn apply_harness_options(
    cargo_cmd: &mut Command,
    options: &TestOptions,
    filter: SelectedFilter<'_>,
) {
    // Everything here is appended after cargo's `--` separator and is
    // therefore scoped to the selected libtest harness.
    if options.harness.fail_fast {
        cargo_cmd.arg("--fail-fast");
    }
    if options.harness.show_output {
        cargo_cmd.arg("--show-output");
    }
    if options.harness.no_capture {
        cargo_cmd.arg("--no-capture");
    }
    if let Some(threads) = options.test_threads {
        cargo_cmd.arg("--test-threads").arg(threads.to_string());
    }
    if let Some(filter_name) = filter.name {
        if filter.exact {
            cargo_cmd.arg("--exact");
        }
        cargo_cmd.arg(filter_name);
    }
    for skip in &options.skip {
        cargo_cmd.arg("--skip").arg(skip);
    }
}

fn run_test_command(
    cargo_cmd: &mut Command,
    print: &Print,
    filter: SelectedFilter<'_>,
) -> Result<TestRunOutput, CliError> {
    let mut child = cargo_cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .expect("bug: piped stdout should be available");
    let stderr = child
        .stderr
        .take()
        .expect("bug: piped stderr should be available");
    let capture_stdout = capture_stream(stdout, OutputStream::Stdout, !print.is_json());
    let capture_stderr = capture_stream(stderr, OutputStream::Stderr, !print.is_json());
    let status = child.wait()?;
    let stdout = join_capture(capture_stdout)?;
    let stderr = join_capture(capture_stderr)?;

    Ok(TestRunOutput {
        filter: filter.name.map(ToString::to_string),
        exact_filter: filter.exact,
        success: status.success(),
        code: status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn update_failure_cache(
    path: &Path,
    package: &str,
    options: &TestOptions,
    cached_tests: &[String],
    executed_tests: &[String],
    failed_tests: &[String],
    outputs: &[TestRunOutput],
) -> Result<Vec<String>, CliError> {
    if outputs.iter().any(|output| !output.success) && failed_tests.is_empty() {
        return Ok(cached_tests.to_vec());
    }

    let next = next_cached_failures(options, cached_tests, executed_tests, failed_tests);
    write_failure_cache(path, package, &next)?;
    Ok(next)
}

fn next_cached_failures(
    options: &TestOptions,
    cached_tests: &[String],
    executed_tests: &[String],
    failed_tests: &[String],
) -> Vec<String> {
    if !options.rerun {
        return dedup_tests(failed_tests.iter().cloned());
    }

    let mut next = Vec::new();
    for test in cached_tests {
        if executed_tests.contains(test) {
            if failed_tests.contains(test) {
                next.push(test.clone());
            }
        } else {
            next.push(test.clone());
        }
    }
    for test in failed_tests {
        if !next.contains(test) {
            next.push(test.clone());
        }
    }
    next
}

#[derive(Debug, Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

fn capture_stream<R>(
    mut reader: R,
    stream: OutputStream,
    replay: bool,
) -> thread::JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let chunk = &buffer[..read];
            captured.extend_from_slice(chunk);
            if replay {
                write_stream(stream, chunk)?;
            }
        }
        Ok(captured)
    })
}

fn write_stream(stream: OutputStream, chunk: &[u8]) -> io::Result<()> {
    match stream {
        OutputStream::Stdout => io::stdout().write_all(chunk),
        OutputStream::Stderr => io::stderr().write_all(chunk),
    }
}

fn join_capture(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, CliError> {
    handle
        .join()
        .map_err(|_| CliError::Other("test output capture thread panicked".into()))?
        .map_err(CliError::Io)
}

fn failure_cache_path(target_directory: &Path, package: &str) -> PathBuf {
    target_directory
        .join("groundhog")
        .join("test-failures")
        .join(format!("{package}.json"))
}

fn read_failure_cache(path: &Path, package: &str) -> Result<Vec<String>, CliError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let Ok(cache) = serde_json::from_str::<FailureCache>(&contents) else {
        return Ok(Vec::new());
    };
    if cache.version != FAILURE_CACHE_VERSION || cache.package != package {
        return Ok(Vec::new());
    }
    Ok(dedup_tests(cache.tests))
}

fn write_failure_cache(path: &Path, package: &str, tests: &[String]) -> Result<(), CliError> {
    if tests.is_empty() {
        return clear_failure_cache(path);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let cache = FailureCache {
        version: FAILURE_CACHE_VERSION,
        package: package.to_string(),
        tests: tests.to_vec(),
    };
    let contents = serde_json::to_vec_pretty(&cache).map_err(|e| CliError::Other(e.to_string()))?;
    fs::write(path, contents)?;
    Ok(())
}

fn clear_failure_cache(path: &Path) -> Result<(), CliError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn filter_cached_tests(cached_tests: &[String], match_test: Option<&str>) -> Vec<String> {
    cached_tests
        .iter()
        .filter(|test| match_test.is_none_or(|filter| test.contains(filter)))
        .cloned()
        .collect()
}

fn extract_failed_tests(stdout: &str, stderr: &str) -> Vec<String> {
    dedup_tests(stdout.lines().chain(stderr.lines()).filter_map(|line| {
        let line = line.trim();
        let name = line.strip_prefix("test ")?.strip_suffix(" ... FAILED")?;
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }))
}

fn dedup_tests(tests: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut unique = Vec::new();
    for test in tests {
        if !unique.contains(&test) {
            unique.push(test);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
    use super::{
        FAILURE_CACHE_VERSION, FailureCache, INTEGRATION_TEST_TARGET, IntegrationTestOptions,
        SelectedFilter, TestHarnessOptions, TestOptions, TestRunOutput, WASM_TEST_TARGET,
        WatchContext, aggregate_summary, apply_harness_options, apply_test_environment,
        combined_output, derive_summary_from_cases, extract_failed_tests, failure_cache_path,
        filter_cached_tests, next_cached_failures, parse_summary_line, parse_test_case_line,
        read_failure_cache, should_stop_rerun, should_watch_file, test_command,
        update_failure_cache, watch_snapshot, write_failure_cache,
    };
    use crate::test_json::{TestCase, TestCaseStatus, TestSummarySource};
    use crate::{CliError, Print};
    #[cfg(feature = "dev-node")]
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn test_watch_context(root: &std::path::Path) -> WatchContext {
        WatchContext {
            root: root.to_path_buf(),
            ignored_dirs: vec![root.join("target")],
        }
    }

    #[cfg(feature = "dev-node")]
    fn integration_run(success: bool) -> super::IntegrationRun {
        let profile = crate::config::ResolvedProfile {
            name: "integration".into(),
            rpc_url: Some("http://127.0.0.1:1234".into()),
            default_key: Some("dev-0".into()),
            contracts: BTreeMap::new(),
            source: None,
        };
        let outputs = vec![TestRunOutput {
            filter: None,
            exact_filter: false,
            success,
            code: if success { 0 } else { 101 },
            stdout: if success {
                "running 1 test\ntest tests::ok ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"
            } else {
                "running 1 test\ntest tests::fails ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"
            }
            .into(),
            stderr: String::new(),
        }];
        let report = super::build_test_report(
            std::path::Path::new("/project"),
            &profile,
            std::path::Path::new("/project/target/demo.wasm"),
            &outputs,
            false,
            std::path::Path::new("/project/target/.groundhog/failures.json"),
            &[],
            &[],
            &[],
            &[],
            false,
        );

        super::IntegrationRun {
            report,
            failed_code: if success { None } else { Some(101) },
        }
    }

    #[cfg(not(feature = "dev-node"))]
    #[test]
    fn integration_path_reports_unavailable_before_wasm_build() {
        let options = TestOptions::default();

        let err = super::run_integration_with_options(
            None,
            &options,
            &IntegrationTestOptions::default(),
            &[],
            &crate::artifacts::ArtifactOptions::default(),
            None,
            "test",
            &Print::quiet(),
        )
        .unwrap_err();

        assert!(
            matches!(err, CliError::IntegrationTestsUnavailable),
            "expected IntegrationTestsUnavailable, got: {err:?}"
        );
    }

    #[test]
    fn integration_path_rejects_watch_mode() {
        let options = TestOptions {
            watch: true,
            ..TestOptions::default()
        };

        let err = super::run_integration_with_options(
            None,
            &options,
            &IntegrationTestOptions::default(),
            &[],
            &crate::artifacts::ArtifactOptions::default(),
            None,
            "test",
            &Print::quiet(),
        )
        .unwrap_err();

        let CliError::InvalidTestOptions(message) = err else {
            panic!("expected InvalidTestOptions, got: {err:?}");
        };
        assert!(message.contains("--integration"));
        assert!(message.contains("--watch"));
    }

    #[test]
    fn integration_path_rejects_rerun_mode() {
        let options = TestOptions {
            rerun: true,
            ..TestOptions::default()
        };

        let err = super::run_integration_with_options(
            None,
            &options,
            &IntegrationTestOptions::default(),
            &[],
            &crate::artifacts::ArtifactOptions::default(),
            None,
            "test",
            &Print::quiet(),
        )
        .unwrap_err();

        let CliError::InvalidTestOptions(message) = err else {
            panic!("expected InvalidTestOptions, got: {err:?}");
        };
        assert!(message.contains("--integration"));
        assert!(message.contains("--rerun"));
    }

    #[test]
    fn integration_command_selects_integration_target() {
        let command = test_command(
            std::path::Path::new("/project"),
            "demo",
            "x86_64-unknown-linux-gnu",
            INTEGRATION_TEST_TARGET,
        );

        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2)
                .any(|window| window[0] == "--test" && window[1] == "integration")
        );
        assert!(
            !args
                .windows(2)
                .any(|window| window[0] == "--test" && window[1] == WASM_TEST_TARGET)
        );
        assert!(!args.iter().any(|arg| arg == "--include-ignored"));
    }

    #[test]
    fn wasm_command_includes_ignored_tests() {
        let command = test_command(
            std::path::Path::new("/project"),
            "demo",
            "x86_64-unknown-linux-gnu",
            WASM_TEST_TARGET,
        );

        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|arg| arg == "--include-ignored"));
    }

    #[test]
    fn integration_test_environment_returns_expected_values() {
        let env = super::integration_test_environment(
            "http://127.0.0.1:1234",
            &format!("0x{}", "11".repeat(32)),
        );

        assert!(env.contains(&("GROUNDHOG_RPC_URL".into(), "http://127.0.0.1:1234".into())));
        assert!(env.iter().any(|(key, value)| {
            key == "GROUNDHOG_CONTRACT_ADDRESS" && value == &format!("0x{}", "11".repeat(32))
        }));
        assert!(env.contains(&("GROUNDHOG_PROFILE".into(), "integration".into())));
        assert!(env.contains(&("GROUNDHOG_DEFAULT_KEY".into(), "dev-0".into())));
    }

    #[test]
    fn apply_environment_sets_integration_values() {
        let mut command = Command::new("cargo");
        let env = super::integration_test_environment(
            "http://127.0.0.1:1234",
            &format!("0x{}", "11".repeat(32)),
        );

        apply_test_environment(&mut command, &env);

        let applied: Vec<_> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value
                        .map(|value| value.to_string_lossy().into_owned())
                        .expect("bug: env var should be set"),
                )
            })
            .collect();
        assert!(applied.contains(&("GROUNDHOG_RPC_URL".into(), "http://127.0.0.1:1234".into())));
        assert!(applied.iter().any(|(key, value)| {
            key == "GROUNDHOG_CONTRACT_ADDRESS" && value == &format!("0x{}", "11".repeat(32))
        }));
        assert!(applied.contains(&("GROUNDHOG_PROFILE".into(), "integration".into())));
        assert!(applied.contains(&("GROUNDHOG_DEFAULT_KEY".into(), "dev-0".into())));
    }

    #[cfg(feature = "dev-node")]
    #[test]
    fn integration_preflight_requires_documented_test_target() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let err = super::ensure_integration_test_target_exists(tmp.path()).unwrap_err();
        assert!(
            matches!(err, CliError::IntegrationTestNotFound(_)),
            "expected IntegrationTestNotFound, got {err:?}"
        );

        fs::create_dir_all(tmp.path().join("tests")).expect("bug: tests dir should be created");
        fs::write(tmp.path().join("tests/integration.rs"), "")
            .expect("bug: integration test should write");
        super::ensure_integration_test_target_exists(tmp.path())
            .expect("bug: integration test target should exist");
    }

    #[cfg(feature = "dev-node")]
    #[test]
    fn integration_outcome_emits_report_when_tests_pass_and_shutdown_fails() {
        super::handle_integration_outcome(
            Ok(integration_run(true)),
            Err(CliError::NodeSupervisor("stop failed".into())),
            "test",
            &Print::quiet(),
        )
        .expect("bug: passing tests should not fail only because cleanup failed");
    }

    #[cfg(feature = "dev-node")]
    #[test]
    fn integration_outcome_preserves_failed_tests_when_shutdown_fails() {
        let err = super::handle_integration_outcome(
            Ok(integration_run(false)),
            Err(CliError::NodeSupervisor("stop failed".into())),
            "test",
            &Print::quiet(),
        )
        .unwrap_err();
        assert!(matches!(err, CliError::CargoFailed(101)));
    }

    #[cfg(feature = "dev-node")]
    #[test]
    fn integration_outcome_preserves_failed_tests_after_clean_shutdown() {
        let err = super::handle_integration_outcome(
            Ok(integration_run(false)),
            Ok(()),
            "test",
            &Print::quiet(),
        )
        .unwrap_err();
        assert!(matches!(err, CliError::CargoFailed(101)));
    }

    #[cfg(feature = "dev-node")]
    #[test]
    fn integration_outcome_preserves_run_error_after_clean_shutdown() {
        let err = super::handle_integration_outcome(
            Err(CliError::Other("deploy failed".into())),
            Ok(()),
            "test",
            &Print::quiet(),
        )
        .unwrap_err();
        assert!(matches!(err, CliError::Other(message) if message == "deploy failed"));
    }

    #[test]
    fn default_harness_options_add_no_args() {
        let mut command = Command::new("cargo");

        apply_harness_options(
            &mut command,
            &TestOptions::default(),
            SelectedFilter {
                name: None,
                exact: false,
            },
        );

        assert_eq!(command.get_args().count(), 0);
    }

    #[test]
    fn harness_options_map_to_libtest_args() {
        let mut command = Command::new("cargo");
        let options = TestOptions {
            match_test: Some("counter".into()),
            test_threads: Some(1),
            skip: vec!["slow".into(), "flaky".into()],
            rerun: false,
            watch: false,
            harness: TestHarnessOptions {
                fail_fast: true,
                show_output: true,
                no_capture: true,
            },
        };

        apply_harness_options(
            &mut command,
            &options,
            SelectedFilter {
                name: options.match_test.as_deref(),
                exact: false,
            },
        );

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
                "counter",
                "--skip",
                "slow",
                "--skip",
                "flaky",
            ]
        );
    }

    #[test]
    fn exact_filter_replaces_match_test_for_rerun() {
        let mut command = Command::new("cargo");
        let options = TestOptions {
            match_test: Some("counter".into()),
            ..TestOptions::default()
        };

        apply_harness_options(
            &mut command,
            &options,
            SelectedFilter {
                name: Some("tests::failed_case"),
                exact: true,
            },
        );

        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["--exact", "tests::failed_case"]);
    }

    #[test]
    fn watch_filter_includes_sources_tests_and_config() {
        let root = PathBuf::from("/project");

        assert!(should_watch_file(&root, &root.join("src/lib.rs")));
        assert!(should_watch_file(
            &root,
            &root.join("tests/fixtures/input.txt")
        ));
        assert!(should_watch_file(&root, &root.join(".cargo/contract.toml")));
        assert!(should_watch_file(&root, &root.join("groundhog.toml")));
        assert!(!should_watch_file(&root, &root.join("README.md")));
    }

    #[test]
    fn watch_snapshot_ignores_target_and_tracks_changes() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        fs::create_dir_all(tmp.path().join("src")).expect("bug: src dir should be created");
        fs::create_dir_all(tmp.path().join("target")).expect("bug: target dir should be created");
        fs::write(tmp.path().join("src/lib.rs"), "pub fn old() {}\n")
            .expect("bug: source should write");
        fs::write(tmp.path().join("target/generated.rs"), "ignored\n")
            .expect("bug: target file should write");

        let watch = test_watch_context(tmp.path());
        let first = watch_snapshot(&watch).expect("bug: watch snapshot should succeed");
        fs::write(tmp.path().join("src/lib.rs"), "pub fn changed() {}\n")
            .expect("bug: source should update");
        let second = watch_snapshot(&watch).expect("bug: watch snapshot should succeed");

        assert!(
            first
                .files
                .contains_key(PathBuf::from("src/lib.rs").as_path())
        );
        assert!(
            !first
                .files
                .contains_key(PathBuf::from("target/generated.rs").as_path())
        );
        assert_ne!(first, second);
    }

    #[test]
    fn watch_snapshot_ignores_custom_target_dir() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        fs::create_dir_all(tmp.path().join("src")).expect("bug: src dir should be created");
        fs::create_dir_all(tmp.path().join("out")).expect("bug: out dir should be created");
        fs::write(tmp.path().join("src/lib.rs"), "pub fn source() {}\n")
            .expect("bug: source should write");
        fs::write(tmp.path().join("out/generated.rs"), "ignored\n")
            .expect("bug: target file should write");
        let watch = WatchContext {
            root: tmp.path().to_path_buf(),
            ignored_dirs: vec![tmp.path().join("out")],
        };

        let snapshot = watch_snapshot(&watch).expect("bug: watch snapshot should succeed");

        assert!(
            snapshot
                .files
                .contains_key(PathBuf::from("src/lib.rs").as_path())
        );
        assert!(
            !snapshot
                .files
                .contains_key(PathBuf::from("out/generated.rs").as_path())
        );
    }

    #[cfg(unix)]
    #[test]
    fn watch_snapshot_skips_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let linked_target = tmp.path().join("linked-target");
        fs::create_dir_all(linked_target.join("src"))
            .expect("bug: linked target src dir should be created");
        fs::write(linked_target.join("src/lib.rs"), "pub fn linked() {}\n")
            .expect("bug: linked source should write");
        symlink(&linked_target, tmp.path().join("linked"))
            .expect("bug: directory symlink should be created");
        let watch = test_watch_context(tmp.path());

        let snapshot = watch_snapshot(&watch).expect("bug: watch snapshot should succeed");

        assert!(
            !snapshot
                .files
                .contains_key(PathBuf::from("linked/src/lib.rs").as_path())
        );
    }

    #[test]
    fn extracts_failed_tests_from_libtest_output() {
        let stdout = "\
running 3 tests
test tests::ok ... ok
test tests::slow_failure ... FAILED
test nested::case::fails ... FAILED

failures:
    tests::slow_failure
    nested::case::fails
";

        let failed = extract_failed_tests(stdout, "");

        assert_eq!(failed, vec!["tests::slow_failure", "nested::case::fails"]);
    }

    #[test]
    fn parses_test_case_status_lines() {
        assert_eq!(
            parse_test_case_line("test tests::ok ... ok"),
            Some(("tests::ok".into(), TestCaseStatus::Passed))
        );
        assert_eq!(
            parse_test_case_line("test tests::fails ... FAILED"),
            Some(("tests::fails".into(), TestCaseStatus::Failed))
        );
        assert_eq!(
            parse_test_case_line("test tests::skip ... ignored, flaky on ci"),
            Some(("tests::skip".into(), TestCaseStatus::Ignored))
        );
        assert_eq!(
            parse_test_case_line("test tests::bench ... measured"),
            Some(("tests::bench".into(), TestCaseStatus::Measured))
        );
    }

    #[test]
    fn parses_libtest_summary_line() {
        let summary = parse_summary_line(
            "test result: FAILED. 1 passed; 2 failed; 3 ignored; \
             4 measured; 5 filtered out; finished in 0.42s",
        )
        .expect("bug: summary line should parse");

        assert_eq!(summary.source, TestSummarySource::Parsed);
        assert_eq!(summary.total, 10);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 2);
        assert_eq!(summary.ignored, 3);
        assert_eq!(summary.measured, 4);
        assert_eq!(summary.filtered_out, 5);
        assert_eq!(summary.duration_seconds, Some(0.42));
    }

    #[test]
    fn ignores_uninformative_libtest_summary_line() {
        let summary = parse_summary_line("test result: ok. (no details)");

        assert_eq!(summary, None);
    }

    #[test]
    fn derives_summary_when_libtest_summary_is_missing() {
        let cases = vec![
            TestCase {
                name: "tests::ok".into(),
                status: TestCaseStatus::Passed,
            },
            TestCase {
                name: "tests::fail".into(),
                status: TestCaseStatus::Failed,
            },
            TestCase {
                name: "tests::skip".into(),
                status: TestCaseStatus::Ignored,
            },
        ];

        let summary = derive_summary_from_cases(&cases);

        assert_eq!(summary.source, TestSummarySource::Derived);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.ignored, 1);
        assert_eq!(summary.measured, 0);
        assert_eq!(summary.filtered_out, 0);
        assert_eq!(summary.duration_seconds, None);
    }

    #[test]
    fn aggregate_summary_preserves_parsed_source_for_single_run() {
        let run = crate::test_json::TestRun {
            filter: None,
            exact_filter: false,
            success: true,
            exit_code: 0,
            summary: parse_summary_line(
                "test result: ok. 2 passed; 0 failed; 0 ignored; \
                 0 measured; 0 filtered out; finished in 0.01s",
            )
            .expect("bug: summary line should parse"),
            tests: vec![],
            stdout: String::new(),
            stderr: String::new(),
        };

        let summary = aggregate_summary(&[run]);

        assert_eq!(summary.source, TestSummarySource::Parsed);
        assert_eq!(summary.total, 2);
        assert_eq!(summary.duration_seconds, Some(0.01));
    }

    #[test]
    fn aggregate_summary_keeps_parsed_source_when_later_run_is_empty() {
        let parsed = |line| parse_summary_line(line).expect("bug: summary line should parse");
        let runs = vec![
            crate::test_json::TestRun {
                filter: Some("tests::a".into()),
                exact_filter: true,
                success: true,
                exit_code: 0,
                summary: parsed(
                    "test result: ok. 1 passed; 0 failed; 0 ignored; \
                     0 measured; 0 filtered out; finished in 0.01s",
                ),
                tests: vec![],
                stdout: String::new(),
                stderr: String::new(),
            },
            crate::test_json::TestRun {
                filter: Some("tests::b".into()),
                exact_filter: true,
                success: false,
                exit_code: 101,
                summary: derive_summary_from_cases(&[]),
                tests: vec![],
                stdout: String::new(),
                stderr: String::new(),
            },
        ];

        let summary = aggregate_summary(&runs);

        assert_eq!(summary.source, TestSummarySource::Parsed);
        assert_eq!(summary.total, 1);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn aggregate_summary_zeroes_filtered_out_for_multiple_runs() {
        let parsed = |line| parse_summary_line(line).expect("bug: summary line should parse");
        let runs = vec![
            crate::test_json::TestRun {
                filter: Some("tests::a".into()),
                exact_filter: true,
                success: false,
                exit_code: 101,
                summary: parsed(
                    "test result: FAILED. 0 passed; 1 failed; 0 ignored; \
                     0 measured; 4 filtered out; finished in 0.01s",
                ),
                tests: vec![],
                stdout: String::new(),
                stderr: String::new(),
            },
            crate::test_json::TestRun {
                filter: Some("tests::b".into()),
                exact_filter: true,
                success: false,
                exit_code: 101,
                summary: parsed(
                    "test result: FAILED. 0 passed; 1 failed; 0 ignored; \
                     0 measured; 4 filtered out; finished in 0.02s",
                ),
                tests: vec![],
                stdout: String::new(),
                stderr: String::new(),
            },
        ];

        let summary = aggregate_summary(&runs);

        assert_eq!(summary.total, 2);
        assert_eq!(summary.failed, 2);
        assert_eq!(summary.filtered_out, 0);
        assert_eq!(summary.duration_seconds, Some(0.03));
    }

    #[test]
    fn deduplicates_failed_tests_from_stdout_and_stderr() {
        let stdout = "test tests::same ... FAILED\n";
        let stderr = "test tests::same ... FAILED\n";

        let failed = extract_failed_tests(stdout, stderr);

        assert_eq!(failed, vec!["tests::same"]);
    }

    #[test]
    fn filters_cached_tests_by_match_substring() {
        let cached = vec![
            "counter::increments".to_string(),
            "wallet::rejects".to_string(),
            "counter::decrements".to_string(),
        ];

        let filtered = filter_cached_tests(&cached, Some("counter"));

        assert_eq!(filtered, vec!["counter::increments", "counter::decrements"]);
    }

    #[test]
    fn filtered_rerun_preserves_nonmatching_cached_failures() {
        let cached = vec!["tests::a".to_string(), "tests::b".to_string()];
        let options = TestOptions {
            rerun: true,
            ..TestOptions::default()
        };
        let executed = vec!["tests::a".to_string()];

        let next = next_cached_failures(&options, &cached, &executed, &[]);

        assert_eq!(next, vec!["tests::b"]);
    }

    #[test]
    fn filtered_rerun_keeps_refailing_test_in_cached_order() {
        let cached = vec!["tests::a".to_string(), "tests::b".to_string()];
        let options = TestOptions {
            rerun: true,
            ..TestOptions::default()
        };
        let executed = vec!["tests::a".to_string()];
        let failed = vec!["tests::a".to_string()];

        let next = next_cached_failures(&options, &cached, &executed, &failed);

        assert_eq!(next, vec!["tests::a", "tests::b"]);
    }

    #[test]
    fn preserves_cache_when_command_fails_without_failed_test_lines() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let path = tmp.path().join("target/groundhog/test-failures/demo.json");
        let cached = vec!["tests::a".to_string(), "tests::b".to_string()];
        write_failure_cache(&path, "demo", &cached).expect("bug: cache should write");
        let options = TestOptions {
            rerun: true,
            ..TestOptions::default()
        };
        let outputs = vec![TestRunOutput {
            filter: Some("tests::a".into()),
            exact_filter: true,
            success: false,
            code: 101,
            stdout: "compile failed".into(),
            stderr: String::new(),
        }];
        let executed = vec!["tests::a".to_string()];

        let next = update_failure_cache(&path, "demo", &options, &cached, &executed, &[], &outputs)
            .expect("bug: cache update should succeed");
        let loaded = read_failure_cache(&path, "demo").expect("bug: cache should read");

        assert_eq!(next, cached);
        assert_eq!(loaded, cached);
    }

    #[test]
    fn combined_output_separates_multiple_rerun_outputs() {
        let outputs = vec![
            TestRunOutput {
                filter: Some("tests::a".into()),
                exact_filter: true,
                success: false,
                code: 101,
                stdout: "a output\n".into(),
                stderr: String::new(),
            },
            TestRunOutput {
                filter: Some("tests::b".into()),
                exact_filter: true,
                success: false,
                code: 101,
                stdout: "b output\n".into(),
                stderr: String::new(),
            },
        ];

        let combined = combined_output(&outputs, |output| &output.stdout);

        assert!(combined.contains("--- rerun: tests::a ---\na output"));
        assert!(combined.contains("--- rerun: tests::b ---\nb output"));
    }

    #[test]
    fn rerun_fail_fast_stops_after_first_failed_invocation() {
        let options = TestOptions {
            rerun: true,
            harness: TestHarnessOptions {
                fail_fast: true,
                ..TestHarnessOptions::default()
            },
            ..TestOptions::default()
        };
        let outputs = vec![TestRunOutput {
            filter: Some("tests::a".into()),
            exact_filter: true,
            success: false,
            code: 101,
            stdout: String::new(),
            stderr: String::new(),
        }];

        assert!(should_stop_rerun(&options, &outputs));
    }

    #[test]
    fn fail_fast_rerun_preserves_unexecuted_cached_failures() {
        let cached = vec!["tests::a".to_string(), "tests::b".to_string()];
        let options = TestOptions {
            rerun: true,
            harness: TestHarnessOptions {
                fail_fast: true,
                ..TestHarnessOptions::default()
            },
            ..TestOptions::default()
        };
        let executed = vec!["tests::a".to_string()];
        let failed = vec!["tests::a".to_string()];

        let next = next_cached_failures(&options, &cached, &executed, &failed);

        assert_eq!(next, vec!["tests::a", "tests::b"]);
    }

    #[test]
    fn failure_cache_path_is_package_scoped() {
        let path = failure_cache_path(PathBuf::from("/tmp/target").as_path(), "demo-contract");

        assert_eq!(
            path,
            PathBuf::from("/tmp/target/groundhog/test-failures/demo-contract.json")
        );
    }

    #[test]
    fn failure_cache_roundtrips_and_deduplicates() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let path = tmp.path().join("target/groundhog/test-failures/demo.json");
        let tests = vec![
            "tests::fails".to_string(),
            "tests::fails".to_string(),
            "tests::also_fails".to_string(),
        ];

        write_failure_cache(&path, "demo", &tests).expect("bug: cache should write");
        let loaded = read_failure_cache(&path, "demo").expect("bug: cache should read");

        assert_eq!(loaded, vec!["tests::fails", "tests::also_fails"]);
    }

    #[test]
    fn empty_failure_cache_clears_file() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let path = tmp.path().join("target/groundhog/test-failures/demo.json");
        write_failure_cache(&path, "demo", &["tests::fails".into()])
            .expect("bug: cache should write");

        write_failure_cache(&path, "demo", &[]).expect("bug: cache should clear");

        assert!(!path.exists());
    }

    #[test]
    fn ignores_cache_for_other_package() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let path = tmp.path().join("target/groundhog/test-failures/demo.json");
        let cache = FailureCache {
            version: FAILURE_CACHE_VERSION,
            package: "other".into(),
            tests: vec!["tests::fails".into()],
        };
        fs::create_dir_all(path.parent().expect("bug: path should have parent"))
            .expect("bug: cache dir should be created");
        fs::write(
            &path,
            serde_json::to_vec(&cache).expect("bug: cache should serialize"),
        )
        .expect("bug: cache should write");

        let loaded = read_failure_cache(&path, "demo").expect("bug: cache should read");

        assert!(loaded.is_empty());
    }
}
