use serial_test::serial;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

mod support;

/// Minimal stand-in for the `.cargo/config.toml` that `groundhog new`
/// scaffolds: both Groundhog target rustflags tables, which is what the
/// workspace-config guard requires at the cargo workspace root.
fn write_scaffold_cargo_config(project_root: &Path) {
    fs::create_dir_all(project_root.join(".cargo")).unwrap();
    fs::write(
        project_root.join(".cargo/config.toml"),
        "[target.wasm32-unknown-unknown]\n\
         rustflags = [\"-C\", \"link-args=-zstack-size=30000\"]\n\n\
         [target.riscv32im-unknown-none-elf]\n\
         rustflags = [\"-C\", \"link-arg=-Tgroundhog-rv32.ld\"]\n",
    )
    .unwrap();
}

fn write_active_profile(project_root: &Path) {
    fs::write(
        project_root.join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();
}

#[test]
fn resolve_package_with_explicit_name() {
    let meta = groundhog_cli::build::CargoMetadata {
        workspace_root: PathBuf::from("/ws"),
        target_directory: PathBuf::from("/ws/target"),
        packages: vec![
            groundhog_cli::build::MetadataPackage {
                name: "alpha".into(),
                manifest_path: PathBuf::from("/ws/alpha/Cargo.toml"),
            },
            groundhog_cli::build::MetadataPackage {
                name: "beta".into(),
                manifest_path: PathBuf::from("/ws/beta/Cargo.toml"),
            },
        ],
    };
    let name =
        groundhog_cli::build::resolve_package(&meta, Some("beta"), &PathBuf::from("/ws")).unwrap();
    assert_eq!(name, "beta");
}

#[test]
fn resolve_package_explicit_name_not_found() {
    let meta = groundhog_cli::build::CargoMetadata {
        workspace_root: PathBuf::from("/ws"),
        target_directory: PathBuf::from("/ws/target"),
        packages: vec![groundhog_cli::build::MetadataPackage {
            name: "alpha".into(),
            manifest_path: PathBuf::from("/ws/alpha/Cargo.toml"),
        }],
    };
    let err = groundhog_cli::build::resolve_package(&meta, Some("nope"), &PathBuf::from("/ws"))
        .unwrap_err();
    assert!(err.to_string().contains("nope"));
}

#[test]
fn resolve_package_from_member_dir() {
    let meta = groundhog_cli::build::CargoMetadata {
        workspace_root: PathBuf::from("/ws"),
        target_directory: PathBuf::from("/ws/target"),
        packages: vec![
            groundhog_cli::build::MetadataPackage {
                name: "alpha".into(),
                manifest_path: PathBuf::from("/ws/alpha/Cargo.toml"),
            },
            groundhog_cli::build::MetadataPackage {
                name: "beta".into(),
                manifest_path: PathBuf::from("/ws/beta/Cargo.toml"),
            },
        ],
    };
    let name = groundhog_cli::build::resolve_package(&meta, None, &PathBuf::from("/ws/alpha/src"))
        .unwrap();
    assert_eq!(name, "alpha");
}

#[test]
fn resolve_package_from_workspace_root_errors() {
    let meta = groundhog_cli::build::CargoMetadata {
        workspace_root: PathBuf::from("/ws"),
        target_directory: PathBuf::from("/ws/target"),
        packages: vec![
            groundhog_cli::build::MetadataPackage {
                name: "alpha".into(),
                manifest_path: PathBuf::from("/ws/alpha/Cargo.toml"),
            },
            groundhog_cli::build::MetadataPackage {
                name: "beta".into(),
                manifest_path: PathBuf::from("/ws/beta/Cargo.toml"),
            },
        ],
    };
    let err =
        groundhog_cli::build::resolve_package(&meta, None, &PathBuf::from("/ws")).unwrap_err();
    assert!(err.to_string().contains("-p"));
}

#[test]
fn resolve_package_standalone_project() {
    let meta = groundhog_cli::build::CargoMetadata {
        workspace_root: PathBuf::from("/proj"),
        target_directory: PathBuf::from("/proj/target"),
        packages: vec![groundhog_cli::build::MetadataPackage {
            name: "my-contract".into(),
            manifest_path: PathBuf::from("/proj/Cargo.toml"),
        }],
    };
    let name =
        groundhog_cli::build::resolve_package(&meta, None, &PathBuf::from("/proj/src")).unwrap();
    assert_eq!(name, "my-contract");
}

#[test]
fn resolve_package_standalone_from_root() {
    let meta = groundhog_cli::build::CargoMetadata {
        workspace_root: PathBuf::from("/proj"),
        target_directory: PathBuf::from("/proj/target"),
        packages: vec![groundhog_cli::build::MetadataPackage {
            name: "my-contract".into(),
            manifest_path: PathBuf::from("/proj/Cargo.toml"),
        }],
    };
    let name = groundhog_cli::build::resolve_package(&meta, None, &PathBuf::from("/proj")).unwrap();
    assert_eq!(name, "my-contract");
}

#[test]
#[serial]
fn cargo_metadata_uses_explicit_target_dir() {
    let tmp = TempDir::new().unwrap();
    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();

    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nprintf '{{\"workspace_root\":\"{}\",\"target_directory\":\"%s\",\"packages\":[]}}' \"$CARGO_TARGET_DIR\"\n",
            tmp.path().display()
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

    let _path = PathGuard::set(fake_bin.as_os_str());

    let artifacts =
        groundhog_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let metadata = groundhog_cli::build::cargo_metadata_with_artifacts(&artifacts).unwrap();

    assert_eq!(
        metadata.target_directory,
        tmp.path().join("explicit-target")
    );
}

#[test]
#[serial]
fn build_rejects_conflicting_target_dir_passthrough() {
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
    let err = groundhog_cli::build::build_project(
        Some("demo-contract"),
        false,
        &["--target-dir".into(), "nested".into()],
        &artifacts,
        None,
        &[],
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
fn build_prefers_metadata_failure_over_redundant_target_dir_precheck() {
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
    let err = groundhog_cli::build::build_project(
        Some("demo-contract"),
        false,
        &["--target-dir".into(), "nested".into()],
        &artifacts,
        None,
        &[],
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
fn build_resolves_relative_config_conflicts_from_workspace_root() {
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
    let err = groundhog_cli::build::build_project(
        Some("demo-contract"),
        false,
        &["--config".into(), "target-config.toml".into()],
        &artifacts,
        None,
        &[],
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
fn build_project_reports_groundhog_processed_wasm_path() {
    let tmp = TempDir::new().unwrap();
    let project_root =
        std::fs::canonicalize(tmp.path()).expect("bug: project path should canonicalize");
    write_active_profile(&project_root);
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir_all(project_root.join(".cargo")).unwrap();
    fs::write(project_root.join(".cargo/contract.toml"), "").unwrap();
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();

    let target_dir = project_root.join("target");
    let raw_wasm = target_dir
        .join("wasm32-unknown-unknown/release")
        .join("demo_contract.wasm");
    let processed_wasm = target_dir.join("groundhog").join("demo_contract.wasm");
    let cargo_args_log = project_root.join("cargo-args.log");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
        project_root.display(),
        target_dir.display(),
        project_root.join("Cargo.toml").display()
    );

    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\nprintf '%s\\n' \"$*\" > '{}'\n/bin/mkdir -p '{}'\nprintf '\\000asm\\001\\000\\000\\000' > '{}'\n",
            cargo_args_log.display(),
            raw_wasm
                .parent()
                .expect("bug: raw wasm parent should exist")
                .display(),
            raw_wasm.display()
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

    let _cwd = CurrentDirGuard::enter(&project_root);
    let _path = PathGuard::set(fake_bin.as_os_str());

    let result = groundhog_cli::build::build_project(
        Some("demo-contract"),
        false,
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        &[],
        &groundhog_cli::Print::quiet(),
    )
    .expect("bug: build should succeed");

    assert_eq!(result.wasm, processed_wasm);
    assert!(processed_wasm.exists());
    assert_eq!(fs::read(&raw_wasm).unwrap(), b"\0asm\x01\0\0\0");

    // `--lib` keeps the wasm build scoped to the cdylib target so a project's
    // RV32 bin wrapper is neither built for wasm (output filename collision
    // with the cdylib) nor allowed to perturb the cdylib codegen under
    // lto=true/codegen-units=1.
    let cargo_args = fs::read_to_string(cargo_args_log).unwrap();
    assert!(cargo_args.contains("--lib"), "cargo args: {cargo_args}");
}

#[test]
#[serial]
fn build_rejects_missing_active_profile_before_compiling() {
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
    let compile_marker = tmp.path().join("compiled");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
        tmp.path().display(),
        tmp.path().join("target").display(),
        tmp.path().join("Cargo.toml").display()
    );
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\n/bin/touch '{}'\n",
            compile_marker.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&cargo).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&cargo, permissions).unwrap();
    }
    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fake_bin.as_os_str());
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("config-home")) };

    let error = groundhog_cli::build::build_project(
        Some("demo-contract"),
        false,
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        &[],
        &groundhog_cli::Print::quiet(),
    )
    .unwrap_err();

    assert_eq!(error.code(), "no_active_profile");
    assert!(!compile_marker.exists());
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn build_project_recopies_raw_wasm_before_reprocessing_on_repeated_runs() {
    let tmp = TempDir::new().unwrap();
    let project_root =
        std::fs::canonicalize(tmp.path()).expect("bug: project path should canonicalize");
    write_active_profile(&project_root);
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir_all(project_root.join(".cargo")).unwrap();
    fs::write(project_root.join(".cargo/contract.toml"), "").unwrap();
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();

    let target_dir = project_root.join("target");
    let raw_wasm = target_dir
        .join("wasm32-unknown-unknown/release")
        .join("demo_contract.wasm");
    let processed_wasm = target_dir.join("groundhog").join("demo_contract.wasm");
    let raw_bytes = b"\0asm\x01\0\0\0";
    let corrupted_processed = b"not wasm";
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
        project_root.display(),
        target_dir.display(),
        project_root.join("Cargo.toml").display()
    );

    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\n/bin/mkdir -p '{}'\nprintf '\\000asm\\001\\000\\000\\000' > '{}'\n",
            raw_wasm
                .parent()
                .expect("bug: raw wasm parent should exist")
                .display(),
            raw_wasm.display()
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

    let _cwd = CurrentDirGuard::enter(&project_root);
    let _path = PathGuard::set(fake_bin.as_os_str());

    groundhog_cli::build::build_project(
        Some("demo-contract"),
        false,
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        &[],
        &groundhog_cli::Print::quiet(),
    )
    .expect("bug: first build should succeed");
    let first_processed = fs::read(&processed_wasm).unwrap();

    fs::write(&processed_wasm, corrupted_processed).unwrap();

    let result = groundhog_cli::build::build_project(
        Some("demo-contract"),
        false,
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        &[],
        &groundhog_cli::Print::quiet(),
    )
    .expect("bug: second build should succeed");

    assert_eq!(result.wasm, processed_wasm);
    assert_eq!(fs::read(&raw_wasm).unwrap(), raw_bytes);
    let rebuilt_processed = fs::read(&processed_wasm).unwrap();
    assert_ne!(rebuilt_processed, corrupted_processed);
    // Both runs start from the same raw bytes, so the processed output should
    // be byte-for-byte identical. That also verifies the pipeline stays
    // deterministic for identical input.
    assert_eq!(rebuilt_processed, first_processed);
}

