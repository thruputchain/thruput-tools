use crate::CliError;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Artifact-related CLI options shared across build, test, deploy, and call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactOptions {
    target_dir: Option<PathBuf>,
}

/// Contract artifact runtime kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// Lowered RV32 artifact emitted as `.rv32a`.
    Rv32,
}

impl ArtifactKind {
    /// Return the stable wire spelling used in CLI JSON and messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rv32 => "rv32",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "rv32" => Some(Self::Rv32),
            _ => None,
        }
    }
}

impl Serialize for ArtifactKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ArtifactKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown artifact kind `{value}`, expected `{}`",
                Self::Rv32.as_str()
            ))
        })
    }
}

impl JsonSchema for ArtifactKind {
    fn schema_name() -> Cow<'static, str> {
        "ArtifactKind".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::ArtifactKind").into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "enum": [ArtifactKind::Rv32.as_str()],
            "description": "Contract artifact runtime kind."
        })
    }
}

/// Artifact path paired with the runtime kind inferred or selected by the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedArtifact {
    /// Runtime kind used to wrap artifact bytes for deploy.
    pub kind: ArtifactKind,
    /// Resolved artifact path on disk.
    pub path: PathBuf,
}

/// Auto-detected artifact and the Cargo package it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackageArtifact {
    /// Resolved contract artifact.
    pub artifact: ResolvedArtifact,
    /// Selected Cargo package name.
    pub package_name: String,
    /// Directory containing the selected package's manifest.
    pub package_root: PathBuf,
}

impl ArtifactOptions {
    /// Create artifact options from an optional target directory.
    pub fn new(target_dir: Option<PathBuf>) -> Self {
        Self { target_dir }
    }

    /// Create artifact options from the original invocation cwd.
    pub fn from_invocation_cwd(target_dir: Option<PathBuf>, invocation_cwd: &Path) -> Self {
        Self {
            target_dir: target_dir.map(|path| normalize_target_dir(path, invocation_cwd)),
        }
    }

    /// Apply artifact settings to a cargo command.
    pub fn apply_to_command(&self, command: &mut Command) {
        if let Some(target_dir) = &self.target_dir {
            command.env("CARGO_TARGET_DIR", target_dir);
        }
    }

    /// Return the configured target directory, if any.
    pub fn target_dir(&self) -> Option<&Path> {
        self.target_dir.as_deref()
    }
}

/// Return the raw Cargo-produced RV32 ELF output path for a package in the given target directory.
pub fn cargo_rv32_elf_output_path(target_dir: &Path, package_name: &str) -> PathBuf {
    target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join(crate::scaffold::to_snake(package_name))
}

/// Return the Groundhog-owned RV32 artifact output path for a package in the given target directory.
pub fn groundhog_rv32_output_path(target_dir: &Path, package_name: &str) -> PathBuf {
    target_dir
        .join("groundhog")
        .join(format!("{}.rv32a", crate::scaffold::to_snake(package_name)))
}

/// Return the Groundhog-owned RV32 build metadata sidecar path.
pub fn groundhog_rv32_metadata_output_path(target_dir: &Path, package_name: &str) -> PathBuf {
    target_dir.join("groundhog").join(format!(
        "{}.rv32.json",
        crate::scaffold::to_snake(package_name)
    ))
}

/// Infer artifact kind from a path extension.
pub fn infer_artifact_kind(path: &Path) -> Result<ArtifactKind, CliError> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rv32a") => Ok(ArtifactKind::Rv32),
        _ => Err(CliError::UnsupportedArtifactExtension(
            path.display().to_string(),
        )),
    }
}

