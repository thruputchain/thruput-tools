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
use crate::terminal::render_shell_arg;

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

/// Rust compilation target installation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RustTargetStatus {
    Installed,
    Missing,
    Unknown,
}

/// Active Rust toolchain and required component status.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustToolchainStatus {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub rust_src: RustTargetStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Resolved local devnode binary and version probe result.
#[derive(Debug, Serialize)]
pub struct DevnodeStatus {
    pub reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Resolved and validated dev-wallet runtime artifact.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevWalletArtifactStatus {
    pub available: bool,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Environment tool checks.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentStatus {
    pub cargo: bool,
    pub rust_toolchain: RustToolchainStatus,
    pub rv32_target: RustTargetStatus,
    pub devnode: DevnodeStatus,
    pub dev_wallet_artifact: DevWalletArtifactStatus,
    pub registry_configured: bool,
    pub cargo_config_path: PathBuf,
}

pub use crate::keys::KeySummary;

/// Key inventory and global default selection.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub keys: Vec<KeySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_default_key: Option<String>,
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
    pub project_default_wallet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_default_wallet: Option<String>,
    /// Transaction format of the explicitly selected wallet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_wallet_kind: Option<crate::config::WalletKind>,
    /// Derivation nonce when the selected wallet is native.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_wallet_account_nonce: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_wallet_source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<crate::config::Network>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub contract_count: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub wallet_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
}

/// Aggregated diagnostic report from all checks.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub environment: EnvironmentStatus,
    pub keys: KeyStatus,
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
pub fn check_environment(project_start: &Path) -> Result<EnvironmentStatus, CliError> {
    let cargo = which::which("cargo").is_ok();
    let (rust_toolchain, rv32_target) = detect_rust_environment(project_start);
    let devnode = check_devnode();
    let dev_wallet_artifact = check_dev_wallet_artifact(&devnode);
    let cargo_config_path = crate::config::cargo_config_path()?;

    let registry_configured = match std::fs::read_to_string(&cargo_config_path) {
        Ok(contents) => contents.contains("[registries.groundhog]"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };

    Ok(EnvironmentStatus {
        cargo,
        rust_toolchain,
        rv32_target,
        devnode,
        dev_wallet_artifact,
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

/// Check project context and convert known profile problems into findings.
pub fn check_project(
    start: &Path,
    explicit_profile: Option<&str>,
) -> Result<ProjectContext, CliError> {
    let config_path = crate::config::find_project_config(start);
    let mut findings = Vec::new();

    match crate::config::resolve_profile(start, explicit_profile) {
        Ok(profile) => {
            let profile_key = profile.default_key.clone();
            let project_default_wallet = crate::config::load_project_profiles(start)?
                .and_then(|profiles| profiles.get(&profile.name).cloned())
                .and_then(|profile| profile.default_wallet);
            let default_wallet_source = profile.default_wallet.as_ref().map(|_| {
                if project_default_wallet.is_some() {
                    "project"
                } else {
                    "global"
                }
            });
            if let Some(ref key_name) = profile_key
                && !crate::dev_keys::is_dev_key(key_name)
            {
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
                             `groundhog keys generate -- {}` \
                             or change the profile's default_key",
                            render_shell_arg(key_name),
                        ))),
                    });
                }
            }
            check_default_wallet(&profile, &mut findings);

            Ok(ProjectContext {
                config_path,
                profile_name: Some(profile.name),
                profile_key,
                project_default_wallet,
                default_wallet_kind: profile
                    .default_wallet
                    .as_ref()
                    .and_then(|name| profile.wallets.get(name))
                    .map(|wallet| wallet.kind),
                default_wallet_account_nonce: profile
                    .default_wallet
                    .as_ref()
                    .and_then(|name| profile.wallets.get(name))
                    .and_then(|wallet| wallet.account_nonce),
                effective_default_wallet: profile.default_wallet,
                default_wallet_source,
                network: profile.network,
                rpc_url: profile.rpc_url,
                contract_count: profile.contracts.len(),
                wallet_count: profile.wallets.len(),
                findings,
            })
        }
        Err(CliError::ProfileNotFound(name)) => Ok(unresolved_profile_context(
            config_path,
            Some(name.clone()),
            Finding {
                level: FindingLevel::Warn,
                code: "profile_not_found",
                message: format!("profile `{name}` not found in project or global config"),
                hint: Some(Cow::Borrowed(
                    "add the profile to project or global config, or use `--profile` to select an existing one",
                )),
            },
        )),
        Err(CliError::NoActiveProfile) => Ok(unresolved_profile_context(
            config_path,
            None,
            Finding {
                level: FindingLevel::Warn,
                code: "no_active_profile",
                message: "no active profile is selected".into(),
                hint: Some(Cow::Borrowed(
                    "run `groundhog init`, pass `--profile`, or set default_profile in project or global config",
                )),
            },
        )),
        Err(CliError::ProfileConfigInvalid { profile, message }) => Ok(unresolved_profile_context(
            config_path,
            Some(profile.clone()),
            Finding {
                level: FindingLevel::Warn,
                code: "profile_config_invalid",
                message: format!("profile `{profile}` is invalid: {message}"),
                hint: Some(Cow::Borrowed(
                    "fix the selected profile or use `--profile` to select a valid one",
                )),
            },
        )),
        Err(e) => Err(e),
    }
}

fn unresolved_profile_context(
    config_path: Option<PathBuf>,
    profile_name: Option<String>,
    finding: Finding,
) -> ProjectContext {
    ProjectContext {
        config_path,
        profile_name,
        findings: vec![finding],
        ..ProjectContext::default()
    }
}