#[test]
#[serial]
fn build_rv32_project_compiles_and_lowers_elf() {
    let tmp = TempDir::new().unwrap();
    let project_root =
        std::fs::canonicalize(tmp.path()).expect("bug: project path should canonicalize");
    write_active_profile(&project_root);
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"demo-contract-rv32\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nentrypoints = []\nrv32-entrypoints = [\"entrypoints\"]\n",
    )
    .unwrap();
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let target_dir = project_root.join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo_contract_rv32");
    let processed_rv32 = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.rv32a");
    let processed_metadata = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.rv32.json");
    let processed_spec_json = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.spec.json");
    let processed_spec_xdr = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.spec.xdr");
    let cargo_args_log = project_root.join("cargo-args.log");
    let elf_fixture = project_root.join("fixture.elf");
    let raw_spec = encode_fragments(&[
        groundhog_spec_tool::SpecFragment::SpecInterface {
            interface_fragment: groundhog_spec_tool::SpecInterfaceFragment {
                interface_name: spec_string("Counter"),
                interface_version: spec_string("0.1.0"),
                throws: vec![].try_into().unwrap(),
            },
        },
        // Spec shaking keeps only anchored interfaces; a real build emits
        // this anchor from the contract impl site.
        groundhog_spec_tool::SpecFragment::InterfaceAnchor {
            interface_anchor: groundhog_contract_spec::SpecInterfaceAnchor {
                interface_name: spec_string("Counter"),
                interface_version: spec_string("0.1.0"),
            },
        },
        groundhog_spec_tool::SpecFragment::SpecMethod {
            method_fragment: groundhog_spec_tool::SpecMethodFragment {
                interface_name: spec_string("Counter"),
                interface_version: spec_string("0.1.0"),
                method: groundhog_spec_tool::ContractSpecMethod {
                    name: spec_string("get"),
                    args: vec![].try_into().unwrap(),
                    return_type: groundhog_spec_tool::ContractType::U64,
                },
            },
        },
    ]);
    fs::write(
        &elf_fixture,
        support::minimal_rv32_elf_with_section(groundhog_spec_tool::RAW_SECTION_NAME, &raw_spec),
    )
    .unwrap();
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract-rv32","manifest_path":"{}","features":{{"entrypoints":[],"rv32-entrypoints":["entrypoints"]}}}}]}}"#,
        project_root.display(),
        target_dir.display(),
        project_root.join("Cargo.toml").display()
    );

    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\nprintf '%s\\n' \"$*\" > '{}'\n/bin/mkdir -p '{}'\n/bin/cp '{}' '{}'\n",
            cargo_args_log.display(),
            raw_elf
                .parent()
                .expect("bug: raw elf parent should exist")
                .display(),
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

    let _cwd = CurrentDirGuard::enter(&project_root);
    let _path = PathGuard::set(fake_bin.as_os_str());

    let result = groundhog_cli::build::build_rv32_project(
        Some("demo-contract-rv32"),
        &["--locked".into()],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        None,
        &groundhog_cli::Print::quiet(),
    )
    .expect("bug: rv32 build should succeed");

    assert_eq!(result.elf, raw_elf);
    assert_eq!(result.rv32, processed_rv32);
    assert_eq!(result.metadata, processed_metadata);
    assert_eq!(result.spec_json, Some(processed_spec_json.clone()));
    assert_eq!(result.spec_xdr, Some(processed_spec_xdr.clone()));
    // Only the minimal nonzero data span is stored; the spec-metadata section
    // is skipped, so the artifact is header + code, far below the full image.
    assert!(result.size_bytes < 64 * 1024);
    assert!(fs::read(&processed_rv32).unwrap().starts_with(b"GHRV32A1"));
    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&processed_metadata).unwrap()).unwrap();
    assert_eq!(
        metadata["artifact_format_fingerprint"],
        format!("{:#018x}", groundhog_rv32_lower::FORMAT_FINGERPRINT)
    );
    let spec_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&processed_spec_json).unwrap()).unwrap();
    assert_eq!(spec_json["format"], "gh_contract_spec_v1");
    assert_eq!(spec_json["interfaces"][0]["name"], "Counter");
    assert_eq!(spec_json["interfaces"][0]["methods"][0]["name"], "get");
    let spec_xdr = fs::read(&processed_spec_xdr).unwrap();
    let spec = groundhog_cli::contract_spec::load_contract_spec_from_xdr(&spec_xdr)
        .expect("bug: rv32 spec sidecar should decode");
    assert_eq!(spec.interfaces.len(), 1);

    let cargo_args = fs::read_to_string(cargo_args_log).unwrap();
    assert!(
        cargo_args.contains(
            "build --release --no-default-features --features rv32-entrypoints --target riscv32im-unknown-none-elf"
        )
    );
    assert!(cargo_args.contains("-p demo-contract-rv32"));
    assert!(cargo_args.contains("--bin demo_contract_rv32"));
    assert!(cargo_args.contains("--locked"));
    // The rv32 build must include the bin target; only the wasm build is
    // scoped with --lib.
    assert!(!cargo_args.contains("--lib"), "cargo args: {cargo_args}");
    assert_eq!(metadata["target_triple"], "riscv32im-unknown-none-elf");
    assert_eq!(
        metadata["build_flags"],
        "cargo build --release --no-default-features --features rv32-entrypoints --target riscv32im-unknown-none-elf -p demo-contract-rv32 --bin demo_contract_rv32 --locked"
    );
}

