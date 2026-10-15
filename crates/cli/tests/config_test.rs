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
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use tempfile::TempDir;

fn global_config_path(tmp: &TempDir) -> PathBuf {
    tmp.path().join("thruput/config.toml")
}

#[test]
fn project_discovery_uses_only_the_thruput_name() {
    let tmp = TempDir::new().unwrap();
    let nested = tmp.path().join("member/src");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        tmp.path().join("groundhog.toml"),
        "default_profile = 'old'\n",
    )
    .unwrap();
    assert_eq!(thruput_cli::config::find_project_config(&nested), None);

    let config = tmp.path().join("thruput.toml");
    fs::write(&config, "default_profile = 'new'\n").unwrap();
    assert_eq!(
        thruput_cli::config::find_project_config(&nested),
        Some(config)
    );
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
    let result = thruput_cli::config::config_dir().unwrap();
    assert_eq!(result, path.join("thruput"));
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn cache_dir_respects_xdg_env() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("custom_cache");
    unsafe { env::set_var("XDG_CACHE_HOME", &path) };
    let result = thruput_cli::config::cache_dir().unwrap();
    assert_eq!(result, path.join("thruput"));
    unsafe { env::remove_var("XDG_CACHE_HOME") };
}

#[test]
#[serial]
fn load_returns_none_when_no_config() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path().join("nonexistent")) };

    let loaded = thruput_cli::config::load_default_key().unwrap();
    assert!(loaded.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn roundtrip_default_key() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "# retained\nplugin = \"keep\"\ndefault_key = \"old\" # key comment\n",
    )
    .unwrap();

    thruput_cli::config::save_default_key("alice").unwrap();
    let loaded = thruput_cli::config::load_default_key().unwrap();
    assert_eq!(loaded.as_deref(), Some("alice"));
    assert!(
        fs::read_to_string(global_config_path(&tmp))
            .unwrap()
            .contains("# key comment")
    );

    thruput_cli::config::clear_default_key().unwrap();
    let cleared = thruput_cli::config::load_default_key().unwrap();
    assert!(cleared.is_none());
    let contents = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(contents.contains("# retained"));
    assert!(contents.contains("plugin = \"keep\""));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
fn project_profile_default_wallet_roundtrips() {
    let profile = thruput_cli::config::ProjectProfile {
        default_wallet: Some("alice-wallet".into()),
        ..Default::default()
    };

    let encoded = toml::to_string(&profile).unwrap();
    let decoded: thruput_cli::config::ProjectProfile = toml::from_str(&encoded).unwrap();

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

    thruput_cli::config::save_default_key("alice").unwrap();

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
    let direct_addr = thruput_encoding::encode(&[0xbb; 32]);
    fs::write(
        tmp.path().join("thruput.toml"),
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

    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
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
    assert_eq!(profile.resolve_wallet("alice").as_deref(), Some("0xabcd"));
    assert_eq!(
        profile.resolve_wallet(&direct_addr).as_deref(),
        Some(direct_addr.as_str())
    );
    assert!(profile.resolve_wallet("missing").is_none());
    assert_eq!(profile.source, Some(tmp.path().join("thruput.toml")));
}

#[test]
#[serial]
fn resolve_profile_uses_env_override() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        r#"
[profile.default]
rpc_url = "http://localhost:9545"

[profile.testnet]
rpc_url = "https://testnet.thruput.network"
faucet_url = "https://faucet.testnet.thruput.network"
default_key = "bob"

[profile.testnet.contracts]
greeter = "0xbeef"
"#,
    )
    .unwrap();
    unsafe { env::set_var("THRUPUT_PROFILE", "testnet") };

    let profile = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.name, "testnet");
    assert_eq!(
        profile.rpc_url.as_deref(),
        Some("https://testnet.thruput.network")
    );
    assert_eq!(
        profile.faucet_url.as_deref(),
        Some("https://faucet.testnet.thruput.network")
    );
    assert_eq!(profile.default_key.as_deref(), Some("bob"));
    assert_eq!(
        profile.contracts.get("greeter").map(String::as_str),
        Some("0xbeef")
    );

    unsafe { env::remove_var("THRUPUT_PROFILE") };
}

#[test]
#[serial]
fn resolve_profile_requires_selection_when_no_config_exists() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let error = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap_err();
    assert_eq!(error.code(), "no_active_profile");

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_requires_selection_when_project_has_no_default_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(tmp.path().join("thruput.toml"), "[contracts]\n").unwrap();

    let error = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap_err();
    assert_eq!(error.code(), "no_active_profile");

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_uses_global_default_profile_when_no_project_config_exists() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        tmp.path().join("thruput/config.toml"),
        r#"
default_profile = "local"

[profile.local]
rpc_url = "http://127.0.0.1:9545"
default_key = "dev-0"
"#,
    )
    .unwrap();

    let profile = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.name, "local");
    assert_eq!(profile.rpc_url.as_deref(), Some("http://127.0.0.1:9545"));
    assert_eq!(profile.default_key.as_deref(), Some("dev-0"));
    assert_eq!(profile.source, Some(tmp.path().join("thruput/config.toml")));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_project_default_profile_overrides_global_default_profile() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        tmp.path().join("thruput/config.toml"),
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
        project.join("thruput.toml"),
        r#"
default_profile = "devnet"

