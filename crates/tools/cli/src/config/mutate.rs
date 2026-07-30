use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, TableLike, value};

use super::atomic::{ConfigFileKind, insert_preserving_decor, transact_document};
use super::dirs::config_file_path;
use super::project::{Network, ProjectProfile, find_project_config};
use super::read::{
    ConfigField, ConfigFieldValue, ConfigScope, load_persisted_config, validate_profile_name,
};
use crate::CliError;

pub const CONFIG_MUTATION_JSON_SCHEMA_VERSION: u32 = 1;

/// Stable JSON data for non-interactive config mutations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMutationReport {
    pub schema_version: u32,
    pub profile: String,
    pub scope: ConfigScope,
    pub path: PathBuf,
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<ConfigField>,
}

/// Set one validated scalar in a selected profile layer.
pub fn set_profile_field(
    start: &Path,
    profile: &str,
    field: ConfigField,
    raw_value: &str,
    global: bool,
    project: bool,
) -> Result<ConfigMutationReport, CliError> {
    let profile = validate_profile_name(profile)?;
    let field_value = field.parse_value(raw_value)?;
    let destination = mutation_destination(start, global, project)?;
    let changed = transact_document(
        &destination.path,
        destination.kind,
        |document| {
            let profile_table = profile_table_mut(document, &destination.path, &profile, true)?
                .ok_or_else(|| config_file_invalid(&destination.path, "profile was not created"))?;
            if profile_table.get(field_key(field)).and_then(Item::as_str)
                == Some(field_value_str(&field_value))
            {
                return Ok(());
            }
            insert_preserving_decor(profile_table, field_key(field), field_item(field_value));
            Ok(())
        },
        |document| validate_document_profile(document, &destination.path, &profile, false),
    )?;
    Ok(mutation_report(profile, destination, changed, Some(field)))
}

/// Remove one scalar from a selected profile layer without creating a table.
pub fn unset_profile_field(
    start: &Path,
    profile: &str,
    field: ConfigField,
    global: bool,
    project: bool,
) -> Result<ConfigMutationReport, CliError> {
    let profile = validate_profile_name(profile)?;
    let destination = mutation_destination(start, global, project)?;
    let changed = transact_document(
        &destination.path,
        destination.kind,
        |document| {
            if let Some(profile) = profile_table_mut(document, &destination.path, &profile, false)?
            {
                profile.remove(field_key(field));
            }
            Ok(())
        },
        |document| validate_document_profile(document, &destination.path, &profile, true),
    )?;
    Ok(mutation_report(profile, destination, changed, Some(field)))
}

/// Select the default profile in one config layer without changing the profile.
pub fn use_profile(
    start: &Path,
    profile: &str,
    global: bool,
    project: bool,
) -> Result<ConfigMutationReport, CliError> {
    let profile = validate_profile_name(profile)?;
    let destination = mutation_destination(start, global, project)?;
    let config = load_persisted_config(start)?;
    let validate_destination_profile = if destination.scope == ConfigScope::Global {
        config
            .inspect(&profile, ConfigScope::Global)
            .map_err(|error| match error {
                CliError::ProfileNotFound(_) => CliError::ProfileNotFoundInScope {
                    profile: profile.clone(),
                    scope: "global",
                },
                error => error,
            })?;
        true
    } else {
        config.resolve(&profile)?;
        config
            .project
            .as_ref()
            .is_some_and(|layer| layer.profiles.contains_key(&profile))
    };
    let changed = transact_document(
        &destination.path,
        destination.kind,
        |document| {
            if document.get("default_profile").and_then(Item::as_str) == Some(profile.as_str()) {
                return Ok(());
            }
            insert_preserving_decor(
                document.as_table_mut(),
                "default_profile",
                value(profile.as_str()),
            );
            Ok(())
        },
        |document| {
            validate_default_profile(document, &destination.path)?;
            if validate_destination_profile {
                validate_document_profile(document, &destination.path, &profile, false)?;
            }
            Ok(())
        },
    )?;
    Ok(mutation_report(profile, destination, changed, None))
}

#[derive(Debug)]
struct MutationDestination {
    scope: ConfigScope,
    path: PathBuf,
    kind: ConfigFileKind,
}

