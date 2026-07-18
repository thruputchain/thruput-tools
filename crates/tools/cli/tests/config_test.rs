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
fn executor_credentials_default_to_sdk_username() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "[executor]\npassword = \"secret\"\n",
    )
    .unwrap();

    let loaded = groundhog_cli::config::load_executor_credentials()
        .unwrap()
        .unwrap();
    assert!(loaded.username.is_empty());
    assert_eq!(loaded.password, "secret");

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
#[cfg(unix)]
#[serial]
fn global_config_rewrite_replaces_permissive_file_with_private_permissions() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let path = global_config_path(&tmp);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "default_key = \"old\"\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    groundhog_cli::config::save_default_key("alice").unwrap();

    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("default_key = \"alice\"")
    );
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(!path.with_extension("toml.tmp").exists());

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

[profile.default.wallets.alice]
address = "0xabcd"
owner_public_key = "0x1111"
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
        profile
            .wallets
            .get("alice")
            .map(|wallet| wallet.address.as_str()),
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
fn resolve_profile_defaults_when_project_config_has_no_default_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(tmp.path().join("groundhog.toml"), "[contracts]\n").unwrap();

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

[profile.devnet.wallets.alice]
address = "{global_alice}"
owner_public_key = "0x{}"
"#,
            artifacts_dir.display(),
            "77".repeat(32)
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

[profile.devnet.wallets.alice]
address = "{project_alice}"
owner_public_key = "0x{}"

[profile.devnet.wallets.bob]
address = "{bob}"
owner_public_key = "0x{}"
"#,
            "88".repeat(32),
            "99".repeat(32)
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
    assert_eq!(
        profile.wallets.get("alice").map(|wallet| &wallet.address),
        Some(&project_alice)
    );
    assert_eq!(
        profile.wallets.get("bob").map(|wallet| &wallet.address),
        Some(&bob)
    );

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

[profile.default.wallets.alice]
address = "0x1111"
owner_public_key = "0x2222"
"#,
    )
    .unwrap();

    let addr = format!("0x{}", "22".repeat(32));
    let owner = format!("0x{}", "33".repeat(32));
    let written = groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        None,
        "alice",
        &addr,
        &owner,
    )
    .unwrap();
    assert_eq!(written, config_path);

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.rpc_url.as_deref(), Some("http://localhost:9545"));
    assert_eq!(profile.default_key.as_deref(), Some("alice"));
    assert_eq!(
        profile
            .wallets
            .get("alice")
            .map(|wallet| wallet.address.as_str()),
        Some(addr.as_str())
    );
    assert_eq!(
        profile
            .wallets
            .get("alice")
            .map(|wallet| wallet.owner_public_key.as_str()),
        Some(owner.as_str())
    );
}

