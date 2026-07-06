//! Shared diagnostic checks for `init` and `status`.
//!
//! Every function here returns data — never modifies state.
//! Non-fatal problems (missing profile, dangling key reference)
//! are captured as [`Finding`] entries rather than hard errors.
//! The [`StatusReport`] aggregates all check results for
//! rendering by `status` or decision-making by `init`.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::CliError;

/// Severity of a non-fatal diagnostic finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingLevel {
    Warn,
    Info,
}

/// A non-fatal diagnostic finding.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub level: FindingLevel,
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<Cow<'static, str>>,
}

/// WASM target installation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WasmTargetStatus {
    Installed,
    Missing,
    Unknown,
}

/// Environment tool checks.
#[derive(Debug, Serialize)]
pub struct EnvironmentStatus {
    pub cargo: bool,
    pub wasm_target: WasmTargetStatus,
    pub wasm_opt: bool,
    pub registry_configured: bool,
    pub cargo_config_path: PathBuf,
}

pub use crate::keys::KeySummary;

/// Key inventory and global default selection.
#[derive(Debug, Serialize)]
pub struct KeyStatus {
    pub keys: Vec<KeySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_default_key: Option<String>,
}

/// Executor credential presence.
#[derive(Debug, Serialize)]
pub struct CredentialStatus {
    pub executor_credentials: bool,
}

/// Project-level context for the current working directory.
#[derive(Debug, Default, Serialize)]
pub struct ProjectContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub contract_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
}

/// Aggregated diagnostic report from all checks.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub environment: EnvironmentStatus,
    pub keys: KeyStatus,
    pub credentials: CredentialStatus,
    pub project: ProjectContext,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_default_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_default_key_source: Option<&'static str>,
}

impl StatusReport {
    /// Returns true when any warn-level finding exists.
    pub fn has_warnings(&self) -> bool {
        self.all_findings()
            .any(|finding| finding.level == FindingLevel::Warn)
    }

    /// Returns the selected default key when it exists on disk.
    pub fn effective_default_key_summary(&self) -> Option<&KeySummary> {
        let selected = self.effective_default_key.as_deref()?;
        self.keys.keys.iter().find(|key| key.name == selected)
    }

    /// Iterates every finding in the report, including
    /// project-level findings.
    pub fn all_findings(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().chain(self.project.findings.iter())
    }
}

// serde skip_serializing_if requires &T signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(val: &usize) -> bool {
    *val == 0
}

