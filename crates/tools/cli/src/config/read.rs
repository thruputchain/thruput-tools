use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::dirs::config_file_path;
use super::global::{LocalRuntimeState, validate_local_runtime_state};
use super::project::{Network, ProjectProfile};
use crate::CliError;

pub const CONFIG_GET_JSON_SCHEMA_VERSION: u32 = 1;
pub const CONFIG_PROFILES_JSON_SCHEMA_VERSION: u32 = 1;

/// A scalar profile field managed by `groundhog config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigField {
    Network,
    RpcUrl,
    NetworkHash,
    FaucetUrl,
    DefaultKey,
}

impl ConfigField {
    /// Parse and validate a field value without mutating configuration.
    pub fn parse_value(self, value: &str) -> Result<ConfigFieldValue, CliError> {
        match self {
            Self::Network => Network::from_str(value.trim())
                .map(ConfigFieldValue::Network)
                .map_err(|message| invalid_field(self, message)),
            Self::RpcUrl | Self::FaucetUrl => {
                let value = value.trim();
                validate_http_url(value).map_err(|message| invalid_field(self, message))?;
                Ok(ConfigFieldValue::String(value.to_owned()))
            }
            Self::NetworkHash => {
                // Accept the `0x` spelling `groundhog_networkInfo` and the
                // faucet descriptor emit, and store the unprefixed form the
                // runtime manifest uses. Reuse the wire type's validation so a
                // profile cannot hold a hash the signing path would later
                // reject.
                let value = value.trim();
                let value = value.strip_prefix("0x").unwrap_or(value);
                groundhog_json_rpc_types::NetworkHashHex::try_from(value)
                    .map_err(|_| {
                        invalid_field(
                            self,
                            "expected 64 lowercase hexadecimal digits, optionally 0x-prefixed",
                        )
                    })
                    .map(|hash| ConfigFieldValue::String(hash.as_ref().to_owned()))
            }
            Self::DefaultKey => {
                let value = value.trim();
                if value.is_empty() {
                    return Err(invalid_field(self, "value cannot be empty"));
                }
                Ok(ConfigFieldValue::String(value.to_owned()))
            }
        }
    }
}

impl std::fmt::Display for ConfigField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Network => "network",
            Self::RpcUrl => "rpc-url",
            Self::NetworkHash => "network-hash",
            Self::FaucetUrl => "faucet-url",
            Self::DefaultKey => "default-key",
        })
    }
}

impl FromStr for ConfigField {
    type Err = CliError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "network" => Ok(Self::Network),
            "rpc-url" => Ok(Self::RpcUrl),
            "network-hash" => Ok(Self::NetworkHash),
            "faucet-url" => Ok(Self::FaucetUrl),
            "default-key" => Ok(Self::DefaultKey),
            _ => Err(CliError::ConfigFieldUnsupported(value.trim().to_owned())),
        }
    }
}

/// A validated value for an editable profile field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigFieldValue {
    Network(Network),
    String(String),
}

/// Stable source kinds used by config inspection and effective status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConfigSourceKind {
    Global,
    Project,
    Explicit,
    Environment,
}

/// The origin of a profile selection or persisted value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSource {
    pub kind: ConfigSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ConfigSource {
    fn layer(kind: ConfigSourceKind, path: &Path) -> Self {
        Self {
            kind,
            path: Some(path.to_path_buf()),
            name: None,
        }
    }
}

/// The selected profile and the source that selected it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSelection {
    pub profile: String,
    pub source: ConfigSource,
}

/// A decoded profile or an isolated schema error for that profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedProfileState {
    Valid(ProjectProfile),
    Invalid { reason: String },
}

/// One physical config file projected to non-secret profile state.
#[derive(Debug, Clone)]
pub struct PersistedConfigLayer {
    pub path: PathBuf,
    pub exists: bool,
    pub default_profile: Option<String>,
    pub profiles: BTreeMap<String, PersistedProfileState>,
}

/// Persisted global and project config, before runtime overlays or signer fallback.
#[derive(Debug, Clone)]
pub struct PersistedConfig {
    pub global: PersistedConfigLayer,
    pub project: Option<PersistedConfigLayer>,
}