[profile.devnet]
default_key = "alice"
"#,
    )
    .unwrap();

    let profile = thruput_cli::config::resolve_profile(&project, None).unwrap();
    assert_eq!(profile.name, "devnet");
    assert_eq!(
        profile.rpc_url.as_deref(),
        Some("https://rpc.devnet.example")
    );
    assert_eq!(profile.default_key.as_deref(), Some("alice"));
    assert_eq!(profile.source, Some(project.join("thruput.toml")));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_profile_merges_global_and_project_profile_fields() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let global_hog = thruput_encoding::encode(&[0x11; 32]);
    let project_hog = thruput_encoding::encode(&[0x22; 32]);
    let counter = thruput_encoding::encode(&[0x33; 32]);
    let global_alice = thruput_encoding::encode(&[0x44; 32]);
    let project_alice = thruput_encoding::encode(&[0x55; 32]);
    let bob = thruput_encoding::encode(&[0x66; 32]);
    let artifacts_dir = tmp.path().join("global-artifacts");
    fs::create_dir(&project).unwrap();
    fs::create_dir(&artifacts_dir).unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        tmp.path().join("thruput/config.toml"),
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
        project.join("thruput.toml"),
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

    let profile = thruput_cli::config::resolve_profile(&project, Some("devnet")).unwrap();
    assert_eq!(
        profile.rpc_url.as_deref(),
        Some("https://project-rpc.devnet.example")
    );
    assert_eq!(profile.default_key.as_deref(), Some("bob"));
    assert_eq!(profile.default_wallet.as_deref(), Some("project-wallet"));
    assert_eq!(
        profile.runtime_manifest.as_deref(),
        Some(tmp.path().join("thruput/global-runtime.json").as_path())
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
    let config_path = tmp.path().join("thruput.toml");
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
        thruput_cli::config::save_profile_default_wallet(tmp.path(), None, " alice-wallet ")
            .unwrap();

    assert_eq!(written, config_path);
    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("# keep this project comment"));
    assert!(contents.contains("rpc_url = \"http://localhost:9545\" # keep this field comment"));
    let profile = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.default_wallet.as_deref(), Some("alice-wallet"));
}

#[test]
#[serial]
fn save_wallet_and_default_uses_one_profile_update() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\n",
    )
    .unwrap();
    let address = thruput_encoding::encode(&[0xaa; 32]);
    let owner = thruput_encoding::encode(&[0xbb; 32]);

    thruput_cli::config::save_profile_wallet_address_with_default(
        tmp.path(),
        None,
        "alice",
        &address,
        &owner,
        true,
    )
    .unwrap();

    let profile = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap();
    assert_eq!(profile.default_wallet.as_deref(), Some("alice"));
    assert_eq!(profile.wallets["alice"].address, address);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_profile_contract_alias_updates_existing_profile() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("thruput.toml");
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

    let addr = thruput_encoding::encode(&[0x22; 32]);
    let written =
        thruput_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", &addr)
            .unwrap();
    assert_eq!(written, config_path);

    let profile = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap();
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

    let addr = thruput_encoding::encode(&[0xbe; 32]);
    let written = thruput_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("testnet"),
        "greeter",
        &addr,
    )
    .unwrap();
    assert_eq!(written, global_config_path(&tmp));

    let profiles = thruput_cli::config::load_project_profiles(tmp.path()).unwrap();
    assert!(profiles.is_none());

    let testnet = thruput_cli::config::resolve_profile(tmp.path(), Some("testnet")).unwrap();
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
    let config_path = tmp.path().join("thruput.toml");
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

    let addr = thruput_encoding::encode(&[0x22; 32]);
    let owner = thruput_encoding::encode(&[0x33; 32]);
    let written =
        thruput_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", &addr, &owner)
            .unwrap();
    assert_eq!(written, config_path);

    let profile = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap();
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

    let addr = thruput_encoding::encode(&[0xbe; 32]);
    let owner = thruput_encoding::encode(&[0xcd; 32]);
    let written = thruput_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("testnet"),
        "alice",
        &addr,
        &owner,
    )
    .unwrap();
    assert_eq!(written, global_config_path(&tmp));

    let profiles = thruput_cli::config::load_project_profiles(tmp.path()).unwrap();
    assert!(profiles.is_none());

    let testnet = thruput_cli::config::resolve_profile(tmp.path(), Some("testnet")).unwrap();
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
        project.join("thruput.toml"),
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
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(global_config_path(&tmp), "# retained\nplugin = \"keep\"\n").unwrap();

    let state = thruput_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: manifest.clone(),
        runtime_artifacts_dir: Some(artifacts_dir.clone()),
    };
    let written = thruput_cli::config::save_local_runtime_state(&state).unwrap();
    assert_eq!(written, global_config_path(&tmp));
    let global_config = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(global_config.contains("# retained"));
    assert!(global_config.contains("plugin = \"keep\""));

    let project_config = fs::read_to_string(project.join("thruput.toml")).unwrap();
    assert!(!project_config.contains("rpc_url"));
    assert!(!project_config.contains("runtime_manifest"));
    assert!(!project_config.contains("runtime_artifacts_dir"));

    let profile = thruput_cli::config::resolve_profile(&project, Some("local")).unwrap();
    assert_eq!(profile.network, Some(thruput_cli::config::Network::Local));
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
    thruput_cli::config::save_default_key("alice").unwrap();
    fs::create_dir_all(tmp.path().join("manifests")).unwrap();
    let manifest = tmp.path().join("manifests/runtime-manifest.json");
    fs::write(&manifest, "{}").unwrap();

    thruput_cli::config::save_local_runtime_state(&thruput_cli::config::LocalRuntimeState {
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

    let error =
        thruput_cli::config::save_local_runtime_state(&thruput_cli::config::LocalRuntimeState {
            node_id: "node-1".into(),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest: invalid_path,
            runtime_artifacts_dir: None,
        })
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

    let error =
        thruput_cli::config::save_local_runtime_state(&thruput_cli::config::LocalRuntimeState {
            node_id: "node-1".into(),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest: tmp.path().join("runtime-manifest.json"),
            runtime_artifacts_dir: Some(invalid_path),
        })
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
        tmp.path().join("thruput.toml"),
        r#"
[profile.devnet]
network = "devnet"
rpc_url = "https://rpc.devnet.example"
"#,
    )
    .unwrap();
    let manifest = tmp.path().join("runtime-manifest.json");
    fs::write(&manifest, "{}").unwrap();

    thruput_cli::config::save_local_runtime_state(&thruput_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9546".into(),
        runtime_manifest: manifest,
        runtime_artifacts_dir: None,
    })
    .unwrap();

    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("devnet")).unwrap();
    assert_eq!(profile.network, Some(thruput_cli::config::Network::Devnet));
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
        tmp.path().join("thruput.toml"),
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

    thruput_cli::config::save_local_runtime_state(&thruput_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: state_manifest,
        runtime_artifacts_dir: None,
    })
    .unwrap();

    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("local")).unwrap();
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
    let state = thruput_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: manifest,
        runtime_artifacts_dir: None,
    };

    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(global_config_path(&tmp), "# retained\nplugin = \"keep\"\n").unwrap();
    thruput_cli::config::save_local_runtime_state(&state).unwrap();
    thruput_cli::config::clear_local_runtime_state_if_matches(&state).unwrap();

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
    let first = thruput_cli::config::LocalRuntimeState {
        node_id: "node-1".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest: first_manifest,
        runtime_artifacts_dir: None,
    };
    let second = thruput_cli::config::LocalRuntimeState {
        node_id: "node-2".into(),
        rpc_url: "http://127.0.0.1:9546".into(),
        runtime_manifest: second_manifest.clone(),
        runtime_artifacts_dir: None,
    };

    thruput_cli::config::save_local_runtime_state(&first).unwrap();
    thruput_cli::config::save_local_runtime_state(&second).unwrap();
    thruput_cli::config::clear_local_runtime_state_if_matches(&first).unwrap();

    fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();
    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("local")).unwrap();
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
        tmp.path().join("thruput.toml"),
        r#"
