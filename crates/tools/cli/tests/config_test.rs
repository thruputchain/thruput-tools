use serial_test::serial;
use std::env;
use std::fs;
#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::TempDir;

fn global_config_path(tmp: &TempDir) -> PathBuf {
    tmp.path().join("groundhog/config.toml")
}

#[cfg(unix)]
fn lock_config(path: &std::path::Path) -> File {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(lock_path))
        .expect("config lock should open");
    // SAFETY: `file` owns a valid descriptor for the duration of this call.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(result, 0, "config lock should be acquired");
    file
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
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "# retained\nplugin = true\n\n[executor]\npassword = \"old\" # password comment\nplugin_token = \"keep\"\n",
    )
    .unwrap();

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
    let contents = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(contents.contains("# retained"));
    assert!(contents.contains("plugin = true"));
    assert!(contents.contains("plugin_token = \"keep\""));
    assert!(contents.contains("# password comment"));

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
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "# retained\nplugin = \"keep\"\ndefault_key = \"old\" # key comment\n",
    )
    .unwrap();

    groundhog_cli::config::save_default_key("alice").unwrap();
    let loaded = groundhog_cli::config::load_default_key().unwrap();
    assert_eq!(loaded.as_deref(), Some("alice"));
    assert!(
        fs::read_to_string(global_config_path(&tmp))
            .unwrap()
            .contains("# key comment")
    );

    groundhog_cli::config::clear_default_key().unwrap();
    let cleared = groundhog_cli::config::load_default_key().unwrap();
    assert!(cleared.is_none());
    let contents = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(contents.contains("# retained"));
    assert!(contents.contains("plugin = \"keep\""));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
fn project_profile_default_wallet_roundtrips() {
    let profile = groundhog_cli::config::ProjectProfile {
        default_wallet: Some("alice-wallet".into()),
        ..Default::default()
    };

    let encoded = toml::to_string(&profile).unwrap();
    let decoded: groundhog_cli::config::ProjectProfile = toml::from_str(&encoded).unwrap();

    assert_eq!(decoded, profile);
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
default_wallet = "alice-wallet"
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

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
    assert_eq!(profile.name, "default");
    assert_eq!(profile.rpc_url.as_deref(), Some("http://localhost:9545"));
    assert_eq!(profile.default_key.as_deref(), Some("alice"));
    assert_eq!(profile.default_wallet.as_deref(), Some("alice-wallet"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(tmp.path().join("runtime-manifest.json").as_path())
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
fn resolve_profile_requires_selection_when_no_config_exists() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let error = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap_err();
    assert_eq!(error.code(), "no_active_profile");

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_requires_selection_when_project_has_no_default_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(tmp.path().join("groundhog.toml"), "[contracts]\n").unwrap();

    let error = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap_err();
    assert_eq!(error.code(), "no_active_profile");

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
default_wallet = "global-wallet"
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
default_wallet = "project-wallet"

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
    assert_eq!(profile.default_wallet.as_deref(), Some("project-wallet"));
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
fn save_profile_default_wallet_preserves_existing_formatting() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("groundhog.toml");
    fs::write(
        &config_path,
        r#"# keep this project comment
default_profile = "default"

[profile.default]
rpc_url = "http://localhost:9545" # keep this field comment
"#,
    )
    .unwrap();

    let written =
        groundhog_cli::config::save_profile_default_wallet(tmp.path(), None, " alice-wallet ")
            .unwrap();

    assert_eq!(written, config_path);
    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("# keep this project comment"));
    assert!(contents.contains("rpc_url = \"http://localhost:9545\" # keep this field comment"));
    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.default_wallet.as_deref(), Some("alice-wallet"));
}

#[test]
#[serial]
fn save_wallet_and_default_uses_one_profile_update() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("groundhog.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();
    let address = format!("0x{}", "aa".repeat(32));
    let owner = format!("0x{}", "bb".repeat(32));

    groundhog_cli::config::save_profile_wallet_address_with_default(
        tmp.path(),
        None,
        "alice",
        &address,
        &owner,
        true,
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.default_wallet.as_deref(), Some("alice"));
    assert_eq!(profile.wallets["alice"].address, address);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_profile_contract_alias_updates_existing_profile() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("groundhog.toml");
    fs::write(
        &config_path,
        r#"# retained
default_profile = "default"

[profile.default]
rpc_url = "http://localhost:9545"
default_key = "alice"
plugin_field = "keep"

[profile.default.contracts]
counter = "0x1111" # alias comment
"#,
    )
    .unwrap();
    #[cfg(unix)]
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();

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
    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("# retained"));
    assert!(contents.contains("# alias comment"));
    assert!(contents.contains("plugin_field = \"keep\""));
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(config_path).unwrap().permissions().mode() & 0o777,
        0o640
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
        r#"# retained