fn check_default_wallet(profile: &crate::config::ResolvedProfile, findings: &mut Vec<Finding>) {
    let Some(wallet_name) = profile.default_wallet.as_deref() else {
        return;
    };
    let key_name = crate::config::resolve_default_key_for_profile(profile)
        .ok()
        .flatten();
    let uses_dev_key = key_name
        .as_deref()
        .and_then(crate::dev_keys::dev_key_index)
        .is_some();
    if uses_dev_key {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "default_wallet_with_dev_key",
            message: format!(
                "profile `{}` selects wallet `{wallet_name}` for manifest-backed key `{}`",
                profile.name,
                key_name.as_deref().expect("bug: dev key must be present")
            ),
            hint: Some(Cow::Borrowed(
                "remove default_wallet; manifest-backed dev keys use their fixed wallet",
            )),
        });
        return;
    }
    let Some(wallet) = profile.wallets.get(wallet_name) else {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "default_wallet_not_saved",
            message: format!(
                "profile `{}` selects wallet `{wallet_name}`, which is not saved",
                profile.name
            ),
            hint: Some(Cow::Owned(format!(
                "run `groundhog wallet use <NAME> --profile={}` to select a saved wallet",
                render_shell_arg(&profile.name),
            ))),
        });
        return;
    };
    let validated =
        match crate::account_resolution::validate_saved_wallet_binding(wallet_name, wallet) {
            Ok(validated) => validated,
            Err(CliError::SavedWalletOwnerPublicKeyInvalid { .. }) => {
                findings.push(Finding {
                    level: FindingLevel::Warn,
                    code: "default_wallet_owner_invalid",
                    message: format!(
                        "wallet `{wallet_name}` has malformed owner_public_key metadata"
                    ),
                    hint: Some(Cow::Borrowed(
                        "remove and recreate the wallet binding with valid owner metadata",
                    )),
                });
                return;
            }
            Err(error) => {
                findings.push(Finding {
                    level: FindingLevel::Warn,
                    code: "default_wallet_address_invalid",
                    message: format!("wallet `{wallet_name}` has a malformed address: {error}"),
                    hint: Some(Cow::Borrowed(
                        "remove and recreate the wallet binding with a valid address",
                    )),
                });
                return;
            }
        };
    let Some(key_name) = key_name else {
        return;
    };
    let Ok(signer) = crate::keys::load_local_signer(&key_name) else {
        return;
    };
    if validated.owner_public_key.as_slice() != signer.verifying_key_bytes().as_slice() {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "default_wallet_owner_mismatch",
            message: format!("wallet `{wallet_name}` is not owned by effective key `{key_name}`"),
            hint: Some(Cow::Owned(format!(
                "select a wallet owned by `{key_name}` or change the profile's default_key"
            ))),
        });
    }
}

/// Run all checks and collect them into a single report.
pub fn collect_report(
    project_start: &Path,
    explicit_profile: Option<&str>,
) -> Result<StatusReport, CliError> {
    let environment = check_environment(project_start)?;
    let keys = check_keys()?;
    let project = check_project(project_start, explicit_profile)?;
    let mut findings = Vec::new();

    add_environment_findings(&environment, &project, &mut findings);

    let (effective_default_key, effective_default_key_source) =
        resolve_effective_key(project_start, explicit_profile, &project, &keys)?;

    add_key_findings(
        &keys,
        &project,
        effective_default_key.as_deref(),
        &mut findings,
    );

    Ok(StatusReport {
        environment,
        keys,
        project,
        findings,
        effective_default_key,
        effective_default_key_source,
    })
}

fn add_environment_findings(
    environment: &EnvironmentStatus,
    project: &ProjectContext,
    findings: &mut Vec<Finding>,
) {
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

    add_rust_toolchain_findings(environment, project, findings);

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

    if !environment.devnode.reachable {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "devnode_unavailable",
            message: environment
                .devnode
                .error
                .clone()
                .unwrap_or_else(|| "hog-devnode binary is unavailable".into()),
            hint: Some(Cow::Borrowed(
                "install groundhog-client or set GROUNDHOG_DEVNODE_BINARY to an executable hog-devnode; fix or unset the variable if it points to an old path",
            )),
        });
    } else if !environment.dev_wallet_artifact.valid {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: if environment.dev_wallet_artifact.available {
                "dev_wallet_artifact_invalid"
            } else {
                "dev_wallet_artifact_unavailable"
            },
            message: environment
                .dev_wallet_artifact
                .error
                .clone()
                .unwrap_or_else(|| "dev_wallet.rv32a is unavailable".into()),
            hint: Some(Cow::Borrowed(
                "install the dev-wallet artifact from the same groundhog-client build as hog-devnode",
            )),
        });
    }
}

fn add_rust_toolchain_findings(
    environment: &EnvironmentStatus,
    project: &ProjectContext,
    findings: &mut Vec<Finding>,
) {
    if !environment.rust_toolchain.available {
        let hint = environment.rust_toolchain.name.as_deref().map_or_else(
            || Cow::Borrowed("install rustup and the Rust toolchain declared by the project"),
            |toolchain| {
                Cow::Owned(format!(
                    "run `rustup toolchain install --component rust-src --target {} -- {}`",
                    crate::build::RV32_TARGET_TRIPLE,
                    render_shell_arg(toolchain),
                ))
            },
        );
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "rust_toolchain_unavailable",
            message: environment
                .rust_toolchain
                .error
                .clone()
                .unwrap_or_else(|| "active Rust toolchain is unavailable".into()),
            hint: Some(hint),
        });
    }
    if environment.rust_toolchain.available
        && project.config_path.is_some()
        && (environment.rust_toolchain.rust_src == RustTargetStatus::Unknown
            || environment.rv32_target == RustTargetStatus::Unknown)
    {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "rust_toolchain_probe_failed",
            message: environment
                .rust_toolchain
                .error
                .clone()
                .unwrap_or_else(|| "could not verify the active project toolchain".into()),
            hint: Some(Cow::Borrowed(
                "run `rustup toolchain list` and inspect the project's rust-toolchain.toml",
            )),
        });
    }
    if environment.rust_toolchain.available
        && project.config_path.is_some()
        && environment.rust_toolchain.rust_src == RustTargetStatus::Missing
    {
        findings.push(Finding {
            level: FindingLevel::Warn,
            code: "rust_src_unavailable",
            message: "rust-src is not installed for the active project toolchain".into(),
            hint: Some(Cow::Borrowed(
                "run `rustup component add rust-src` from the project directory",
            )),
        });
    }
}

fn detect_rust_environment(project_start: &Path) -> (RustToolchainStatus, RustTargetStatus) {
    let (installed, default) = match installed_rust_toolchains() {
        Ok(toolchains) => toolchains,
        Err(error) => {
            return unavailable_rust_environment(None, error.to_string());
        }
    };
    let requested = match requested_rust_toolchain(project_start, &installed, default.as_deref()) {
        Ok(Some(toolchain)) => toolchain,
        Ok(None) => {
            return unavailable_rust_environment(
                None,
                "rustup has no default or project toolchain".into(),
            );
        }
        Err(error) => return unavailable_rust_environment(None, error.to_string()),
    };
    let Some(name) = matching_installed_toolchain(&requested, &installed) else {
        return (
            RustToolchainStatus {
                available: false,
                name: Some(requested.clone()),
                rust_src: RustTargetStatus::Unknown,
                error: Some(format!("Rust toolchain `{requested}` is not installed")),
            },
            RustTargetStatus::Unknown,
        );
    };

    let targets = rustup_installed_list(project_start, "target", &name);
    let components = rustup_installed_list(project_start, "component", &name);
    let rv32_target = targets
        .as_deref()
        .map_or(RustTargetStatus::Unknown, |installed| {
            rust_target_status(installed, crate::build::RV32_TARGET_TRIPLE)
        });
    let rust_src = components
        .as_deref()
        .map_or(RustTargetStatus::Unknown, |installed| {
            rust_target_status(installed, "rust-src")
        });
    let probe_errors = [targets.err(), components.err()]
        .into_iter()
        .flatten()
        .map(|error| error.to_string())
        .collect::<Vec<_>>();

    (
        RustToolchainStatus {
            available: true,
            name: Some(name.clone()),
            rust_src,
            error: (!probe_errors.is_empty()).then(|| probe_errors.join("; ")),
        },
        rv32_target,
    )
}