/// Check the local toolchain and registry configuration.
pub fn check_environment() -> Result<EnvironmentStatus, CliError> {
    let cargo = which::which("cargo").is_ok();
    let wasm_target = detect_wasm_target();
    let wasm_opt = which::which("wasm-opt").is_ok();
    let cargo_config_path = crate::auth::cargo_config_path()?;

    let registry_configured = match std::fs::read_to_string(&cargo_config_path) {
        Ok(contents) => contents.contains("[registries.groundhog]"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };

    Ok(EnvironmentStatus {
        cargo,
        wasm_target,
        wasm_opt,
        registry_configured,
        cargo_config_path,
    })
}

/// Check stored signing keys and global default-key selection.
pub fn check_keys() -> Result<KeyStatus, CliError> {
    Ok(KeyStatus {
        keys: crate::keys::load_key_summaries()?,
        global_default_key: crate::config::load_default_key()?,
    })
}

/// Check whether executor credentials are stored.
pub fn check_credentials() -> Result<CredentialStatus, CliError> {
    Ok(CredentialStatus {
        executor_credentials: crate::config::load_executor_credentials()?.is_some(),
    })
}

/// Check project context and convert known profile problems into findings.
pub fn check_project(
    start: &Path,
    explicit_profile: Option<&str>,
) -> Result<ProjectContext, CliError> {
    let config_path = crate::config::find_project_config(start);
    let Some(config_path) = config_path else {
        return Ok(ProjectContext::default());
    };

    let mut findings = Vec::new();

    match crate::config::resolve_profile(start, explicit_profile) {
        Ok(profile) => {
            let profile_key = profile.default_key.clone();
            if let Some(ref key_name) = profile_key {
                let key_path = crate::config::keys_dir()?.join(format!("{key_name}.json"));
                if !key_path.exists() {
                    findings.push(Finding {
                        level: FindingLevel::Warn,
                        code: "key_not_on_disk",
                        message: format!(
                            "profile `{}` references key `{key_name}` \
                             which does not exist",
                            profile.name
                        ),
                        hint: Some(Cow::Owned(format!(
                            "generate it with \
                             `groundhog keys generate {key_name}` \
                             or change the profile's default_key"
                        ))),
                    });
                }
            }

            Ok(ProjectContext {
                config_path: Some(config_path),
                profile_name: Some(profile.name),
                profile_key,
                rpc_url: profile.rpc_url,
                contract_count: profile.contracts.len(),
                findings,
            })
        }
        Err(CliError::ProfileNotFound(name)) => {
            findings.push(Finding {
                level: FindingLevel::Warn,
                code: "profile_not_found",
                message: format!("profile `{name}` not found in project or global config"),
                hint: Some(Cow::Borrowed(
                    "add the profile to project or global config, or \
                     use `--profile` to select an existing \
                     one",
                )),
            });
            Ok(ProjectContext {
                config_path: Some(config_path),
                profile_name: Some(name),
                findings,
                ..ProjectContext::default()
            })
        }
        Err(CliError::NoProjectConfig(name)) => {
            findings.push(Finding {
                level: FindingLevel::Warn,
                code: "no_project_config",
                message: format!(
                    "profile `{name}` requested but no project or global \
                     config was found"
                ),
                hint: Some(Cow::Borrowed(
                    "run `groundhog init`, create groundhog.toml, or use \
                     a profile that does not require config",
                )),
            });
            Ok(ProjectContext {
                profile_name: Some(name),
                findings,
                ..ProjectContext::default()
            })
        }
        Err(e) => Err(e),
    }
}

/// Run all checks and collect them into a single report.
pub fn collect_report(
    project_start: &Path,
    explicit_profile: Option<&str>,
) -> Result<StatusReport, CliError> {
    let environment = check_environment()?;
    let keys = check_keys()?;
    let credentials = check_credentials()?;
    let project = check_project(project_start, explicit_profile)?;
    let mut findings = Vec::new();

    if !environment.cargo {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "no_cargo",
            message: "cargo not found on PATH".into(),
            hint: Some(Cow::Borrowed(
                "install Rust and ensure `cargo` is on \
                 your PATH",
            )),
        });
    }

    if !environment.registry_configured {
        findings.push(Finding {
            level: FindingLevel::Info,
            code: "no_registry",
            message: "global Groundhog cargo registry not configured".into(),
            hint: Some(Cow::Borrowed(
                "new projects include project-local registry config; run `cargo login --registry groundhog` before building if needed; Cargo stores the token globally in CARGO_HOME",
            )),
        });
    }

    let (effective_default_key, effective_default_key_source) =
        resolve_effective_key(&project, &keys);

    add_key_findings(
        &keys,
        &project,
        effective_default_key.as_deref(),
        &mut findings,
    );

    if !credentials.executor_credentials {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "no_credentials",
            message: "executor credentials not found".into(),
            hint: Some(Cow::Borrowed("run `groundhog login` to store credentials")),
        });
    }

    Ok(StatusReport {
        environment,
        keys,
        credentials,
        project,
        findings,
        effective_default_key,
        effective_default_key_source,
    })
}

fn detect_wasm_target() -> WasmTargetStatus {
    let Ok(output) = std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
    else {
        return WasmTargetStatus::Unknown;
    };

    if !output.status.success() {
        return WasmTargetStatus::Unknown;
    }

    if String::from_utf8_lossy(&output.stdout).contains("wasm32-unknown-unknown") {
        WasmTargetStatus::Installed
    } else {
        WasmTargetStatus::Missing
    }
}

