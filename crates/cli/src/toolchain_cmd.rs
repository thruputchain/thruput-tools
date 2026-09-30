use std::path::PathBuf;

use groundhog_toolchain::{
    Catalog, DEFAULT_PUBLIC_ARTIFACT_BASE_URL, HostTarget, HttpSource, Installer,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{cache_dir, load_toolchain_version, save_toolchain_version};
use crate::result::CommandResult;
use crate::{CliError, Print};

/// Version of the toolchain command JSON payload schema.
pub const TOOLCHAIN_JSON_SCHEMA_VERSION: u32 = 1;
/// Override for the public artifact origin; catalog digests remain authoritative.
pub const TOOLCHAIN_BASE_URL_ENV: &str = "GROUNDHOG_TOOLCHAIN_BASE_URL";

/// Origin represented by one toolchain command result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ToolchainSourceKind {
    Catalog,
    PublicArtifact,
    ManagedCache,
}

/// Stable JSON row describing one exact managed bundle.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainEntry {
    pub version: String,
    pub target: String,
    pub source_kind: ToolchainSourceKind,
    /// Executable path beneath the configured cache, including when not installed.
    pub path: PathBuf,
    /// SHA-256 of the published archive, not the extracted executable.
    pub sha256: String,
    /// Matches the persisted selection, or the catalog default when none is saved.
    pub active: bool,
    /// A complete installation was present when this report was produced.
    pub installed: bool,
}

/// Stable JSON data for `groundhog toolchain list`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainListReport {
    pub schema_version: u32,
    pub default_version: String,
    /// Saved selection (possibly absent from this catalog), or the default when unset.
    pub selected_version: String,
    pub target: String,
    pub toolchains: Vec<ToolchainEntry>,
}

/// Stable JSON data for a toolchain state-changing or lookup command.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainActionReport {
    pub schema_version: u32,
    pub version: String,
    pub target: String,
    pub source_kind: ToolchainSourceKind,
    /// Executable path beneath the configured cache, including when not installed.
    pub path: PathBuf,
    /// Published archive digest; omitted when cleaning a version absent from the catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Matches the persisted selection, or the catalog default when none is saved.
    pub active: bool,
    /// A complete installation was present when this report was produced.
    pub installed: bool,
    /// This command installed or removed a bundle, or changed the saved selection.
    pub changed: bool,
}

/// Ensure the requested, saved, or default managed bundle is installed and report it.
/// Checks host compatibility before cache reuse or acquisition. Offline mode only
/// accepts a complete cached bundle; installation does not change the saved selection.
pub fn install(
    requested: Option<&str>,
    offline: bool,
    command: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let context = Context::load(requested)?;
    context
        .catalog
        .artifact(&context.version, context.target)?
        .validate_current_host(context.target)?;
    let installer = Installer::new(cache_dir()?);
    if offline {
        let installation = installer.find(&context.catalog, &context.version, context.target)?;
        let Some(installation) = installation else {
            return Err(CliError::ToolchainOfflineMissing {
                version: context.version,
                target: context.target.triple().to_string(),
            });
        };
        return emit_action(
            command,
            print,
            context.report(
                installation.binary_path(),
                ToolchainSourceKind::ManagedCache,
                true,
                false,
            )?,
        );
    }

    let source = artifact_source()?;
    print.status(format_args!(
        "Installing hog-devnode {} for {}",
        context.version, context.target
    ));
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let installation = runtime.block_on(installer.install(
        &context.catalog,
        &context.version,
        context.target,
        &source,
    ))?;
    let source_kind = if installation.installed_now() {
        ToolchainSourceKind::PublicArtifact
    } else {
        ToolchainSourceKind::ManagedCache
    };
    let report = context.report(
        installation.binary_path(),
        source_kind,
        true,
        installation.installed_now(),
    )?;
    emit_action(command, print, report)
}

/// Report catalog entries and cached availability for this host target without mutation.
/// Does not download or require runtime compatibility, and reports a stale saved
/// selection even when that version is absent from the embedded catalog.
pub fn list(command: &'static str, print: &Print) -> Result<(), CliError> {
    let context = Context::load(None)?;
    let installer = Installer::new(cache_dir()?);
    let mut toolchains = Vec::new();
    for (version, artifact) in context.catalog.artifacts_for_target(context.target) {
        let is_installed = installer
            .find(&context.catalog, version, context.target)?
            .is_some();
        toolchains.push(ToolchainEntry {
            version: version.to_string(),
            target: context.target.triple().to_string(),
            source_kind: if is_installed {
                ToolchainSourceKind::ManagedCache
            } else {
                ToolchainSourceKind::Catalog
            },
            path: installer
                .installation_path(version, context.target)
                .join("hog-devnode"),
            sha256: artifact.sha256().to_string(),
            active: version == context.active_version,
            installed: is_installed,
        });
    }
    let report = ToolchainListReport {
        schema_version: TOOLCHAIN_JSON_SCHEMA_VERSION,
        default_version: context.catalog.default_version().to_string(),
        selected_version: context.active_version,
        target: context.target.triple().to_string(),
        toolchains,
    };
    if print.is_json() {
        print.json(&CommandResult::new(command, report))
    } else {
        if !report.toolchains.iter().any(|entry| entry.active) {
            print.warn(format_args!("Selected hog-devnode {} is absent from this catalog; run `groundhog toolchain use {}` to select its default", report.selected_version, report.default_version));
        }
        for entry in &report.toolchains {
            let active = if entry.active { " (active)" } else { "" };
            let state = if entry.installed {
                "installed"
            } else {
                "available"
            };
            print.data_line(format_args!("{}{} — {state}", entry.version, active))?;
        }
        Ok(())
    }
}

