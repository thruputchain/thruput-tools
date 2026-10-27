use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;
use thruput_cli::CliError;
use thruput_cli::artifacts::{
    ArtifactKind, ArtifactOptions, ManifestFlags, cargo_rv32_elf_output_path,
    ensure_no_target_dir_passthrough_conflicts, infer_artifact_kind,
    thruput_rv32_metadata_output_path, thruput_rv32_output_path,
};

#[test]
fn cargo_rv32_elf_output_path_uses_cargo_layout() {
    let path =
        cargo_rv32_elf_output_path(PathBuf::from("/tmp/out").as_path(), "demo-contract-rv32");
    assert_eq!(
        path,
        PathBuf::from("/tmp/out/riscv32im-unknown-none-elf/release/demo_contract_rv32")
    );
}

#[test]
fn thruput_rv32_output_paths_use_flat_layout() {
    let artifact =
        thruput_rv32_output_path(PathBuf::from("/tmp/out").as_path(), "demo-contract-rv32");
    let metadata = thruput_rv32_metadata_output_path(
        PathBuf::from("/tmp/out").as_path(),
        "demo-contract-rv32",
    );
    assert_eq!(
        artifact,
        PathBuf::from("/tmp/out/thruput/demo_contract_rv32.rv32a")
    );
    assert_eq!(
        metadata,
        PathBuf::from("/tmp/out/thruput/demo_contract_rv32.rv32.json")
    );
}

#[test]
fn infer_artifact_kind_uses_extension() {
    assert_eq!(
        infer_artifact_kind(PathBuf::from("contract.rv32a").as_path())
            .expect("bug: rv32 should infer"),
        ArtifactKind::Rv32
    );

    let err = infer_artifact_kind(PathBuf::from("contract.bin").as_path())
        .expect_err("bug: unsupported extension should error");
    assert!(matches!(err, CliError::UnsupportedArtifactExtension(path) if path == "contract.bin"));
}

#[test]
fn artifact_kind_serde_and_schema_use_wire_strings() {
    assert_eq!(
        serde_json::to_value(ArtifactKind::Rv32).expect("bug: rv32 should serialize"),
        serde_json::json!("rv32")
    );
    assert_eq!(
        serde_json::from_value::<ArtifactKind>(serde_json::json!("rv32"))
            .expect("bug: rv32 should deserialize"),
        ArtifactKind::Rv32
    );
    assert!(serde_json::from_value::<ArtifactKind>(serde_json::json!("rv32a")).is_err());

    let schema = serde_json::to_value(schemars::schema_for!(ArtifactKind))
        .expect("bug: artifact schema should serialize");
    assert_eq!(schema["enum"], serde_json::json!(["rv32"]));
}

#[test]
fn apply_to_command_sets_cargo_target_dir() {
    let options = ArtifactOptions::new(Some(PathBuf::from("/tmp/out")));
    let mut command = Command::new("cargo");
    options.apply_to_command(&mut command);

    let envs = command.get_envs().collect::<Vec<_>>();
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].0.to_string_lossy(), "CARGO_TARGET_DIR");
    assert_eq!(
        envs[0]
            .1
            .expect("bug: env value should be present")
            .to_string_lossy(),
        "/tmp/out"
    );
}

#[test]
fn normalizes_relative_target_dir_against_invocation_cwd() {
    let invocation_cwd = PathBuf::from("/tmp/project/contracts/demo");
    let options = ArtifactOptions::from_invocation_cwd(
        Some(PathBuf::from("../artifacts/out")),
        &invocation_cwd,
    );

    let mut command = Command::new("cargo");
    options.apply_to_command(&mut command);

    let envs = command.get_envs().collect::<Vec<_>>();
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].0.to_string_lossy(), "CARGO_TARGET_DIR");
    assert_eq!(
        envs[0]
            .1
            .expect("bug: env value should be present")
            .to_string_lossy(),
        "/tmp/project/contracts/artifacts/out"
    );
}

struct PassthroughCase {
    name: &'static str,
    /// CLI-selected target dir; `None` disables the check.
    target_dir: Option<&'static str>,
    /// Passthrough args; `<base>` expands to the config base directory.
    args: &'static [&'static str],
    conflicts: bool,
}