#[test]
#[serial]
fn build_rv32_project_with_stack_size_uses_cargo_rustc_defsym() {
    let tmp = TempDir::new().unwrap();
    let project_root =
        std::fs::canonicalize(tmp.path()).expect("bug: project path should canonicalize");
    write_active_profile(&project_root);
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"demo-contract-rv32\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nentrypoints = []\nrv32-entrypoints = [\"entrypoints\"]\n",
    )
    .unwrap();
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let target_dir = project_root.join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo_contract_rv32");
    let processed_metadata = target_dir
        .join("groundhog")
        .join("demo_contract_rv32.rv32.json");
    let cargo_args_log = project_root.join("cargo-args.log");
    let elf_fixture = project_root.join("fixture.elf");
    fs::write(&elf_fixture, support::minimal_rv32_elf()).unwrap();
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract-rv32","manifest_path":"{}","features":{{"entrypoints":[],"rv32-entrypoints":["entrypoints"]}}}}]}}"#,
        project_root.display(),
        target_dir.display(),
        project_root.join("Cargo.toml").display()
    );

    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\nprintf '%s\\n' \"$*\" > '{}'\n/bin/mkdir -p '{}'\n/bin/cp '{}' '{}'\n",
            cargo_args_log.display(),
            raw_elf
                .parent()
                .expect("bug: raw elf parent should exist")
                .display(),
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

    let _cwd = CurrentDirGuard::enter(&project_root);
    let _path = PathGuard::set(fake_bin.as_os_str());

    groundhog_cli::build::build_rv32_project(
        Some("demo-contract-rv32"),
        &["--locked".into()],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        Some(0x6000),
        &groundhog_cli::Print::quiet(),
    )
    .expect("bug: rv32 build with --stack-size should succeed");

    // The defsym rides on `cargo rustc` trailing flags so it composes with
    // the project's own .cargo/config.toml rustflags instead of replacing
    // them; user cargo args stay ahead of the `--` separator.
    let cargo_args = fs::read_to_string(&cargo_args_log).unwrap();
    assert!(cargo_args.starts_with("rustc "), "cargo args: {cargo_args}");
    assert!(cargo_args.contains("--locked -- -C link-arg=--defsym=__stack_size=24576"));

    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&processed_metadata).unwrap()).unwrap();
    assert_eq!(
        metadata["build_flags"],
        "cargo rustc --release --no-default-features --features rv32-entrypoints --target riscv32im-unknown-none-elf -p demo-contract-rv32 --bin demo_contract_rv32 --locked -- -C link-arg=--defsym=__stack_size=24576"
    );
}