fn unavailable_rust_environment(
    name: Option<String>,
    error: String,
) -> (RustToolchainStatus, RustTargetStatus) {
    (
        RustToolchainStatus {
            available: false,
            name,
            rust_src: RustTargetStatus::Unknown,
            error: Some(error),
        },
        RustTargetStatus::Unknown,
    )
}

/// Toolchain name used only to probe whether `cargo` obeys rustup overrides.
///
/// Never installed: rustup rejects the override and exits before any network
/// access, which is what makes the probe cheap.
const RUSTUP_OVERRIDE_PROBE: &str = "groundhog-rustup-probe-not-a-toolchain";

/// Reports whether the `cargo` that a build would invoke is rustup-managed,
/// and so honors a project's `rust-toolchain.toml` pin, targets, and
/// components.
///
/// Probes behavior rather than presence, because rustup being installed is
/// not sufficient: a distro-packaged `cargo` earlier on `PATH` than rustup's
/// shim ignores `rust-toolchain.toml` even on a machine that has rustup.
/// Rustup's shim rejects an uninstalled `RUSTUP_TOOLCHAIN` override and exits
/// non-zero; any other `cargo` ignores the variable and prints its version.
///
/// Reports `true` unless `cargo` positively demonstrates that it ignored the
/// override, so an unusual environment stays quiet rather than raising a false
/// alarm.
pub(crate) fn cargo_is_rustup_managed() -> bool {
    let probe = std::process::Command::new("cargo")
        .arg("--version")
        .current_dir(rustup_neutral_directory(&std::env::temp_dir()))
        .env("RUSTUP_TOOLCHAIN", RUSTUP_OVERRIDE_PROBE)
        .output();
    match probe {
        Ok(output) => {
            let ignored_override = output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .trim_start()
                    .starts_with("cargo ");
            !ignored_override
        }
        // No `cargo` at all is a separate, already-reported problem.
        Err(_) => true,
    }
}