default_profile = "default"

[profile.default]
rpc_url = "http://localhost:9545"
default_key = "alice"
plugin_field = "keep"

[profile.default.wallets]

[profile.default.wallets.alice]
address = "0x1111" # wallet comment
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
    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("# retained"));
    assert!(contents.contains("# wallet comment"));
    assert!(contents.contains("plugin_field = \"keep\""));
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
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(global_config_path(&tmp), "# retained\nplugin = \"keep\"\n").unwrap();

    let state = groundhog_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: manifest.clone(),
        runtime_artifacts_dir: Some(artifacts_dir.clone()),
    };
    let written = groundhog_cli::config::save_local_runtime_state(&state).unwrap();
    assert_eq!(written, global_config_path(&tmp));
    let global_config = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(global_config.contains("# retained"));
    assert!(global_config.contains("plugin = \"keep\""));

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
#[cfg(unix)]
#[serial]
fn save_local_runtime_state_rejects_non_utf8_manifest_path() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let invalid_path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/manifest-\xff".to_vec()));

    let error = groundhog_cli::config::save_local_runtime_state(
        &groundhog_cli::config::LocalRuntimeState {
            node_id: "node-1".into(),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest: invalid_path,
            runtime_artifacts_dir: None,
        },
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("runtime_manifest must be valid UTF-8")
    );
    assert!(!global_config_path(&tmp).exists());
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[cfg(unix)]
#[serial]
fn save_local_runtime_state_rejects_non_utf8_artifacts_path() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let invalid_path = PathBuf::from(std::ffi::OsString::from_vec(
        b"/tmp/artifacts-\xff".to_vec(),
    ));

    let error = groundhog_cli::config::save_local_runtime_state(
        &groundhog_cli::config::LocalRuntimeState {
            node_id: "node-1".into(),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest: tmp.path().join("runtime-manifest.json"),
            runtime_artifacts_dir: Some(invalid_path),
        },
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("runtime_artifacts_dir must be valid UTF-8")
    );
    assert!(!global_config_path(&tmp).exists());
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

    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(global_config_path(&tmp), "# retained\nplugin = \"keep\"\n").unwrap();
    groundhog_cli::config::save_local_runtime_state(&state).unwrap();
    groundhog_cli::config::clear_local_runtime_state_if_matches(&state).unwrap();

    let config = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(!config.contains("local_runtime_state"));
    assert!(config.contains("# retained"));
    assert!(config.contains("plugin = \"keep\""));

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
default_profile = "default"

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
default_profile = "default"

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
    let result = groundhog_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("default"),
        "counter",
        &addr,
    );
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
        Some("default"),
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
        Some("default"),
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
default_profile = "default"

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

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();

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

    groundhog_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("default"),
        "counter",
        &addr_a,
    )
    .unwrap();
    groundhog_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("default"),
        "greeter",
        &addr_b,
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
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
        Some("default"),
        "alice",
        &addr_a,
        &owner_a,
    )
    .unwrap();
    groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "bob",
        &addr_b,
        &owner_b,
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
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
        Some("default"),
        "alice",
        &addr_a,
        &owner_a,
    )
    .unwrap();
    groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "bob",
        &addr_b,
        &owner_b,
    )
    .unwrap();
    let config_path = global_config_path(&tmp);
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(&config_path, format!("# retained\nplugin = true\n{config}")).unwrap();
    groundhog_cli::config::remove_profile_wallet_address(tmp.path(), Some("default"), "alice")
        .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
    assert!(!profile.wallets.contains_key("alice"));
    assert_eq!(
        profile
            .wallets
            .get("bob")
            .map(|wallet| wallet.address.as_str()),
        Some(addr_b.as_str())
    );
    let config = fs::read_to_string(config_path).unwrap();
    assert!(config.contains("# retained"));
    assert!(config.contains("plugin = true"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn remove_wallet_clears_matching_default_in_same_write() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let address = format!("0x{}", "aa".repeat(32));
    let owner = format!("0x{}", "bb".repeat(32));
    fs::write(tmp.path().join("groundhog.toml"), "").unwrap();

    groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "alice",
        &address,
        &owner,
    )
    .unwrap();
    groundhog_cli::config::save_profile_default_wallet(tmp.path(), Some("default"), "alice")
        .unwrap();
    groundhog_cli::config::remove_profile_wallet_address(tmp.path(), Some("default"), "alice")
        .unwrap();

    let contents = fs::read_to_string(tmp.path().join("groundhog.toml")).unwrap();
    assert!(!contents.contains("default_wallet"));
    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
    assert!(profile.default_wallet.is_none());
    assert!(!profile.wallets.contains_key("alice"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn remove_wallet_rejects_missing_entry() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let addr = format!("0x{}", "aa".repeat(32));
    let owner = format!("0x{}", "bb".repeat(32));

    groundhog_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "alice",
        &addr,
        &owner,
    )
    .unwrap();
    let result =
        groundhog_cli::config::remove_profile_wallet_address(tmp.path(), Some("default"), "bob");

    assert!(result.is_err());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn initialize_global_profiles_preserves_inline_profile_fields() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        r#"profile = { local = { rpc_url = "http://127.0.0.1:9999", plugin = "keep" } }
"#,
    )
    .unwrap();

    groundhog_cli::config::initialize_global_profiles().unwrap();

    let local = groundhog_cli::config::resolve_profile(tmp.path(), Some("local")).unwrap();
    assert_eq!(local.rpc_url.as_deref(), Some("http://127.0.0.1:9999"));
    let contents = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(contents.contains("plugin = \"keep\""));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[cfg(unix)]
#[serial]
fn concurrent_global_writers_preserve_independent_updates() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    let path = global_config_path(&tmp);
    fs::write(&path, "plugin = true\n").unwrap();
    let lock = lock_config(&path);
    let (started_tx, started_rx) = std::sync::mpsc::channel();

    let key_started = started_tx.clone();
    let key_writer = std::thread::spawn(move || {
        key_started.send(()).unwrap();
        groundhog_cli::config::save_default_key("alice")
    });
    let credentials_writer = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        groundhog_cli::config::save_executor_credentials(
            &groundhog_cli::config::ExecutorCredentials {
                username: "user".into(),
                password: "secret".into(),
            },
        )
    });

    started_rx.recv().unwrap();
    started_rx.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(lock);
    key_writer.join().unwrap().unwrap();
    credentials_writer.join().unwrap().unwrap();

    assert_eq!(
        groundhog_cli::config::load_default_key()
            .unwrap()
            .as_deref(),
        Some("alice")
    );
    assert_eq!(
        groundhog_cli::config::load_executor_credentials()
            .unwrap()
            .unwrap()
            .password,
        "secret"
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[cfg(unix)]
fn concurrent_project_writers_preserve_independent_updates() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("groundhog.toml");
    fs::write(&path, "[profile.default]\nplugin = true\n").unwrap();
    let lock = lock_config(&path);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let alias_address = format!("0x{}", "aa".repeat(32));
    let wallet_address = format!("0x{}", "bb".repeat(32));
    let owner = format!("0x{}", "cc".repeat(32));

    let project = tmp.path().to_path_buf();
    let alias_started = started_tx.clone();
    let alias_writer = std::thread::spawn(move || {
        alias_started.send(()).unwrap();
        groundhog_cli::config::save_profile_contract_alias(
            &project,
            Some("default"),
            "counter",
            &alias_address,
        )
    });
    let project = tmp.path().to_path_buf();
    let wallet_writer = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        groundhog_cli::config::save_profile_wallet_address(
            &project,
            Some("default"),
            "alice",
            &wallet_address,
            &owner,
        )
    });

    started_rx.recv().unwrap();
    started_rx.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(lock);
    alias_writer.join().unwrap().unwrap();
    wallet_writer.join().unwrap().unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
    assert!(profile.contracts.contains_key("counter"));
    assert!(profile.wallets.contains_key("alice"));
}