#[test]
fn build_run_rejects_stack_size_for_wasm_target() {
    let err = groundhog_cli::build::run(
        None,
        groundhog_cli::build::BuildTarget::Wasm,
        false,
        &[],
        Some(4096),
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        "build",
        &groundhog_cli::Print::quiet(),
    )
    .expect_err("wasm target should reject --stack-size");
    assert!(matches!(
        err,
        groundhog_cli::CliError::InvalidBuildOptions(_)
    ));
}

/// A user-level `RUSTFLAGS` makes cargo ignore the project's
/// `.cargo/config.toml` target rustflags entirely, so the guard must refuse
/// to build before cargo runs. The empty temp dir would otherwise fail with
/// a cargo-metadata error, so seeing the rustflags error proves the guard
/// fires first.
#[test]
fn build_rejects_env_rustflags_before_running_cargo() {
    let tmp = TempDir::new().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .arg("build")
        .current_dir(tmp.path())
        .env("RUSTFLAGS", "-C opt-level=2")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .expect("bug: groundhog binary should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected build to fail: {stderr}");
    assert!(
        stderr.contains("RUSTFLAGS"),
        "expected RUSTFLAGS in error, got: {stderr}"
    );
    assert!(
        stderr.contains(".cargo/config.toml"),
        "expected actionable hint pointing at .cargo/config.toml, got: {stderr}"
    );
}