default_profile = "default"

[profile.default]
rpc_url = "http://localhost:9545"
"#,
    )
    .unwrap();

    thruput_cli::config::save_default_key("alice").unwrap();

    let resolved = thruput_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert_eq!(resolved.as_deref(), Some("alice"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_default_key_prefers_profile_value_over_global_selection() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("thruput.toml"),
        r#"
default_profile = "default"

[profile.default]
rpc_url = "http://localhost:9545"
default_key = "project-key"
"#,
    )
    .unwrap();

    thruput_cli::config::save_default_key("global-key").unwrap();

    let resolved = thruput_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert_eq!(resolved.as_deref(), Some("project-key"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_default_key_does_not_infer_key_from_local_runtime() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let runtime_manifest = tmp.path().join("runtime-manifest.json");
    write_local_runtime_manifest(&runtime_manifest, &[(0, "faucet"), (1, "local_user")]);
    fs::write(
        tmp.path().join("thruput.toml"),
        r#"
default_profile = "local"

[profile.local]
network = "local"
"#,
    )
    .unwrap();
    thruput_cli::config::save_local_runtime_state(&thruput_cli::config::LocalRuntimeState {
        node_id: "local-node".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest,
        runtime_artifacts_dir: None,
    })
    .unwrap();

    let resolved = thruput_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert_eq!(resolved, None);

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

fn write_local_runtime_manifest(path: &std::path::Path, accounts: &[(u32, &str)]) {
    let accounts: Vec<_> = accounts
        .iter()
        .map(|(index, role)| {
            let signer = thruput_signing::derive_default_dev_account_signer(*index as usize);
            serde_json::json!({
                "index": index,
                "role": role,
                "publicKey": format!("0x{}", hex::encode(signer.verifying_key_bytes())),
                "walletAddress": format!("0x{}", "71".repeat(32)),
                "initialStorageTokens": "1000000000"
            })
        })
        .collect();
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 3,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test00000000000",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "producer": { "binary": "hog-devnode", "version": "test" },
            "endpoints": { "scheduler": "127.0.0.1:1000" },
            "bootstrapAccountDerivation": {
                "scheme": "groundhog-bootstrap-account-v1",
                "seed": thruput_signing::DEFAULT_DEV_ACCOUNT_SEED
            },
            "bootstrapAccounts": accounts,
            "bootstrap": {
                "storageCostModel": {
                    "hash": format!("0x{}", "53".repeat(32)),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
#[serial]
fn resolve_default_key_does_not_use_builtin_dev_key_for_overridden_local_rpc() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let runtime_manifest = tmp.path().join("runtime-manifest.json");
    fs::write(&runtime_manifest, "{}").unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        r#"
default_profile = "local"

[profile.local]
network = "local"
rpc_url = "https://rpc.example.test"
"#,
    )
    .unwrap();
    thruput_cli::config::save_local_runtime_state(&thruput_cli::config::LocalRuntimeState {
        node_id: "local-node".into(),
        rpc_url: "http://127.0.0.1:9545".into(),
        runtime_manifest,
        runtime_artifacts_dir: None,
    })
    .unwrap();

    let resolved = thruput_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert!(resolved.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn resolve_default_key_does_not_use_builtin_dev_key_for_devnet_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("thruput.toml"),
        r#"
default_profile = "devnet"

[profile.devnet]
network = "devnet"
"#,
    )
    .unwrap();

    let resolved = thruput_cli::config::resolve_default_key(tmp.path(), None).unwrap();
    assert!(resolved.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn save_alias_rejects_empty_alias() {
    let tmp = TempDir::new().unwrap();
    let addr = thruput_encoding::encode(&[0xab; 32]);
    let result = thruput_cli::config::save_profile_contract_alias(tmp.path(), None, "", &addr);
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_alias_rejects_empty_address() {
    let tmp = TempDir::new().unwrap();
    let result = thruput_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", "");
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_alias_rejects_malformed_address() {
    let tmp = TempDir::new().unwrap();
    let result =
        thruput_cli::config::save_profile_contract_alias(tmp.path(), None, "counter", "banana");
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_alias_accepts_valid_hex_address() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let addr = thruput_encoding::encode(&[0xab; 32]);
    let result = thruput_cli::config::save_profile_contract_alias(
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
    let addr = thruput_encoding::encode(&[0xab; 32]);
    let owner = thruput_encoding::encode(&[0xcd; 32]);
    let result =
        thruput_cli::config::save_profile_wallet_address(tmp.path(), None, "", &addr, &owner);
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_wallet_rejects_empty_address() {
    let tmp = TempDir::new().unwrap();
    let owner = thruput_encoding::encode(&[0xcd; 32]);
    let result =
        thruput_cli::config::save_profile_wallet_address(tmp.path(), None, "alice", "", &owner);
    assert!(result.is_err());
}

#[test]
#[serial]
fn save_wallet_rejects_malformed_address() {
    let tmp = TempDir::new().unwrap();
    let owner = thruput_encoding::encode(&[0xcd; 32]);
    let result = thruput_cli::config::save_profile_wallet_address(
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
    let addr = thruput_encoding::encode(&[0xab; 32]);
    let owner = thruput_encoding::encode(&[0xcd; 32]);
    let result = thruput_cli::config::save_profile_wallet_address(
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
        tmp.path().join("thruput.toml"),
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

    let result = thruput_cli::config::resolve_profile(tmp.path(), None);

    assert!(result.is_err());
}

#[test]
#[serial]
fn resolve_profile_accepts_wallet_table_missing_owner_for_stable_validation() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
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

    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();

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
    let addr_a = thruput_encoding::encode(&[0xaa; 32]);
    let addr_b = thruput_encoding::encode(&[0xbb; 32]);

    thruput_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("default"),
        "counter",
        &addr_a,
    )
    .unwrap();
    thruput_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("default"),
        "greeter",
        &addr_b,
    )
    .unwrap();

    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
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
    let addr_a = thruput_encoding::encode(&[0xaa; 32]);
    let addr_b = thruput_encoding::encode(&[0xbb; 32]);
    let owner_a = thruput_encoding::encode(&[0xcc; 32]);
    let owner_b = thruput_encoding::encode(&[0xdd; 32]);

    thruput_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "alice",
        &addr_a,
        &owner_a,
    )
    .unwrap();
    thruput_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "bob",
        &addr_b,
        &owner_b,
    )
    .unwrap();

    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
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
    let addr_a = thruput_encoding::encode(&[0xaa; 32]);
    let addr_b = thruput_encoding::encode(&[0xbb; 32]);
    let owner_a = thruput_encoding::encode(&[0xcc; 32]);
    let owner_b = thruput_encoding::encode(&[0xdd; 32]);

    thruput_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "alice",
        &addr_a,
        &owner_a,
    )
    .unwrap();
    thruput_cli::config::save_profile_wallet_address(
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
    thruput_cli::config::remove_profile_wallet_address(tmp.path(), Some("default"), "alice")
        .unwrap();

    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
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
    let address = thruput_encoding::encode(&[0xaa; 32]);
    let owner = thruput_encoding::encode(&[0xbb; 32]);
    fs::write(tmp.path().join("thruput.toml"), "").unwrap();

    thruput_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "alice",
        &address,
        &owner,
    )
    .unwrap();
    thruput_cli::config::save_profile_default_wallet(tmp.path(), Some("default"), "alice").unwrap();
    thruput_cli::config::remove_profile_wallet_address(tmp.path(), Some("default"), "alice")
        .unwrap();

    let contents = fs::read_to_string(tmp.path().join("thruput.toml")).unwrap();
    assert!(!contents.contains("default_wallet"));
    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
    assert!(profile.default_wallet.is_none());
    assert!(!profile.wallets.contains_key("alice"));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn remove_wallet_rejects_missing_entry() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let addr = thruput_encoding::encode(&[0xaa; 32]);
    let owner = thruput_encoding::encode(&[0xbb; 32]);

    thruput_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("default"),
        "alice",
        &addr,
        &owner,
    )
    .unwrap();
    let result =
        thruput_cli::config::remove_profile_wallet_address(tmp.path(), Some("default"), "bob");

    assert!(result.is_err());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn initialize_global_profiles_preserves_inline_profile_fields() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        global_config_path(&tmp),
        r#"profile = { local = { rpc_url = "http://127.0.0.1:9999", plugin = "keep" } }
"#,
    )
    .unwrap();

    thruput_cli::config::initialize_global_profiles().unwrap();

    let local = thruput_cli::config::resolve_profile(tmp.path(), Some("local")).unwrap();
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
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    let path = global_config_path(&tmp);
    fs::write(&path, "plugin = true\n").unwrap();
    let lock = lock_config(&path);
    let (started_tx, started_rx) = std::sync::mpsc::channel();

    let key_started = started_tx.clone();
    let key_writer = std::thread::spawn(move || {
        key_started.send(()).unwrap();
        thruput_cli::config::save_default_key("alice")
    });
    let profiles_writer = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        thruput_cli::config::initialize_global_profiles()
    });

    started_rx.recv().unwrap();
    started_rx.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(lock);
    key_writer.join().unwrap().unwrap();
    profiles_writer.join().unwrap().unwrap();

    assert_eq!(
        thruput_cli::config::load_default_key().unwrap().as_deref(),
        Some("alice")
    );
    assert_eq!(
        thruput_cli::config::resolve_profile(tmp.path(), Some("devnet"))
            .unwrap()
            .rpc_url
            .as_deref(),
        Some("https://rpc.thruput.network")
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[cfg(unix)]
fn concurrent_project_writers_preserve_independent_updates() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    fs::write(&path, "[profile.default]\nplugin = true\n").unwrap();
    let lock = lock_config(&path);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let alias_address = thruput_encoding::encode(&[0xaa; 32]);
    let wallet_address = thruput_encoding::encode(&[0xbb; 32]);
    let owner = thruput_encoding::encode(&[0xcc; 32]);

    let project = tmp.path().to_path_buf();
    let alias_started = started_tx.clone();
    let alias_writer = std::thread::spawn(move || {
        alias_started.send(()).unwrap();
        thruput_cli::config::save_profile_contract_alias(
            &project,
            Some("default"),
            "counter",
            &alias_address,
        )
    });
    let project = tmp.path().to_path_buf();
    let wallet_writer = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        thruput_cli::config::save_profile_wallet_address(
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

    let profile = thruput_cli::config::resolve_profile(tmp.path(), Some("default")).unwrap();
    assert!(profile.contracts.contains_key("counter"));
    assert!(profile.wallets.contains_key("alice"));
}

#[test]
#[serial]
fn persisted_profile_merge_tracks_sources_and_normalizes_runtime_paths() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
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
        project.join("thruput.toml"),
        r#"[profile.devnet]
faucet_url = "https://faucet.example"
runtime_manifest = "state/runtime.json"

[profile.devnet.contracts]
counter = "0x02"
"#,
    )
    .unwrap();

    let config = thruput_cli::config::load_persisted_config(&project).unwrap();
    let resolved = config.resolve("devnet").unwrap();
    let raw_project = config
        .inspect("devnet", thruput_cli::config::ConfigScope::Project)
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
        Some(thruput_cli::config::ConfigSourceKind::Global)
    );
    assert_eq!(
        resolved
            .sources
            .faucet_url
            .as_ref()
            .map(|source| source.kind),
        Some(thruput_cli::config::ConfigSourceKind::Project)
    );
    assert_eq!(
        resolved
            .sources
            .contracts
            .get("counter")
            .map(|source| source.kind),
        Some(thruput_cli::config::ConfigSourceKind::Project)
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn profile_listing_isolates_invalid_and_active_missing_profiles() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::write(
        tmp.path().join("thruput.toml"),
        r#"[profile.valid]
network = "devnet"

[profile.invalid]
network = "preview"
"#,
    )
    .unwrap();

    let config = thruput_cli::config::load_persisted_config(tmp.path()).unwrap();
    let valid = config.resolve("valid").unwrap();
    let report = config.profiles_report(Some("missing")).unwrap();

    assert_eq!(
        valid.values.network,
        Some(thruput_cli::config::Network::Devnet)
    );
    assert_eq!(report.active_profile.as_deref(), Some("missing"));
    assert_eq!(report.profiles.len(), 3);
    assert!(report.profiles.iter().any(|row| {
        row.profile == "invalid" && row.status == thruput_cli::config::ConfigProfileStatus::Invalid
    }));
    assert!(report.profiles.iter().any(|row| {
        row.profile == "missing"
            && row.active
            && row.status == thruput_cli::config::ConfigProfileStatus::Missing
    }));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_get_json_projection_omits_protected_global_state() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
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

    let config = thruput_cli::config::load_persisted_config(tmp.path()).unwrap();
    let selection = config.selection(None).unwrap().unwrap();
    let report = thruput_cli::config::ConfigGetReport::new(
        config.resolve("devnet").unwrap(),
        Some(selection.source),
        thruput_cli::config::ConfigScope::Resolved,
        true,
    );
    let json = serde_json::to_string(&report).unwrap();
    let roundtrip: thruput_cli::config::ConfigGetReport = serde_json::from_str(&json).unwrap();

    assert!(json.contains("\"schemaVersion\":2"));
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

    assert!(thruput_cli::config::ConfigField::from_str("network").is_ok());
    assert!(
        thruput_cli::config::ConfigField::RpcUrl
            .parse_value("https://rpc.example")
            .is_ok()
    );
    assert!(
        thruput_cli::config::ConfigField::FaucetUrl
            .parse_value("relative/path")
            .is_err()
    );
    assert!(
        thruput_cli::config::ConfigField::DefaultKey
            .parse_value("  ")
            .is_err()
    );
}

#[test]
#[serial]
fn config_new_creates_complete_project_profile_and_preserves_unrelated_content() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    fs::write(
        &path,
        "# retained\ndefault_profile = \"devnet\"\nplugin = true\n",
    )
    .unwrap();

    let report = thruput_cli::config::create_profile(
        tmp.path(),
        "staging",
        &thruput_cli::config::NewProfileOptions {
            network: "devnet".into(),
            rpc_url: Some("https://rpc.example".into()),
            network_hash: None,
            faucet_url: Some("https://faucet.example".into()),
            default_key: Some("alice".into()),
            select: true,
        },
        false,
        true,
    )
    .unwrap();

    assert!(report.changed);
    assert_eq!(report.scope, thruput_cli::config::ConfigScope::Project);
    assert_eq!(report.path, path);
    assert_eq!(
        report.created_fields,
        [
            thruput_cli::config::ConfigField::DefaultKey,
            thruput_cli::config::ConfigField::FaucetUrl,
            thruput_cli::config::ConfigField::Network,
            thruput_cli::config::ConfigField::RpcUrl,
        ]
    );
    assert!(report.selected);
    assert_eq!(
        fs::read_to_string(report.path).unwrap(),
        "# retained\ndefault_profile = \"staging\"\nplugin = true\n\n[profile.staging]\nnetwork = \"devnet\"\nrpc_url = \"https://rpc.example\"\nfaucet_url = \"https://faucet.example\"\ndefault_key = \"alice\"\n"
    );
}

#[test]
#[serial]
fn config_new_rejects_existing_target_layer_profile_without_changes() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    let contents = "# retained\ndefault_profile = \"local\"\n\n[profile]\ndevnet = \"malformed\"\n\n[profile.local]\nnetwork = \"local\"\n";
    fs::write(&path, contents).unwrap();

    let error = thruput_cli::config::create_profile(
        tmp.path(),
        "devnet",
        &thruput_cli::config::NewProfileOptions {
            network: "mainnet".into(),
            rpc_url: None,
            network_hash: None,
            faucet_url: None,
            default_key: None,
            select: true,
        },
        false,
        true,
    )
    .unwrap_err();

    assert_eq!(error.code(), "profile_already_exists");
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
}

