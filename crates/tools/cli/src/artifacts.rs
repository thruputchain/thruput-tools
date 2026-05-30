use crate::CliError;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Artifact-related CLI options shared across build, test, deploy, and call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactOptions {
    target_dir: Option<PathBuf>,
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

/// Return the raw Cargo-produced WASM output path for a package in the given target directory.
pub fn cargo_wasm_output_path(target_dir: &Path, package_name: &str) -> PathBuf {
    target_dir
        .join("wasm32-unknown-unknown/release")
        .join(format!("{}.wasm", crate::scaffold::to_snake(package_name)))
}

/// Return the raw Cargo-produced RV32 ELF output path for a package in the given target directory.
pub fn cargo_rv32_elf_output_path(target_dir: &Path, package_name: &str) -> PathBuf {
    target_dir
        .join("riscv32im-unknown-none-elf/release")
        .join(package_name)
}

/// Return the Groundhog-owned processed WASM output path for a package in the given target directory.
pub fn groundhog_wasm_output_path(target_dir: &Path, package_name: &str) -> PathBuf {
    target_dir
        .join("groundhog")
        .join(format!("{}.wasm", crate::scaffold::to_snake(package_name)))
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
        ArtifactOptions, cargo_rv32_elf_output_path, cargo_wasm_output_path,
        ensure_no_target_dir_passthrough_conflicts, groundhog_rv32_metadata_output_path,
        groundhog_rv32_output_path, groundhog_wasm_output_path,
    };
    use crate::CliError;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn cargo_wasm_output_path_uses_cargo_layout() {
        let path = cargo_wasm_output_path(PathBuf::from("/tmp/out").as_path(), "demo-contract");
        assert_eq!(
            path,
            PathBuf::from("/tmp/out/wasm32-unknown-unknown/release/demo_contract.wasm")
        );
    }

    #[test]
    fn cargo_rv32_elf_output_path_uses_cargo_layout() {
        let path =
            cargo_rv32_elf_output_path(PathBuf::from("/tmp/out").as_path(), "demo-contract-rv32");
        assert_eq!(
            path,
            PathBuf::from("/tmp/out/riscv32im-unknown-none-elf/release/demo-contract-rv32")
        );
    }

    #[test]
    fn groundhog_wasm_output_path_uses_flat_groundhog_layout() {
        let path = groundhog_wasm_output_path(PathBuf::from("/tmp/out").as_path(), "demo-contract");
        assert_eq!(path, PathBuf::from("/tmp/out/groundhog/demo_contract.wasm"));
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
