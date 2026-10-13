use serial_test::serial;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

mod support;

fn write_rv32_build_fixture(project_root: &Path) -> (PathBuf, PathBuf) {
    fs::create_dir_all(project_root.join(".cargo")).unwrap();
    fs::write(project_root.join(".cargo/contract.toml"), "").unwrap();
    fs::write(
        project_root.join(".cargo/config.toml"),
        "[target.riscv32im-unknown-none-elf]\n\
         rustflags = [\"-C\", \"link-arg=-Tgroundhog-rv32.ld\"]\n",
    )
    .unwrap();
    let fixture = project_root.join("fixture.elf");
    fs::write(&fixture, support::minimal_rv32_elf()).unwrap();
    let raw_elf = project_root.join("target/riscv32im-unknown-none-elf/release/demo_contract");
    (fixture, raw_elf)
}

/// Verify that `test_cmd::run` fails when run outside
/// a cargo project (cargo metadata fails).
#[test]
#[serial]
fn test_fails_outside_project() {
    let tmp = TempDir::new().unwrap();
    let _cwd = CurrentDirGuard::enter(tmp.path());

    let result = thruput_cli::test_cmd::run(
        None,
        &[],
        &thruput_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &thruput_cli::Print::quiet(),
    );

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("cargo metadata failed"),
        "expected CargoMetadataFailed, got: {err}"
    );
}

#[test]
#[serial]
fn test_rejects_conflicting_target_dir_passthrough() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join(".cargo")).unwrap();
    fs::write(tmp.path().join(".cargo/contract.toml"), "").unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
        tmp.path().display(),
        tmp.path().join("target").display(),
        tmp.path().join("Cargo.toml").display()
    );
    fs::write(
        &cargo,
        format!("#!/bin/sh\nprintf '%s' '{metadata_json}'\n"),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&cargo).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cargo, perms).unwrap();
    }

    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fake_bin.as_os_str());

    let artifacts =
        thruput_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let err = thruput_cli::test_cmd::run(
        Some("demo-contract"),
        &["--target-dir".into(), "nested".into()],
        &artifacts,
        None,
        "test",
        &thruput_cli::Print::quiet(),
    )
    .unwrap_err();

    assert!(
        matches!(err, thruput_cli::CliError::TargetDirConflict),
        "expected TargetDirConflict, got: {err:?}"
    );
}

#[test]
#[serial]
fn test_prefers_metadata_failure_over_redundant_target_dir_precheck() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join(".cargo")).unwrap();
    fs::write(tmp.path().join(".cargo/contract.toml"), "").unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s\n' 'synthetic metadata failure' >&2\n  exit 1\nfi\nexit 0\n",
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&cargo).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cargo, perms).unwrap();
    }

    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fake_bin.as_os_str());

    let artifacts =
        thruput_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let err = thruput_cli::test_cmd::run(
        Some("demo-contract"),
        &["--target-dir".into(), "nested".into()],
        &artifacts,
        None,
        "test",
        &thruput_cli::Print::quiet(),
    )
    .unwrap_err();

    assert!(
        matches!(err, thruput_cli::CliError::CargoMetadataFailed(_)),
        "expected CargoMetadataFailed, got: {err:?}"
    );
}

#[test]
#[serial]
fn test_resolves_relative_config_conflicts_from_project_root() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join(".cargo")).unwrap();
    fs::write(tmp.path().join(".cargo/contract.toml"), "").unwrap();
    fs::write(
        tmp.path().join("target-config.toml"),
        "[build]\ntarget-dir = \"nested\"\n",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("src/bin")).unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
        tmp.path().display(),
        tmp.path().join("target").display(),
        tmp.path().join("Cargo.toml").display()
    );
    fs::write(
        &cargo,
        format!("#!/bin/sh\nprintf '%s' '{metadata_json}'\n"),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&cargo).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cargo, perms).unwrap();
    }

    let _cwd = CurrentDirGuard::enter(&tmp.path().join("src/bin"));
    let _path = PathGuard::set(fake_bin.as_os_str());

    let artifacts =
        thruput_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let err = thruput_cli::test_cmd::run(
        Some("demo-contract"),
        &["--config".into(), "target-config.toml".into()],
        &artifacts,
        None,
        "test",
        &thruput_cli::Print::quiet(),
    )
    .unwrap_err();

    assert!(
        matches!(err, thruput_cli::CliError::TargetDirConflict),
        "expected TargetDirConflict, got: {err:?}"
    );
}