#[test]
#[cfg(unix)]
fn config_new_validates_all_values_before_waiting_for_lock() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    fs::write(&path, "# retained\n").unwrap();
    let _lock = lock_config(&path);

    let error = thruput_cli::config::create_profile(
        tmp.path(),
        "staging",
        &thruput_cli::config::NewProfileOptions {
            network: "devnet".into(),
            rpc_url: Some("relative/path".into()),
            network_hash: None,
            faucet_url: None,
            default_key: None,
            select: false,
        },
        false,
        true,
    )
    .unwrap_err();

    assert_eq!(error.code(), "config_value_invalid");
}

#[test]
#[serial]
fn concurrent_config_new_allows_only_one_creator() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("thruput.toml"), "# retained\n").unwrap();
    let options = thruput_cli::config::NewProfileOptions {
        network: "devnet".into(),
        rpc_url: None,
        network_hash: None,
        faucet_url: None,
        default_key: None,
        select: false,
    };
    let project = tmp.path().to_path_buf();
    let first_options = options.clone();
    let first = std::thread::spawn(move || {
        thruput_cli::config::create_profile(&project, "staging", &first_options, false, true)
    });
    let project = tmp.path().to_path_buf();
    let second = std::thread::spawn(move || {
        thruput_cli::config::create_profile(&project, "staging", &options, false, true)
    });

    let results = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(thruput_cli::CliError::code)
            .collect::<Vec<_>>(),
        ["profile_already_exists"]
    );
}