fn resolve_effective_key(
    project: &ProjectContext,
    keys: &KeyStatus,
) -> (Option<String>, Option<&'static str>) {
    if let Some(profile_key) = project.profile_key.clone() {
        (Some(profile_key), Some("profile"))
    } else if let Some(global_key) = keys.global_default_key.clone() {
        (Some(global_key), Some("global"))
    } else {
        (None, None)
    }
}

fn add_key_findings(
    keys: &KeyStatus,
    project: &ProjectContext,
    effective_default_key: Option<&str>,
    findings: &mut Vec<Finding>,
) {
    let has_profile_key_warning = project.findings.iter().any(|f| f.code == "key_not_on_disk");
    let effective_default_exists =
        effective_default_key_summary(&keys.keys, effective_default_key).is_some();

    if keys.keys.is_empty() {
        add_missing_keys_finding(findings);
        if let Some(selected) = effective_default_key
            && !effective_default_exists
            && !has_profile_key_warning
        {
            findings.push(missing_selected_key_finding(
                selected,
                "run `groundhog keys generate` to recreate \
                 it or `groundhog keys use <name>` to \
                 choose another key",
            ));
        }
    } else if effective_default_key.is_none() {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "no_default_key",
            message: "signing keys exist but no default is \
                      selected"
                .into(),
            hint: Some(Cow::Borrowed(
                "run `groundhog keys use <name>` to set a \
                 default",
            )),
        });
    } else if let Some(selected) = effective_default_key
        && !effective_default_exists
        && !has_profile_key_warning
    {
        findings.push(missing_selected_key_finding(
            selected,
            "run `groundhog keys ls` to inspect available \
             keys and `groundhog keys use <name>` to \
             pick one",
        ));
    }
}

fn effective_default_key_summary<'a>(
    keys: &'a [KeySummary],
    selected: Option<&str>,
) -> Option<&'a KeySummary> {
    let selected = selected?;
    keys.iter().find(|key| key.name == selected)
}

fn add_missing_keys_finding(findings: &mut Vec<Finding>) {
    findings.push(Finding {
        level: FindingLevel::Warn,
        code: "no_keys",
        message: "no signing keys found".into(),
        hint: Some(Cow::Borrowed("run `groundhog keys generate` to create one")),
    });
}

