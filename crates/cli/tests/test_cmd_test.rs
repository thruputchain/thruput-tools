use serial_test::serial;
use std::env;
use std::fs;
use tempfile::TempDir;

/// Verify that `test_cmd::run` fails when run outside
/// a cargo project (cargo metadata fails).
#[test]
#[serial]
fn test_fails_outside_project() {
    let tmp = TempDir::new().unwrap();
    let prev_dir = env::current_dir().unwrap();

    env::set_current_dir(tmp.path()).unwrap();

    let result = groundhog_cli::test_cmd::run(
        None,
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        "test",
        &groundhog_cli::Print::quiet(),
    );

    env::set_current_dir(prev_dir).unwrap();

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

    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"--config\" ]; then\n  shift 2\nfi\nif [ \"$1\" = \"build\" ]; then\n  /bin/mkdir -p '{}'\n  printf '\\000asm\\001\\000\\000\\000' > '{}'\n  exit 0\nfi\nexit 0\n",
            format!(
                r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
                tmp.path().display(),
                tmp.path().join("target").display(),
                tmp.path().join("Cargo.toml").display()
            ),
            tmp.path()
                .join("target/wasm32-unknown-unknown/release")
                .display(),
            tmp.path()
                .join("target/wasm32-unknown-unknown/release/demo_contract.wasm")
                .display()
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

    let prev_dir = env::current_dir().unwrap();
    let prev_path = env::var_os("PATH");
    env::set_current_dir(tmp.path()).unwrap();
    unsafe { env::set_var("PATH", fake_bin.as_os_str()) };

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

    env::set_current_dir(prev_dir).unwrap();
    match prev_path {
        Some(path) => unsafe { env::set_var("PATH", path) },
        None => unsafe { env::remove_var("PATH") },
    }

    assert!(
        matches!(err, groundhog_cli::CliError::TargetDirConflict),
        "expected TargetDirConflict, got: {err:?}"
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
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"--config\" ]; then\n  shift 2\nfi\nif [ \"$1\" = \"build\" ]; then\n  /bin/mkdir -p '{}'\n  printf '\\000asm\\001\\000\\000\\000' > '{}'\n  exit 0\nfi\nexit 0\n",
            format!(
                r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
                tmp.path().display(),
                tmp.path().join("target").display(),
                tmp.path().join("Cargo.toml").display()
            ),
            tmp.path()
                .join("target/wasm32-unknown-unknown/release")
                .display(),
            tmp.path()
                .join("target/wasm32-unknown-unknown/release/demo_contract.wasm")
                .display()
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

    let prev_dir = env::current_dir().unwrap();
    let prev_path = env::var_os("PATH");
    env::set_current_dir(tmp.path().join("src/bin")).unwrap();
    unsafe { env::set_var("PATH", fake_bin.as_os_str()) };

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

    env::set_current_dir(prev_dir).unwrap();
    match prev_path {
        Some(path) => unsafe { env::set_var("PATH", path) },
        None => unsafe { env::remove_var("PATH") },
    }

    assert!(
        matches!(err, groundhog_cli::CliError::TargetDirConflict),
        "expected TargetDirConflict, got: {err:?}"
    );
}
