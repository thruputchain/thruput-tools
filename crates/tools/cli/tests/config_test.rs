use serial_test::serial;
use std::env;
use std::fs;
use tempfile::TempDir;

#[test]
#[serial]
fn config_dir_respects_xdg_env() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("custom_config");
    unsafe { env::set_var("XDG_CONFIG_HOME", &path) };
    let result = groundhog_cli::config::config_dir().unwrap();
    assert_eq!(result, path.join("groundhog"));
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn cache_dir_respects_xdg_env() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("custom_cache");
    unsafe { env::set_var("XDG_CACHE_HOME", &path) };
    let result = groundhog_cli::config::cache_dir().unwrap();
    assert_eq!(result, path.join("groundhog"));
    unsafe { env::remove_var("XDG_CACHE_HOME") };
}

#[test]
#[serial]
fn roundtrip_executor_credentials() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "alice".into(),
        password: "secret".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();

    let loaded = groundhog_cli::config::load_executor_credentials()
        .unwrap()
        .unwrap();
    assert_eq!(loaded.username, "alice");
    assert_eq!(loaded.password, "secret");

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn remove_executor_credentials_clears_them() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "alice".into(),
        password: "secret".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();
    groundhog_cli::config::remove_executor_credentials().unwrap();

    let loaded = groundhog_cli::config::load_executor_credentials().unwrap();
    assert!(loaded.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn load_returns_none_when_no_config() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("nonexistent")) };

    let loaded = groundhog_cli::config::load_executor_credentials().unwrap();
    assert!(loaded.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn roundtrip_default_key() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    groundhog_cli::config::save_default_key("alice").unwrap();
    let loaded = groundhog_cli::config::load_default_key().unwrap();
    assert_eq!(loaded.as_deref(), Some("alice"));

    groundhog_cli::config::clear_default_key().unwrap();
    let cleared = groundhog_cli::config::load_default_key().unwrap();
    assert!(cleared.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_loads_default_project_profile() {
    let tmp = TempDir::new().unwrap();
    let direct_addr = format!("0x{}", "bb".repeat(32));
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"
[profile.default]
rpc_url = "http://localhost:9545"
default_key = "alice"
runtime_manifest = "runtime-manifest.json"

[profile.default.contracts]
counter = "0x1234"

[profile.default.wallets]
alice = "0xabcd"
"#,
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.name, "default");
    assert_eq!(profile.rpc_url.as_deref(), Some("http://localhost:9545"));
    assert_eq!(profile.default_key.as_deref(), Some("alice"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(std::path::Path::new("runtime-manifest.json"))
    );
    assert_eq!(
        profile.contracts.get("counter").map(String::as_str),
        Some("0x1234")
    );
    assert_eq!(
        profile.wallets.get("alice").map(String::as_str),
        Some("0xabcd")
    );
    assert_eq!(profile.resolve_contract("counter"), "0x1234");
    assert_eq!(profile.resolve_contract("0xabcd"), "0xabcd");
    assert_eq!(profile.resolve_wallet("alice").as_deref(), Some("0xabcd"));
    assert_eq!(
        profile.resolve_wallet(&direct_addr).as_deref(),
        Some(direct_addr.as_str())
    );
    assert!(profile.resolve_wallet("missing").is_none());
    assert_eq!(profile.source, Some(tmp.path().join("groundhog.toml")));
}

#[test]
#[serial]
fn resolve_profile_uses_env_override() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"
[profile.default]
rpc_url = "http://localhost:9545"

[profile.testnet]
rpc_url = "https://testnet.groundhog.network"
faucet_url = "https://faucet.testnet.groundhog.network"
default_key = "bob"

[profile.testnet.contracts]
greeter = "0xbeef"
"#,
    )
    .unwrap();
    unsafe { env::set_var("GROUNDHOG_PROFILE", "testnet") };

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.name, "testnet");
    assert_eq!(
        profile.rpc_url.as_deref(),
        Some("https://testnet.groundhog.network")
    );
    assert_eq!(
        profile.faucet_url.as_deref(),
        Some("https://faucet.testnet.groundhog.network")
    );
    assert_eq!(profile.default_key.as_deref(), Some("bob"));
    assert_eq!(
        profile.contracts.get("greeter").map(String::as_str),
        Some("0xbeef")
    );

    unsafe { env::remove_var("GROUNDHOG_PROFILE") };
}

