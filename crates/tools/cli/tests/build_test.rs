use serial_test::serial;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

mod support;

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
fn build_project_recopies_raw_wasm_before_reprocessing_on_repeated_runs() {
    let tmp = TempDir::new().unwrap();
    let project_root =
        std::fs::canonicalize(tmp.path()).expect("bug: project path should canonicalize");
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
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"demo-contract-rv32\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
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
        r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract-rv32","manifest_path":"{}"}}]}}"#,
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
        &groundhog_cli::Print::quiet(),
    )
    .expect("bug: rv32 build should succeed");

    assert_eq!(result.elf, raw_elf);
    assert_eq!(result.rv32, processed_rv32);
    assert_eq!(result.metadata, processed_metadata);
    assert_eq!(result.spec_json, Some(processed_spec_json.clone()));
    assert_eq!(result.spec_xdr, Some(processed_spec_xdr.clone()));
    // v4 stores only the high-water prefix; the spec-metadata section is
    // skipped, so the artifact is header + code, far below the old full image.
    assert!(result.size_bytes < 64 * 1024);
    assert!(fs::read(&processed_rv32).unwrap().starts_with(b"GHRV32A1"));
    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&processed_metadata).unwrap()).unwrap();
    assert_eq!(
        metadata["artifact_format_fingerprint"],
        "0x3aed01915d1a5e1d"
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