fn missing_selected_key_finding(selected: &str, hint: &'static str) -> Finding {
    Finding {
        level: FindingLevel::Warn,
        code: "default_key_not_on_disk",
        message: format!(
            "selected default key `{selected}` does not \
             exist on disk"
        ),
        hint: Some(Cow::Borrowed(hint)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn empty_report_has_no_warnings() {
        let report = StatusReport {
            environment: EnvironmentStatus {
                cargo: true,
                wasm_target: WasmTargetStatus::Installed,
                wasm_opt: true,
                registry_configured: true,
                cargo_config_path: PathBuf::from("/tmp"),
            },
            keys: KeyStatus {
                keys: Vec::new(),
                global_default_key: None,
            },
            credentials: CredentialStatus {
                executor_credentials: true,
            },
            project: ProjectContext::default(),
            findings: Vec::new(),
            effective_default_key: None,
            effective_default_key_source: None,
        };
        assert!(!report.has_warnings());
    }

    #[test]
    fn check_environment_detects_cargo() {
        let env = check_environment().unwrap();
        assert!(env.cargo);
    }

    #[test]
    #[serial]
    fn check_environment_detects_missing_registry_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cargo_home = tmp.path().join("cargo-home");

        unsafe {
            std::env::set_var("CARGO_HOME", &cargo_home);
        }

        let env = check_environment().unwrap();
        assert!(!env.registry_configured);

        unsafe {
            std::env::remove_var("CARGO_HOME");
        }
    }

    #[test]
    #[serial]
    fn check_keys_empty_when_no_keys_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let status = check_keys().unwrap();
        assert!(status.keys.is_empty());
        assert!(status.global_default_key.is_none());

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    #[serial]
    fn check_credentials_false_when_none_stored() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let status = check_credentials().unwrap();
        assert!(!status.executor_credentials);

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn check_project_returns_empty_when_no_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = check_project(tmp.path(), None).unwrap();
        assert!(context.config_path.is_none());
        assert!(context.findings.is_empty());
    }

    #[test]
    #[serial]
    fn check_project_finds_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path().join("config-home"));
        }
        let config = tmp.path().join("groundhog.toml");
        std::fs::write(
            &config,
            "[profile.default]\nrpc_url = \"http://localhost\"\n",
        )
        .unwrap();

        let context = check_project(tmp.path(), None).unwrap();
        assert_eq!(context.config_path, Some(config));
        assert_eq!(context.profile_name.as_deref(), Some("default"));
        assert_eq!(context.rpc_url.as_deref(), Some("http://localhost"));
        assert!(context.findings.is_empty());

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn check_project_missing_profile_is_finding() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("groundhog.toml"), "[profile.default]\n").unwrap();

        let context = check_project(tmp.path(), Some("staging")).unwrap();
        assert_eq!(
            context.config_path.as_ref().map(|path| path.exists()),
            Some(true)
        );
        assert_eq!(context.findings.len(), 1);
        assert_eq!(context.findings[0].code, "profile_not_found");
        assert_eq!(context.findings[0].level, FindingLevel::Warn);
    }

    #[test]
    #[serial]
    fn check_project_dangling_key_ref_is_finding() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let project = tmp.path().join("proj");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(
            project.join("groundhog.toml"),
            "[profile.default]\ndefault_key = \"ghost\"\n",
        )
        .unwrap();

        let context = check_project(&project, None).unwrap();
        assert_eq!(context.profile_key.as_deref(), Some("ghost"));
        assert_eq!(context.findings.len(), 1);
        assert_eq!(context.findings[0].code, "key_not_on_disk");

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    #[serial]
    fn collect_report_warns_on_missing_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("CARGO_HOME", tmp.path().join("cargo-home"));
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let report = collect_report(tmp.path(), None).unwrap();
        assert!(!report.credentials.executor_credentials);
        assert!(report.has_warnings());
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "no_credentials")
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "no_keys")
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "no_registry"
                    && finding.level == FindingLevel::Info)
        );

        unsafe {
            std::env::remove_var("CARGO_HOME");
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn has_warnings_returns_true_for_warn_finding() {
        let report = StatusReport {
            environment: EnvironmentStatus {
                cargo: true,
                wasm_target: WasmTargetStatus::Installed,
                wasm_opt: true,
                registry_configured: true,
                cargo_config_path: PathBuf::from("/tmp"),
            },
            keys: KeyStatus {
                keys: Vec::new(),
                global_default_key: None,
            },
            credentials: CredentialStatus {
                executor_credentials: true,
            },
            project: ProjectContext::default(),
            findings: vec![Finding {
                level: FindingLevel::Warn,
                code: "test_warning",
                message: "test".into(),
                hint: None,
            }],
            effective_default_key: None,
            effective_default_key_source: None,
        };
        assert!(report.has_warnings());
    }

    #[test]
    #[serial]
    fn collect_report_warns_no_default_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("CARGO_HOME", tmp.path().join("cargo-home"));
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        // Generate a key, then clear the default selection.
        crate::keys::generate(
            &Some("orphan".into()),
            false,
            "keys.generate",
            &crate::Print::quiet(),
        )
        .unwrap();
        crate::config::clear_default_key().unwrap();

        let report = collect_report(tmp.path(), None).unwrap();
        assert!(report.findings.iter().any(|f| f.code == "no_default_key"));

        unsafe {
            std::env::remove_var("CARGO_HOME");
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn check_project_no_config_with_explicit_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No groundhog.toml, but explicit non-default profile.
        let context = check_project(tmp.path(), Some("staging")).unwrap();
        assert!(context.config_path.is_none());
        // No groundhog.toml means find_project_config returns
        // None, so we get an empty ProjectContext (not a
        // finding) because resolve_profile is never called.
        assert!(context.findings.is_empty());
    }
}