/// Cargo treats a set-but-empty `CARGO_ENCODED_RUSTFLAGS` as a full override
/// of config rustflags, so even an empty value must be rejected.
#[test]
fn build_rejects_empty_cargo_encoded_rustflags() {
    let tmp = TempDir::new().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .arg("build")
        .current_dir(tmp.path())
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .env_remove("RUSTFLAGS")
        .output()
        .expect("bug: groundhog binary should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected build to fail: {stderr}");
    assert!(
        stderr.contains("CARGO_ENCODED_RUSTFLAGS"),
        "expected CARGO_ENCODED_RUSTFLAGS in error, got: {stderr}"
    );
}

/// The rv32 build path loses per-crate codegen flags such as
/// `-Z trap-unreachable=false` under a user-level `RUSTFLAGS`, which only
/// surfaces later as artifact-validation rejections — so it guards too.
#[test]
fn build_rv32_rejects_env_rustflags() {
    let tmp = TempDir::new().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(["build", "--target", "rv32"])
        .current_dir(tmp.path())
        .env("RUSTFLAGS", "-C opt-level=2")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .expect("bug: groundhog binary should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected build to fail: {stderr}");
    assert!(
        stderr.contains("RUSTFLAGS"),
        "expected RUSTFLAGS in error, got: {stderr}"
    );
}