/// Why the `rustup` probe could not report the toolchain layout.
#[derive(Debug, thiserror::Error)]
enum RustToolchainProbeError {
    /// `rustup` itself could not be run.
    #[error("could not run rustup: {source}")]
    Rustup {
        #[source]
        source: std::io::Error,
    },
    /// `rustup <kind> list` could not be run.
    #[error("could not run rustup {kind} list: {source}")]
    RustupList {
        /// `target` or `component`.
        kind: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// `rustup` ran and refused to list the installed toolchains.
    #[error("could not list installed Rust toolchains: {detail}")]
    ListToolchains {
        /// What `rustup` reported on stderr.
        detail: String,
    },
    /// `rustup` ran and refused to list one toolchain's targets or components.
    #[error("could not list installed {kind}s for `{toolchain}`: {detail}")]
    ListForToolchain {
        /// `target` or `component`.
        kind: &'static str,
        /// The toolchain being inspected.
        toolchain: String,
        /// What `rustup` reported on stderr.
        detail: String,
    },
    /// A toolchain pinning file could not be read.
    #[error("could not read {path}: {source}")]
    ReadPin {
        /// The pinning file.
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// A `rust-toolchain.toml` is not valid TOML.
    #[error("could not parse {path}: {source}")]
    ParsePin {
        /// The pinning file.
        path: String,
        #[source]
        source: toml::de::Error,
    },
    /// A `rust-toolchain.toml` parses but names no channel.
    #[error("{path} does not declare toolchain.channel")]
    PinMissingChannel {
        /// The pinning file.
        path: String,
    },
    /// A legacy `rust-toolchain` file holds no channel.
    #[error("{path} is empty")]
    PinEmpty {
        /// The pinning file.
        path: String,
    },
}

fn installed_rust_toolchains() -> Result<(Vec<String>, Option<String>), RustToolchainProbeError> {
    let output = installed_rust_toolchains_command()
        .output()
        .map_err(|source| RustToolchainProbeError::Rustup { source })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(RustToolchainProbeError::ListToolchains {
            detail: detail.trim().to_owned(),
        });
    }

    Ok(parse_installed_rust_toolchains(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_installed_rust_toolchains(output: &str) -> (Vec<String>, Option<String>) {
    let mut installed = Vec::new();
    let mut default = None;
    for line in output.lines() {
        let Some(name) = line.split_whitespace().next() else {
            continue;
        };
        if name == "no" && line.trim() == "no installed toolchains" {
            continue;
        }
        installed.push(name.to_string());
        let is_default = line
            .rsplit_once(" (")
            .and_then(|(_, annotations)| annotations.strip_suffix(')'))
            .is_some_and(|annotations| {
                annotations
                    .split(',')
                    .any(|annotation| annotation.trim() == "default")
            });
        if is_default {
            default = Some(name.to_string());
        }
    }
    (installed, default)
}

fn installed_rust_toolchains_command() -> std::process::Command {
    let mut command = std::process::Command::new("rustup");
    let neutral_directory = rustup_neutral_directory(&std::env::temp_dir());
    command
        .args(["toolchain", "list"])
        .env_remove("RUSTUP_TOOLCHAIN")
        .current_dir(neutral_directory);
    command
}

fn rustup_neutral_directory(path: &Path) -> PathBuf {
    path.ancestors().last().unwrap_or(path).to_path_buf()
}

fn requested_rust_toolchain(
    project_start: &Path,
    installed: &[String],
    default: Option<&str>,
) -> Result<Option<String>, RustToolchainProbeError> {
    if let Some(toolchain) = std::env::var_os("RUSTUP_TOOLCHAIN")
        && !toolchain.is_empty()
    {
        return Ok(Some(toolchain.to_string_lossy().into_owned()));
    }
    if let Some(toolchain) = rustup_directory_override(project_start, installed) {
        return Ok(Some(toolchain));
    }
    if let Some(toolchain) = rust_toolchain_file(project_start)? {
        return Ok(Some(toolchain));
    }
    Ok(default.map(str::to_string))
}

fn rustup_directory_override(project_start: &Path, installed: &[String]) -> Option<String> {
    let output = std::process::Command::new("rustup")
        .args(["override", "list"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let project_start =
        std::fs::canonicalize(project_start).unwrap_or_else(|_| project_start.to_path_buf());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let toolchain = installed
                .iter()
                .find(|toolchain| line.ends_with(toolchain.as_str()))?;
            let directory = line.strip_suffix(toolchain.as_str())?.trim_end();
            let directory = std::fs::canonicalize(directory).ok()?;
            project_start
                .starts_with(&directory)
                .then(|| (directory.components().count(), toolchain.clone()))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, toolchain)| toolchain)
}

fn rust_toolchain_file(project_start: &Path) -> Result<Option<String>, RustToolchainProbeError> {
    for directory in project_start.ancestors() {
        let toml_path = directory.join("rust-toolchain.toml");
        if toml_path.is_file() {
            let contents = std::fs::read_to_string(&toml_path).map_err(|source| {
                RustToolchainProbeError::ReadPin {
                    path: toml_path.display().to_string(),
                    source,
                }
            })?;
            let document: toml::Value =
                toml::from_str(&contents).map_err(|source| RustToolchainProbeError::ParsePin {
                    path: toml_path.display().to_string(),
                    source,
                })?;
            let channel = document
                .get("toolchain")
                .and_then(|toolchain| toolchain.get("channel"))
                .and_then(toml::Value::as_str)
                .filter(|channel| !channel.is_empty())
                .ok_or_else(|| RustToolchainProbeError::PinMissingChannel {
                    path: toml_path.display().to_string(),
                })?;
            return Ok(Some(channel.to_string()));
        }

        let legacy_path = directory.join("rust-toolchain");
        if legacy_path.is_file() {
            let contents = std::fs::read_to_string(&legacy_path).map_err(|source| {
                RustToolchainProbeError::ReadPin {
                    path: legacy_path.display().to_string(),
                    source,
                }
            })?;
            let channel = contents.lines().next().unwrap_or_default().trim();
            if channel.is_empty() {
                return Err(RustToolchainProbeError::PinEmpty {
                    path: legacy_path.display().to_string(),
                });
            }
            return Ok(Some(channel.to_string()));
        }
    }
    Ok(None)
}

fn matching_installed_toolchain(requested: &str, installed: &[String]) -> Option<String> {
    installed
        .iter()
        .find(|toolchain| toolchain.as_str() == requested)
        .or_else(|| {
            installed.iter().find(|toolchain| {
                toolchain
                    .strip_prefix(requested)
                    .and_then(|suffix| suffix.strip_prefix('-'))
                    .is_some_and(looks_like_host_triple)
            })
        })
        .cloned()
}

fn looks_like_host_triple(suffix: &str) -> bool {
    let architecture = suffix.split('-').next().unwrap_or_default();
    matches!(
        architecture,
        "aarch64"
            | "arm"
            | "arm64ec"
            | "armv7"
            | "i586"
            | "i686"
            | "loongarch64"
            | "mips"
            | "mips64"
            | "mips64el"
            | "mipsel"
            | "powerpc"
            | "powerpc64"
            | "powerpc64le"
            | "riscv64gc"
            | "s390x"
            | "sparc64"
            | "x86_64"
    ) && suffix.split('-').count() >= 3
}

fn rustup_installed_list(
    project_start: &Path,
    kind: &'static str,
    toolchain: &str,
) -> Result<String, RustToolchainProbeError> {
    let output = std::process::Command::new("rustup")
        .args([
            kind,
            "list",
            "--toolchain",
            toolchain,
            "--installed",
            "--quiet",
        ])
        .current_dir(project_start)
        .output()
        .map_err(|source| RustToolchainProbeError::RustupList { kind, source })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr);
        Err(RustToolchainProbeError::ListForToolchain {
            kind,
            toolchain: toolchain.to_owned(),
            detail: detail.trim().to_owned(),
        })
    }
}

fn rust_target_status(installed: &str, target: &str) -> RustTargetStatus {
    if installed.lines().any(|line| line.trim() == target) {
        RustTargetStatus::Installed
    } else {
        RustTargetStatus::Missing
    }
}

fn check_devnode() -> DevnodeStatus {
    let path = match crate::node::find_local_node_binary() {
        Ok(crate::node::LocalNodeDiscovery::Local(path)) => path,
        Ok(crate::node::LocalNodeDiscovery::Selected { version, reason }) => {
            return DevnodeStatus {
                reachable: false,
                path: None,
                version: Some(version),
                error: Some(reason),
            };
        }
        Err(error) => {
            return DevnodeStatus {
                reachable: false,
                path: None,
                version: None,
                error: Some(error.to_string()),
            };
        }
    };

    match probe_devnode_version(&path) {
        Ok(version) => DevnodeStatus {
            reachable: true,
            path: Some(path),
            version,
            error: None,
        },
        Err(error) => DevnodeStatus {
            reachable: false,
            path: Some(path),
            version: None,
            error: Some(error.to_string()),
        },
    }
}

fn check_dev_wallet_artifact(devnode: &DevnodeStatus) -> DevWalletArtifactStatus {
    let Some(binary) = devnode.path.as_deref() else {
        return DevWalletArtifactStatus {
            available: false,
            valid: false,
            path: None,
            sha256: None,
            error: None,
        };
    };
    let Some(path) = crate::node::dev_wallet_artifact_for_binary(binary) else {
        return DevWalletArtifactStatus {
            available: false,
            valid: false,
            path: None,
            sha256: None,
            error: Some(format!(
                "could not find {} for {}",
                crate::node::DEV_WALLET_ARTIFACT_FILENAME,
                binary.display()
            )),
        };
    };

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return DevWalletArtifactStatus {
                available: true,
                valid: false,
                path: Some(path),
                sha256: None,
                error: Some(format!("could not read dev-wallet artifact: {error}")),
            };
        }
    };
    let sha256 = groundhog_rv32_artifact::sha256_hex(&bytes);
    match groundhog_rv32_artifact::verify_artifact(&bytes) {
        Ok(()) => DevWalletArtifactStatus {
            available: true,
            valid: true,
            path: Some(path),
            sha256: Some(sha256),
            error: None,
        },
        Err(error) => DevWalletArtifactStatus {
            available: true,
            valid: false,
            path: Some(path),
            sha256: Some(sha256),
            error: Some(format!("dev-wallet artifact is invalid: {error}")),
        },
    }
}