#[test]
#[serial]
fn test_failed_rv32_tests_update_rerun_cache() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let (elf_fixture, raw_elf) = write_rv32_build_fixture(tmp.path());
    fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
        tmp.path().display(),
        tmp.path().join("target").display(),
        tmp.path().join("Cargo.toml").display()
    );
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"--config\" ]; then\n  shift 2\nfi\nif [ \"$1\" = \"build\" ]; then\n  /bin/mkdir -p '{}'\n  /bin/cp '{}' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"test\" ]; then\n  printf 'running 1 test\\n'\n  printf 'test tests::fails ... FAILED\\n'\n  exit 101\nfi\nexit 0\n",
            metadata_json,
            raw_elf.parent().unwrap().display(),
            elf_fixture.display(),
            raw_elf.display()
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&cargo).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cargo, perms).unwrap();
    }

    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fake_bin.as_os_str());
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("config-home")) };

    let err = thruput_cli::test_cmd::run_with_options(
        Some("demo-contract"),
        &thruput_cli::test_cmd::TestOptions::default(),
        &[],
        &thruput_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &thruput_cli::Print::new(true, true),
    )
    .unwrap_err();

    assert!(
        matches!(err, thruput_cli::CliError::CommandReportedFailure),
        "expected CommandReportedFailure, got: {err:?}"
    );
    let cache_path = tmp
        .path()
        .join("target/thruput/test-failures/demo-contract-rv32.json");
    let cache = fs::read_to_string(cache_path).unwrap();
    assert!(cache.contains("\"package\": \"demo-contract\""));
    assert!(cache.contains("\"tests::fails\""));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

/// A build warning reaches `test --json`, so automation learns that the
/// contract's TypeScript bindings cannot be generated even when its tests pass.
#[test]
#[serial]
fn test_json_lists_a_build_warning() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let (elf_fixture, raw_elf) = write_rv32_build_fixture(tmp.path());
    // The bindings reserve the `__gh_` prefix, so codegen would refuse this spec.
    fs::write(
        &elf_fixture,
        support::minimal_rv32_elf_with_section(
            thruput_spec_tool::RAW_SECTION_NAME,
            &support::contract_spec_raw_for_interface("__gh_Counter"),
        ),
    )
    .unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    // Real Cargo metadata uses canonical paths, including macOS's /private/var alias.
    let project_root = tmp.path().canonicalize().unwrap();
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
        project_root.display(),
        project_root.join("target").display(),
        project_root.join("Cargo.toml").display()
    );
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"--config\" ]; then\n  shift 2\nfi\nif [ \"$1\" = \"build\" ]; then\n  /bin/mkdir -p '{}'\n  /bin/cp '{}' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"test\" ]; then\n  printf 'running 1 test\\n'\n  printf 'test tests::works ... ok\\n'\n  exit 0\nfi\nexit 0\n",
            metadata_json,
            raw_elf.parent().unwrap().display(),
            elf_fixture.display(),
            raw_elf.display()
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&cargo).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cargo, perms).unwrap();
    }

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_thruput"))
        .current_dir(tmp.path())
        .env("PATH", &fake_bin)
        .env("XDG_CONFIG_HOME", tmp.path().join("config-home"))
        .args(["--json", "test", "-p", "demo-contract"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "expected one JSON record, got: {stdout}");
    let payload: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["data"]["success"], true);
    let warnings = payload["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["code"], "ts_bindings_failed");
}

