use serial_test::serial;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

mod support;

/// Minimal stand-in for the `.cargo/config.toml` that `groundhog new`
/// scaffolds: the Groundhog RV32 target rustflags table, which is what the
/// workspace-config guard requires at the cargo workspace root.
fn write_scaffold_cargo_config(project_root: &Path) {
    fs::create_dir_all(project_root.join(".cargo")).unwrap();
    fs::write(
        project_root.join(".cargo/config.toml"),
        "[target.riscv32im-unknown-none-elf]\n\
         rustflags = [\"-C\", \"link-arg=-Tgroundhog-rv32.ld\"]\n",
    )
    .unwrap();
}

/// The `.cargo/contract.toml` overlay every contract build now requires at the
/// cargo workspace root: `groundhog build` passes it as `--config`, so its
/// absence refuses the build before cargo runs.
/// Contents are irrelevant to these tests — only the fake cargo sees them.
fn write_contract_toml(workspace_root: &Path) {
    fs::create_dir_all(workspace_root.join(".cargo")).unwrap();
    fs::write(workspace_root.join(".cargo/contract.toml"), "").unwrap();
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
                version: "0.1.0".into(),
                manifest_path: PathBuf::from("/ws/alpha/Cargo.toml"),
            },
            groundhog_cli::build::MetadataPackage {
                name: "beta".into(),
                version: "0.1.0".into(),
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
            version: "0.1.0".into(),
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
                version: "0.1.0".into(),
                manifest_path: PathBuf::from("/ws/alpha/Cargo.toml"),
            },
            groundhog_cli::build::MetadataPackage {
                name: "beta".into(),
                version: "0.1.0".into(),
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
                version: "0.1.0".into(),
                manifest_path: PathBuf::from("/ws/alpha/Cargo.toml"),
            },
            groundhog_cli::build::MetadataPackage {
                name: "beta".into(),
                version: "0.1.0".into(),
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
            version: "0.1.0".into(),
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
            version: "0.1.0".into(),
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
        groundhog_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let err = groundhog_cli::build::build_rv32_project(
        Some("demo-contract"),
        &["--target-dir".into(), "nested".into()],
        &artifacts,
        None,
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
    let err = groundhog_cli::build::build_rv32_project(
        Some("demo-contract"),
        &["--target-dir".into(), "nested".into()],
        &artifacts,
        None,
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
        groundhog_cli::artifacts::ArtifactOptions::new(Some(tmp.path().join("explicit-target")));
    let err = groundhog_cli::build::build_rv32_project(
        Some("demo-contract"),
        &["--config".into(), "target-config.toml".into()],
        &artifacts,
        None,
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
    let compile_marker = tmp.path().join("compiled");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
        tmp.path().display(),
        tmp.path().join("target").display(),
        tmp.path().join("Cargo.toml").display()
    );
    write_fake_cargo_with_metadata(
        &fake_bin,
        &metadata_json,
        CONFIG_GET_GROUNDHOG_SCRIPT,
        &format!(": > '{}'", compile_marker.display()),
    );
    let _cwd = CurrentDirGuard::enter(tmp.path());
    let _path = PathGuard::set(fake_bin.as_os_str());
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("config-home")) };

    let error = groundhog_cli::build::build_rv32_project(
        Some("demo-contract"),
        &[],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
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
    write_contract_toml(&project_root);

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
                emits: vec![].try_into().unwrap(),
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
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract-rv32","version":"0.1.0","manifest_path":"{}","features":{{"entrypoints":[],"rv32-entrypoints":["entrypoints"]}}}}]}}"#,
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
        &[],
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
    // The `build-std` overlay applies to rv32 too, and `--config` only counts
    // ahead of the subcommand.
    assert!(
        cargo_args.starts_with(&format!(
            "--config {} build --release",
            project_root.join(".cargo/contract.toml").display()
        )),
        "cargo args: {cargo_args}"
    );
    assert!(
        cargo_args.contains(
            "build --release --no-default-features --features rv32-entrypoints --target riscv32im-unknown-none-elf"
        )
    );
    assert!(cargo_args.contains("-p demo-contract-rv32"));
    assert!(cargo_args.contains("--bin demo_contract_rv32"));
    assert!(cargo_args.contains("--locked"));
    // The RV32 build must include the contract's bin target.
    assert!(!cargo_args.contains("--lib"), "cargo args: {cargo_args}");
    assert_eq!(metadata["target_triple"], "riscv32im-unknown-none-elf");
    // The sidecar records the overlay by its workspace-relative path: it
    // changes codegen, but the absolute path is specific to this machine.
    assert_eq!(
        metadata["build_flags"],
        "cargo --config .cargo/contract.toml build --release --no-default-features --features rv32-entrypoints --target riscv32im-unknown-none-elf -p demo-contract-rv32 --bin demo_contract_rv32 --locked"
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
    write_contract_toml(&project_root);

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
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract-rv32","version":"0.1.0","manifest_path":"{}","features":{{"entrypoints":[],"rv32-entrypoints":["entrypoints"]}}}}]}}"#,
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
        &[],
        &groundhog_cli::Print::quiet(),
    )
    .expect("bug: rv32 build with --stack-size should succeed");

    // The defsym rides on `cargo rustc` trailing flags so it composes with
    // the project's own .cargo/config.toml rustflags instead of replacing
    // them; user cargo args stay ahead of the `--` separator.
    let cargo_args = fs::read_to_string(&cargo_args_log).unwrap();
    // The overlay rides ahead of the subcommand on this path too.
    assert!(
        cargo_args.starts_with(&format!(
            "--config {} rustc ",
            project_root.join(".cargo/contract.toml").display()
        )),
        "cargo args: {cargo_args}"
    );
    assert!(cargo_args.contains("--locked -- -C link-arg=--defsym=__stack_size=24576"));

    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&processed_metadata).unwrap()).unwrap();
    assert_eq!(
        metadata["build_flags"],
        "cargo --config .cargo/contract.toml rustc --release --no-default-features --features rv32-entrypoints --target riscv32im-unknown-none-elf -p demo-contract-rv32 --bin demo_contract_rv32 --locked -- -C link-arg=--defsym=__stack_size=24576"
    );
}
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
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"{pkg_name}","version":"0.1.0","manifest_path":"{}"}}]}}"#,
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
        &["build", "-p", "demo-contract-rv32"],
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
    write_contract_toml(&root);
    let fake_bin = root.join("bin");
    write_fake_cargo_with_metadata(&fake_bin, &metadata_json, CONFIG_GET_CUSTOM_SCRIPT, "");
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    let output = run_groundhog_with_fake_cargo(
        &member,
        &fake_bin,
        &cargo_home,
        &["build", "-p", "demo-contract-rv32"],
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
    write_contract_toml(&root);
    let fake_bin = root.join("bin");
    write_fake_cargo_with_metadata(&fake_bin, &metadata_json, CONFIG_GET_NOT_NIGHTLY, "");
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    let output = run_groundhog_with_fake_cargo(
        &member,
        &fake_bin,
        &cargo_home,
        &["build", "-p", "demo-contract-rv32"],
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
    write_contract_toml(&root);
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
        &["build", "-p", "demo-contract-rv32"],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("linker search path"),
        "expected the linker-script search-path hint, got: {stderr}"
    );
}