/// Per-value origins for a persisted profile projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSources {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<ConfigSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<ConfigSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_hash: Option<ConfigSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faucet_url: Option<ConfigSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_key: Option<ConfigSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_wallet: Option<ConfigSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_manifest: Option<ConfigSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_artifacts_dir: Option<ConfigSource>,
    #[serde(default)]
    pub contracts: BTreeMap<String, ConfigSource>,
    #[serde(default)]
    pub wallets: BTreeMap<String, ConfigSource>,
}

/// A valid persisted profile with global/project provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedProfile {
    pub name: String,
    pub values: ProjectProfile,
    pub sources: ProfileSources,
    pub global: bool,
    pub project: bool,
}

/// Scope represented by a config inspection report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConfigScope {
    Resolved,
    Global,
    Project,
}

/// Stable JSON data for `groundhog config get`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigGetReport {
    pub schema_version: u32,
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_source: Option<ConfigSource>,
    pub scope: ConfigScope,
    pub network: Option<Network>,
    pub rpc_url: Option<String>,
    pub network_hash: Option<String>,
    pub faucet_url: Option<String>,
    pub default_key: Option<String>,
    pub default_wallet: Option<String>,
    pub runtime_manifest: Option<PathBuf>,
    pub runtime_artifacts_dir: Option<PathBuf>,
    pub contracts: BTreeMap<String, String>,
    pub wallets: BTreeMap<String, ConfigWalletBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<ProfileSources>,
}

/// Saved wallet binding projected into the versioned config JSON contract.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigWalletBinding {
    pub address: String,
    pub owner_public_key: String,
}

/// Validation state of a profile row in `config profiles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConfigProfileStatus {
    Valid,
    Invalid,
    Missing,
}

/// One profile row in the stable `config profiles` JSON report.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigProfileRow {
    pub profile: String,
    pub active: bool,
    pub global: bool,
    pub project: bool,
    pub network: Option<Network>,
    pub status: ConfigProfileStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
}

/// Stable JSON data for `groundhog config profiles`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigProfilesReport {
    pub schema_version: u32,
    pub active_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_source: Option<ConfigSource>,
    pub profiles: Vec<ConfigProfileRow>,
}

impl PersistedConfig {
    /// Resolve profile-selection precedence without requiring the selected profile to exist.
    pub fn selection(&self, explicit: Option<&str>) -> Result<Option<ProfileSelection>, CliError> {
        if let Some(name) = explicit {
            return Ok(Some(ProfileSelection {
                profile: validate_profile_name(name)?,
                source: ConfigSource {
                    kind: ConfigSourceKind::Explicit,
                    path: None,
                    name: None,
                },
            }));
        }

        if let Some(name) = std::env::var("GROUNDHOG_PROFILE")
            .ok()
            .filter(|name| !name.trim().is_empty())
        {
            return Ok(Some(ProfileSelection {
                profile: validate_profile_name(&name)?,
                source: ConfigSource {
                    kind: ConfigSourceKind::Environment,
                    path: None,
                    name: Some("GROUNDHOG_PROFILE".into()),
                },
            }));
        }

        if let Some(layer) = self.project.as_ref()
            && let Some(name) = layer.default_profile.as_deref()
        {
            return Ok(Some(ProfileSelection {
                profile: name.to_owned(),
                source: ConfigSource::layer(ConfigSourceKind::Project, &layer.path),
            }));
        }

        Ok(self
            .global
            .default_profile
            .as_ref()
            .map(|name| ProfileSelection {
                profile: name.clone(),
                source: ConfigSource::layer(ConfigSourceKind::Global, &self.global.path),
            }))
    }