/// `thruput test` runs host tests without an explicit `--target`, so its
/// artifacts share `target/debug` with plain `cargo test` and IDE builds
/// instead of populating a second per-triple host tree.
#[test]
#[serial]
fn test_cargo_test_invocation_omits_host_target() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let (elf_fixture, raw_elf) = write_rv32_build_fixture(tmp.path());
    fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let args_log = tmp.path().join("test-args.log");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
        tmp.path().display(),
        tmp.path().join("target").display(),
        tmp.path().join("Cargo.toml").display()
    );
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"--config\" ]; then\n  shift 2\nfi\nif [ \"$1\" = \"build\" ]; then\n  /bin/mkdir -p '{}'\n  /bin/cp '{}' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"test\" ]; then\n  printf '%s\\n' \"$@\" > '{}'\n  printf 'running 1 test\\ntest tests::passes ... ok\\n'\n  exit 0\nfi\nexit 0\n",
            metadata_json,
            raw_elf.parent().unwrap().display(),
            elf_fixture.display(),
            raw_elf.display(),
            args_log.display()
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&cargo).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cargo, perms).unwrap();
    }

    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fake_bin.as_os_str());
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("config-home")) };

    thruput_cli::test_cmd::run_with_options(
        Some("demo-contract"),
        &thruput_cli::test_cmd::TestOptions::default(),
        &[],
        &thruput_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &thruput_cli::Print::quiet(),
    )
    .unwrap();

    let recorded = fs::read_to_string(&args_log).unwrap();
    let args: Vec<&str> = recorded.lines().collect();
    assert!(
        !args.contains(&"--target"),
        "cargo test should not pin a host --target, got: {args:?}"
    );
    assert!(
        args.contains(&"--test"),
        "expected a --test target, got: {args:?}"
    );
    assert!(
        args.contains(&"test_rv32"),
        "expected test_rv32 target, got: {args:?}"
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

struct CurrentDirGuard {
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn enter(path: &Path) -> Self {
        let previous = env::current_dir().expect("bug: current dir should be readable");
        env::set_current_dir(path).expect("bug: current dir should be set");
        Self { previous }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        env::set_current_dir(&self.previous).expect("bug: current dir should be restored");
    }
}

struct PathGuard(Option<OsString>);

impl PathGuard {
    fn set(new: &OsStr) -> Self {
        let previous = env::var_os("PATH");
        unsafe { env::set_var("PATH", new) };
        Self(previous)
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(path) => unsafe { env::set_var("PATH", path) },
            None => unsafe { env::remove_var("PATH") },
        }
    }
}

/// A fake-cargo workspace of contract members with `test_rv32` targets plus a
/// `helper` library; every `cargo test` invocation's arguments land in `args_log`.
struct WorkspaceFixture {
    fake_bin: PathBuf,
    args_log: PathBuf,
}

fn write_workspace_fixture(
    root: &Path,
    packages: &[&str],
    failing: &str,
    build_failing: &str,
) -> WorkspaceFixture {
    fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::create_dir_all(root.join(".cargo")).unwrap();
    fs::write(root.join(".cargo/contract.toml"), "").unwrap();
    fs::write(
        root.join(".cargo/config.toml"),
        "[target.riscv32im-unknown-none-elf]\n\
         rustflags = [\"-C\", \"link-arg=-Tgroundhog-rv32.ld\"]\n",
    )
    .unwrap();
    fs::write(
        root.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();
    let fixture = root.join("fixture.elf");
    fs::write(&fixture, support::minimal_rv32_elf()).unwrap();

    let mut members = Vec::new();
    let mut snake_cases = Vec::new();
    for package in packages {
        fs::create_dir_all(root.join(package)).unwrap();
        let snake = package.replace('-', "_");
        members.push(format!(
            r#"{{"name":"{package}","version":"0.1.0","manifest_path":"{}","targets":[{{"name":"{snake}","kind":["bin"]}},{{"name":"test_rv32","kind":["test"]}}]}}"#,
            root.join(package).join("Cargo.toml").display()
        ));
        snake_cases.push(format!("    {package}) snake={snake} ;;\n"));
    }
    members.push(format!(
        r#"{{"name":"helper","version":"0.1.0","manifest_path":"{}","targets":[{{"name":"helper","kind":["lib"]}}]}}"#,
        root.join("helper/Cargo.toml").display()
    ));
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{}]}}"#,
        root.display(),
        root.join("target").display(),
        members.join(",")
    );

    let fake_bin = root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let args_log = root.join("test-args.log");
    let release_dir = root.join("target/riscv32im-unknown-none-elf/release");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\n\
             if [ \"$1\" = \"--config\" ]; then\n  shift 2\nfi\n\
             if [ \"$1\" = \"build\" ]; then\n\
             \x20 pkg=''\n\
             \x20 while [ $# -gt 0 ]; do\n    if [ \"$1\" = \"-p\" ]; then pkg=\"$2\"; fi\n    shift\n  done\n\
             \x20 if [ \"$pkg\" = \"{build_failing}\" ]; then\n    echo 'synthetic build failure' >&2\n    exit 1\n  fi\n\
             \x20 case \"$pkg\" in\n{snake_cases}    *) exit 1 ;;\n  esac\n\
             \x20 /bin/mkdir -p '{release}'\n\
             \x20 /bin/cp '{fixture}' \"{release}/$snake\"\n\
             \x20 exit 0\n\
             fi\n\
             if [ \"$1\" = \"test\" ]; then\n\
             \x20 printf '%s\\n' \"$@\" >> '{log}'\n\
             \x20 echo '--- end ---' >> '{log}'\n\
             \x20 if [ \"$3\" = \"{failing}\" ]; then\n\
             \x20   printf 'running 1 test\\ntest tests::fails ... FAILED\\n'\n\
             \x20   exit 101\n\
             \x20 fi\n\
             \x20 printf 'running 2 tests\\ntest tests::first ... ok\\ntest tests::second ... ok\\n'\n\
             \x20 exit 0\n\
             fi\n\
             exit 0\n",
            snake_cases = snake_cases.concat(),
            release = release_dir.display(),
            fixture = fixture.display(),
            log = args_log.display(),
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&cargo).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cargo, perms).unwrap();
    }

    WorkspaceFixture { fake_bin, args_log }
}