/// The RV32 compile needs `build-std` because `-C panic=immediate-abort` is
/// incompatible with the prebuilt `core`. That only reaches cargo through the
/// `.cargo/contract.toml` overlay, and only when `--config` precedes the
/// subcommand.
#[test]
fn build_rv32_passes_the_contract_toml_overlay_to_cargo() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).expect("bug: workspace path should canonicalize");
    let (member, metadata_json) = write_bootstrapped_workspace(&root, "demo-contract-rv32");
    write_active_profile(&root);
    write_contract_toml(&root);
    let cargo_args_log = root.join("cargo-args.log");
    let fake_bin = root.join("bin");
    write_fake_cargo_with_metadata(
        &fake_bin,
        &metadata_json,
        CONFIG_GET_GROUNDHOG_SCRIPT,
        &format!("printf '%s\\n' \"$*\" > '{}'", cargo_args_log.display()),
    );
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    // The compile fails downstream on the ELF the fake cargo never produces;
    // the recorded argv is what this test is about.
    run_groundhog_with_fake_cargo(
        &member,
        &fake_bin,
        &cargo_home,
        &["build", "-p", "demo-contract-rv32"],
    );

    let cargo_args = fs::read_to_string(&cargo_args_log).unwrap();
    assert!(
        cargo_args.starts_with(&format!(
            "--config {} build ",
            root.join(".cargo/contract.toml").display()
        )),
        "expected the overlay ahead of the subcommand, got: {cargo_args}"
    );
}