#[test]
#[serial]
fn persisted_profile_merge_tracks_sources_and_normalizes_runtime_paths() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        global_config_path(&tmp),
        r#"default_profile = "devnet"

[profile.devnet]
network = "devnet"
rpc_url = "https://global.example"

[profile.devnet.contracts]
HOG = "0x01"
"#,
    )
    .unwrap();
    fs::write(
        project.join("groundhog.toml"),
        r#"[profile.devnet]
faucet_url = "https://faucet.example"
runtime_manifest = "state/runtime.json"

[profile.devnet.contracts]
counter = "0x02"
"#,
    )
    .unwrap();

    let config = groundhog_cli::config::load_persisted_config(&project).unwrap();
    let resolved = config.resolve("devnet").unwrap();
    let raw_project = config
        .inspect("devnet", groundhog_cli::config::ConfigScope::Project)
        .unwrap();

    assert_eq!(
        resolved.values.rpc_url.as_deref(),
        Some("https://global.example")
    );
    assert_eq!(
        resolved.values.runtime_manifest.as_deref(),
        Some(project.join("state/runtime.json").as_path())
    );
    assert_eq!(
        raw_project.values.runtime_manifest.as_deref(),
        Some(std::path::Path::new("state/runtime.json"))
    );
    assert_eq!(
        resolved.sources.rpc_url.as_ref().map(|source| source.kind),
        Some(groundhog_cli::config::ConfigSourceKind::Global)
    );
    assert_eq!(
        resolved
            .sources
            .faucet_url
            .as_ref()
            .map(|source| source.kind),
        Some(groundhog_cli::config::ConfigSourceKind::Project)
    );
    assert_eq!(
        resolved
            .sources
            .contracts
            .get("counter")
            .map(|source| source.kind),
        Some(groundhog_cli::config::ConfigSourceKind::Project)
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn profile_listing_isolates_invalid_and_active_missing_profiles() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("groundhog.toml"),
        r#"[profile.valid]
network = "devnet"

[profile.invalid]
network = "preview"
"#,
    )
    .unwrap();

    let config = groundhog_cli::config::load_persisted_config(tmp.path()).unwrap();
    let valid = config.resolve("valid").unwrap();
    let report = config.profiles_report(Some("missing")).unwrap();

    assert_eq!(
        valid.values.network,
        Some(groundhog_cli::config::Network::Devnet)
    );
    assert_eq!(report.active_profile.as_deref(), Some("missing"));
    assert_eq!(report.profiles.len(), 3);
    assert!(report.profiles.iter().any(|row| {
        row.profile == "invalid"
            && row.status == groundhog_cli::config::ConfigProfileStatus::Invalid
    }));
    assert!(report.profiles.iter().any(|row| {
        row.profile == "missing"
            && row.active
            && row.status == groundhog_cli::config::ConfigProfileStatus::Missing
    }));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_get_json_projection_omits_protected_global_state() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        r#"default_profile = "devnet"