#[test]
#[serial]
fn resolve_profile_defaults_when_no_project_config_exists() {
    let tmp = TempDir::new().unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.name, "default");
    assert!(profile.rpc_url.is_none());
    assert!(profile.default_key.is_none());
    assert!(profile.contracts.is_empty());
    assert!(profile.wallets.is_empty());
    assert!(profile.source.is_none());
}

#[test]
#[serial]
fn save_profile_contract_alias_updates_existing_profile() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("groundhog.toml");
    fs::write(
        &config_path,
        r#"
[profile.default]
rpc_url = "http://localhost:9545"
default_key = "alice"

[profile.default.contracts]
counter = "0x1111"
"#,
    )
    .unwrap();

    let addr = format!("0x{}", "22".repeat(32));
    let written =
        groundhog_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", &addr)
            .unwrap();
    assert_eq!(written, config_path);

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.rpc_url.as_deref(), Some("http://localhost:9545"));
    assert_eq!(profile.default_key.as_deref(), Some("alice"));
    assert_eq!(
        profile.contracts.get("counter").map(String::as_str),
        Some(addr.as_str())
    );
}

#[test]
#[serial]
fn save_profile_contract_alias_creates_selected_profile() {
    let tmp = TempDir::new().unwrap();

    let addr = format!("0x{}", "be".repeat(32));
    let written = groundhog_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("testnet"),
        "greeter",
        &addr,
    )
    .unwrap();
    assert_eq!(written, tmp.path().join("groundhog.toml"));

    let profiles = groundhog_cli::config::load_project_profiles(tmp.path()).unwrap();
    let profiles = profiles.unwrap();
    assert!(!profiles.contains_key("default"));

    let testnet = groundhog_cli::config::resolve_profile(tmp.path(), Some("testnet")).unwrap();
    assert_eq!(
        testnet.contracts.get("greeter").map(String::as_str),
        Some(addr.as_str())
    );
}

#[test]
#[serial]
fn save_profile_wallet_address_updates_existing_profile() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("groundhog.toml");
    fs::write(
        &config_path,
        r#"
[profile.default]
rpc_url = "http://localhost:9545"
default_key = "alice"

[profile.default.wallets]
alice = "0x1111"
"#,
    )
    .unwrap();

    let addr = format!("0x{}", "22".repeat(32));
    let written =
        groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr)
            .unwrap();
    assert_eq!(written, config_path);

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.rpc_url.as_deref(), Some("http://localhost:9545"));
    assert_eq!(profile.default_key.as_deref(), Some("alice"));
    assert_eq!(
        profile.wallets.get("alice").map(String::as_str),
        Some(addr.as_str())
    );
}

#[test]
#[serial]
fn save_profile_wallet_address_creates_selected_profile() {
    let tmp = TempDir::new().unwrap();

    let addr = format!("0x{}", "be".repeat(32));
    let written = groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("testnet"),
        "alice",
        &addr,
    )
    .unwrap();
    assert_eq!(written, tmp.path().join("groundhog.toml"));

    let profiles = groundhog_cli::config::load_project_profiles(tmp.path()).unwrap();
    let profiles = profiles.unwrap();
    assert!(!profiles.contains_key("default"));

    let testnet = groundhog_cli::config::resolve_profile(tmp.path(), Some("testnet")).unwrap();
    assert_eq!(
        testnet.wallets.get("alice").map(String::as_str),
        Some(addr.as_str())
    );
}