/// Persist an exact catalog version as the managed selection and report its cache state.
/// Requires host compatibility but does not download; replaces stale selections.
pub fn use_version(version: &str, command: &'static str, print: &Print) -> Result<(), CliError> {
    let mut context = Context::load(Some(version))?;
    context
        .catalog
        .artifact(&context.version, context.target)?
        .validate_current_host(context.target)?;
    let installer = Installer::new(cache_dir()?);
    let is_installed = installer
        .find(&context.catalog, &context.version, context.target)?
        .is_some();
    let changed = save_toolchain_version(&context.version)?;
    context.active_version.clone_from(&context.version);
    let report = context.report(
        installer
            .installation_path(&context.version, context.target)
            .join("hog-devnode"),
        if is_installed {
            ToolchainSourceKind::ManagedCache
        } else {
            ToolchainSourceKind::Catalog
        },
        is_installed,
        changed,
    )?;
    emit_action(command, print, report)
}

/// Report the selected managed executable, failing if incompatible or not installed.
/// This read-only lookup neither downloads nor falls back to explicit binaries or PATH.
pub fn which(command: &'static str, print: &Print) -> Result<(), CliError> {
    let context = Context::load(None)?;
    context
        .catalog
        .artifact(&context.version, context.target)?
        .validate_current_host(context.target)?;
    let installer = Installer::new(cache_dir()?);
    let installation = installer
        .find(&context.catalog, &context.version, context.target)?
        .ok_or_else(|| CliError::ToolchainNotInstalled {
            version: context.version.clone(),
            target: context.target.triple().to_string(),
        })?;
    let report = context.report(
        installation.binary_path(),
        ToolchainSourceKind::ManagedCache,
        true,
        false,
    )?;
    if print.is_json() {
        print.json(&CommandResult::new(command, report))
    } else {
        print.data_line(report.path.display())
    }
}

/// Remove the requested, saved, or default cached bundle without changing the selection.
/// Accepts retired versions and does not check runtime compatibility or download.
/// Fails promptly with a busy error while a node or installer holds the bundle lock.
pub fn clean(
    requested: Option<&str>,
    command: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let context = Context::load(requested)?;
    let installer = Installer::new(cache_dir()?);
    let path = installer.installation_path(&context.version, context.target);
    let changed = installer.clean(&context.version, context.target)?;
    let report = ToolchainActionReport {
        schema_version: TOOLCHAIN_JSON_SCHEMA_VERSION,
        sha256: context
            .catalog
            .artifact(&context.version, context.target)
            .ok()
            .map(|a| a.sha256().to_string()),
        active: context.version == context.active_version,
        version: context.version,
        target: context.target.triple().to_string(),
        source_kind: ToolchainSourceKind::ManagedCache,
        path: path.join("hog-devnode"),
        installed: false,
        changed,
    };
    emit_action(command, print, report)
}

struct Context {
    catalog: Catalog,
    version: String,
    active_version: String,
    target: HostTarget,
}

impl Context {
    fn load(requested: Option<&str>) -> Result<Self, CliError> {
        let catalog = Catalog::embedded()?;
        let configured = load_toolchain_version()?;
        let active_version = configured.unwrap_or_else(|| catalog.default_version().to_string());
        let version = requested.unwrap_or(&active_version).to_string();
        let target = HostTarget::current()?;
        Ok(Self {
            catalog,
            version,
            active_version,
            target,
        })
    }

    fn report(
        &self,
        path: PathBuf,
        source_kind: ToolchainSourceKind,
        installed: bool,
        changed: bool,
    ) -> Result<ToolchainActionReport, CliError> {
        let artifact = self.catalog.artifact(&self.version, self.target)?;
        Ok(ToolchainActionReport {
            schema_version: TOOLCHAIN_JSON_SCHEMA_VERSION,
            version: self.version.clone(),
            target: self.target.triple().to_string(),
            source_kind,
            path,
            sha256: Some(artifact.sha256().to_string()),
            active: self.version == self.active_version,
            installed,
            changed,
        })
    }
}

fn emit_action(
    command: &'static str,
    print: &Print,
    report: ToolchainActionReport,
) -> Result<(), CliError> {
    if print.is_json() {
        print.json(&CommandResult::new(command, report))
    } else {
        print.summary_card(
            "hog-devnode toolchain",
            &[
                ("Version".to_string(), report.version),
                ("Target".to_string(), report.target),
                ("Path".to_string(), report.path.display().to_string()),
            ],
        );
        Ok(())
    }
}

pub(crate) fn artifact_base_url() -> String {
    std::env::var(TOOLCHAIN_BASE_URL_ENV)
        .unwrap_or_else(|_| DEFAULT_PUBLIC_ARTIFACT_BASE_URL.to_string())
}

/// Construct artifact transport with the CLI's shared proxy validation.
pub(crate) fn artifact_source() -> Result<HttpSource, CliError> {
    let client = crate::http_client::build_client(
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_mins(5))
            .redirect(reqwest::redirect::Policy::none()),
    )?;
    Ok(HttpSource::with_client(artifact_base_url(), client))
}