default_key = "top-level-secret-selection"

[executor]
username = "private-user"
password = "private-password"

[local_runtime_state]
node_id = "node-private"
rpc_url = "http://127.0.0.1:9545"
runtime_manifest = "/private/runtime.json"

[profile.devnet]
rpc_url = "https://rpc.example"

[profile.devnet.wallets.primary]
address = "0x1111111111111111111111111111111111111111111111111111111111111111"
owner_public_key = "0x2222222222222222222222222222222222222222222222222222222222222222"
"#,
    )
    .unwrap();

    let config = groundhog_cli::config::load_persisted_config(tmp.path()).unwrap();
    let selection = config.selection(None).unwrap().unwrap();
    let report = groundhog_cli::config::ConfigGetReport::new(
        config.resolve("devnet").unwrap(),
        Some(selection.source),
        groundhog_cli::config::ConfigScope::Resolved,
        true,
    );
    let json = serde_json::to_string(&report).unwrap();
    let roundtrip: groundhog_cli::config::ConfigGetReport = serde_json::from_str(&json).unwrap();

    assert!(json.contains("\"schemaVersion\":1"));
    assert!(json.contains("\"ownerPublicKey\""));
    assert!(!json.contains("owner_public_key"));
    assert_eq!(roundtrip, report);
    assert!(!json.contains("private-user"));
    assert!(!json.contains("private-password"));
    assert!(!json.contains("top-level-secret-selection"));
    assert!(!json.contains("node-private"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
fn config_field_validation_accepts_typed_values_and_rejects_invalid_values() {
    use std::str::FromStr as _;

    assert!(groundhog_cli::config::ConfigField::from_str("network").is_ok());
    assert!(
        groundhog_cli::config::ConfigField::RpcUrl
            .parse_value("https://rpc.example")
            .is_ok()
    );
    assert!(
        groundhog_cli::config::ConfigField::FaucetUrl
            .parse_value("relative/path")
            .is_err()
    );
    assert!(
        groundhog_cli::config::ConfigField::DefaultKey
            .parse_value("  ")
            .is_err()
    );
}

#[test]
fn config_set_creates_project_overlay_and_preserves_unrelated_content() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("groundhog.toml");
    fs::write(
        &path,
        "# retained\ndefault_profile = \"devnet\"\nplugin = true\n",
    )
    .unwrap();

    let report = groundhog_cli::config::set_profile_field(
        tmp.path(),
        "devnet",
        groundhog_cli::config::ConfigField::RpcUrl,
        "https://rpc.example",
        false,
        true,
    )
    .unwrap();

    assert!(report.changed);
    assert_eq!(report.scope, groundhog_cli::config::ConfigScope::Project);
    assert_eq!(report.path, path);
    assert_eq!(
        report.field,
        Some(groundhog_cli::config::ConfigField::RpcUrl)
    );
    assert_eq!(
        fs::read_to_string(report.path).unwrap(),
        "# retained\ndefault_profile = \"devnet\"\nplugin = true\n\n[profile.devnet]\nrpc_url = \"https://rpc.example\"\n"
    );
}

#[test]
fn config_set_reports_unchanged_for_existing_value() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        "[profile.devnet]\nnetwork = 'devnet'\n",
    )
    .unwrap();

    let report = groundhog_cli::config::set_profile_field(
        tmp.path(),
        "devnet",
        groundhog_cli::config::ConfigField::Network,
        "devnet",
        false,
        true,
    )
    .unwrap();

    assert!(!report.changed);
    assert_eq!(
        fs::read_to_string(tmp.path().join("groundhog.toml")).unwrap(),
        "[profile.devnet]\nnetwork = 'devnet'\n"
    );
}

