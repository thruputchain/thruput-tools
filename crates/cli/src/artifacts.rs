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

    /// Apply artifact settings to a cargo command.
    pub fn apply_to_command(&self, command: &mut Command) {
        if let Some(target_dir) = &self.target_dir {
            command.env("CARGO_TARGET_DIR", target_dir);
        }
    }
}

/// Return the expected WASM output path for a package in the given target directory.
pub fn wasm_output_path(target_dir: &Path, package_name: &str) -> PathBuf {
    target_dir
        .join("wasm32-unknown-unknown/release")
        .join(format!("{}.wasm", crate::scaffold::to_snake(package_name)))
}

/// Ensure passthrough cargo arguments do not override the CLI target directory.
pub fn ensure_no_target_dir_passthrough_conflicts(
    options: &ArtifactOptions,
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
            if let Some(path) = passthrough_args.get(index + 1) {
                if config_file_sets_target_dir(Path::new(path))? {
                    return Err(CliError::TargetDirConflict);
                }
            }
            index += 2;
            continue;
        }

        if let Some(path) = arg.strip_prefix("--config=") {
            if config_file_sets_target_dir(Path::new(path))? {
                return Err(CliError::TargetDirConflict);
            }
        }

        index += 1;
    }

    Ok(())
}

fn config_file_sets_target_dir(path: &Path) -> Result<bool, CliError> {
    let contents = fs::read_to_string(path)?;
    let value: toml::Value =
        toml::from_str(&contents).map_err(|err| CliError::Other(err.to_string()))?;

    Ok(value
        .get("build")
        .and_then(|build| build.get("target-dir"))
        .is_some())
}

#[cfg(test)]
mod tests {
    use super::{ArtifactOptions, ensure_no_target_dir_passthrough_conflicts, wasm_output_path};
    use crate::CliError;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn wasm_output_path_uses_custom_target_directory() {
        let path = wasm_output_path(PathBuf::from("/tmp/out").as_path(), "demo-contract");
        assert_eq!(
            path,
            PathBuf::from("/tmp/out/wasm32-unknown-unknown/release/demo_contract.wasm")
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
    fn detects_cli_target_dir_conflict_in_passthrough_args() {
        let options = ArtifactOptions::new(Some(PathBuf::from("out")));
        let err = ensure_no_target_dir_passthrough_conflicts(
            &options,
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
            &["--config".into(), config_path.display().to_string()],
        )
        .unwrap_err();

        assert!(matches!(err, CliError::TargetDirConflict));
    }
}