#[test]
#[serial]
fn config_new_project_allows_overlay_for_global_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "[profile.staging]\nnetwork = \"devnet\"\nrpc_url = \"https://global.example\"\n",
    )
    .unwrap();
    fs::write(tmp.path().join("thruput.toml"), "# project\n").unwrap();

    let report = thruput_cli::config::create_profile(
        tmp.path(),
        "staging",
        &thruput_cli::config::NewProfileOptions {
            network: "devnet".into(),
            rpc_url: Some("https://project.example".into()),
            network_hash: None,
            faucet_url: None,
            default_key: None,
            select: false,
        },
        false,
        true,
    )
    .unwrap();

    assert!(report.changed);
    assert_eq!(
        thruput_cli::config::load_persisted_config(tmp.path())
            .unwrap()
            .resolve("staging")
            .unwrap()
            .values
            .rpc_url
            .as_deref(),
        Some("https://project.example")
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_new_project_use_rejects_invalid_global_layer_without_changes() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "[profile.staging]\nnetwork = \"preview\"\n",
    )
    .unwrap();
    let project_path = tmp.path().join("thruput.toml");
    let project_contents = "# project\n";
    fs::write(&project_path, project_contents).unwrap();

    let error = thruput_cli::config::create_profile(
        tmp.path(),
        "staging",
        &thruput_cli::config::NewProfileOptions {
            network: "devnet".into(),
            rpc_url: None,
            network_hash: None,
            faucet_url: None,
            default_key: None,
            select: true,
        },
        false,
        true,
    )
    .unwrap_err();

    assert_eq!(error.code(), "profile_config_invalid");
    assert_eq!(fs::read_to_string(project_path).unwrap(), project_contents);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_rm_removes_complete_profile_and_matching_default() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    fs::write(
        &path,
        "# retained\ndefault_profile = \"staging\"\nplugin = true\n\n[profile.keep]\nnetwork = \"local\"\n\n[profile.staging]\nnetwork = \"devnet\"\nunknown = true\n\n[profile.staging.contracts]\ncounter = \"0x1234\"\n\n[profile.staging.wallets.alice]\naddress = \"0xabcd\"\nowner_public_key = \"0xef01\"\n",
    )
    .unwrap();

    let report = thruput_cli::config::remove_profile(tmp.path(), "staging", false, true).unwrap();

    assert!(report.changed);
    assert!(report.cleared_default);
    assert_eq!(report.scope, thruput_cli::config::ConfigScope::Project);
    assert_eq!(report.path, path);
    assert_eq!(
        fs::read_to_string(report.path).unwrap(),
        "# retained\nplugin = true\n\n[profile.keep]\nnetwork = \"local\"\n"
    );
}