#[test]
fn config_unset_missing_project_profile_is_noop_without_creating_table() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("groundhog.toml");
    let contents = "# retained\nplugin = true\n";
    fs::write(&path, contents).unwrap();

    let report = groundhog_cli::config::unset_profile_field(
        tmp.path(),
        "missing",
        groundhog_cli::config::ConfigField::FaucetUrl,
        false,
        true,
    )
    .unwrap();

    assert!(!report.changed);
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
}

#[test]
fn config_set_validation_failure_leaves_selected_profile_unchanged() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("groundhog.toml");
    let contents = "# retained\n[profile.devnet]\nrpc_url = \"relative/path\"\nunknown = true\n";
    fs::write(&path, contents).unwrap();

    let error = groundhog_cli::config::set_profile_field(
        tmp.path(),
        "devnet",
        groundhog_cli::config::ConfigField::Network,
        "devnet",
        false,
        true,
    )
    .unwrap_err();

    assert_eq!(error.code(), "profile_config_invalid");
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
}

#[test]
#[serial]
fn config_use_updates_only_selected_default_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    let path = global_config_path(&tmp);
    fs::write(
        &path,
        "# retained\ndefault_profile = \"local\"\nunknown = true\n\n[profile.local]\nnetwork = \"local\"\n\n[profile.devnet]\nnetwork = \"devnet\"\n",
    )
    .unwrap();

    let report = groundhog_cli::config::use_profile(tmp.path(), "devnet", true, false).unwrap();

    assert!(report.changed);
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "# retained\ndefault_profile = \"devnet\"\nunknown = true\n\n[profile.local]\nnetwork = \"local\"\n\n[profile.devnet]\nnetwork = \"devnet\"\n"
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_use_global_rejects_project_only_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "default_profile = \"local\"\n\n[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        "[profile.project-only]\nnetwork = \"devnet\"\n",
    )
    .unwrap();

    let error =
        groundhog_cli::config::use_profile(tmp.path(), "project-only", true, false).unwrap_err();

    assert_eq!(error.code(), "profile_not_found_in_scope");
    assert_eq!(
        error.to_string(),
        "profile `project-only` not found in global config"
    );
    assert!(
        fs::read_to_string(global_config_path(&tmp))
            .unwrap()
            .starts_with("default_profile = \"local\"")
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_use_reports_noop_without_normalizing_string_style() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    let path = global_config_path(&tmp);
    let contents = "default_profile = 'devnet'\n\n[profile.devnet]\nnetwork = \"devnet\"\n";
    fs::write(&path, contents).unwrap();

    let report = groundhog_cli::config::use_profile(tmp.path(), "devnet", true, false).unwrap();

    assert!(!report.changed);
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn malformed_toml_reports_the_config_path() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let path = tmp.path().join("groundhog.toml");
    fs::write(&path, "[profile.devnet\n").unwrap();

    let error = groundhog_cli::config::load_persisted_config(tmp.path()).unwrap_err();

    assert_eq!(error.code(), "config_parse_failed");
    assert!(error.to_string().contains(path.to_string_lossy().as_ref()));
    let details = error.details_json().unwrap();
    assert_eq!(details["path"], path.display().to_string());
    assert!(details["line"].as_u64().is_some());
    assert!(details["column"].as_u64().is_some());
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_unrelated_profile_does_not_hide_global_scalars_or_valid_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        r#"default_profile = "valid"
default_key = "alice"

[executor]
username = "sdk"
password = "secret"

[profile.valid]
network = "devnet"

[profile.invalid]
network = "preview"
"#,
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();
    let key = groundhog_cli::config::load_default_key().unwrap();
    let credentials = groundhog_cli::config::load_executor_credentials()
        .unwrap()
        .unwrap();

    assert_eq!(profile.name, "valid");
    assert_eq!(key.as_deref(), Some("alice"));
    assert_eq!(credentials.username, "sdk");
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_local_runtime_state_does_not_block_non_local_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        r#"default_profile = "devnet"

[profile.devnet]
network = "devnet"
rpc_url = "https://rpc.example"

[local_runtime_state]
node_id = "node-1"
rpc_url = "not-a-url"
runtime_manifest = "relative.json"
"#,
    )
    .unwrap();

    let profile = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap();

    assert_eq!(profile.name, "devnet");
    assert_eq!(profile.rpc_url.as_deref(), Some("https://rpc.example"));
    assert_eq!(
        groundhog_cli::config::resolve_default_key_for_profile(&profile).unwrap(),
        None
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_local_runtime_state_is_rejected_for_local_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        global_config_path(&tmp),
        r#"default_profile = "local"

[profile.local]
network = "local"

[local_runtime_state]
node_id = "node-1"
rpc_url = "http://"
runtime_manifest = "/tmp/runtime.json"
"#,
    )
    .unwrap();

    let error = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap_err();

    assert_eq!(error.code(), "config_file_invalid");
    assert!(error.to_string().contains("local_runtime_state is invalid"));
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_persisted_default_profile_reports_its_layer_path() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    let global_path = global_config_path(&tmp);
    fs::write(&global_path, "default_profile = \" \"\n").unwrap();

    let global_error = groundhog_cli::config::load_persisted_config(tmp.path()).unwrap_err();

    assert_eq!(global_error.code(), "config_file_invalid");
    assert!(
        global_error
            .to_string()
            .contains(global_path.to_string_lossy().as_ref())
    );

    fs::write(&global_path, "").unwrap();
    let project_path = tmp.path().join("groundhog.toml");
    fs::write(&project_path, "default_profile = \" \"\n").unwrap();

    let project_error = groundhog_cli::config::load_persisted_config(tmp.path()).unwrap_err();

    assert_eq!(project_error.code(), "config_file_invalid");
    assert!(
        project_error
            .to_string()
            .contains(project_path.to_string_lossy().as_ref())
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_profile_reason_identifies_layer_without_duplicate_wrapping() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let project_path = tmp.path().join("groundhog.toml");
    fs::write(
        &project_path,
        "default_profile = \"broken\"\n\n[profile.broken]\nnetwork = \"preview\"\n",
    )
    .unwrap();

    let error = groundhog_cli::config::resolve_profile(tmp.path(), None).unwrap_err();
    let message = error.to_string();

    assert_eq!(
        message.matches("profile `broken` is invalid").count(),
        1,
        "{message}"
    );
    assert!(message.contains("project config"));
    assert!(message.contains(project_path.to_string_lossy().as_ref()));
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}
