mod atomic;
mod dirs;
mod global;
mod mutate;
mod project;
mod read;

fn line_column(contents: &str, offset: usize) -> (usize, usize) {
    let prefix = contents.get(..offset).unwrap_or(contents);
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let current_line = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, current_line)| current_line);
    (line, current_line.chars().count() + 1)
}

pub(crate) use dirs::cargo_config_path;

pub use dirs::{cache_dir, config_dir, home_dir, keys_dir};
pub use global::{
    LocalRuntimeState, clear_default_key, clear_local_runtime_state_if_matches,
    initialize_global_profiles, load_default_key, save_default_key, save_local_runtime_state,
};
pub use mutate::{
    CONFIG_MUTATION_JSON_SCHEMA_VERSION, CONFIG_NEW_JSON_SCHEMA_VERSION,
    CONFIG_REMOVE_JSON_SCHEMA_VERSION, ConfigMutationReport, ConfigNewReport, ConfigRemoveReport,
    NewProfileOptions, create_profile, remove_profile, set_profile_field, unset_profile_field,
    use_profile,
};
pub(crate) use mutate::{prepare_profile_removal, remove_confirmed_profile};
pub use project::{
    Network, ProfileOverrides, ProjectProfile, ResolvedProfile, SavedWalletBinding,
    active_profile_name, find_project_config, load_project_profiles, parse_contract_alias,
    remove_profile_wallet_address, resolve_default_key, resolve_default_key_for_profile,
    resolve_profile, resolve_profile_with_overrides, save_profile_contract_alias,
    save_profile_default_wallet, save_profile_wallet_address,
    save_profile_wallet_address_with_default,
};
pub(crate) use project::{prepare_profile_wallet_removal, remove_prepared_profile_wallet};
pub use read::{
    CONFIG_GET_JSON_SCHEMA_VERSION, CONFIG_PROFILES_JSON_SCHEMA_VERSION, ConfigField,
    ConfigFieldValue, ConfigGetReport, ConfigProfileRow, ConfigProfileStatus, ConfigProfilesReport,
    ConfigScope, ConfigSource, ConfigSourceKind, ConfigWalletBinding, PersistedConfig,
    PersistedConfigLayer, PersistedProfile, PersistedProfileState, ProfileSelection,
    ProfileSources, load_persisted_config, resolve_persisted_profile, validate_profile_name,
};
