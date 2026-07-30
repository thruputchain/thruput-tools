use std::cell::Cell;
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
pub const CONFIG_NEW_JSON_SCHEMA_VERSION: u32 = 1;
pub const CONFIG_REMOVE_JSON_SCHEMA_VERSION: u32 = 1;

/// Typed values accepted when creating a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProfileOptions {
    pub network: String,
    pub rpc_url: Option<String>,
    pub faucet_url: Option<String>,
    pub default_key: Option<String>,
    pub select: bool,
}

/// Stable JSON data for `groundhog config new`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigNewReport {
    pub schema_version: u32,
    pub profile: String,
    pub scope: ConfigScope,
    pub path: PathBuf,
    pub changed: bool,
    pub created_fields: Vec<ConfigField>,
    pub selected: bool,
}

/// Stable JSON data for `groundhog config rm`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigRemoveReport {
    pub schema_version: u32,
    pub profile: String,
    pub scope: ConfigScope,
    pub path: PathBuf,
    pub changed: bool,
    pub cleared_default: bool,
}

pub(crate) struct ProfileRemovalConfirmation {
    profile: String,
    destination: MutationDestination,
    expected_document: String,
}

impl ProfileRemovalConfirmation {
    pub(crate) fn scope(&self) -> ConfigScope {
        self.destination.scope
    }
}

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

/// Create one complete profile in a selected config layer.
pub fn create_profile(
    start: &Path,
    profile: &str,
    options: &NewProfileOptions,
    global: bool,
    project: bool,
) -> Result<ConfigNewReport, CliError> {
    let profile = validate_profile_name(profile)?;
    let fields = parse_new_profile_fields(options)?;
    let mut created_fields = fields.iter().map(|(field, _)| *field).collect::<Vec<_>>();
    created_fields.sort_by_key(ToString::to_string);
    let destination = mutation_destination(start, global, project)?;
    let scope = scope_name(destination.scope);
    let changed = transact_document(
        &destination.path,
        destination.kind,
        |document| {
            if profile_exists(document, &destination.path, &profile)? {
                return Err(CliError::ProfileAlreadyExists {
                    profile: profile.clone(),
                    scope,
                });
            }
            let profile_table = profile_table_mut(document, &destination.path, &profile, true)?
                .ok_or_else(|| config_file_invalid(&destination.path, "profile was not created"))?;
            for (field, value) in fields {
                insert_preserving_decor(profile_table, field_key(field), field_item(value));
            }
            if options.select {
                insert_preserving_decor(
                    document.as_table_mut(),
                    "default_profile",
                    value(profile.as_str()),
                );
            }
            Ok(())
        },
        |document| {
            validate_document_profile(document, &destination.path, &profile, false)?;
            if options.select {
                validate_default_profile(document, &destination.path)?;
            }
            Ok(())
        },
    )?;
    Ok(ConfigNewReport {
        schema_version: CONFIG_NEW_JSON_SCHEMA_VERSION,
        profile,
        scope: destination.scope,
        path: destination.path,
        changed,
        created_fields,
        selected: options.select,
    })
}

/// Remove an entire profile from one config layer and clear its matching default.
pub fn remove_profile(
    start: &Path,
    profile: &str,
    global: bool,
    project: bool,
) -> Result<ConfigRemoveReport, CliError> {
    let profile = validate_profile_name(profile)?;
    let destination = mutation_destination(start, global, project)?;
    remove_profile_from_destination(profile, destination, None)
}

pub(crate) fn prepare_profile_removal(
    start: &Path,
    profile: &str,
    global: bool,
    project: bool,
) -> Result<ProfileRemovalConfirmation, CliError> {
    let profile = validate_profile_name(profile)?;
    let destination = mutation_destination(start, global, project)?;
    let document = super::atomic::load_document(&destination.path)?;
    if !profile_exists(&document, &destination.path, &profile)? {
        return Err(CliError::ProfileNotFoundInScope {
            profile,
            scope: scope_name(destination.scope),
        });
    }
    Ok(ProfileRemovalConfirmation {
        profile,
        destination,
        expected_document: document.to_string(),
    })
}

pub(crate) fn remove_confirmed_profile(
    confirmation: ProfileRemovalConfirmation,
) -> Result<ConfigRemoveReport, CliError> {
    let ProfileRemovalConfirmation {
        profile,
        destination,
        expected_document,
    } = confirmation;
    remove_profile_from_destination(profile, destination, Some(&expected_document))
}