/// `cargo config get` reply: the rustflags are not set (a workspace root
/// without the Groundhog cargo config).
const CONFIG_GET_NOT_SET: &str = "echo 'error: config value `target.riscv32im-unknown-none-elf.rustflags` is not set' >&2\n  exit 101";
/// `cargo config get` reply: rustflags carrying the Groundhog linker script.
const CONFIG_GET_GROUNDHOG_SCRIPT: &str =
    "printf '%s' '[\"-C\",\"link-arg=-Tgroundhog-rv32.ld\"]'\n  exit 0";
/// `cargo config get` reply: rustflags selecting a custom linker script.
const CONFIG_GET_CUSTOM_SCRIPT: &str = "printf '%s' '[\"-C\",\"link-arg=-Tcustom.ld\"]'\n  exit 0";
/// `cargo config get` reply: a non-nightly toolchain rejecting `-Z`.
const CONFIG_GET_NOT_NIGHTLY: &str = "echo 'error: the `-Z` flag is only accepted on the nightly channel of Cargo, but this is the `stable` channel' >&2\n  exit 101";

/// Install a fake `cargo` that answers `cargo metadata` with the given JSON,
/// answers `cargo config` with `config_reply` (a shell snippet), runs
/// `build_snippet` for everything else, and exits successfully.
fn write_fake_cargo_with_metadata(
    fake_bin: &Path,
    metadata_json: &str,
    config_reply: &str,
    build_snippet: &str,
) {
    fs::create_dir_all(fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\nif [ \"$1\" = \"config\" ]; then\n  {config_reply}\nfi\n{build_snippet}\nexit 0\n"
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
}

/// A project generated by `groundhog new` and later wrapped into a parent
/// Cargo workspace, leaving `.cargo/` behind in the member directory. Cargo
/// only reads `.cargo/config.toml` from its invocation directory upward, and
/// the build runs cargo from the workspace root — so the member config is
/// silently ignored and the build must be refused.
fn write_bootstrapped_workspace(root: &Path, pkg_name: &str) -> (PathBuf, String) {
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    let member = root.join("member");
    fs::create_dir_all(member.join("src")).unwrap();
    fs::write(
        member.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nentrypoints = []\n"
        ),
    )
    .unwrap();
    fs::write(member.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
    fs::write(member.join("src/main.rs"), "fn main() {}\n").unwrap();
    write_scaffold_cargo_config(&member);
    fs::write(member.join(".cargo/contract.toml"), "").unwrap();
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"{pkg_name}","manifest_path":"{}"}}]}}"#,
        root.display(),
        root.join("target").display(),
        member.join("Cargo.toml").display()
    );
    (member, metadata_json)
}