const PASSTHROUGH_CASES: &[PassthroughCase] = &[
    PassthroughCase {
        name: "--target-dir passthrough arg",
        target_dir: Some("out"),
        args: &["--target-dir", "nested"],
        conflicts: true,
    },
    PassthroughCase {
        name: "--target-dir ignored when target dir option is disabled",
        target_dir: None,
        args: &["--target-dir", "nested"],
        conflicts: false,
    },
    PassthroughCase {
        name: "absolute config file setting build.target-dir",
        target_dir: Some("out"),
        args: &["--config", "<base>/config.toml"],
        conflicts: true,
    },
    PassthroughCase {
        name: "relative config file resolved against explicit base directory",
        target_dir: Some("out"),
        args: &["--config", "nested/config.toml"],
        conflicts: true,
    },
    PassthroughCase {
        name: "missing config file neither errors nor conflicts",
        target_dir: Some("out"),
        args: &["--config", "missing.toml"],
        conflicts: false,
    },
    PassthroughCase {
        name: "invalid TOML config file neither errors nor conflicts",
        target_dir: Some("out"),
        args: &["--config", "invalid.toml"],
        conflicts: false,
    },
    PassthroughCase {
        name: "inline config target dir as separate arg",
        target_dir: Some("out"),
        args: &["--config", "build.target-dir=\"nested\""],
        conflicts: true,
    },
    PassthroughCase {
        name: "inline config target dir in --config= form",
        target_dir: Some("out"),
        args: &["--config=build.target-dir=\"nested\""],
        conflicts: true,
    },
    PassthroughCase {
        name: "spaced inline config target dir",
        target_dir: Some("out"),
        args: &["--config", "build.target-dir = \"nested\""],
        conflicts: true,
    },
    PassthroughCase {
        name: "inline config without target dir",
        target_dir: Some("out"),
        args: &["--config", "term.color=always"],
        conflicts: false,
    },
    PassthroughCase {
        name: "spaced inline config without target dir",
        target_dir: Some("out"),
        args: &["--config", "term.color = \"always\""],
        conflicts: false,
    },
];

#[test]
fn target_dir_passthrough_conflicts() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let base = tmp.path();
    std::fs::create_dir_all(base.join("nested")).expect("bug: config dir should be created");
    for config in ["config.toml", "nested/config.toml"] {
        std::fs::write(base.join(config), "[build]\ntarget-dir = \"nested\"\n")
            .expect("bug: config file should be written");
    }
    std::fs::write(
        base.join("invalid.toml"),
        "[build\n target-dir = \"nested\"\n",
    )
    .expect("bug: config file should be written");

    for case in PASSTHROUGH_CASES {
        let options = ArtifactOptions::new(case.target_dir.map(PathBuf::from));
        let args = case
            .args
            .iter()
            .map(|arg| arg.replace("<base>", &base.display().to_string()))
            .collect::<Vec<_>>();

        let result = ensure_no_target_dir_passthrough_conflicts(&options, base, &args);

        if case.conflicts {
            assert!(
                matches!(result, Err(CliError::TargetDirConflict)),
                "{}: expected TargetDirConflict, got {result:?}",
                case.name
            );
        } else {
            assert!(result.is_ok(), "{}: expected Ok, got {result:?}", case.name);
        }
    }
}

#[test]
fn manifest_flags_read_only_resolve_flags_from_cargo_args() {
    let flags = ManifestFlags::from_cargo_args(&[
        "--locked".into(),
        "--frozen".into(),
        "--release".into(),
        "-p".into(),
        "demo".into(),
    ]);
    let mut command = Command::new("cargo");
    flags.apply_to_command(&mut command);

    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(args, vec!["--locked", "--frozen"]);
}

#[test]
fn manifest_flags_default_is_empty() {
    assert_eq!(
        ManifestFlags::from_cargo_args(&[]),
        ManifestFlags::default()
    );
}

#[test]
fn manifest_flags_apply_only_enabled_flags() {
    let flags = ManifestFlags::from_cargo_args(&["--locked".into(), "--offline".into()]);
    let mut command = Command::new("cargo");
    command.arg("metadata");
    flags.apply_to_command(&mut command);

    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(args, vec!["metadata", "--locked", "--offline"]);
}

#[test]
fn manifest_flags_apply_nothing_when_absent() {
    let flags = ManifestFlags::default();
    let mut command = Command::new("cargo");
    flags.apply_to_command(&mut command);
    assert_eq!(command.get_args().count(), 0);
}