#[test]
#[serial]
fn save_profile_wallet_address_creates_selected_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let addr = format!("0x{}", "be".repeat(32));
    let owner = format!("0x{}", "cd".repeat(32));
    let written = groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("testnet"),
        "alice",
        &addr,
        &owner,
    )
    .unwrap();
    assert_eq!(written, global_config_path(&tmp));

    let profiles = groundhog_cli::config::load_project_profiles(tmp.path()).unwrap();
    assert!(profiles.is_none());

    let testnet = groundhog_cli::config::resolve_profile(tmp.path(), Some("testnet")).unwrap();
    assert_eq!(
        testnet
            .wallets
            .get("alice")
            .map(|wallet| wallet.address.as_str()),
        Some(addr.as_str())
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn local_runtime_state_writes_global_and_resolves_for_local_profile() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("groundhog.toml"),
        r#"
[profile.local]
network = "local"
default_key = "dev-0"
"#,
    )
    .unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("manifests")).unwrap();
    let manifest = tmp.path().join("manifests/runtime-manifest.json");
    let artifacts_dir = tmp.path().join("hog-devnode");
    fs::write(&manifest, "{}").unwrap();
    fs::create_dir(&artifacts_dir).unwrap();

    let state = groundhog_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: manifest.clone(),
        runtime_artifacts_dir: Some(artifacts_dir.clone()),
    };
    let written = groundhog_cli::config::save_local_runtime_state(&state).unwrap();
    assert_eq!(written, global_config_path(&tmp));

    let project_config = fs::read_to_string(project.join("groundhog.toml")).unwrap();
    assert!(!project_config.contains("rpc_url"));
    assert!(!project_config.contains("runtime_manifest"));
    assert!(!project_config.contains("runtime_artifacts_dir"));

    let profile = groundhog_cli::config::resolve_profile(&project, Some("local")).unwrap();
    assert_eq!(profile.network, Some(groundhog_cli::config::Network::Local));
    assert_eq!(profile.default_key.as_deref(), Some("dev-0"));
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
fn save_local_runtime_state_keeps_global_config_private() {
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

    groundhog_cli::config::save_local_runtime_state(&groundhog_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: manifest,
        runtime_artifacts_dir: None,
    })
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
fn non_local_profile_ignores_local_runtime_state() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"
[profile.devnet]
network = "devnet"
rpc_url = "https://rpc.devnet.example"
"#,
    )
    .unwrap();
    let manifest = tmp.path().join("runtime-manifest.json");
    fs::write(&manifest, "{}").unwrap();

    groundhog_cli::config::save_local_runtime_state(&groundhog_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9546".into(),
        runtime_manifest: manifest,
        runtime_artifacts_dir: None,
    })
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("devnet")).unwrap();
    assert_eq!(
        profile.network,
        Some(groundhog_cli::config::Network::Devnet)
    );
    assert_eq!(
        profile.rpc_url.as_deref(),
        Some("https://rpc.devnet.example")
    );
    assert!(profile.runtime_manifest.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn local_profile_explicit_fields_override_local_runtime_state() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let profile_manifest = tmp.path().join("profile-runtime-manifest.json");
    let state_manifest = tmp.path().join("state-runtime-manifest.json");
    fs::write(&profile_manifest, "{}").unwrap();
    fs::write(&state_manifest, "{}").unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        format!(
            r#"
[profile.local]
network = "local"
rpc_url = "http://127.0.0.1:9999"
runtime_manifest = "{}"
"#,
            profile_manifest.display()
        ),
    )
    .unwrap();

    groundhog_cli::config::save_local_runtime_state(&groundhog_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: state_manifest,
        runtime_artifacts_dir: None,
    })
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("local")).unwrap();
    assert_eq!(profile.rpc_url.as_deref(), Some("http://127.0.0.1:9999"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(profile_manifest.as_path())
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn clear_local_runtime_state_removes_matching_state() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let manifest = tmp.path().join("runtime-manifest.json");
    fs::write(&manifest, "{}").unwrap();
    let state = groundhog_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: manifest,
        runtime_artifacts_dir: None,
    };

    groundhog_cli::config::save_local_runtime_state(&state).unwrap();
    groundhog_cli::config::clear_local_runtime_state_if_matches(&state).unwrap();

    let config = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(!config.contains("local_runtime_state"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn clear_local_runtime_state_preserves_changed_state() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let first_manifest = tmp.path().join("runtime-manifest-first.json");
    let second_manifest = tmp.path().join("runtime-manifest-second.json");
    fs::write(&first_manifest, "{}").unwrap();
    fs::write(&second_manifest, "{}").unwrap();
    let first = groundhog_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: first_manifest,
        runtime_artifacts_dir: None,
    };
    let second = groundhog_cli::config::LocalRuntimeState {
        node_id: "node-2".into(),
        rpc_url: "http://127.0.0.1:9546".into(),
        runtime_manifest: second_manifest.clone(),
        runtime_artifacts_dir: None,
    };

    groundhog_cli::config::save_local_runtime_state(&first).unwrap();
    groundhog_cli::config::save_local_runtime_state(&second).unwrap();
    groundhog_cli::config::clear_local_runtime_state_if_matches(&first).unwrap();

    fs::write(
        tmp.path().join("groundhog.toml"),
        "[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();
    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("local")).unwrap();
    assert_eq!(profile.rpc_url.as_deref(), Some("http://127.0.0.1:9546"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(second_manifest.as_path())
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
fn resolve_default_key_uses_builtin_dev_key_for_local_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let runtime_manifest = tmp.path().join("runtime-manifest.json");
    fs::write(&runtime_manifest, "{}").unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"
default_profile = "local"

[profile.local]
network = "local"
"#,
    )
    .unwrap();
    groundhog_cli::config::save_local_runtime_state(&groundhog_cli::config::LocalRuntimeState {
        node_id: "local-node".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest,
        runtime_artifacts_dir: None,
    })
    .unwrap();

    let resolved = groundhog_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert_eq!(resolved.as_deref(), Some("dev-0"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_default_key_does_not_use_builtin_dev_key_for_overridden_local_rpc() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let runtime_manifest = tmp.path().join("runtime-manifest.json");
    fs::write(&runtime_manifest, "{}").unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"
default_profile = "local"

[profile.local]
network = "local"
rpc_url = "https://rpc.example.test"
"#,
    )
    .unwrap();
    groundhog_cli::config::save_local_runtime_state(&groundhog_cli::config::LocalRuntimeState {
        node_id: "local-node".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest,
        runtime_artifacts_dir: None,
    })
    .unwrap();

    let resolved = groundhog_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert!(resolved.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_default_key_does_not_use_builtin_dev_key_for_devnet_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"
default_profile = "devnet"

[profile.devnet]
network = "devnet"
"#,
    )
    .unwrap();

    let resolved = groundhog_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert!(resolved.is_none());

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
    let owner = format!("0x{}", "cd".repeat(32));
    let result =
        groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "", &addr, &owner);
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_wallet_rejects_empty_address() {
    let tmp = TempDir::new().unwrap();
    let owner = format!("0x{}", "cd".repeat(32));
    let result =
        groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", "", &owner);
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_wallet_rejects_malformed_address() {
    let tmp = TempDir::new().unwrap();
    let owner = format!("0x{}", "cd".repeat(32));
    let result = groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        None,
        "alice",
        "banana",
        &owner,
    );
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_wallet_accepts_valid_hex_address() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let addr = format!("0x{}", "ab".repeat(32));
    let owner = format!("0x{}", "cd".repeat(32));
    let result = groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        None,
        "alice",
        &addr,
        &owner,
    );
    assert!(result.is_ok());
    assert!(global_config_path(&tmp).exists());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_rejects_legacy_string_wallet_binding() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        format!(
            r#"
[profile.default.wallets]
alice = "0x{}"
"#,
            "ab".repeat(32)
        ),
    )
    .unwrap();

    let result = groundhog_cli::config::resolve_profile(tmp.path(), None);

    assert!(result.is_err());
}

#[test]
#[serial]
fn resolve_profile_accepts_wallet_table_missing_owner_for_stable_validation() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        format!(
            r#"
default_profile = "default"

[profile.default.wallets.alice]
address = "0x{}"
"#,
            "ab".repeat(32)
        ),
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();

    assert_eq!(
        profile
            .wallets
            .get("alice")
            .map(|wallet| wallet.owner_public_key.as_str()),
        Some("")
    );
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
    let owner_a = format!("0x{}", "cc".repeat(32));
    let owner_b = format!("0x{}", "dd".repeat(32));

    groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        None,
        "alice",
        &addr_a,
        &owner_a,
    )
    .unwrap();
    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "bob", &addr_b, &owner_b)
        .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(
        profile
            .wallets
            .get("alice")
            .map(|wallet| wallet.address.as_str()),
        Some(addr_a.as_str())
    );
    assert_eq!(
        profile
            .wallets
            .get("bob")
            .map(|wallet| wallet.address.as_str()),
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
    let owner_a = format!("0x{}", "cc".repeat(32));
    let owner_b = format!("0x{}", "dd".repeat(32));

    groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        None,
        "alice",
        &addr_a,
        &owner_a,
    )
    .unwrap();
    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "bob", &addr_b, &owner_b)
        .unwrap();
    groundhog_cli::config::remove_profile_wallet_address(tmp.path(), None, "alice").unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert!(!profile.wallets.contains_key("alice"));
    assert_eq!(
        profile
            .wallets
            .get("bob")
            .map(|wallet| wallet.address.as_str()),
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
    let owner = format!("0x{}", "bb".repeat(32));

    groundhog_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr, &owner)
        .unwrap();
    let result = groundhog_cli::config::remove_profile_wallet_address(tmp.path(), None, "bob");

    assert!(result.is_err());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}