#[test]
#[serial]
fn config_rm_rejects_missing_target_layer_profile_without_changes() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    let contents = "# retained\n[profile.keep]\nnetwork = \"local\"\n";
    fs::write(&path, contents).unwrap();

    let error =
        thruput_cli::config::remove_profile(tmp.path(), "missing", false, true).unwrap_err();

    assert_eq!(error.code(), "profile_not_found_in_scope");
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
}

#[test]
#[serial]
fn config_rm_preserves_comment_position_when_default_is_between_keys() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    fs::write(
        &path,
        "before = true\n# selected profile\ndefault_profile = \"staging\"\nafter = true\n\n[profile.staging]\nnetwork = \"devnet\"\n",
    )
    .unwrap();

    thruput_cli::config::remove_profile(tmp.path(), "staging", false, true).unwrap();

    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "before = true\n# selected profile\nafter = true\n"
    );
}

#[test]
#[serial]
fn config_rm_preserves_leading_comment_when_profile_is_only_content() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    fs::write(
        &path,
        "# retained\ndefault_profile = \"staging\"\n\n[profile.staging]\nnetwork = \"devnet\"\n",
    )
    .unwrap();

    thruput_cli::config::remove_profile(tmp.path(), "staging", false, true).unwrap();

    assert_eq!(fs::read_to_string(path).unwrap(), "# retained\n");
}

#[test]
#[serial]
fn concurrent_config_rm_allows_only_one_removal() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.staging]\nnetwork = \"devnet\"\n",
    )
    .unwrap();
    let project = tmp.path().to_path_buf();
    let first = std::thread::spawn(move || {
        thruput_cli::config::remove_profile(&project, "staging", false, true)
    });
    let project = tmp.path().to_path_buf();
    let second = std::thread::spawn(move || {
        thruput_cli::config::remove_profile(&project, "staging", false, true)
    });

    let results = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(thruput_cli::CliError::code)
            .collect::<Vec<_>>(),
        ["profile_not_found_in_scope"]
    );
}

#[test]
#[serial]
fn config_set_rejects_missing_target_layer_profile_without_creating_it() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "[profile.missing]\nnetwork = \"devnet\"\n",
    )
    .unwrap();
    let path = tmp.path().join("thruput.toml");
    let contents = "# retained\nplugin = true\n";
    fs::write(&path, contents).unwrap();

    let error = thruput_cli::config::set_profile_field(
        tmp.path(),
        "missing",
        thruput_cli::config::ConfigField::Network,
        "devnet",
        false,
        true,
    )
    .unwrap_err();

    assert_eq!(error.code(), "profile_not_found_in_scope");
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_set_reports_unchanged_for_existing_value() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.devnet]\nnetwork = 'devnet'\n",
    )
    .unwrap();

    let report = thruput_cli::config::set_profile_field(
        tmp.path(),
        "devnet",
        thruput_cli::config::ConfigField::Network,
        "devnet",
        false,
        true,
    )
    .unwrap();

    assert!(!report.changed);
    assert_eq!(
        fs::read_to_string(tmp.path().join("thruput.toml")).unwrap(),
        "[profile.devnet]\nnetwork = 'devnet'\n"
    );
}