/// Why `<devnode> --version` did not report a version.
#[derive(Debug, thiserror::Error)]
enum DevnodeProbeError {
    /// A capture file for the probe's output could not be opened or cloned.
    #[error("could not capture {binary} --version: {source}")]
    Capture {
        /// The binary being probed.
        binary: String,
        #[source]
        source: std::io::Error,
    },
    /// The output cap does not fit the platform's resource-limit type.
    #[error("devnode output limit does not fit rlim_t")]
    OutputLimitUnrepresentable,
    /// The probe could not be started.
    #[error("could not run {binary}: {source}")]
    Spawn {
        /// The binary being probed.
        binary: String,
        #[source]
        source: std::io::Error,
    },
    /// A capture file could not be measured or re-read.
    #[error("could not {action} devnode {stream}: {source}")]
    Output {
        /// What the probe was doing: `inspect`, `rewind`, or `read`.
        action: &'static str,
        /// `stdout` or `stderr`.
        stream: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The probe wrote more than this probe is willing to read back.
    #[error("{binary} --version produced at least {limit} bytes of output")]
    OutputTooLarge {
        /// The binary being probed.
        binary: String,
        /// The byte cap the probe enforces.
        limit: u64,
    },
    /// The probe did not exit within its budget.
    #[error("{binary} --version timed out after {timeout_ms} ms")]
    Timeout {
        /// The binary being probed.
        binary: String,
        /// The budget the probe allowed.
        timeout_ms: u128,
    },
    /// Waiting on the probe failed.
    #[error("could not wait for {binary} --version: {source}")]
    Wait {
        /// The binary being probed.
        binary: String,
        #[source]
        source: std::io::Error,
    },
    /// The probe exited nonzero without a diagnostic.
    #[error("{binary} --version exited with {status}")]
    ExitStatus {
        /// The binary being probed.
        binary: String,
        /// The reported exit status.
        status: String,
    },
    /// The probe exited nonzero and said why.
    #[error("{binary} --version failed: {detail}")]
    Failed {
        /// The binary being probed.
        binary: String,
        /// What the probe reported on stderr.
        detail: String,
    },
    /// The probe succeeded but printed something this cannot read as a version.
    #[error("{binary} returned unexpected --version output")]
    UnexpectedOutput {
        /// The binary being probed.
        binary: String,
    },
}

fn probe_devnode_version(path: &Path) -> Result<Option<String>, DevnodeProbeError> {
    probe_devnode_version_with_timeout(path, std::time::Duration::from_secs(2))
}

fn probe_devnode_version_with_timeout(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Option<String>, DevnodeProbeError> {
    const OUTPUT_LIMIT: u64 = 64 * 1024;

    let capture_failed = |source| DevnodeProbeError::Capture {
        binary: path.display().to_string(),
        source,
    };
    let stdout_file = tempfile::tempfile().map_err(capture_failed)?;
    let stderr_file = tempfile::tempfile().map_err(capture_failed)?;
    let mut command = std::process::Command::new(path);
    command
        .arg("--version")
        .stdout(stdout_file.try_clone().map_err(capture_failed)?);
    command.stderr(stderr_file.try_clone().map_err(capture_failed)?);
    configure_probe_process(&mut command, OUTPUT_LIMIT)?;
    let mut child = command.spawn().map_err(|source| DevnodeProbeError::Spawn {
        binary: path.display().to_string(),
        source,
    })?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        let captured_bytes = probe_output_size_or_terminate(&mut child, || {
            probe_output_size(&stdout_file, &stderr_file)
        })?;
        if captured_bytes >= OUTPUT_LIMIT {
            terminate_probe_processes(&mut child);
            return Err(DevnodeProbeError::OutputTooLarge {
                binary: path.display().to_string(),
                limit: OUTPUT_LIMIT,
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                terminate_probe_descendants(child.id());
                break status;
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
            }
            Ok(None) => {
                terminate_probe_processes(&mut child);
                return Err(DevnodeProbeError::Timeout {
                    binary: path.display().to_string(),
                    timeout_ms: timeout.as_millis(),
                });
            }
            Err(source) => {
                terminate_probe_processes(&mut child);
                return Err(DevnodeProbeError::Wait {
                    binary: path.display().to_string(),
                    source,
                });
            }
        }
    };
    if probe_output_size(&stdout_file, &stderr_file)? >= OUTPUT_LIMIT
        || probe_hit_file_size_limit(status)
    {
        return Err(DevnodeProbeError::OutputTooLarge {
            binary: path.display().to_string(),
            limit: OUTPUT_LIMIT,
        });
    }
    let stdout = read_probe_output(stdout_file, "stdout", OUTPUT_LIMIT)?;
    let stderr = read_probe_output(stderr_file, "stderr", OUTPUT_LIMIT)?;

    if !status.success() {
        let detail = stderr.trim();
        let binary = path.display().to_string();
        return Err(if detail.is_empty() {
            DevnodeProbeError::ExitStatus {
                binary,
                status: status.to_string(),
            }
        } else {
            DevnodeProbeError::Failed {
                binary,
                detail: detail.to_owned(),
            }
        });
    }

    let line = stdout.lines().next().unwrap_or_default().trim();
    for program in ["hog-devnode", "hogd-dev"] {
        if line == program {
            return Ok(None);
        }
        if let Some(version) = line
            .strip_prefix(program)
            .and_then(|rest| rest.strip_prefix(' '))
        {
            let version = version.trim();
            return Ok((!version.is_empty()).then(|| version.to_string()));
        }
    }

    Err(DevnodeProbeError::UnexpectedOutput {
        binary: path.display().to_string(),
    })
}

#[cfg(unix)]
fn probe_hit_file_size_limit(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;

    status.signal() == Some(libc::SIGXFSZ)
}

#[cfg(not(unix))]
fn probe_hit_file_size_limit(_status: std::process::ExitStatus) -> bool {
    false
}

#[cfg(unix)]
fn configure_probe_process(
    command: &mut std::process::Command,
    output_limit: u64,
) -> Result<(), DevnodeProbeError> {
    use std::os::unix::process::CommandExt;

    let output_limit = libc::rlim_t::try_from(output_limit)
        .map_err(|_| DevnodeProbeError::OutputLimitUnrepresentable)?;
    command.process_group(0);
    // SAFETY: `setrlimit` is async-signal-safe and the closure only uses its
    // captured integer before `exec` in the child process.
    unsafe {
        command.pre_exec(move || {
            let limit = libc::rlimit {
                rlim_cur: output_limit,
                rlim_max: output_limit,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &raw const limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn configure_probe_process(
    _command: &mut std::process::Command,
    _output_limit: u64,
) -> Result<(), DevnodeProbeError> {
    Ok(())
}

fn terminate_probe_processes(child: &mut std::process::Child) {
    terminate_probe_descendants(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn terminate_probe_descendants(process_group: u32) {
    if let Ok(process_group) = i32::try_from(process_group) {
        // SAFETY: a negative PID addresses the process group created for this
        // probe. Failure is harmless when the group has already exited.
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn terminate_probe_descendants(_process_group: u32) {}

fn probe_output_size(
    stdout: &std::fs::File,
    stderr: &std::fs::File,
) -> Result<u64, DevnodeProbeError> {
    let measure = |file: &std::fs::File, stream| {
        file.metadata()
            .map(|metadata| metadata.len())
            .map_err(|source| DevnodeProbeError::Output {
                action: "inspect",
                stream,
                source,
            })
    };
    Ok(measure(stdout, "stdout")?.saturating_add(measure(stderr, "stderr")?))
}

fn probe_output_size_or_terminate<E>(
    child: &mut std::process::Child,
    probe: impl FnOnce() -> Result<u64, E>,
) -> Result<u64, E> {
    probe().inspect_err(|_| terminate_probe_processes(child))
}

fn read_probe_output(
    mut file: std::fs::File,
    stream: &'static str,
    limit: u64,
) -> Result<String, DevnodeProbeError> {
    use std::io::{Read, Seek};

    file.rewind().map_err(|source| DevnodeProbeError::Output {
        action: "rewind",
        stream,
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| DevnodeProbeError::Output {
            action: "read",
            stream,
            source,
        })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn resolve_effective_key(
    project_start: &Path,
    explicit_profile: Option<&str>,
    project: &ProjectContext,
    keys: &KeyStatus,
) -> Result<(Option<String>, Option<&'static str>), CliError> {
    if let Some(profile_key) = project.profile_key.clone() {
        Ok((Some(profile_key), Some("profile")))
    } else if let Some(global_key) = keys.global_default_key.clone() {
        Ok((Some(global_key), Some("global")))
    } else if project.network == Some(crate::config::Network::Local) {
        let key = crate::config::resolve_default_key(project_start, explicit_profile)?;
        let source = key.as_ref().map(|_| "local built-in");
        Ok((key, source))
    } else {
        Ok((None, None))
    }
}

fn add_key_findings(
    keys: &KeyStatus,
    project: &ProjectContext,
    effective_default_key: Option<&str>,
    findings: &mut Vec<Finding>,
) {
    // Dev keys are derived, regardless of whether a profile, global config, or
    // the local default selected them. Runtime-manifest checks happen at use.
    let uses_dev_key = effective_default_key.is_some_and(crate::dev_keys::is_dev_key);
    let has_profile_key_warning = project.findings.iter().any(|f| f.code == "key_not_on_disk");
    let effective_default_exists =
        effective_default_key_summary(&keys.keys, effective_default_key).is_some();

    if keys.keys.is_empty() && !uses_dev_key {
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
        && !uses_dev_key
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

    /// Path to a checked-in `hog-devnode` stand-in under `fixtures/devnode`.
    ///
    /// These stubs are committed rather than written during the test:
    /// `fs::write` holds a descriptor open for writing, children forked from
    /// other test threads inherit it, and executing a file that any process
    /// still holds open for writing fails with `ETXTBSY`.
    #[cfg(unix)]
    fn devnode_fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/devnode")
            .join(name)
    }

    #[test]
    fn default_wallet_check_reports_missing_binding() {
        let profile = crate::config::ResolvedProfile {
            name: "dev&net".into(),
            network: None,
            rpc_url: None,
            network_hash: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("alice".into()),
            default_wallet: Some("trading".into()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: std::collections::BTreeMap::new(),
            wallets: std::collections::BTreeMap::new(),
            source: None,
        };
        let mut findings = Vec::new();

        check_default_wallet(&profile, &mut findings);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "default_wallet_not_saved");
        assert_eq!(
            findings[0].hint.as_deref(),
            Some("run `groundhog wallet use <NAME> --profile='dev&net'` to select a saved wallet")
        );
    }

    #[test]
    fn default_wallet_check_rejects_selection_for_dev_key() {
        let mut wallets = std::collections::BTreeMap::new();
        wallets.insert(
            "trading".into(),
            crate::config::SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: format!("0x{}", "11".repeat(32)),
                owner_public_key: format!("0x{}", "22".repeat(32)),
            },
        );
        let profile = crate::config::ResolvedProfile {
            name: "local".into(),
            network: None,
            rpc_url: None,
            network_hash: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".into()),
            default_wallet: Some("trading".into()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: std::collections::BTreeMap::new(),
            wallets,
            source: None,
        };
        let mut findings = Vec::new();

        check_default_wallet(&profile, &mut findings);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "default_wallet_with_dev_key");
    }

    #[test]
    fn default_wallet_check_reports_malformed_owner_without_effective_key() {
        let mut wallets = std::collections::BTreeMap::new();
        wallets.insert(
            "trading".into(),
            crate::config::SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: format!("0x{}", "11".repeat(32)),
                owner_public_key: "not-hex".into(),
            },
        );
        let profile = crate::config::ResolvedProfile {
            name: "devnet".into(),
            network: None,
            rpc_url: None,
            network_hash: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: None,
            default_wallet: Some("trading".into()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: std::collections::BTreeMap::new(),
            wallets,
            source: None,
        };
        let mut findings = Vec::new();

        check_default_wallet(&profile, &mut findings);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "default_wallet_owner_invalid");
    }

    #[test]
    fn default_wallet_check_reports_malformed_address() {
        let mut wallets = std::collections::BTreeMap::new();
        wallets.insert(
            "trading".into(),
            crate::config::SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: "not-an-address".into(),
                owner_public_key: format!("0x{}", "22".repeat(32)),
            },
        );
        let profile = crate::config::ResolvedProfile {
            name: "devnet".into(),
            network: None,
            rpc_url: None,
            network_hash: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: None,
            default_wallet: Some("trading".into()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: std::collections::BTreeMap::new(),
            wallets,
            source: None,
        };
        let mut findings = Vec::new();

        check_default_wallet(&profile, &mut findings);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "default_wallet_address_invalid");
    }

    #[test]
    fn empty_report_has_no_warnings() {
        let report = StatusReport {
            environment: EnvironmentStatus {
                cargo: true,
                rust_toolchain: RustToolchainStatus {
                    available: true,
                    name: Some("nightly-test".into()),
                    rust_src: RustTargetStatus::Installed,
                    error: None,
                },
                rv32_target: RustTargetStatus::Installed,
                devnode: DevnodeStatus {
                    reachable: true,
                    path: Some(PathBuf::from("/usr/bin/hog-devnode")),
                    version: Some("0.1.0".into()),
                    error: None,
                },
                dev_wallet_artifact: DevWalletArtifactStatus {
                    available: true,
                    valid: true,
                    path: Some(PathBuf::from("/usr/share/groundhog/dev_wallet.rv32a")),
                    sha256: Some("abc123".into()),
                    error: None,
                },
                registry_configured: true,
                cargo_config_path: PathBuf::from("/tmp"),
            },
            keys: KeyStatus {
                keys: Vec::new(),
                global_default_key: None,
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
        let env = check_environment(Path::new(".")).unwrap();
        assert!(env.cargo);
        assert!(env.rust_toolchain.available);
    }

    #[test]
    fn rust_target_status_matches_complete_target_names() {
        let installed = "riscv32im-unknown-none-elf\nx86_64-unknown-linux-gnu\n";
        assert_eq!(
            rust_target_status(installed, crate::build::RV32_TARGET_TRIPLE),
            RustTargetStatus::Installed
        );
    }

    #[test]
    fn rust_toolchain_file_resolves_the_nearest_project_pin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project/src");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            tmp.path().join("project/rust-toolchain.toml"),
            "[toolchain]\nchannel = \"nightly-2026-01-30\"\n",
        )
        .unwrap();

        assert_eq!(
            rust_toolchain_file(&project).unwrap().as_deref(),
            Some("nightly-2026-01-30")
        );
    }

    #[test]
    fn requested_toolchain_matches_an_installed_host_toolchain() {
        let installed = vec!["nightly-2026-01-30-aarch64-apple-darwin".into()];
        assert_eq!(
            matching_installed_toolchain("nightly-2026-01-30", &installed).as_deref(),
            Some("nightly-2026-01-30-aarch64-apple-darwin")
        );
        assert!(matching_installed_toolchain("nightly-2026-01-31", &installed).is_none());
        assert!(matching_installed_toolchain("nightly", &installed).is_none());

        let rolling = vec!["nightly-aarch64-apple-darwin".into()];
        assert_eq!(
            matching_installed_toolchain("nightly", &rolling).as_deref(),
            Some("nightly-aarch64-apple-darwin")
        );
    }

    #[test]
    fn installed_toolchain_probe_uses_the_filesystem_root() {
        let command = installed_rust_toolchains_command();
        let nested_temp_dir = std::env::temp_dir().join("project/target/tmp");
        let filesystem_root = rustup_neutral_directory(&nested_temp_dir);

        assert_eq!(command.get_current_dir(), Some(filesystem_root.as_path()));
        assert!(filesystem_root.parent().is_none());
        assert!(
            command
                .get_envs()
                .any(|(name, value)| { name == "RUSTUP_TOOLCHAIN" && value.is_none() })
        );
    }

    #[test]
    fn installed_toolchain_parser_recognizes_default_annotation() {
        for annotations in ["default", "active, default"] {
            let output = format!("1.93.0-aarch64-apple-darwin ({annotations})\n");
            let (installed, default) = parse_installed_rust_toolchains(&output);

            assert_eq!(installed, ["1.93.0-aarch64-apple-darwin"]);
            assert_eq!(default.as_deref(), Some("1.93.0-aarch64-apple-darwin"));
        }
    }

    #[test]
    fn installed_toolchain_parser_ignores_incidental_default_text() {
        let output = "my-default-toolchain-aarch64-apple-darwin (active)\n\
                      nightly-aarch64-apple-darwin (active, defaultish)\n";
        let (installed, default) = parse_installed_rust_toolchains(output);

        assert_eq!(
            installed,
            [
                "my-default-toolchain-aarch64-apple-darwin",
                "nightly-aarch64-apple-darwin"
            ]
        );
        assert_eq!(default, None);
    }

    #[test]
    fn project_toolchain_probe_failure_does_not_mask_missing_rust_src() {
        let environment = EnvironmentStatus {
            cargo: true,
            rust_toolchain: RustToolchainStatus {
                available: true,
                name: Some("nightly-test".into()),
                rust_src: RustTargetStatus::Missing,
                error: Some("target probe failed".into()),
            },
            rv32_target: RustTargetStatus::Unknown,
            devnode: DevnodeStatus {
                reachable: true,
                path: Some(PathBuf::from("/usr/bin/hog-devnode")),
                version: Some("test".into()),
                error: None,
            },
            dev_wallet_artifact: DevWalletArtifactStatus {
                available: true,
                valid: true,
                path: Some(PathBuf::from("/usr/share/groundhog/dev_wallet.rv32a")),
                sha256: Some("abc123".into()),
                error: None,
            },
            registry_configured: true,
            cargo_config_path: PathBuf::from("/tmp/config.toml"),
        };
        let project = ProjectContext {
            config_path: Some(PathBuf::from("/tmp/groundhog.toml")),
            ..ProjectContext::default()
        };
        let mut findings = Vec::new();

        add_environment_findings(&environment, &project, &mut findings);

        assert!(findings.iter().any(|finding| {
            finding.code == "rust_toolchain_probe_failed"
                && finding.level == FindingLevel::Warn
                && finding.message == "target probe failed"
        }));
        assert!(findings.iter().any(|finding| {
            finding.code == "rust_src_unavailable" && finding.level == FindingLevel::Warn
        }));
    }

    #[cfg(unix)]
    #[test]
    fn devnode_version_probe_accepts_the_current_binary_name() {
        let binary = devnode_fixture("version.sh");

        assert_eq!(
            probe_devnode_version(&binary).unwrap().as_deref(),
            Some("0.1.12")
        );
    }

    #[cfg(unix)]
    #[test]
    fn devnode_version_probe_times_out_and_reaps_the_child() {
        let binary = devnode_fixture("hang.sh");
        let started = std::time::Instant::now();

        let error =
            probe_devnode_version_with_timeout(&binary, std::time::Duration::from_millis(50))
                .unwrap_err();

        assert!(
            matches!(error, DevnodeProbeError::Timeout { .. }),
            "{error}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn devnode_output_probe_error_terminates_and_reaps_the_child() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "while :; do :; done"]);
        configure_probe_process(&mut command, 64 * 1024).unwrap();
        let mut child = command.spawn().unwrap();

        let error =
            probe_output_size_or_terminate(&mut child, || Err("could not inspect devnode stdout"))
                .unwrap_err();

        assert_eq!(error, "could not inspect devnode stdout");
        assert!(child.try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn devnode_version_probe_does_not_wait_for_descendant_output_handles() {
        let binary = devnode_fixture("orphan.sh");
        let started = std::time::Instant::now();

        let version =
            probe_devnode_version_with_timeout(&binary, std::time::Duration::from_millis(1_500))
                .unwrap();

        assert_eq!(version.as_deref(), Some("0.1.12"));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn devnode_version_probe_rejects_excessive_output() {
        let binary = devnode_fixture("flood.sh");

        let error = probe_devnode_version_with_timeout(&binary, std::time::Duration::from_secs(5))
            .unwrap_err();

        assert!(
            matches!(error, DevnodeProbeError::OutputTooLarge { .. }),
            "{error}"
        );
    }

    #[test]
    fn dev_wallet_artifact_check_rejects_invalid_artifacts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let binary = tmp.path().join("hog-devnode");
        std::fs::write(&binary, []).unwrap();
        let contracts = tmp.path().join("contracts");
        std::fs::create_dir(&contracts).unwrap();
        std::fs::write(contracts.join("dev_wallet.rv32a"), b"not an artifact").unwrap();
        let status = check_dev_wallet_artifact(&DevnodeStatus {
            reachable: true,
            path: Some(binary),
            version: Some("test".into()),
            error: None,
        });

        assert!(status.available);
        assert!(!status.valid);
        assert_eq!(status.sha256.as_deref().map(str::len), Some(64));
        assert!(
            status
                .error
                .as_deref()
                .is_some_and(|error| error.contains("invalid"))
        );
    }

    #[test]
    #[serial]
    fn check_environment_detects_missing_registry_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cargo_home = tmp.path().join("cargo-home");

        unsafe {
            std::env::set_var("CARGO_HOME", &cargo_home);
        }

        let env = check_environment(tmp.path()).unwrap();
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
    fn check_project_reports_no_active_profile_when_no_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let context = check_project(tmp.path(), None).unwrap();
        assert!(context.config_path.is_none());
        assert!(context.profile_name.is_none());
        assert!(context.rpc_url.is_none());
        assert_eq!(context.contract_count, 0);
        assert_eq!(context.findings[0].code, "no_active_profile");

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
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
            "default_profile = \"default\"\n\n[profile.default]\nrpc_url = \"http://localhost\"\n",
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
            "default_profile = \"default\"\n\n[profile.default]\ndefault_key = \"ghost;echo bad\"\n",
        )
        .unwrap();

        let context = check_project(&project, None).unwrap();
        assert_eq!(context.profile_key.as_deref(), Some("ghost;echo bad"));
        assert_eq!(context.findings.len(), 1);
        assert_eq!(context.findings[0].code, "key_not_on_disk");
        assert!(
            context.findings[0]
                .hint
                .as_deref()
                .is_some_and(|hint| hint.contains("groundhog keys generate -- 'ghost;echo bad'"))
        );

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    #[serial]
    fn profile_key_findings_distinguish_derived_and_missing_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }
        let project = tmp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        for (key, missing) in [
            ("dev-1", false),
            ("dev-99", false),
            ("alice", true),
            ("dev-", true),
            ("dev-abc", true),
        ] {
            std::fs::write(
                project.join("groundhog.toml"),
                format!("[profile.default]\ndefault_key = \"{key}\"\n"),
            )
            .unwrap();
            let context = check_project(&project, Some("default")).unwrap();
            assert_eq!(
                context.findings.iter().any(|f| f.code == "key_not_on_disk"),
                missing,
                "{key}",
            );
        }
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn selected_key_findings_distinguish_derived_and_missing_keys() {
        for stored_keys in [
            Vec::new(),
            vec![KeySummary {
                name: "stored".into(),
                address: "unused".into(),
                is_default: false,
            }],
        ] {
            let keys = KeyStatus {
                keys: stored_keys,
                global_default_key: None,
            };
            for (key, missing) in [
                ("dev-1", false),
                ("dev-99", false),
                ("alice", true),
                ("dev-", true),
                ("dev-abc", true),
            ] {
                let mut findings = Vec::new();
                add_key_findings(&keys, &ProjectContext::default(), Some(key), &mut findings);
                assert_eq!(
                    findings.iter().any(|f| f.code == "no_keys"),
                    missing && keys.keys.is_empty(),
                    "{key}"
                );
                assert_eq!(
                    findings.iter().any(|f| f.code == "default_key_not_on_disk"),
                    missing,
                    "{key}",
                );
            }
        }
    }

    #[test]
    #[serial]
    fn collect_report_does_not_require_ffi_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("CARGO_HOME", tmp.path().join("cargo-home"));
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
            std::env::remove_var("HOGD_FFI_PASSWORD");
        }

        let report = collect_report(tmp.path(), None).unwrap();
        assert!(report.has_warnings());
        assert!(
            report
                .findings
                .iter()
                .all(|finding| finding.code != "no_credentials")
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
                rust_toolchain: RustToolchainStatus {
                    available: true,
                    name: Some("nightly-test".into()),
                    rust_src: RustTargetStatus::Installed,
                    error: None,
                },
                rv32_target: RustTargetStatus::Installed,
                devnode: DevnodeStatus {
                    reachable: true,
                    path: Some(PathBuf::from("/usr/bin/hog-devnode")),
                    version: Some("0.1.0".into()),
                    error: None,
                },
                dev_wallet_artifact: DevWalletArtifactStatus {
                    available: true,
                    valid: true,
                    path: Some(PathBuf::from("/usr/share/groundhog/dev_wallet.rv32a")),
                    sha256: Some("abc123".into()),
                    error: None,
                },
                registry_configured: true,
                cargo_config_path: PathBuf::from("/tmp"),
            },
            keys: KeyStatus {
                keys: Vec::new(),
                global_default_key: None,
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
    #[serial]
    fn check_project_no_config_with_explicit_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        // No groundhog.toml, but an explicit profile selection.
        let context = check_project(tmp.path(), Some("staging")).unwrap();
        assert!(context.config_path.is_none());
        assert_eq!(context.profile_name.as_deref(), Some("staging"));
        assert_eq!(context.findings.len(), 1);
        assert_eq!(context.findings[0].code, "profile_not_found");

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