/// Run the real `groundhog` binary with a fake cargo on `PATH` and an
/// isolated `CARGO_HOME`, so the workspace-config warning is deterministic
/// regardless of the developer's global cargo config.
fn run_groundhog_with_fake_cargo(
    cwd: &Path,
    fake_bin: &Path,
    cargo_home: &Path,
    args: &[&str],
) -> std::process::Output {
    let path = match env::var_os("PATH") {
        Some(existing) => {
            let mut entries = vec![fake_bin.to_path_buf()];
            entries.extend(env::split_paths(&existing));
            env::join_paths(entries).expect("bug: PATH should join")
        }
        None => fake_bin.to_path_buf().into_os_string(),
    };
    std::process::Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .env("CARGO_HOME", cargo_home)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .expect("bug: groundhog binary should run")
}

#[test]
fn build_rv32_refuses_missing_linker_script_naming_member_leftover() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).expect("bug: workspace path should canonicalize");
    let (member, metadata_json) = write_bootstrapped_workspace(&root, "demo-contract-rv32");
    let fake_bin = root.join("bin");
    write_fake_cargo_with_metadata(&fake_bin, &metadata_json, CONFIG_GET_NOT_SET, "");
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    let output = run_groundhog_with_fake_cargo(
        &member,
        &fake_bin,
        &cargo_home,
        &["build", "--target", "rv32", "-p", "demo-contract-rv32"],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("no `-T` linker script"),
        "expected the missing-linker-script error, got: {stderr}"
    );
    // The member's leftover config is the strongest signal this workspace was
    // bootstrapped on top of `groundhog new`; the hint must name it.
    let member_config = member.join(".cargo/config.toml");
    assert!(
        stderr.contains(&member_config.display().to_string()),
        "expected hint to name {}, got: {stderr}",
        member_config.display()
    );
    // Refused before compiling: a link with the default memory layout can
    // never produce a valid artifact.
    assert!(
        !stderr.contains("Compiling"),
        "expected the build to refuse before compiling, got: {stderr}"
    );
}

#[test]
fn build_rv32_warns_on_custom_linker_script_and_proceeds() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).expect("bug: workspace path should canonicalize");
    let (member, metadata_json) = write_bootstrapped_workspace(&root, "demo-contract-rv32");
    write_active_profile(&root);
    let fake_bin = root.join("bin");
    write_fake_cargo_with_metadata(&fake_bin, &metadata_json, CONFIG_GET_CUSTOM_SCRIPT, "");
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    let output = run_groundhog_with_fake_cargo(
        &member,
        &fake_bin,
        &cargo_home,
        &["build", "--target", "rv32", "-p", "demo-contract-rv32"],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("custom.ld"),
        "expected the custom-linker-script warning, got: {stderr}"
    );
    // A deliberately customized script is not blocked: the build proceeds
    // through the cargo invocation and fails on the missing ELF output.
    assert!(!output.status.success());
    assert!(
        stderr.contains("demo_contract_rv32"),
        "expected the build to proceed to the ELF check, got: {stderr}"
    );
}