/// The overlay is not optional: without it the rv32 compile fails deep in
/// cargo on the panic strategy, so it is refused up front instead.
#[test]
fn build_rv32_refuses_a_missing_contract_toml() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).expect("bug: workspace path should canonicalize");
    let (member, metadata_json) = write_bootstrapped_workspace(&root, "demo-contract-rv32");
    write_active_profile(&root);
    let fake_bin = root.join("bin");
    write_fake_cargo_with_metadata(&fake_bin, &metadata_json, CONFIG_GET_GROUNDHOG_SCRIPT, "");
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    let output = run_groundhog_with_fake_cargo(
        &member,
        &fake_bin,
        &cargo_home,
        &["build", "-p", "demo-contract-rv32"],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains(".cargo/contract.toml` not found"),
        "expected the missing-overlay error, got: {stderr}"
    );
    // The hint has to name what goes in the file, and warn off the rustflags
    // that would duplicate the linker script.
    assert!(
        stderr.contains("build-std") && stderr.contains("[target.*]"),
        "expected the build-std hint, got: {stderr}"
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
/// Drives the real binary (rather than `build_rv32_project`) so the global-flag
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
    write_scaffold_cargo_config(&project_root);
    write_contract_toml(&project_root);
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();

    let target_dir = project_root.join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo_contract");
    let elf_fixture = project_root.join("fixture.elf");
    fs::write(&elf_fixture, support::minimal_rv32_elf()).unwrap();
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
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
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s' '{metadata_json}'\n  exit 0\nfi\nif [ \"$1\" = \"config\" ]; then\n  printf '%s' '[\"-C\",\"link-arg=-Tgroundhog-rv32.ld\"]'\n  exit 0\nfi\n/bin/mkdir -p '{}'\n/bin/cp '{}' '{}'\n",
            raw_elf
                .parent()
                .expect("bug: raw ELF parent should exist")
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

    let path = match env::var_os("PATH") {
        Some(existing) => {
            let mut entries = vec![fake_bin.clone()];
            entries.extend(env::split_paths(&existing));
            env::join_paths(entries).expect("bug: PATH should join")
        }
        None => fake_bin.clone().into_os_string(),
    };

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(["--show-cargo-commands", "build", "-p", "demo-contract"])
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
        stderr.contains("riscv32im-unknown-none-elf"),
        "expected target triple in trace, got: {stderr}"
    );
}

/// What cargo says while it reads the registry has to reach the user.
///
/// The dependency-graph resolve is the first thing `groundhog build` runs
/// that touches the registry, and it used to be captured whole: a registry
/// that was slow, or not answering at all, left the build printing nothing
/// with no way to tell a stall from progress. Drives the real binary against
/// a `cargo` that reports registry work the way cargo does, and asserts that
/// report lands on the build's own stderr.
#[test]
fn build_relays_what_cargo_reports_while_it_reads_the_registry() {
    let tmp = TempDir::new().unwrap();
    let project_root =
        std::fs::canonicalize(tmp.path()).expect("bug: project path should canonicalize");
    write_active_profile(&project_root);
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    write_scaffold_cargo_config(&project_root);
    write_contract_toml(&project_root);
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();

    let target_dir = project_root.join("target");
    let raw_elf = target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join("demo_contract");
    let elf_fixture = project_root.join("fixture.elf");
    fs::write(&elf_fixture, support::minimal_rv32_elf()).unwrap();
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
        project_root.display(),
        target_dir.display(),
        project_root.join("Cargo.toml").display()
    );

    // `--no-deps` is the manifest-only call; the resolve is the one that
    // reads the registry, and reports it on stderr the way cargo does.
    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"metadata\" ]; then\n\
             \x20 case \" $* \" in\n\
             \x20   *\" --no-deps \"*) printf '%s' '{metadata_json}'; exit 0 ;;\n\
             \x20 esac\n\
             \x20 echo '    Updating `groundhog` index' >&2\n\
             \x20 echo '    Blocking waiting for file lock on package cache' >&2\n\
             \x20 printf '%s' '{{\"packages\":[],\"resolve\":{{\"nodes\":[]}}}}'\n\
             \x20 exit 0\n\
             fi\n\
             if [ \"$1\" = \"config\" ]; then\n\
             \x20 printf '%s' '[\"-C\",\"link-arg=-Tgroundhog-rv32.ld\"]'\n\
             \x20 exit 0\n\
             fi\n\
             /bin/mkdir -p '{}'\n\
             /bin/cp '{}' '{}'\n",
            raw_elf
                .parent()
                .expect("bug: raw ELF parent should exist")
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

    let path = match env::var_os("PATH") {
        Some(existing) => {
            let mut entries = vec![fake_bin.clone()];
            entries.extend(env::split_paths(&existing));
            env::join_paths(entries).expect("bug: PATH should join")
        }
        None => fake_bin.clone().into_os_string(),
    };

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(["build", "-p", "demo-contract"])
        .current_dir(&project_root)
        .env("PATH", path)
        .output()
        .expect("bug: groundhog binary should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "build failed: {stderr}");
    assert!(
        stderr.contains("Updating `groundhog` index"),
        "expected cargo's registry progress to be relayed, got: {stderr}"
    );
    assert!(
        stderr.contains("Blocking waiting for file lock on package cache"),
        "expected the wait cargo never ends to be visible, got: {stderr}"
    );
}