/// The packages the fake `cargo test` was invoked for, in invocation order.
fn tested_packages(args_log: &Path) -> Vec<String> {
    let recorded = fs::read_to_string(args_log).unwrap_or_default();
    recorded
        .split("--- end ---\n")
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            let lines: Vec<&str> = block.lines().collect();
            let index = lines
                .iter()
                .position(|line| *line == "-p")
                .expect("bug: fake cargo test should receive -p");
            lines[index + 1].to_string()
        })
        .collect()
}

#[test]
#[serial]
fn test_workspace_runs_every_rv32_member_and_reports_failures() {
    let tmp = TempDir::new().unwrap();
    let fixture = write_workspace_fixture(
        tmp.path(),
        &["beta-contract", "alpha-contract"],
        "beta-contract",
        "",
    );

    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fixture.fake_bin.as_os_str());
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("config-home")) };

    let err = thruput_cli::test_cmd::run_with_options(
        None,
        &thruput_cli::test_cmd::TestOptions {
            workspace: true,
            ..thruput_cli::test_cmd::TestOptions::default()
        },
        &[],
        &thruput_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &thruput_cli::Print::quiet(),
    )
    .unwrap_err();

    match err {
        thruput_cli::CliError::WorkspaceTestsFailed { failed, total } => {
            assert_eq!(failed, vec!["beta-contract".to_string()]);
            assert_eq!(total, 2);
        }
        other => panic!("expected WorkspaceTestsFailed, got: {other:?}"),
    }
    assert_eq!(
        tested_packages(&fixture.args_log),
        vec!["alpha-contract".to_string(), "beta-contract".to_string()],
        "members run in name order; the helper library is not a test package"
    );
    let cache = fs::read_to_string(
        tmp.path()
            .join("target/thruput/test-failures/beta-contract-rv32.json"),
    )
    .unwrap();
    assert!(cache.contains("\"tests::fails\""));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn test_workspace_fail_fast_stops_after_first_failing_member() {
    let tmp = TempDir::new().unwrap();
    let fixture = write_workspace_fixture(
        tmp.path(),
        &["alpha-contract", "beta-contract", "gamma-contract"],
        "beta-contract",
        "",
    );

    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fixture.fake_bin.as_os_str());
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("config-home")) };

    let err = thruput_cli::test_cmd::run_with_options(
        None,
        &thruput_cli::test_cmd::TestOptions {
            workspace: true,
            harness: thruput_cli::test_cmd::TestHarnessOptions {
                fail_fast: true,
                ..thruput_cli::test_cmd::TestHarnessOptions::default()
            },
            ..thruput_cli::test_cmd::TestOptions::default()
        },
        &[],
        &thruput_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &thruput_cli::Print::quiet(),
    )
    .unwrap_err();

    match err {
        thruput_cli::CliError::WorkspaceTestsFailed { failed, total } => {
            assert_eq!(failed, vec!["beta-contract".to_string()]);
            assert_eq!(total, 3);
        }
        other => panic!("expected WorkspaceTestsFailed, got: {other:?}"),
    }
    assert_eq!(
        tested_packages(&fixture.args_log),
        vec!["alpha-contract".to_string(), "beta-contract".to_string()],
        "--fail-fast should leave gamma-contract unrun"
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

/// `thruput --json test --workspace` emits one record whose `data` is the
/// workspace report: aggregate summary, one per-package test report each
/// naming its package, and the members `--fail-fast` skipped.
#[test]
#[serial]
fn test_workspace_json_reports_every_member() {
    let tmp = TempDir::new().unwrap();
    let fixture = write_workspace_fixture(
        tmp.path(),
        &["alpha-contract", "beta-contract", "gamma-contract"],
        "beta-contract",
        "",
    );

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_thruput"))
        .current_dir(tmp.path())
        .env("PATH", &fixture.fake_bin)
        .env("XDG_CONFIG_HOME", tmp.path().join("config-home"))
        .args(["--json", "test", "--workspace", "--fail-fast"])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "a failing member should fail the command"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "expected one JSON record, got: {stdout}");
    let payload: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["command"], "test");
    let data = &payload["data"];
    assert_eq!(data["schemaVersion"], 2);
    assert_eq!(data["workspaceRoot"], tmp.path().display().to_string());
    assert_eq!(data["success"], false);
    assert_eq!(data["summary"]["passed"], "2");
    assert_eq!(data["summary"]["failed"], "1");
    assert_eq!(data["skipped"], serde_json::json!(["gamma-contract"]));
    let packages = data["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0]["package"], "alpha-contract");
    assert_eq!(packages[0]["schemaVersion"], 7);
    assert_eq!(packages[0]["success"], true);
    assert_eq!(packages[0]["artifact"]["kind"], "rv32");
    assert_eq!(packages[1]["package"], "beta-contract");
    assert_eq!(packages[1]["success"], false);
    assert_eq!(
        packages[1]["rerun"]["failedTests"],
        serde_json::json!(["tests::fails"])
    );
}