    /// Resolve a valid profile from the persisted global/project layers.
    pub fn resolve(&self, name: &str) -> Result<PersistedProfile, CliError> {
        let global = self.global.profiles.get(name);
        let project = self
            .project
            .as_ref()
            .and_then(|layer| layer.profiles.get(name));

        let invalid_reason = global
            .and_then(|state| invalid_state_reason(state, "global", &self.global.path))
            .or_else(|| {
                self.project.as_ref().and_then(|layer| {
                    project.and_then(|state| invalid_state_reason(state, "project", &layer.path))
                })
            });
        if let Some(message) = invalid_reason {
            return Err(CliError::ProfileConfigInvalid {
                profile: name.to_owned(),
                message,
            });
        }

        let global = valid_profile(global);
        let project = valid_profile(project);
        match (global, project, self.project.as_ref()) {
            (None, None, _) | (_, Some(_), None) => Err(CliError::ProfileNotFound(name.to_owned())),
            (Some(values), None, _) => Ok(profile_from_layer(
                name,
                values,
                &ConfigSource::layer(ConfigSourceKind::Global, &self.global.path),
                true,
            )),
            (None, Some(values), Some(layer)) => Ok(profile_from_layer(
                name,
                values,
                &ConfigSource::layer(ConfigSourceKind::Project, &layer.path),
                true,
            )),
            (Some(global), Some(project), Some(layer)) => {
                let global = profile_from_layer(
                    name,
                    global,
                    &ConfigSource::layer(ConfigSourceKind::Global, &self.global.path),
                    true,
                );
                let project = profile_from_layer(
                    name,
                    project,
                    &ConfigSource::layer(ConfigSourceKind::Project, &layer.path),
                    true,
                );
                Ok(merge_profiles(global, project))
            }
        }
    }

    /// Resolve one raw layer or the merged persisted profile.
    pub fn inspect(&self, name: &str, scope: ConfigScope) -> Result<PersistedProfile, CliError> {
        match scope {
            ConfigScope::Resolved => self.resolve(name),
            ConfigScope::Global => profile_from_state(
                name,
                self.global.profiles.get(name),
                &ConfigSource::layer(ConfigSourceKind::Global, &self.global.path),
                false,
            ),
            ConfigScope::Project => {
                let layer = self
                    .project
                    .as_ref()
                    .ok_or(CliError::ProjectConfigUnavailable)?;
                profile_from_state(
                    name,
                    layer.profiles.get(name),
                    &ConfigSource::layer(ConfigSourceKind::Project, &layer.path),
                    false,
                )
            }
        }
    }

    /// Build the sorted, independently validated profile-list report.
    pub fn profiles_report(
        &self,
        explicit: Option<&str>,
    ) -> Result<ConfigProfilesReport, CliError> {
        let selection = self.selection(explicit)?;
        let mut names = self
            .global
            .profiles
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(project) = self.project.as_ref() {
            names.extend(project.profiles.keys().cloned());
        }
        if let Some(selection) = selection.as_ref() {
            names.insert(selection.profile.clone());
        }

        let profiles = names
            .into_iter()
            .map(|name| self.profile_row(&name, selection.as_ref()))
            .collect();
        Ok(ConfigProfilesReport {
            schema_version: CONFIG_PROFILES_JSON_SCHEMA_VERSION,
            active_profile: selection
                .as_ref()
                .map(|selection| selection.profile.clone()),
            selection_source: selection.map(|selection| selection.source),
            profiles,
        })
    }

    fn profile_row(&self, name: &str, selection: Option<&ProfileSelection>) -> ConfigProfileRow {
        let global = self.global.profiles.contains_key(name);
        let project = self
            .project
            .as_ref()
            .is_some_and(|layer| layer.profiles.contains_key(name));
        let (network, status, invalid_reason) = match self.resolve(name) {
            Ok(profile) => (profile.values.network, ConfigProfileStatus::Valid, None),
            Err(CliError::ProfileNotFound(_)) => (None, ConfigProfileStatus::Missing, None),
            Err(error) => (None, ConfigProfileStatus::Invalid, Some(error.to_string())),
        };
        ConfigProfileRow {
            profile: name.to_owned(),
            active: selection.is_some_and(|selection| selection.profile == name),
            global,
            project,
            network,
            status,
            invalid_reason,
        }
    }
}