/// A build that loses its stderr must take cargo down with it.
///
/// `groundhog build 2>&1 | head` closes the relay's reader partway through,
/// so the next write fails and the resolve ends early. Dropping the child
/// there would neither kill nor reap it, leaving a `cargo metadata` holding
/// the package-cache lock after the CLI has gone — the next build then waits
/// on a lock held by a process nobody can see, which is the hang relaying
/// cargo's output exists to prevent.
///
/// The fake cargo goes quiet after the write that fails, the way cargo does
/// while it waits on that lock, so nothing but a kill stops it; it records
/// having finished only if it was left to run.
#[cfg(unix)]
#[test]
fn build_stops_cargo_when_the_relay_loses_its_reader() {
    use std::io::BufRead;

    let tmp = TempDir::new().unwrap();
    let project_root =
        std::fs::canonicalize(tmp.path()).expect("bug: project path should canonicalize");
    write_active_profile(&project_root);
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    write_scaffold_cargo_config(&project_root);
    write_contract_toml(&project_root);

    let finished = project_root.join("cargo-ran-to-completion");
    let metadata_json = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
        project_root.display(),
        project_root.join("target").display(),
        project_root.join("Cargo.toml").display()
    );

    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"metadata\" ]; then\n\
             \x20 case \" $* \" in\n\
             \x20   *\" --no-deps \"*) printf '%s' '{metadata_json}'; exit 0 ;;\n\
             \x20 esac\n\
             \x20 echo '    Updating `groundhog` index' >&2\n\
             \x20 /bin/sleep 1\n\
             \x20 echo '    Blocking waiting for file lock on package cache' >&2\n\
             \x20 /bin/sleep 2\n\
             \x20 /bin/touch '{}'\n\
             \x20 exit 0\n\
             fi\n\
             if [ \"$1\" = \"config\" ]; then\n\
             \x20 printf '%s' '[\"-C\",\"link-arg=-Tgroundhog-rv32.ld\"]'\n\
             \x20 exit 0\n\
             fi\n\
             exit 0\n",
            finished.display()
        ),
    )
    .unwrap();

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

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(["build", "-p", "demo-contract"])
        .current_dir(&project_root)
        .env("PATH", path)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("bug: groundhog binary should run");

    // Read until cargo's first relayed line, then close the reader — the
    // `| head` case. The build's next relayed write is the one that fails.
    let stderr = child
        .stderr
        .take()
        .expect("bug: child stderr is piped above");
    let mut lines = std::io::BufReader::new(stderr).lines();
    let relayed = lines
        .find(|line| {
            line.as_ref()
                .is_ok_and(|line| line.contains("Updating `groundhog` index"))
        })
        .expect("bug: the build should relay cargo's registry output");
    relayed.expect("bug: reading the relayed line should succeed");
    drop(lines);

    child.wait().expect("bug: the build should exit");

    // Past the point a cargo left running would have recorded finishing.
    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(
        !finished.exists(),
        "cargo outlived the build that spawned it"
    );
}