#[test]
fn config_set_network_hash_parses_its_field_name_and_persists() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.devnet]\nnetwork = 'devnet'\n",
    )
    .unwrap();
    let hash = thruput_encoding::encode(&[0x33; 32]);

    // Parse the field name the way the CLI does, so a variant wired everywhere
    // except `FromStr` cannot pass this.
    let field: thruput_cli::config::ConfigField = "network-hash".parse().unwrap();
    let report =
        thruput_cli::config::set_profile_field(tmp.path(), "devnet", field, &hash, false, true)
            .unwrap();

    assert!(report.changed);
    assert!(
        fs::read_to_string(tmp.path().join("thruput.toml"))
            .unwrap()
            .contains(&format!("network_hash = \"{hash}\""))
    );
}

#[test]
fn saved_strkey_identifiers_are_persisted_as_base58() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.devnet]\nnetwork = 'devnet'\n",
    )
    .unwrap();
    let address = thruput_encoding::address_strkey(&[0xab; 32]);
    let owner = thruput_encoding::public_key_strkey(&[0xcd; 32]);
    thruput_cli::config::save_profile_contract_alias(
        tmp.path(),
        Some("devnet"),
        "counter",
        &address,
    )
    .unwrap();
    thruput_cli::config::save_profile_wallet_address(
        tmp.path(),
        Some("devnet"),
        "alice",
        &address,
        &owner,
    )
    .unwrap();
    let document: toml::Value =
        toml::from_str(&fs::read_to_string(tmp.path().join("thruput.toml")).unwrap()).unwrap();
    let profile = &document["profile"]["devnet"];
    assert_eq!(
        profile["contracts"]["counter"].as_str(),
        Some(thruput_encoding::encode(&[0xab; 32]).as_str())
    );
    assert_eq!(
        profile["wallets"]["alice"]["address"].as_str(),
        Some(thruput_encoding::encode(&[0xab; 32]).as_str())
    );
    assert_eq!(
        profile["wallets"]["alice"]["owner_public_key"].as_str(),
        Some(thruput_encoding::encode(&[0xcd; 32]).as_str())
    );
}

#[test]
fn config_set_network_hash_rejects_legacy_hex_without_writing() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.devnet]\nnetwork = 'devnet'\n",
    )
    .unwrap();
    let hash = "ab".repeat(32);
    let field: thruput_cli::config::ConfigField = "network-hash".parse().unwrap();

    let original = fs::read_to_string(tmp.path().join("thruput.toml")).unwrap();
    let error = thruput_cli::config::set_profile_field(
        tmp.path(),
        "devnet",
        field,
        &format!("0x{hash}"),
        false,
        true,
    )
    .unwrap_err();

    assert_eq!(error.code(), "config_value_invalid");
    assert_eq!(
        fs::read_to_string(tmp.path().join("thruput.toml")).unwrap(),
        original
    );
}

#[test]
fn config_set_network_hash_rejects_a_non_canonical_value() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.devnet]\nnetwork = 'devnet'\n",
    )
    .unwrap();
    let field: thruput_cli::config::ConfigField = "network-hash".parse().unwrap();

    // A legacy 64-digit hex value is not a fixed-width base58 network hash.
    let error = thruput_cli::config::set_profile_field(
        tmp.path(),
        "devnet",
        field,
        &"A".repeat(64),
        false,
        true,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        thruput_cli::CliError::ConfigValueInvalid { .. }
    ));
    assert_eq!(
        fs::read_to_string(tmp.path().join("thruput.toml")).unwrap(),
        "[profile.devnet]\nnetwork = 'devnet'\n"
    );
}