impl ConfigGetReport {
    /// Project a persisted profile into the versioned config JSON contract.
    pub fn new(
        profile: PersistedProfile,
        selection_source: Option<ConfigSource>,
        scope: ConfigScope,
        include_sources: bool,
    ) -> Self {
        let ProjectProfile {
            network,
            rpc_url,
            network_hash,
            faucet_url,
            default_key,
            default_wallet,
            runtime_manifest,
            runtime_artifacts_dir,
            contracts,
            wallets,
        } = profile.values;
        Self {
            schema_version: CONFIG_GET_JSON_SCHEMA_VERSION,
            profile: profile.name,
            selection_source,
            scope,
            network,
            rpc_url,
            network_hash,
            faucet_url,
            default_key,
            default_wallet,
            runtime_manifest,
            runtime_artifacts_dir,
            contracts,
            wallets: wallets
                .into_iter()
                .map(|(name, wallet)| {
                    (
                        name,
                        ConfigWalletBinding {
                            address: wallet.address,
                            owner_public_key: wallet.owner_public_key,
                        },
                    )
                })
                .collect(),
            sources: include_sources.then_some(profile.sources),
        }
    }
}

/// Load both persisted config layers without exposing protected global state.
pub fn load_persisted_config(start: &Path) -> Result<PersistedConfig, CliError> {
    let global_path = config_file_path()?;
    let global = read_layer(&global_path)?;
    let project = super::project::find_project_config(start)
        .map(|path| read_layer(&path))
        .transpose()?;
    Ok(PersistedConfig { global, project })
}

/// Resolve the active profile from persisted files without runtime overlays.
pub fn resolve_persisted_profile(
    start: &Path,
    explicit: Option<&str>,
) -> Result<(PersistedProfile, ProfileSelection), CliError> {
    let config = load_persisted_config(start)?;
    let selection = config
        .selection(explicit)?
        .ok_or(CliError::NoActiveProfile)?;
    let profile = config.resolve(&selection.profile)?;
    Ok((profile, selection))
}

/// Trim and validate a profile name used for selection or mutation.
pub fn validate_profile_name(name: &str) -> Result<String, CliError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CliError::ProfileNameInvalid("profile name cannot be empty"));
    }
    if name.chars().any(char::is_control) {
        return Err(CliError::ProfileNameInvalid(
            "profile name cannot contain control characters",
        ));
    }
    Ok(name.to_owned())
}

pub(crate) fn load_local_runtime_state() -> Result<Option<LocalRuntimeState>, CliError> {
    let path = config_file_path()?;
    let state = load_global_field("local_runtime_state")?;
    if let Some(state) = state.as_ref() {
        validate_local_runtime_state(state).map_err(|error| CliError::ConfigFileInvalid {
            path,
            message: format!("local_runtime_state is invalid: {error}"),
        })?;
    }
    Ok(state)
}

pub(crate) fn load_global_field<T: DeserializeOwned>(key: &str) -> Result<Option<T>, CliError> {
    let path = config_file_path()?;
    let Some(root) = read_optional_root(&path)? else {
        return Ok(None);
    };
    root.get(key)
        .cloned()
        .map(toml::Value::try_into)
        .transpose()
        .map_err(|error| CliError::ConfigFileInvalid {
            path,
            message: format!("{key} is invalid: {error}"),
        })
}

fn read_layer(path: &Path) -> Result<PersistedConfigLayer, CliError> {
    let Some(root) = read_optional_root(path)? else {
        return Ok(PersistedConfigLayer {
            path: path.to_path_buf(),
            exists: false,
            default_profile: None,
            profiles: BTreeMap::new(),
        });
    };
    let table = root.as_table().ok_or_else(|| CliError::ConfigFileInvalid {
        path: path.to_path_buf(),
        message: "config root must be a table".into(),
    })?;
    let default_profile = table
        .get("default_profile")
        .map(|value| {
            value.as_str().ok_or_else(|| CliError::ConfigFileInvalid {
                path: path.to_path_buf(),
                message: "default_profile must be a string".into(),
            })
        })
        .transpose()?
        .map(|name| {
            validate_profile_name(name).map_err(|error| CliError::ConfigFileInvalid {
                path: path.to_path_buf(),
                message: format!("default_profile is invalid: {error}"),
            })
        })
        .transpose()?;
    let profile_table = table
        .get("profile")
        .map(|value| {
            value.as_table().ok_or_else(|| CliError::ConfigFileInvalid {
                path: path.to_path_buf(),
                message: "profile must be a table".into(),
            })
        })
        .transpose()?;
    let profiles = profile_table
        .into_iter()
        .flat_map(|profiles| profiles.iter())
        .map(|(name, value)| {
            let state = decode_profile(name, value);
            (name.clone(), state)
        })
        .collect();
    Ok(PersistedConfigLayer {
        path: path.to_path_buf(),
        exists: true,
        default_profile,
        profiles,
    })
}