/// Build an ELF whose raw spec section carries `format`, with a body that is
/// deliberately not decodable: the format check must fire before any fragment
/// byte is read, and well-formed bytes would let a too-late check pass.
fn rv32_elf_with_raw_spec_format(format: u32) -> Vec<u8> {
    support::minimal_rv32_elf_with_section(&format!("groundhogspecrawv{format}"), &[0xff; 16])
}

fn finalize_rv32_spec_err(elf_bytes: &[u8]) -> String {
    let dir = TempDir::new().unwrap();
    let elf = dir.path().join("contract.elf");
    fs::write(&elf, elf_bytes).unwrap();

    groundhog_cli::contract_spec::finalize_rv32_spec(
        &elf,
        &dir.path().join("contract.rv32a"),
        &groundhog_cli::Print::quiet(),
    )
    .expect_err("a raw spec format this build cannot read must be an error, not `no spec`")
    .to_string()
}

#[test]
fn rv32_raw_spec_section_newer_than_this_build_reports_skew() {
    let supported = groundhog_spec_tool::RAW_SPEC_FORMAT;
    let message = finalize_rv32_spec_err(&rv32_elf_with_raw_spec_format(supported + 1));

    assert!(
        message.contains("newer Groundhog SDK"),
        "expected a version-skew diagnosis, got: {message}"
    );
    assert!(
        message.contains(&(supported + 1).to_string()) && message.contains(&supported.to_string()),
        "expected both versions named, got: {message}"
    );
}

#[test]
fn rv32_raw_spec_section_older_than_this_build_reports_skew() {
    let supported = groundhog_spec_tool::RAW_SPEC_FORMAT;
    let message = finalize_rv32_spec_err(&rv32_elf_with_raw_spec_format(supported - 1));

    assert!(
        message.contains("older Groundhog SDK"),
        "expected a version-skew diagnosis, got: {message}"
    );
    assert!(
        message.contains(&(supported - 1).to_string()) && message.contains(&supported.to_string()),
        "expected both versions named, got: {message}"
    );
}

#[test]
#[serial]
fn rv32_build_refuses_a_newer_contract_spec_format_before_compiling() {
    let message =
        rv32_build_with_declared_raw_spec_format(groundhog_spec_tool::RAW_SPEC_FORMAT + 1);

    assert!(
        message.contains("newer Groundhog SDK"),
        "expected a version-skew diagnosis, got: {message}"
    );
}

#[test]
#[serial]
fn rv32_build_refuses_an_older_contract_spec_format_before_compiling() {
    let message =
        rv32_build_with_declared_raw_spec_format(groundhog_spec_tool::RAW_SPEC_FORMAT - 1);

    assert!(
        message.contains("older Groundhog SDK"),
        "expected a version-skew diagnosis, got: {message}"
    );
}