#[test]
#[cfg(unix)]
#[serial]
fn config_set_updates_global_file_symlink_target_without_replacing_link() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let global_path = global_config_path(&tmp);
    fs::create_dir_all(global_path.parent().unwrap()).unwrap();
    let managed_dir = tmp.path().join("dotfiles");
    fs::create_dir(&managed_dir).unwrap();
    let managed_path = managed_dir.join("config.toml");
    fs::write(&managed_path, "[profile.devnet]\nnetwork = \"devnet\"\n").unwrap();
    symlink(&managed_path, &global_path).unwrap();

    let report = thruput_cli::config::set_profile_field(
        tmp.path(),
        "devnet",
        thruput_cli::config::ConfigField::RpcUrl,
        "https://rpc.example",
        true,
        false,
    )
    .unwrap();

    assert!(report.changed);
    assert_eq!(report.path, global_path);
    assert!(
        fs::symlink_metadata(&global_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(managed_path).unwrap(),
        "[profile.devnet]\nnetwork = \"devnet\"\nrpc_url = \"https://rpc.example\"\n"
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_unset_missing_project_profile_is_noop_without_creating_table() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    let contents = "# retained\nplugin = true\n";
    fs::write(&path, contents).unwrap();

    let report = thruput_cli::config::unset_profile_field(
        tmp.path(),
        "missing",
        thruput_cli::config::ConfigField::FaucetUrl,
        false,
        true,
    )
    .unwrap();

    assert!(!report.changed);
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
}

#[test]
#[serial]
fn config_set_validation_failure_leaves_selected_profile_unchanged() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("thruput.toml");
    let contents = "# retained\n[profile.devnet]\nrpc_url = \"relative/path\"\nunknown = true\n";
    fs::write(&path, contents).unwrap();

    let error = thruput_cli::config::set_profile_field(
        tmp.path(),
        "devnet",
        thruput_cli::config::ConfigField::Network,
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
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    let path = global_config_path(&tmp);
    fs::write(
        &path,
        "# retained\ndefault_profile = \"local\"\nunknown = true\n\n[profile.local]\nnetwork = \"local\"\n\n[profile.devnet]\nnetwork = \"devnet\"\n",
    )
    .unwrap();

    let report = thruput_cli::config::use_profile(tmp.path(), "devnet", true, false).unwrap();

    assert!(report.changed);
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "# retained\ndefault_profile = \"devnet\"\nunknown = true\n\n[profile.local]\nnetwork = \"local\"\n\n[profile.devnet]\nnetwork = \"devnet\"\n"
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[cfg(unix)]
#[serial]
fn config_use_project_locks_global_before_updating_selection() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    let global_path = global_config_path(&tmp);
    fs::write(&global_path, "[profile.devnet]\nnetwork = \"devnet\"\n").unwrap();
    let project_path = tmp.path().join("thruput.toml");
    let project_contents = "# project\n";
    fs::write(&project_path, project_contents).unwrap();
    let _lock = lock_config(&global_path);

    let error = thruput_cli::config::use_profile(tmp.path(), "devnet", false, true).unwrap_err();

    assert_eq!(error.code(), "config_lock_timeout");
    assert_eq!(fs::read_to_string(project_path).unwrap(), project_contents);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[cfg(unix)]
#[serial]
fn config_use_project_validates_global_after_waiting_for_lock() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    let global_path = global_config_path(&tmp);
    fs::write(&global_path, "[profile.devnet]\nnetwork = \"devnet\"\n").unwrap();
    let project_path = tmp.path().join("thruput.toml");
    let project_contents = "# project\n";
    fs::write(&project_path, project_contents).unwrap();
    let lock = lock_config(&global_path);
    let project = tmp.path().to_path_buf();
    let mutation = std::thread::spawn(move || {
        thruput_cli::config::use_profile(&project, "devnet", false, true)
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    fs::write(&global_path, "[profile.devnet]\nnetwork = \"preview\"\n").unwrap();
    drop(lock);

    let error = mutation.join().unwrap().unwrap_err();

    assert_eq!(error.code(), "profile_config_invalid");
    assert_eq!(fs::read_to_string(project_path).unwrap(), project_contents);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn config_use_global_rejects_project_only_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        global_config_path(&tmp),
        "default_profile = \"local\"\n\n[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("thruput.toml"),
        "[profile.project-only]\nnetwork = \"devnet\"\n",
    )
    .unwrap();

    let error =
        thruput_cli::config::use_profile(tmp.path(), "project-only", true, false).unwrap_err();

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
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    let path = global_config_path(&tmp);
    let contents = "default_profile = 'devnet'\n\n[profile.devnet]\nnetwork = \"devnet\"\n";
    fs::write(&path, contents).unwrap();

    let report = thruput_cli::config::use_profile(tmp.path(), "devnet", true, false).unwrap();

    assert!(!report.changed);
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn malformed_toml_reports_the_config_path() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let path = tmp.path().join("thruput.toml");
    fs::write(&path, "[profile.devnet\n").unwrap();

    let error = thruput_cli::config::load_persisted_config(tmp.path()).unwrap_err();

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
fn malformed_toml_reports_character_based_column() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let path = tmp.path().join("thruput.toml");
    fs::write(&path, "name = \"é\" trailing\n").unwrap();

    let error = thruput_cli::config::load_persisted_config(tmp.path()).unwrap_err();

    assert_eq!(error.code(), "config_parse_failed");
    assert_eq!(error.details_json().unwrap()["column"], 12);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_unrelated_profile_does_not_hide_global_scalars_or_valid_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
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

    let profile = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap();
    let key = thruput_cli::config::load_default_key().unwrap();

    assert_eq!(profile.name, "valid");
    assert_eq!(key.as_deref(), Some("alice"));
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_local_runtime_state_does_not_block_non_local_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
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

    let profile = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap();

    assert_eq!(profile.name, "devnet");
    assert_eq!(profile.rpc_url.as_deref(), Some("https://rpc.example"));
    assert_eq!(
        thruput_cli::config::resolve_default_key_for_profile(&profile).unwrap(),
        None
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_local_runtime_state_is_rejected_for_local_profile() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
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

    let error = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap_err();

    assert_eq!(error.code(), "config_file_invalid");
    assert!(error.to_string().contains("local_runtime_state is invalid"));
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn invalid_persisted_default_profile_reports_its_layer_path() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    let global_path = global_config_path(&tmp);
    fs::write(&global_path, "default_profile = \" \"\n").unwrap();

    let global_error = thruput_cli::config::load_persisted_config(tmp.path()).unwrap_err();

    assert_eq!(global_error.code(), "config_file_invalid");
    assert!(
        global_error
            .to_string()
            .contains(global_path.to_string_lossy().as_ref())
    );

    fs::write(&global_path, "").unwrap();
    let project_path = tmp.path().join("thruput.toml");
    fs::write(&project_path, "default_profile = \" \"\n").unwrap();

    let project_error = thruput_cli::config::load_persisted_config(tmp.path()).unwrap_err();

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
    let project_path = tmp.path().join("thruput.toml");
    fs::write(
        &project_path,
        "default_profile = \"broken\"\n\n[profile.broken]\nnetwork = \"preview\"\n",
    )
    .unwrap();

    let error = thruput_cli::config::resolve_profile(tmp.path(), None).unwrap_err();
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

#[test]
#[serial]
fn unrelated_global_writes_preserve_ignored_legacy_executor_table() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    let legacy =
        "[executor]\npassword = 42 # legacy value is not interpreted\nplugin_token = \"keep\"\n";
    fs::write(global_config_path(&tmp), legacy).unwrap();
    thruput_cli::config::initialize_global_profiles().unwrap();
    thruput_cli::config::save_default_key("alice").unwrap();
    let contents = fs::read_to_string(global_config_path(&tmp)).unwrap();
    assert!(contents.contains(legacy));
    assert_eq!(
        thruput_cli::config::load_default_key().unwrap().as_deref(),
        Some("alice")
    );
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}