#[test]
fn build_rv32_warns_on_non_nightly_toolchain_and_proceeds() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).expect("bug: workspace path should canonicalize");
    let (member, metadata_json) = write_bootstrapped_workspace(&root, "demo-contract-rv32");
    write_active_profile(&root);
    let fake_bin = root.join("bin");
    write_fake_cargo_with_metadata(&fake_bin, &metadata_json, CONFIG_GET_NOT_NIGHTLY, "");
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    let output = run_groundhog_with_fake_cargo(
        &member,
        &fake_bin,
        &cargo_home,
        &["build", "--target", "rv32", "-p", "demo-contract-rv32"],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // A non-nightly toolchain at the workspace root is the rust-toolchain.toml
    // flavor of the same bootstrap mistake; the warning must say so.
    assert!(
        stderr.contains("rust-toolchain.toml"),
        "expected the non-nightly toolchain warning, got: {stderr}"
    );
    assert!(!output.status.success());
    assert!(
        stderr.contains("demo_contract_rv32"),
        "expected the build to proceed to the ELF check, got: {stderr}"
    );
}

#[test]
fn build_rv32_link_failure_mentioning_script_gets_search_path_hint() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).expect("bug: workspace path should canonicalize");
    let (member, metadata_json) = write_bootstrapped_workspace(&root, "demo-contract-rv32");
    write_active_profile(&root);
    let fake_bin = root.join("bin");
    // Config resolves correctly, but the link cannot find the script — the
    // moved/missing groundhog-rv32.ld case, detectable only from lld's error.
    write_fake_cargo_with_metadata(
        &fake_bin,
        &metadata_json,
        CONFIG_GET_GROUNDHOG_SCRIPT,
        "echo 'ld.lld: error: unable to find groundhog-rv32.ld' >&2\nexit 1",
    );
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    let output = run_groundhog_with_fake_cargo(
        &member,
        &fake_bin,
        &cargo_home,
        &["build", "--target", "rv32", "-p", "demo-contract-rv32"],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("linker search path"),
        "expected the linker-script search-path hint, got: {stderr}"
    );
}

fn spec_string(value: &str) -> groundhog_spec_tool::ContractSpecString {
    value.try_into().expect("bug: test spec string should fit")
}

fn encode_fragments(fragments: &[groundhog_spec_tool::SpecFragment]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for fragment in fragments {
        bytes.extend(xdrs::txs::encode(fragment).expect("bug: test spec fragment should encode"));
    }
    bytes
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

/// `--show-cargo-commands` traces the cargo build invocation to stderr.
///
/// Drives the real binary (rather than `build_project`) so the global-flag
/// wiring through `run_cargo` is exercised end to end. Env/cwd are set on the
/// child so no process-global state is mutated.
#[test]
fn build_show_cargo_commands_traces_invocation_to_stderr() {
    let tmp = TempDir::new().unwrap();
    let project_root =
        std::fs::canonicalize(tmp.path()).expect("bug: project path should canonicalize");
    write_active_profile(&project_root);
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir_all(project_root.join(".cargo")).unwrap();
    fs::write(project_root.join(".cargo/contract.toml"), "").unwrap();
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();

    let target_dir = project_root.join("target");
    let raw_wasm = target_dir
        .join("wasm32-unknown-unknown/release")
        .join("demo_contract.wasm");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","manifest_path":"{}"}}]}}"#,
        project_root.display(),
        target_dir.display(),
        project_root.join("Cargo.toml").display()
    );

    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\n/bin/mkdir -p '{}'\nprintf '\\000asm\\001\\000\\000\\000' > '{}'\n",
            raw_wasm
                .parent()
                .expect("bug: raw wasm parent should exist")
                .display(),
            raw_wasm.display()
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

    let path = match env::var_os("PATH") {
        Some(existing) => {
            let mut entries = vec![fake_bin.clone()];
            entries.extend(env::split_paths(&existing));
            env::join_paths(entries).expect("bug: PATH should join")
        }
        None => fake_bin.clone().into_os_string(),
    };

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args([
            "--show-cargo-commands",
            "build",
            "--target",
            "wasm",
            "-p",
            "demo-contract",
        ])
        .current_dir(&project_root)
        .env("PATH", path)
        .output()
        .expect("bug: groundhog binary should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "build failed: {stderr}");
    assert!(
        stderr.contains("+ ") && stderr.contains("cargo") && stderr.contains("build --release"),
        "expected traced cargo command on stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("wasm32-unknown-unknown"),
        "expected target triple in trace, got: {stderr}"
    );
}