fn mutation_destination(
    start: &Path,
    global: bool,
    project: bool,
) -> Result<MutationDestination, CliError> {
    if global && project {
        return Err(CliError::ConfigScopeConflict);
    }
    let project_path = find_project_config(start);
    if project {
        return project_path
            .map(|path| MutationDestination {
                scope: ConfigScope::Project,
                path,
                kind: ConfigFileKind::Project,
            })
            .ok_or(CliError::ProjectConfigUnavailable);
    }
    if !global && project_path.is_some() {
        return Err(CliError::ConfigScopeAmbiguous);
    }
    Ok(MutationDestination {
        scope: ConfigScope::Global,
        path: config_file_path()?,
        kind: ConfigFileKind::Global,
    })
}

fn mutation_report(
    profile: String,
    destination: MutationDestination,
    changed: bool,
    field: Option<ConfigField>,
) -> ConfigMutationReport {
    ConfigMutationReport {
        schema_version: CONFIG_MUTATION_JSON_SCHEMA_VERSION,
        profile,
        scope: destination.scope,
        path: destination.path,
        changed,
        field,
    }
}

fn profile_table_mut<'a>(
    document: &'a mut DocumentMut,
    path: &Path,
    profile: &str,
    create: bool,
) -> Result<Option<&'a mut dyn TableLike>, CliError> {
    if !document.contains_key("profile") {
        if !create {
            return Ok(None);
        }
        let mut profiles = Table::new();
        profiles.set_implicit(true);
        document.insert("profile", Item::Table(profiles));
    }
    let profiles = document["profile"]
        .as_table_like_mut()
        .ok_or_else(|| config_file_invalid(path, "profile must be a table"))?;
    if !profiles.contains_key(profile) {
        if !create {
            return Ok(None);
        }
        profiles.insert(profile, Item::Table(Table::new()));
    }
    profiles
        .get_mut(profile)
        .and_then(Item::as_table_like_mut)
        .map(Some)
        .ok_or_else(|| config_file_invalid(path, "profile entry must be a table"))
}

fn validate_document_profile(
    document: &DocumentMut,
    path: &Path,
    profile: &str,
    allow_missing: bool,
) -> Result<(), CliError> {
    let root = parse_document(document, path)?;
    let profiles = root
        .get("profile")
        .map(|value| {
            value.as_table().ok_or_else(|| CliError::ConfigFileInvalid {
                path: path.to_path_buf(),
                message: "profile must be a table".into(),
            })
        })
        .transpose()?;
    let value = profiles.and_then(|profiles| profiles.get(profile));
    let Some(value) = value else {
        return if allow_missing {
            Ok(())
        } else {
            Err(CliError::ProfileNotFound(profile.to_owned()))
        };
    };
    let decoded = value
        .clone()
        .try_into::<ProjectProfile>()
        .map_err(|error| invalid_profile(path, profile, &error.to_string()))?;
    super::read::validate_profile(decoded)
        .map(|_| ())
        .map_err(|message| invalid_profile(path, profile, &message))
}

fn validate_default_profile(document: &DocumentMut, path: &Path) -> Result<(), CliError> {
    let root = parse_document(document, path)?;
    let name = root
        .get("default_profile")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| CliError::ConfigFileInvalid {
            path: path.to_path_buf(),
            message: "default_profile must be a string".into(),
        })?;
    validate_profile_name(name).map(|_| ())
}

fn parse_document(document: &DocumentMut, path: &Path) -> Result<toml::Table, CliError> {
    toml::from_str(&document.to_string())
        .map_err(|error| config_file_invalid(path, error.message()))
}

fn config_file_invalid(path: &Path, message: &str) -> CliError {
    CliError::ConfigFileInvalid {
        path: path.to_path_buf(),
        message: message.to_owned(),
    }
}

fn invalid_profile(path: &Path, profile: &str, message: &str) -> CliError {
    CliError::ProfileConfigInvalid {
        profile: profile.to_owned(),
        message: format!("is invalid in config `{}`: {message}", path.display()),
    }
}

fn field_key(field: ConfigField) -> &'static str {
    match field {
        ConfigField::Network => "network",
        ConfigField::RpcUrl => "rpc_url",
        ConfigField::FaucetUrl => "faucet_url",
        ConfigField::DefaultKey => "default_key",
    }
}

fn field_item(field_value: ConfigFieldValue) -> Item {
    match field_value {
        ConfigFieldValue::Network(network) => value(network_name(network)),
        ConfigFieldValue::String(string) => value(string),
    }
}

fn field_value_str(field_value: &ConfigFieldValue) -> &str {
    match field_value {
        ConfigFieldValue::Network(network) => network_name(*network),
        ConfigFieldValue::String(string) => string,
    }
}

fn network_name(network: Network) -> &'static str {
    match network {
        Network::Local => "local",
        Network::Devnet => "devnet",
        Network::Mainnet => "mainnet",
    }
}
