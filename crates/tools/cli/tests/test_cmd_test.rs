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

    let result = groundhog_cli::test_cmd::run(
        None,
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &groundhog_cli::Print::quiet(),
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
        tmp.path().join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
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
        groundhog_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let err = groundhog_cli::test_cmd::run(
        Some("demo-contract"),
        &["--target-dir".into(), "nested".into()],
        &artifacts,
        None,
        "test",
        &groundhog_cli::Print::quiet(),
    )
    .unwrap_err();

    assert!(
        matches!(err, groundhog_cli::CliError::TargetDirConflict),
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
        groundhog_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let err = groundhog_cli::test_cmd::run(
        Some("demo-contract"),
        &["--target-dir".into(), "nested".into()],
        &artifacts,
        None,
        "test",
        &groundhog_cli::Print::quiet(),
    )
    .unwrap_err();

    assert!(
        matches!(err, groundhog_cli::CliError::CargoMetadataFailed(_)),
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
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
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
        groundhog_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let err = groundhog_cli::test_cmd::run(
        Some("demo-contract"),
        &["--config".into(), "target-config.toml".into()],
        &artifacts,
        None,
        "test",
        &groundhog_cli::Print::quiet(),
    )
    .unwrap_err();

    assert!(
        matches!(err, groundhog_cli::CliError::TargetDirConflict),
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
        tmp.path().join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
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

    let err = groundhog_cli::test_cmd::run_with_options(
        Some("demo-contract"),
        &groundhog_cli::test_cmd::TestOptions::default(),
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &groundhog_cli::Print::new(true, true),
    )
    .unwrap_err();

    assert!(
        matches!(err, groundhog_cli::CliError::CommandReportedFailure),
        "expected CommandReportedFailure, got: {err:?}"
    );
    let cache_path = tmp
        .path()
        .join("target/groundhog/test-failures/demo-contract-rv32.json");
    let cache = fs::read_to_string(cache_path).unwrap();
    assert!(cache.contains("\"package\": \"demo-contract\""));
    assert!(cache.contains("\"tests::fails\""));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

/// `groundhog test` runs host tests without an explicit `--target`, so its
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
        tmp.path().join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    let args_log = tmp.path().join("test-args.log");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
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

    groundhog_cli::test_cmd::run_with_options(
        Some("demo-contract"),
        &groundhog_cli::test_cmd::TestOptions::default(),
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &groundhog_cli::Print::quiet(),
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
