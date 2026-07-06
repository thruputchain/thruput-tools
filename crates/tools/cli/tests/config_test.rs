use serial_test::serial;
use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::TempDir;

fn global_config_path(tmp: &TempDir) -> PathBuf {
    tmp.path().join("groundhog/config.toml")
}

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
default_profile = "default"

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
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.name, "default");
    assert!(profile.rpc_url.is_none());
    assert!(profile.default_key.is_none());
    assert!(profile.contracts.is_empty());
    assert!(profile.wallets.is_empty());
    assert!(profile.source.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_uses_global_default_profile_when_no_project_config_exists() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        tmp.path().join("groundhog/config.toml"),
        r#"
default_profile = "local"

[profile.local]
rpc_url = "http://127.0.0.1:9545"
default_key = "dev-0"
"#,
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.name, "local");
    assert_eq!(profile.rpc_url.as_deref(), Some("http://127.0.0.1:9545"));
    assert_eq!(profile.default_key.as_deref(), Some("dev-0"));
    assert_eq!(
        profile.source,
        Some(tmp.path().join("groundhog/config.toml"))
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_project_default_profile_overrides_global_default_profile() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        tmp.path().join("groundhog/config.toml"),
        r#"
default_profile = "local"

[profile.local]
rpc_url = "http://127.0.0.1:9545"

[profile.devnet]
rpc_url = "https://rpc.devnet.example"
"#,
    )
    .unwrap();
    fs::write(
        project.join("groundhog.toml"),
        r#"
default_profile = "devnet"

[profile.devnet]
default_key = "alice"
"#,
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(&project, None).unwrap();
    assert_eq!(profile.name, "devnet");
    assert_eq!(
        profile.rpc_url.as_deref(),
        Some("https://rpc.devnet.example")
    );
    assert_eq!(profile.default_key.as_deref(), Some("alice"));
    assert_eq!(profile.source, Some(project.join("groundhog.toml")));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_merges_global_and_project_profile_fields() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let global_hog = format!("0x{}", "11".repeat(32));
    let project_hog = format!("0x{}", "22".repeat(32));
    let counter = format!("0x{}", "33".repeat(32));
    let global_alice = format!("0x{}", "44".repeat(32));
    let project_alice = format!("0x{}", "55".repeat(32));
    let bob = format!("0x{}", "66".repeat(32));
    let artifacts_dir = tmp.path().join("global-artifacts");
    fs::create_dir(&project).unwrap();
    fs::create_dir(&artifacts_dir).unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        tmp.path().join("groundhog/config.toml"),
        format!(
            r#"
[profile.devnet]
rpc_url = "https://rpc.devnet.example"
runtime_manifest = "global-runtime.json"
runtime_artifacts_dir = "{}"

[profile.devnet.contracts]
HOG = "{global_hog}"

[profile.devnet.wallets]
alice = "{global_alice}"
"#,
            artifacts_dir.display()
        ),
    )
    .unwrap();
    fs::write(
        project.join("groundhog.toml"),
        format!(
            r#"
[profile.devnet]
rpc_url = "https://project-rpc.devnet.example"
default_key = "bob"

[profile.devnet.contracts]
HOG = "{project_hog}"
counter = "{counter}"

[profile.devnet.wallets]
alice = "{project_alice}"
bob = "{bob}"
"#
        ),
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(&project, Some("devnet")).unwrap();
    assert_eq!(
        profile.rpc_url.as_deref(),
        Some("https://project-rpc.devnet.example")
    );
    assert_eq!(profile.default_key.as_deref(), Some("bob"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(tmp.path().join("groundhog/global-runtime.json").as_path())
    );
    assert_eq!(
        profile.runtime_artifacts_dir.as_deref(),
        Some(artifacts_dir.as_path())
    );
    assert_eq!(profile.contracts.get("HOG"), Some(&project_hog));
    assert_eq!(profile.contracts.get("counter"), Some(&counter));
    assert_eq!(profile.wallets.get("alice"), Some(&project_alice));
    assert_eq!(profile.wallets.get("bob"), Some(&bob));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_profile_contract_alias_updates_existing_profile() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("groundhog.toml");
    fs::write(
        &config_path,
        r#"
default_profile = "default"

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
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let addr = format!("0x{}", "be".repeat(32));
    let written = groundhog_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("testnet"),
        "greeter",
        &addr,
    )
    .unwrap();
    assert_eq!(written, global_config_path(&tmp));

    let profiles = groundhog_cli::config::load_project_profiles(tmp.path()).unwrap();
    assert!(profiles.is_none());

    let testnet = groundhog_cli::config::resolve_profile(tmp.path(), Some("testnet")).unwrap();
    assert_eq!(
        testnet.contracts.get("greeter").map(String::as_str),
        Some(addr.as_str())
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_profile_wallet_address_updates_existing_profile() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("groundhog.toml");
    fs::write(
        &config_path,
        r#"
default_profile = "default"

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
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let addr = format!("0x{}", "be".repeat(32));
    let written = groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("testnet"),
        "alice",
        &addr,
    )
    .unwrap();
    assert_eq!(written, global_config_path(&tmp));

    let profiles = groundhog_cli::config::load_project_profiles(tmp.path()).unwrap();
    assert!(profiles.is_none());

    let testnet = groundhog_cli::config::resolve_profile(tmp.path(), Some("testnet")).unwrap();
    assert_eq!(
        testnet.wallets.get("alice").map(String::as_str),
        Some(addr.as_str())
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_profile_local_node_state_writes_global_when_no_project_config_exists() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("manifests")).unwrap();
    let manifest = tmp.path().join("manifests/runtime-manifest.json");
    let artifacts_dir = tmp.path().join("hog-devnode");
    fs::write(&manifest, "{}").unwrap();
    fs::create_dir(&artifacts_dir).unwrap();

    let written = groundhog_cli::config::save_profile_local_node_state(
        tmp.path(),
        Some("myfaveprofile"),
        "http://127.0.0.1:9545",
        &manifest,
        Some(&artifacts_dir),
    )
    .unwrap();
    assert_eq!(written, global_config_path(&tmp));

    let profile =
        groundhog_cli::config::resolve_profile(tmp.path(), Some("myfaveprofile")).unwrap();
    assert_eq!(profile.rpc_url.as_deref(), Some("http://127.0.0.1:9545"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(manifest.as_path())
    );
    assert_eq!(
        profile.runtime_artifacts_dir.as_deref(),
        Some(artifacts_dir.as_path())
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[cfg(unix)]
#[serial]
fn save_profile_local_node_state_keeps_global_config_private() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "alice".into(),
        password: "secret".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();
    fs::create_dir_all(tmp.path().join("manifests")).unwrap();
    let manifest = tmp.path().join("manifests/runtime-manifest.json");
    fs::write(&manifest, "{}").unwrap();

    groundhog_cli::config::save_profile_local_node_state(
        tmp.path(),
        Some("local"),
        "http://127.0.0.1:9545",
        &manifest,
        None,
    )
    .unwrap();

    let mode = fs::metadata(global_config_path(&tmp))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_profile_local_node_state_writes_project_when_project_config_exists() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("groundhog.toml"),
        r#"
[profile.local]
default_key = "dev-0"
"#,
    )
    .unwrap();
    let manifest = tmp.path().join("runtime-manifest.json");
    fs::write(&manifest, "{}").unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let written = groundhog_cli::config::save_profile_local_node_state(
        &project,
        Some("local"),
        "http://127.0.0.1:9546",
        &manifest,
        None,
    )
    .unwrap();
    assert_eq!(written, project.join("groundhog.toml"));

    let profile = groundhog_cli::config::resolve_profile(&project, Some("local")).unwrap();
    assert_eq!(profile.default_key.as_deref(), Some("dev-0"));
    assert_eq!(profile.rpc_url.as_deref(), Some("http://127.0.0.1:9546"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(manifest.as_path())
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn clear_profile_local_node_state_removes_matching_runtime_fields() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("groundhog.toml"),
        r#"
[profile.local]
default_key = "dev-0"

[profile.local.contracts]
HOG = "0x1111"
"#,
    )
    .unwrap();
    let manifest = tmp.path().join("runtime-manifest.json");
    let artifacts_dir = tmp.path().join("hog-devnode");
    fs::write(&manifest, "{}").unwrap();
    fs::create_dir(&artifacts_dir).unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    groundhog_cli::config::save_profile_local_node_state(
        &project,
        Some("local"),
        "http://127.0.0.1:9545",
        &manifest,
        Some(&artifacts_dir),
    )
    .unwrap();
    let written = groundhog_cli::config::clear_profile_local_node_state_if_matches(
        &project,
        Some("local"),
        "http://127.0.0.1:9545",
        &manifest,
        Some(&artifacts_dir),
    )
    .unwrap();
    assert_eq!(written, project.join("groundhog.toml"));

    let profile = groundhog_cli::config::resolve_profile(&project, Some("local")).unwrap();
    assert_eq!(profile.default_key.as_deref(), Some("dev-0"));
    assert!(profile.rpc_url.is_none());
    assert!(profile.runtime_manifest.is_none());
    assert!(profile.runtime_artifacts_dir.is_none());
    assert_eq!(
        profile.contracts.get("HOG").map(String::as_str),
        Some("0x1111")
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn clear_profile_local_node_state_preserves_changed_runtime_fields() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("groundhog.toml"), "[profile.local]\n").unwrap();
    let first_manifest = tmp.path().join("runtime-manifest-first.json");
    let second_manifest = tmp.path().join("runtime-manifest-second.json");
    let first_artifacts = tmp.path().join("hog-devnode-first");
    let second_artifacts = tmp.path().join("hog-devnode-second");
    fs::write(&first_manifest, "{}").unwrap();
    fs::write(&second_manifest, "{}").unwrap();
    fs::create_dir(&first_artifacts).unwrap();
    fs::create_dir(&second_artifacts).unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    groundhog_cli::config::save_profile_local_node_state(
        &project,
        Some("local"),
        "http://127.0.0.1:9545",
        &first_manifest,
        Some(&first_artifacts),
    )
    .unwrap();
    groundhog_cli::config::save_profile_local_node_state(
        &project,
        Some("local"),
        "http://127.0.0.1:9546",
        &second_manifest,
        Some(&second_artifacts),
    )
    .unwrap();

    groundhog_cli::config::clear_profile_local_node_state_if_matches(
        &project,
        Some("local"),
        "http://127.0.0.1:9545",
        &first_manifest,
        Some(&first_artifacts),
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(&project, Some("local")).unwrap();
    assert_eq!(profile.rpc_url.as_deref(), Some("http://127.0.0.1:9546"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(second_manifest.as_path())
    );
    assert_eq!(
        profile.runtime_artifacts_dir.as_deref(),
        Some(second_artifacts.as_path())
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
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
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let addr = format!("0x{}", "ab".repeat(32));
    let result =
        groundhog_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", &addr);
    assert!(result.is_ok());
    assert!(global_config_path(&tmp).exists());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
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
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let addr = format!("0x{}", "ab".repeat(32));
    let result =
        groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr);
    assert!(result.is_ok());
    assert!(global_config_path(&tmp).exists());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_second_alias_preserves_first() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
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

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_second_wallet_preserves_first() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
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

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn remove_wallet_deletes_saved_entry() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
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

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn remove_wallet_rejects_missing_entry() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let addr = format!("0x{}", "aa".repeat(32));

    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr).unwrap();
    let result = groundhog_cli::config::remove_profile_wallet_address(tmp.path(), None, "bob");

    assert!(result.is_err());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}