/// Run an rv32 build whose resolved `groundhog-contract-spec` declares
/// `declared`, asserting it refuses before cargo is asked to compile
/// anything. Returns the error message for the caller to inspect.
fn rv32_build_with_declared_raw_spec_format(declared: u32) -> String {
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
    write_scaffold_cargo_config(&project_root);

    let cargo_args_log = project_root.join("cargo-args.log");
    let metadata_args_log = project_root.join("metadata-args.log");
    // `--no-deps` reports workspace members only, so the preflight has to ask
    // for the resolved graph: serve the SDK package on that call alone, and
    // the test fails if the preflight reads the wrong one.
    let workspace_metadata = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract-rv32","version":"0.1.0","manifest_path":"{}","features":{{"entrypoints":[],"rv32-entrypoints":["entrypoints"]}}}}]}}"#,
        project_root.display(),
        project_root.join("target").display(),
        project_root.join("Cargo.toml").display()
    );
    // Shaped like a real contract graph: the member reaches
    // `groundhog-contract-spec` through `sdk`, not directly.
    let dep_graph_metadata = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[
            {{"id":"member","name":"demo-contract-rv32","version":"0.1.0","manifest_path":"{}","metadata":null}},
            {{"id":"sdk","name":"sdk","version":"0.1.0","manifest_path":"{}","metadata":null}},
            {{"id":"spec","name":"groundhog-contract-spec","version":"0.1.0","manifest_path":"{}","metadata":{{"groundhog":{{"raw_spec_format":{declared}}}}}}}
        ],"resolve":{{"nodes":[
            {{"id":"member","dependencies":["sdk"]}},
            {{"id":"sdk","dependencies":["spec"]}},
            {{"id":"spec","dependencies":[]}}
        ]}}}}"#,
        project_root.display(),
        project_root.join("target").display(),
        project_root.join("Cargo.toml").display(),
        project_root.join("Cargo.toml").display(),
        project_root.join("Cargo.toml").display()
    );

    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  case \"$*\" in\n    *--no-deps*) printf '%s' '{workspace_metadata}' ;;\n    *) printf '%s\\n' \"$*\" >> '{}'; printf '%s' '{dep_graph_metadata}' ;;\n  esac\n  exit 0\nfi\nprintf '%s\\n' \"$*\" >> '{}'\n",
            metadata_args_log.display(),
            cargo_args_log.display()
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

    let error = groundhog_cli::build::build_rv32_project(
        Some("demo-contract-rv32"),
        &["--locked".into()],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        None,
        &[],
        &groundhog_cli::Print::quiet(),
    )
    .expect_err("a raw spec format this build cannot read must refuse the build");

    let message = error.to_string();
    assert!(
        message.contains(&declared.to_string())
            && message.contains(&groundhog_spec_tool::RAW_SPEC_FORMAT.to_string()),
        "expected both formats named, got: {message}"
    );
    // Nothing was handed to cargo beyond the metadata queries: the point of
    // the preflight is refusing in seconds rather than after a full compile.
    assert!(
        !cargo_args_log.exists(),
        "preflight must refuse before cargo runs, but cargo was invoked with: {}",
        fs::read_to_string(&cargo_args_log).unwrap_or_default()
    );
    // The caller asked for `--locked`; a preflight that resolves the graph
    // without it can rewrite the lockfile the build was promised would not
    // change. `--offline` and `--frozen` carry the same contract.
    let metadata_args = fs::read_to_string(&metadata_args_log).unwrap_or_default();
    assert!(
        metadata_args.contains("--locked"),
        "the preflight metadata call must honor the build's manifest flags, got: {metadata_args}"
    );

    message
}

#[test]
#[serial]
fn rv32_build_proceeds_when_the_dependency_graph_is_unavailable() {
    // Offline and vendored builds must keep working: resolving the graph can
    // need the network, so a failed `cargo metadata` warns and lets the build
    // run rather than refusing a contract that may be perfectly fine.
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
    write_scaffold_cargo_config(&project_root);
    write_contract_toml(&project_root);

    let cargo_args_log = project_root.join("cargo-args.log");
    let workspace_metadata = format!(
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract-rv32","version":"0.1.0","manifest_path":"{}","features":{{"entrypoints":[],"rv32-entrypoints":["entrypoints"]}}}}]}}"#,
        project_root.display(),
        project_root.join("target").display(),
        project_root.join("Cargo.toml").display()
    );

    let fake_bin = project_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let cargo = fake_bin.join("cargo");
    // The deps-resolving metadata call fails the way an offline cargo does;
    // the `--no-deps` call still works from the lockfile.
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  case \"$*\" in\n    *--no-deps*) printf '%s' '{workspace_metadata}' ;;\n    *) echo 'error: failed to download from registry (offline)' >&2; exit 101 ;;\n  esac\n  exit 0\nfi\nprintf '%s\\n' \"$*\" >> '{}'\n",
            cargo_args_log.display()
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

    let error = groundhog_cli::build::build_rv32_project(
        Some("demo-contract-rv32"),
        &["--locked".into()],
        &groundhog_cli::artifacts::ArtifactOptions::default(),
        None,
        None,
        &[],
        &groundhog_cli::Print::quiet(),
    )
    .expect_err("the fake cargo produces no ELF, so the build fails downstream");

    // Whatever stopped it, it must not be the preflight: an unreadable graph
    // is not evidence of a version mismatch.
    let message = error.to_string();
    assert!(
        !message.contains("Groundhog SDK"),
        "an unavailable dep graph must not be reported as version skew: {message}"
    );
    assert!(
        cargo_args_log.exists()
            && fs::read_to_string(&cargo_args_log)
                .unwrap()
                .contains("build --release"),
        "the build must reach cargo despite the unresolvable dependency graph"
    );
}