fn remove_profile_from_destination(
    profile: String,
    destination: MutationDestination,
    expected_document: Option<&str>,
) -> Result<ConfigRemoveReport, CliError> {
    let scope = scope_name(destination.scope);
    let cleared_default = Cell::new(false);
    let changed = transact_document(
        &destination.path,
        destination.kind,
        |document| {
            if expected_document.is_some_and(|expected| document.to_string() != expected) {
                return Err(CliError::ConfigChangedDuringConfirmation {
                    path: destination.path.clone(),
                });
            }
            let implicit_profiles = document
                .get("profile")
                .and_then(Item::as_table)
                .is_some_and(Table::is_implicit);
            let Some(profiles) = document.get_mut("profile") else {
                return Err(CliError::ProfileNotFoundInScope {
                    profile: profile.clone(),
                    scope,
                });
            };
            let profiles = profiles
                .as_table_like_mut()
                .ok_or_else(|| config_file_invalid(&destination.path, "profile must be a table"))?;
            if profiles.remove(&profile).is_none() {
                return Err(CliError::ProfileNotFoundInScope {
                    profile: profile.clone(),
                    scope,
                });
            }
            let remove_empty_profiles = implicit_profiles && profiles.is_empty();
            if remove_empty_profiles {
                document.remove("profile");
            }
            if document.get("default_profile").and_then(Item::as_str) == Some(profile.as_str()) {
                remove_document_key_preserving_prefix(document, "default_profile");
                cleared_default.set(true);
            }
            Ok(())
        },
        |document| {
            if profile_exists(document, &destination.path, &profile)? {
                return Err(config_file_invalid(
                    &destination.path,
                    "profile was not removed",
                ));
            }
            if cleared_default.get() && document.contains_key("default_profile") {
                return Err(config_file_invalid(
                    &destination.path,
                    "default profile was not cleared",
                ));
            }
            Ok(())
        },
    )?;
    Ok(ConfigRemoveReport {
        schema_version: CONFIG_REMOVE_JSON_SCHEMA_VERSION,
        profile,
        scope: destination.scope,
        path: destination.path,
        changed,
        cleared_default: cleared_default.get(),
    })
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
            let profile_table = profile_table_mut(document, &destination.path, &profile, false)?
                .ok_or_else(|| CliError::ProfileNotFoundInScope {
                    profile: profile.clone(),
                    scope: scope_name(destination.scope),
                })?;
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

fn parse_new_profile_fields(
    options: &NewProfileOptions,
) -> Result<Vec<(ConfigField, ConfigFieldValue)>, CliError> {
    let mut fields = vec![(
        ConfigField::Network,
        ConfigField::Network.parse_value(&options.network)?,
    )];
    for (field, value) in [
        (ConfigField::RpcUrl, options.rpc_url.as_deref()),
        (ConfigField::FaucetUrl, options.faucet_url.as_deref()),
        (ConfigField::DefaultKey, options.default_key.as_deref()),
    ] {
        if let Some(value) = value {
            fields.push((field, field.parse_value(value)?));
        }
    }
    Ok(fields)
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

fn profile_exists(document: &DocumentMut, path: &Path, profile: &str) -> Result<bool, CliError> {
    let Some(profiles) = document.get("profile") else {
        return Ok(false);
    };
    let profiles = profiles
        .as_table_like()
        .ok_or_else(|| config_file_invalid(path, "profile must be a table"))?;
    Ok(profiles.contains_key(profile))
}

fn remove_document_key_preserving_prefix(document: &mut DocumentMut, key: &str) -> Option<Item> {
    let prefix = document
        .as_table()
        .key(key)
        .and_then(|key| key.leaf_decor().prefix())
        .and_then(|prefix| prefix.as_str())
        .unwrap_or_default()
        .to_owned();
    let next_key = {
        let mut items = document.as_table().iter();
        items
            .find(|(candidate, _)| *candidate == key)
            .and_then(|_| items.next())
            .map(|(key, _)| key.to_owned())
    };
    let removed = document.remove(key)?;
    if prefix.is_empty() {
        return Some(removed);
    }
    if let Some(next_key) = next_key {
        let mut key = document.as_table_mut().key_mut(&next_key)?;
        let existing = key
            .leaf_decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default()
            .to_owned();
        key.leaf_decor_mut()
            .set_prefix(format!("{prefix}{existing}"));
    } else {
        let trailing = document.trailing().as_str().unwrap_or_default();
        document.set_trailing(format!("{prefix}{trailing}"));
    }
    Some(removed)
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

fn scope_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Global => "global",
        ConfigScope::Project => "project",
        ConfigScope::Resolved => "resolved",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{prepare_profile_removal, remove_confirmed_profile};

    #[test]
    fn confirmed_removal_rejects_profile_replaced_after_prompt() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("groundhog.toml");
        fs::write(&path, "[profile.staging]\nnetwork = \"devnet\"\n").unwrap();
        let confirmation = prepare_profile_removal(tmp.path(), "staging", false, true).unwrap();
        let replacement = "[profile.staging]\nnetwork = \"mainnet\"\n";
        fs::write(&path, replacement).unwrap();

        let error = remove_confirmed_profile(confirmation).unwrap_err();

        assert_eq!(error.code(), "config_changed_during_confirmation");
        assert_eq!(fs::read_to_string(path).unwrap(), replacement);
    }
}