#[test]
#[serial]
fn test_workspace_build_error_ends_the_run_with_that_error() {
    let tmp = TempDir::new().unwrap();
    let fixture = write_workspace_fixture(
        tmp.path(),
        &["alpha-contract", "beta-contract", "gamma-contract"],
        "",
        "beta-contract",
    );

    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fixture.fake_bin.as_os_str());
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("config-home")) };

    let err = thruput_cli::test_cmd::run_with_options(
        None,
        &thruput_cli::test_cmd::TestOptions {
            workspace: true,
            ..thruput_cli::test_cmd::TestOptions::default()
        },
        &[],
        &thruput_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &thruput_cli::Print::quiet(),
    )
    .unwrap_err();

    assert!(
        matches!(err, thruput_cli::CliError::CargoFailedWithOutput { .. }),
        "expected the build failure itself, got: {err:?}"
    );
    assert_eq!(
        tested_packages(&fixture.args_log),
        vec!["alpha-contract".to_string()],
        "beta-contract never built, so neither it nor gamma-contract should run tests"
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

/// A member that fails to build under `--json` still yields the workspace
/// record: the members that completed, the errored member, the members left
/// unrun, and the build failure in the envelope's `error`.
#[test]
#[serial]
fn test_workspace_json_reports_a_member_that_fails_to_build() {
    let tmp = TempDir::new().unwrap();
    let fixture = write_workspace_fixture(
        tmp.path(),
        &["alpha-contract", "beta-contract", "gamma-contract"],
        "",
        "beta-contract",
    );

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_thruput"))
        .current_dir(tmp.path())
        .env("PATH", &fixture.fake_bin)
        .env("XDG_CONFIG_HOME", tmp.path().join("config-home"))
        .args(["--json", "test", "--workspace"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "expected one JSON record, got: {stdout}");
    let payload: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["command"], "test");
    assert_eq!(payload["error"]["code"], "cargo_failed");
    assert!(
        payload["error"]["details"]["stderr"]
            .as_str()
            .unwrap()
            .contains("synthetic build failure"),
        "expected the build failure output, got: {}",
        payload["error"]
    );
    let data = &payload["data"];
    assert_eq!(data["schemaVersion"], 2);
    assert_eq!(data["success"], false);
    assert_eq!(data["errored"], "beta-contract");
    assert_eq!(data["skipped"], serde_json::json!(["gamma-contract"]));
    assert_eq!(data["summary"]["passed"], "2");
    assert_eq!(data["summary"]["failed"], "0");
    let packages = data["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0]["package"], "alpha-contract");
    assert_eq!(packages[0]["success"], true);
}