#[test]
#[serial]
fn resolve_default_key_falls_back_to_global_selection() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"
[profile.default]
rpc_url = "http://localhost:9545"
"#,
    )
    .unwrap();

    groundhog_cli::config::save_default_key("alice").unwrap();

    let resolved = groundhog_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert_eq!(resolved.as_deref(), Some("alice"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_default_key_prefers_profile_value_over_global_selection() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"
[profile.default]
rpc_url = "http://localhost:9545"
default_key = "project-key"
"#,
    )
    .unwrap();

    groundhog_cli::config::save_default_key("global-key").unwrap();

    let resolved = groundhog_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert_eq!(resolved.as_deref(), Some("project-key"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_alias_rejects_empty_alias() {
    let tmp = TempDir::new().unwrap();
    let addr = format!("0x{}", "ab".repeat(32));
    let result = groundhog_cli::config::save_profile_contract_alias(tmp.path(), None, "", &addr);
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_alias_rejects_empty_address() {
    let tmp = TempDir::new().unwrap();
    let result =
        groundhog_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", "");
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_alias_rejects_malformed_address() {
    let tmp = TempDir::new().unwrap();
    let result =
        groundhog_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", "banana");
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_alias_accepts_valid_hex_address() {
    let tmp = TempDir::new().unwrap();
    let addr = format!("0x{}", "ab".repeat(32));
    let result =
        groundhog_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", &addr);
    assert!(result.is_ok());
}

#[test]
#[serial]
fn save_wallet_rejects_empty_name() {
    let tmp = TempDir::new().unwrap();
    let addr = format!("0x{}", "ab".repeat(32));
    let result = groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "", &addr);
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_wallet_rejects_empty_address() {
    let tmp = TempDir::new().unwrap();
    let result = groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", "");
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_wallet_rejects_malformed_address() {
    let tmp = TempDir::new().unwrap();
    let result =
        groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", "banana");
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_wallet_accepts_valid_hex_address() {
    let tmp = TempDir::new().unwrap();
    let addr = format!("0x{}", "ab".repeat(32));
    let result =
        groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr);
    assert!(result.is_ok());
}

#[test]
#[serial]
fn save_second_alias_preserves_first() {
    let tmp = TempDir::new().unwrap();
    let addr_a = format!("0x{}", "aa".repeat(32));
    let addr_b = format!("0x{}", "bb".repeat(32));

    groundhog_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", &addr_a)
        .unwrap();
    groundhog_cli::config::save_profile_contract_alias(tmp.path(), None, "greeter", &addr_b)
        .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(
        profile.contracts.get("counter").map(String::as_str),
        Some(addr_a.as_str())
    );
    assert_eq!(
        profile.contracts.get("greeter").map(String::as_str),
        Some(addr_b.as_str())
    );
}

#[test]
#[serial]
fn save_second_wallet_preserves_first() {
    let tmp = TempDir::new().unwrap();
    let addr_a = format!("0x{}", "aa".repeat(32));
    let addr_b = format!("0x{}", "bb".repeat(32));

    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr_a).unwrap();
    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "bob", &addr_b).unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(
        profile.wallets.get("alice").map(String::as_str),
        Some(addr_a.as_str())
    );
    assert_eq!(
        profile.wallets.get("bob").map(String::as_str),
        Some(addr_b.as_str())
    );
}

#[test]
#[serial]
fn remove_wallet_deletes_saved_entry() {
    let tmp = TempDir::new().unwrap();
    let addr_a = format!("0x{}", "aa".repeat(32));
    let addr_b = format!("0x{}", "bb".repeat(32));

    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr_a).unwrap();
    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "bob", &addr_b).unwrap();
    groundhog_cli::config::remove_profile_wallet_address(tmp.path(), None, "alice").unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert!(!profile.wallets.contains_key("alice"));
    assert_eq!(
        profile.wallets.get("bob").map(String::as_str),
        Some(addr_b.as_str())
    );
}

#[test]
#[serial]
fn remove_wallet_rejects_missing_entry() {
    let tmp = TempDir::new().unwrap();
    let addr = format!("0x{}", "aa".repeat(32));

    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr).unwrap();
    let result = groundhog_cli::config::remove_profile_wallet_address(tmp.path(), None, "bob");

    assert!(result.is_err());
}
