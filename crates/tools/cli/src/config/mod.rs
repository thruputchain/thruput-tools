mod atomic;
mod dirs;
mod global;
mod mutate;
mod project;
mod read;

pub use dirs::{cache_dir, config_dir, home_dir, keys_dir};
pub(crate) use global::load_global_profile;
pub use global::{
    ExecutorCredentials, LocalRuntimeState, clear_default_key,
    clear_local_runtime_state_if_matches, initialize_global_profiles, load_default_key,
    load_executor_credentials, save_default_key, save_executor_credentials,
    save_local_runtime_state,
};
pub use mutate::{
    CONFIG_MUTATION_JSON_SCHEMA_VERSION, ConfigMutationReport, set_profile_field,
    unset_profile_field, use_profile,
};
pub use project::{
    Network, ProfileOverrides, ProjectProfile, ResolvedProfile, SavedWalletBinding,
    active_profile_name, find_project_config, load_project_profiles, remove_profile_wallet_address,
    resolve_default_key, resolve_default_key_for_profile, resolve_profile,
    resolve_profile_with_overrides, save_profile_contract_alias, save_profile_default_wallet,
    save_profile_wallet_address, save_profile_wallet_address_with_default,
};
pub use read::{
    CONFIG_GET_JSON_SCHEMA_VERSION, CONFIG_PROFILES_JSON_SCHEMA_VERSION, ConfigField,
    ConfigFieldValue, ConfigGetReport, ConfigProfileRow, ConfigProfileStatus, ConfigProfilesReport,
    ConfigScope, ConfigSource, ConfigSourceKind, ConfigWalletBinding, PersistedConfig,
    PersistedConfigLayer, PersistedProfile, PersistedProfileState, ProfileSelection,
    ProfileSources, load_persisted_config, resolve_persisted_profile, validate_profile_name,
};