fn read_optional_root(path: &Path) -> Result<Option<toml::Value>, CliError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    toml::from_str(&contents).map(Some).map_err(|error| {
        let (line, column) = error
            .span()
            .map(|span| super::line_column(&contents, span.start))
            .map_or((None, None), |(line, column)| (Some(line), Some(column)));
        CliError::ConfigParse {
            path: path.to_path_buf(),
            message: error.message().to_owned(),
            line,
            column,
        }
    })
}

fn decode_profile(name: &str, value: &toml::Value) -> PersistedProfileState {
    let result = validate_profile_name(name)
        .map_err(|error| error.to_string())
        .map(|_| ())
        .and_then(|()| {
            value
                .clone()
                .try_into::<ProjectProfile>()
                .map_err(|error| error.to_string())
        })
        .and_then(|profile| validate_profile(profile).map_err(|error| error.to_string()));
    match result {
        Ok(profile) => PersistedProfileState::Valid(profile),
        Err(reason) => PersistedProfileState::Invalid { reason },
    }
}

/// Why a persisted profile's fields were rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileFieldError {
    /// A URL field does not hold an absolute HTTP(S) URL.
    #[error("{field} {detail}")]
    Url {
        /// The offending field's config key.
        field: &'static str,
        /// What the field must be instead.
        detail: &'static str,
    },
    /// `default_key` is present but blank.
    #[error("default_key cannot be empty")]
    EmptyDefaultKey,
}

pub(super) fn validate_profile(
    profile: ProjectProfile,
) -> Result<ProjectProfile, ProfileFieldError> {
    if let Some(url) = profile.rpc_url.as_deref() {
        validate_http_url(url).map_err(|detail| ProfileFieldError::Url {
            field: "rpc_url",
            detail,
        })?;
    }
    if let Some(url) = profile.faucet_url.as_deref() {
        validate_http_url(url).map_err(|detail| ProfileFieldError::Url {
            field: "faucet_url",
            detail,
        })?;
    }
    if profile
        .default_key
        .as_deref()
        .is_some_and(|key| key.trim().is_empty())
    {
        return Err(ProfileFieldError::EmptyDefaultKey);
    }
    Ok(profile)
}

pub(super) fn validate_http_url(value: &str) -> Result<(), &'static str> {
    let url = reqwest::Url::parse(value).map_err(|_| "must be an absolute HTTP(S) URL")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("must be an absolute HTTP(S) URL");
    }
    Ok(())
}

fn invalid_field(field: ConfigField, message: &str) -> CliError {
    CliError::ConfigValueInvalid {
        field: field.to_string(),
        message: message.to_owned(),
    }
}

fn valid_profile(state: Option<&PersistedProfileState>) -> Option<ProjectProfile> {
    match state {
        Some(PersistedProfileState::Valid(profile)) => Some(profile.clone()),
        Some(PersistedProfileState::Invalid { .. }) | None => None,
    }
}

fn profile_from_layer(
    name: &str,
    mut values: ProjectProfile,
    source: &ConfigSource,
    normalize: bool,
) -> PersistedProfile {
    if normalize && let Some(path) = source.path.as_deref() {
        normalize_paths(&mut values, path);
    }
    let global = source.kind == ConfigSourceKind::Global;
    let project = source.kind == ConfigSourceKind::Project;
    let sources = sources_for_profile(&values, source);
    PersistedProfile {
        name: name.to_owned(),
        values,
        sources,
        global,
        project,
    }
}

