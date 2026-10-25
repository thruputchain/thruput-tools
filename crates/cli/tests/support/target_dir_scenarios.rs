//! Target-dir passthrough scenarios shared by `thruput build` and `thruput
//! test`: both commands read cargo metadata, then refuse passthrough args that
//! would move the target dir away from the CLI-selected one.

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use thruput_cli::CliError;
use thruput_cli::artifacts::ArtifactOptions;

/// The error a scenario must produce.
#[derive(Debug, Clone, Copy)]
pub enum Expected {
    TargetDirConflict,
    CargoMetadataFailed,
}

#[derive(Debug)]
pub struct Scenario {
    pub name: &'static str,
    /// Passthrough cargo args handed to the command.
    pub passthrough: &'static [&'static str],
    /// Directory, relative to the project root, the command runs from.
    pub cwd: &'static str,
    /// Whether the fake `cargo metadata` fails.
    pub metadata_fails: bool,
    pub expected: Expected,
}

pub const SCENARIOS: [Scenario; 3] = [
    Scenario {
        name: "rejects conflicting --target-dir passthrough",
        passthrough: &["--target-dir", "nested"],
        cwd: "",
        metadata_fails: false,
        expected: Expected::TargetDirConflict,
    },
    Scenario {
        name: "prefers metadata failure over redundant target-dir precheck",
        passthrough: &["--target-dir", "nested"],
        cwd: "",
        metadata_fails: true,
        expected: Expected::CargoMetadataFailed,
    },
    Scenario {
        name: "resolves relative --config conflicts from the workspace root",
        passthrough: &["--config", "target-config.toml"],
        cwd: "src/bin",
        metadata_fails: false,
        expected: Expected::TargetDirConflict,
    },
];

/// A single-package `demo-contract` project with a fake `cargo` on `fake_bin`.
#[derive(Debug)]
pub struct Project {
    /// Keeps the project directory alive for the scenario.
    pub _tmp: TempDir,
    pub fake_bin: PathBuf,
    pub cwd: PathBuf,
    pub artifacts: ArtifactOptions,
    pub passthrough: Vec<String>,
}

impl Scenario {
    pub fn setup(&self) -> Project {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo-contract\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(root.join(".cargo/contract.toml"), "").unwrap();
        fs::write(
            root.join("target-config.toml"),
            "[build]\ntarget-dir = \"nested\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("src/bin")).unwrap();

        let fake_bin = root.join("bin");
        fs::create_dir_all(&fake_bin).unwrap();
        let cargo = fake_bin.join("cargo");
        let script = if self.metadata_fails {
            "#!/bin/sh\nif [ \"$1\" = \"metadata\" ]; then\n  printf '%s\n' 'synthetic metadata failure' >&2\n  exit 1\nfi\nexit 0\n"
                .to_owned()
        } else {
            let metadata_json = format!(
                r#"{{"workspace_root":"{}","target_directory":"{}","packages":[{{"name":"demo-contract","version":"0.1.0","manifest_path":"{}"}}]}}"#,
                root.display(),
                root.join("target").display(),
                root.join("Cargo.toml").display()
            );
            format!("#!/bin/sh\nprintf '%s' '{metadata_json}'\n")
        };
        fs::write(&cargo, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&cargo).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&cargo, perms).unwrap();
        }

        Project {
            fake_bin,
            cwd: root.join(self.cwd),
            artifacts: ArtifactOptions::new(Some(root.join("explicit-target"))),
            passthrough: self.passthrough.iter().map(|&arg| arg.to_owned()).collect(),
            _tmp: tmp,
        }
    }

    pub fn assert_outcome<T: std::fmt::Debug>(&self, result: Result<T, CliError>) {
        let err = result.expect_err(self.name);
        let matched = match self.expected {
            Expected::TargetDirConflict => matches!(err, CliError::TargetDirConflict),
            Expected::CargoMetadataFailed => matches!(err, CliError::CargoMetadataFailed(_)),
        };
        assert!(
            matched,
            "{}: expected {:?}, got: {err:?}",
            self.name, self.expected
        );
    }
}
