use crate::artifacts::{self, ArtifactOptions};
use crate::{CliError, Print};
use serde::{Deserialize, Serialize};
use serde_json::json;
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

/// Boolean controls forwarded to the `test_wasm` libtest harness.
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
        ) {
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
        crate::build::build_project(package, false, &[], artifacts, explicit_profile, print)?;

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
    let cargo_stdout = combined_output(&outputs, |output| &output.stdout);
    let cargo_stderr = combined_output(&outputs, |output| &output.stderr);
    let cached_tests_after = update_failure_cache(
        &cache_path,
        &build.project_name,
        options,
        &cached_tests,
        &executed_tests,
        &failed_tests,
        &outputs,
    )?;

    if let Some(failed) = outputs.iter().find(|output| !output.success) {
        if print.is_json() {
            return Err(CliError::CargoFailedWithOutput {
                code: failed.code,
                stdout: cargo_stdout,
                stderr: cargo_stderr,
            });
        }
        return Err(CliError::CargoFailed(failed.code));
    }

    print.json(&crate::result::CommandResult::new(
        cmd,
        json!({
            "project_root": root,
            "profile": profile,
            "wasm": build.wasm,
            "cargo_stdout": cargo_stdout,
            "cargo_stderr": cargo_stderr,
            "rerun": {
                "enabled": options.rerun,
                "cache_path": cache_path,
                "cached_tests": cached_tests,
                "cached_tests_after": cached_tests_after,
                "executed_tests": executed_tests,
                "failed_tests": failed_tests,
            },
            "watch": {
                "enabled": options.watch,
            },
        }),
    ))?;
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
    print.json(&crate::result::CommandResult::new(
        cmd,
        json!({
            "project_root": build.project_root,
            "profile": profile,
            "wasm": build.wasm,
            "cargo_stdout": "",
            "cargo_stderr": "",
            "rerun": {
                "enabled": true,
                "cache_path": cache_path,
                "cached_tests": cached_tests,
                "cached_tests_after": cached_tests,
                "executed_tests": [],
                "failed_tests": [],
            },
            "watch": {
                "enabled": watch_enabled,
            },
        }),
    ))?;
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

        let mut cargo_cmd = test_command(context.root, context.project_name, context.host_target);
        context.artifacts.apply_to_command(&mut cargo_cmd);
        apply_harness_options(&mut cargo_cmd, options, *filter);
        for arg in context.cargo_args {
            cargo_cmd.arg(arg);
        }

        outputs.push(run_test_command(
            &mut cargo_cmd,
            context.print,
            filter.name,
        )?);
        if should_stop_rerun(options, &outputs) {
            break;
        }
    }
    Ok(outputs)
}

fn should_stop_rerun(options: &TestOptions, outputs: &[TestRunOutput]) -> bool {
    options.rerun && options.harness.fail_fast && outputs.last().is_some_and(|o| !o.success)
}

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

fn test_command(root: &Path, project_name: &str, host_target: &str) -> Command {
    let mut cargo_cmd = Command::new("cargo");
    cargo_cmd
        .current_dir(root)
        .arg("test")
        .arg("-p")
        .arg(project_name)
        .arg("--test")
        .arg("test_wasm")
        .arg("--target")
        .arg(host_target)
        .arg("--features")
        .arg("dev")
        .arg("--")
        .arg("--include-ignored");
    cargo_cmd
}

fn apply_harness_options(
    cargo_cmd: &mut Command,
    options: &TestOptions,
    filter: SelectedFilter<'_>,
) {
    // Everything here is appended after cargo's `--` separator and is
    // therefore scoped to the `test_wasm` libtest harness.
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
    filter: Option<&str>,
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
        filter: filter.map(ToString::to_string),
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
        FAILURE_CACHE_VERSION, FailureCache, SelectedFilter, TestHarnessOptions, TestOptions,
        TestRunOutput, WatchContext, apply_harness_options, combined_output, extract_failed_tests,
        failure_cache_path, filter_cached_tests, next_cached_failures, read_failure_cache,
        should_stop_rerun, should_watch_file, update_failure_cache, watch_snapshot,
        write_failure_cache,
    };
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
                success: false,
                code: 101,
                stdout: "a output\n".into(),
                stderr: String::new(),
            },
            TestRunOutput {
                filter: Some("tests::b".into()),
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