fn profile_from_state(
    name: &str,
    state: Option<&PersistedProfileState>,
    source: &ConfigSource,
    normalize: bool,
) -> Result<PersistedProfile, CliError> {
    match state {
        Some(PersistedProfileState::Valid(profile)) => {
            Ok(profile_from_layer(name, profile.clone(), source, normalize))
        }
        Some(PersistedProfileState::Invalid { reason }) => Err(CliError::ProfileConfigInvalid {
            profile: name.to_owned(),
            message: invalid_source_reason(source, reason),
        }),
        None => Err(CliError::ProfileNotFound(name.to_owned())),
    }
}

fn invalid_state_reason(state: &PersistedProfileState, layer: &str, path: &Path) -> Option<String> {
    match state {
        PersistedProfileState::Valid(_) => None,
        PersistedProfileState::Invalid { reason } => Some(format!(
            "is invalid in {layer} config `{}`: {reason}",
            path.display()
        )),
    }
}

fn invalid_source_reason(source: &ConfigSource, reason: &str) -> String {
    let layer = match source.kind {
        ConfigSourceKind::Global => "global",
        ConfigSourceKind::Project => "project",
        ConfigSourceKind::Explicit | ConfigSourceKind::Environment => "selected",
    };
    source.path.as_ref().map_or_else(
        || format!("is invalid: {reason}"),
        |path| {
            format!(
                "is invalid in {layer} config `{}`: {reason}",
                path.display()
            )
        },
    )
}

fn sources_for_profile(profile: &ProjectProfile, source: &ConfigSource) -> ProfileSources {
    ProfileSources {
        network: profile.network.map(|_| source.clone()),
        rpc_url: profile.rpc_url.as_ref().map(|_| source.clone()),
        network_hash: profile.network_hash.as_ref().map(|_| source.clone()),
        faucet_url: profile.faucet_url.as_ref().map(|_| source.clone()),
        default_key: profile.default_key.as_ref().map(|_| source.clone()),
        default_wallet: profile.default_wallet.as_ref().map(|_| source.clone()),
        runtime_manifest: profile.runtime_manifest.as_ref().map(|_| source.clone()),
        runtime_artifacts_dir: profile
            .runtime_artifacts_dir
            .as_ref()
            .map(|_| source.clone()),
        contracts: profile
            .contracts
            .keys()
            .map(|name| (name.clone(), source.clone()))
            .collect(),
        wallets: profile
            .wallets
            .keys()
            .map(|name| (name.clone(), source.clone()))
            .collect(),
    }
}

fn merge_profiles(mut global: PersistedProfile, project: PersistedProfile) -> PersistedProfile {
    macro_rules! replace_scalar {
        ($field:ident) => {
            if project.values.$field.is_some() {
                global.values.$field = project.values.$field;
                global.sources.$field = project.sources.$field;
            }
        };
    }
    replace_scalar!(network);
    replace_scalar!(rpc_url);
    replace_scalar!(network_hash);
    replace_scalar!(faucet_url);
    replace_scalar!(default_key);
    replace_scalar!(default_wallet);
    replace_scalar!(runtime_manifest);
    replace_scalar!(runtime_artifacts_dir);
    global.values.contracts.extend(project.values.contracts);
    global.values.wallets.extend(project.values.wallets);
    global.sources.contracts.extend(project.sources.contracts);
    global.sources.wallets.extend(project.sources.wallets);
    global.project = true;
    global
}

fn normalize_paths(profile: &mut ProjectProfile, config_path: &Path) {
    let Some(parent) = config_path.parent() else {
        return;
    };
    if let Some(path) = profile.runtime_manifest.as_mut()
        && path.is_relative()
    {
        *path = parent.join(&*path);
    }
    if let Some(path) = profile.runtime_artifacts_dir.as_mut()
        && path.is_relative()
    {
        *path = parent.join(&*path);
    }
}

impl FromStr for Network {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local" => Ok(Self::Local),
            "devnet" => Ok(Self::Devnet),
            "mainnet" => Ok(Self::Mainnet),
            _ => Err("must be one of local, devnet, or mainnet"),
        }
    }
}
