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
fn thruput_rv32_output_paths_use_flat_groundhog_layout() {
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

#[test]
fn detects_cli_target_dir_conflict_in_passthrough_args() {
    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let base_dir = PathBuf::from("/tmp");
    let err = ensure_no_target_dir_passthrough_conflicts(
        &options,
        &base_dir,
        &["--target-dir".into(), "nested".into()],
    )
    .unwrap_err();

    assert!(matches!(err, CliError::TargetDirConflict));
}

#[test]
fn detects_config_file_target_dir_conflict() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, "[build]\ntarget-dir = \"nested\"\n")
        .expect("bug: config file should be written");

    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let err = ensure_no_target_dir_passthrough_conflicts(
        &options,
        tmp.path(),
        &["--config".into(), config_path.display().to_string()],
    )
    .unwrap_err();

    assert!(matches!(err, CliError::TargetDirConflict));
}

#[test]
fn detects_inline_config_target_dir_conflict_with_separate_arg() {
    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let base_dir = PathBuf::from("/tmp");
    let err = ensure_no_target_dir_passthrough_conflicts(
        &options,
        &base_dir,
        &["--config".into(), "build.target-dir=\"nested\"".into()],
    )
    .unwrap_err();

    assert!(matches!(err, CliError::TargetDirConflict));
}

#[test]
fn detects_inline_config_target_dir_conflict_with_equals_form() {
    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let base_dir = PathBuf::from("/tmp");
    let err = ensure_no_target_dir_passthrough_conflicts(
        &options,
        &base_dir,
        &["--config=build.target-dir=\"nested\"".into()],
    )
    .unwrap_err();

    assert!(matches!(err, CliError::TargetDirConflict));
}

#[test]
fn allows_inline_config_without_target_dir() {
    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let base_dir = PathBuf::from("/tmp");
    let result = ensure_no_target_dir_passthrough_conflicts(
        &options,
        &base_dir,
        &["--config".into(), "term.color=always".into()],
    );

    assert!(result.is_ok());
}

#[test]
fn detects_spaced_inline_config_target_dir_conflict() {
    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let base_dir = PathBuf::from("/tmp");
    let err = ensure_no_target_dir_passthrough_conflicts(
        &options,
        &base_dir,
        &["--config".into(), "build.target-dir = \"nested\"".into()],
    )
    .unwrap_err();

    assert!(matches!(err, CliError::TargetDirConflict));
}

#[test]
fn allows_spaced_inline_config_without_target_dir() {
    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let base_dir = PathBuf::from("/tmp");
    let result = ensure_no_target_dir_passthrough_conflicts(
        &options,
        &base_dir,
        &["--config".into(), "term.color = \"always\"".into()],
    );

    assert!(result.is_ok());
}

#[test]
fn returns_ok_when_target_dir_option_is_disabled() {
    let options = ArtifactOptions::new(None);
    let base_dir = PathBuf::from("/tmp");
    let result = ensure_no_target_dir_passthrough_conflicts(
        &options,
        &base_dir,
        &["--target-dir".into(), "nested".into()],
    );

    assert!(result.is_ok());
}

#[test]
fn resolves_relative_config_path_against_explicit_base_directory() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let config_path = tmp.path().join("nested/config.toml");
    std::fs::create_dir_all(config_path.parent().expect("bug: parent should exist"))
        .expect("bug: config dir should be created");
    std::fs::write(&config_path, "[build]\ntarget-dir = \"nested\"\n")
        .expect("bug: config file should be written");

    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let result = ensure_no_target_dir_passthrough_conflicts(
        &options,
        tmp.path(),
        &["--config".into(), "nested/config.toml".into()],
    );

    assert!(matches!(result, Err(CliError::TargetDirConflict)));
}

#[test]
fn missing_config_file_does_not_error_or_conflict() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let result = ensure_no_target_dir_passthrough_conflicts(
        &options,
        tmp.path(),
        &["--config".into(), "missing.toml".into()],
    );

    assert!(result.is_ok());
}

#[test]
fn invalid_toml_config_file_does_not_error_or_conflict() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let config_path = tmp.path().join("invalid.toml");
    std::fs::write(&config_path, "[build\n target-dir = \"nested\"\n")
        .expect("bug: config file should be written");

    let options = ArtifactOptions::new(Some(PathBuf::from("out")));
    let result = ensure_no_target_dir_passthrough_conflicts(
        &options,
        tmp.path(),
        &["--config".into(), "invalid.toml".into()],
    );

    assert!(result.is_ok());
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