/// Resolve a Groundhog-owned build artifact for an explicit or inferred package.
pub fn resolve_groundhog_artifact(
    cwd: &Path,
    artifacts: &ArtifactOptions,
    package: Option<&str>,
) -> Result<ResolvedPackageArtifact, CliError> {
    let metadata = crate::build::cargo_metadata_with_artifacts(artifacts)?;
    let pkg_name = crate::build::resolve_package(&metadata, package, cwd)?;
    let path = groundhog_rv32_output_path(&metadata.target_directory, pkg_name);

    if !path.try_exists()? {
        return Err(CliError::ArtifactNotFound {
            kind: ArtifactKind::Rv32.as_str(),
            path: path.display().to_string(),
        });
    }

    let package_root = metadata
        .packages
        .iter()
        .find(|candidate| candidate.name == pkg_name)
        .and_then(|candidate| candidate.manifest_path.parent())
        .map_or_else(|| metadata.workspace_root.clone(), Path::to_path_buf);

    Ok(ResolvedPackageArtifact {
        artifact: ResolvedArtifact {
            kind: ArtifactKind::Rv32,
            path,
        },
        package_name: pkg_name.to_owned(),
        package_root,
    })
}

/// Ensure passthrough cargo arguments do not override the CLI target directory.
pub fn ensure_no_target_dir_passthrough_conflicts(
    options: &ArtifactOptions,
    config_base_dir: &Path,
    passthrough_args: &[String],
) -> Result<(), CliError> {
    if options.target_dir.is_none() {
        return Ok(());
    }

    let mut index = 0;
    while index < passthrough_args.len() {
        let arg = &passthrough_args[index];

        if arg == "--target-dir" || arg.starts_with("--target-dir=") {
            return Err(CliError::TargetDirConflict);
        }

        if arg == "--config" {
            if let Some(path) = passthrough_args.get(index + 1)
                && config_arg_sets_target_dir(path, config_base_dir)
            {
                return Err(CliError::TargetDirConflict);
            }
            index += 2;
            continue;
        }

        if let Some(path) = arg.strip_prefix("--config=")
            && config_arg_sets_target_dir(path, config_base_dir)
        {
            return Err(CliError::TargetDirConflict);
        }

        index += 1;
    }

    Ok(())
}

fn config_arg_sets_target_dir(arg: &str, config_base_dir: &Path) -> bool {
    if arg.contains('=') {
        let (key, _) = arg.split_once('=').expect("bug: arg contains '='");
        return key.trim() == "build.target-dir";
    }

    let config_path = config_base_dir.join(arg);
    config_file_sets_target_dir(config_path.as_path())
}

fn config_file_sets_target_dir(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
        return false;
    };

    value
        .get("build")
        .and_then(|build| build.get("target-dir"))
        .is_some()
}

fn normalize_target_dir(target_dir: PathBuf, invocation_cwd: &Path) -> PathBuf {
    let absolute = if target_dir.is_absolute() {
        target_dir
    } else {
        invocation_cwd.join(target_dir)
    };

    normalize_path_components(&absolute)
}

fn normalize_path_components(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::{
        ArtifactKind, ArtifactOptions, cargo_rv32_elf_output_path,
        ensure_no_target_dir_passthrough_conflicts, groundhog_rv32_metadata_output_path,
        groundhog_rv32_output_path, infer_artifact_kind,
    };
    use crate::CliError;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

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
    fn groundhog_rv32_output_paths_use_flat_groundhog_layout() {
        let artifact =
            groundhog_rv32_output_path(PathBuf::from("/tmp/out").as_path(), "demo-contract-rv32");
        let metadata = groundhog_rv32_metadata_output_path(
            PathBuf::from("/tmp/out").as_path(),
            "demo-contract-rv32",
        );
        assert_eq!(
            artifact,
            PathBuf::from("/tmp/out/groundhog/demo_contract_rv32.rv32a")
        );
        assert_eq!(
            metadata,
            PathBuf::from("/tmp/out/groundhog/demo_contract_rv32.rv32.json")
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
        assert!(
            matches!(err, CliError::UnsupportedArtifactExtension(path) if path == "contract.bin")
        );
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
}
