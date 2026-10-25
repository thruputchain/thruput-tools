use serial_test::serial;
use std::fs;
use tempfile::TempDir;
use thruput_cli::Print;
use thruput_cli::init::InitOptions;

#[path = "support/env_guard.rs"]
mod env_guard;
use env_guard::EnvGuard;

fn quiet() -> Print {
    Print::quiet()
}

#[test]
#[serial_test::parallel]
fn new_creates_subdirectory_and_scaffolds() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-project");

    thruput_cli::new::run_in(&dir, "my-project", Some("counter"), "new", &quiet()).unwrap();

    assert!(dir.join("Cargo.toml").exists());
    assert!(dir.join(".gitignore").exists());
    assert!(dir.join("thruput.toml").exists());
    assert!(dir.join("src/lib.rs").exists());
    let config = fs::read_to_string(dir.join("thruput.toml")).unwrap();
    assert!(!config.contains("\ndefault_profile ="));
    assert!(config.contains("[profile.local]"));
    assert!(config.contains("network = \"local\""));
    assert!(config.contains("[profile.local.contracts]"));
    assert!(config.contains("# [profile.local.wallets.alice]"));
    assert!(config.contains("[profile.futurenet]"));
    assert!(config.contains("network = \"futurenet\""));
    assert!(config.contains("[profile.futurenet.contracts]"));
    assert!(config.contains("# [profile.futurenet.wallets.alice]"));
    assert!(config.contains("# owner_public_key ="));
    assert!(!config.contains("rpc_url"));
    assert!(!config.contains("faucet_url"));
    assert!(!config.contains("default_key"));
}

#[test]
#[serial_test::parallel]
fn new_fails_if_dir_exists() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("existing");
    fs::create_dir(&dir).unwrap();

    let result = thruput_cli::new::run_in(&dir, "existing", Some("counter"), "new", &quiet());
    assert!(result.is_err());
}

#[test]
#[serial]
fn init_configures_profiles_and_succeeds() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    // Pre-store a key so the wizard does not prompt for key generation.
    thruput_cli::keys::generate(
        &Some("default".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();

    let result = thruput_cli::init::run(tmp.path(), &InitOptions::default(), "init", &quiet());

    result.unwrap();
    assert!(!cargo_home.join("config.toml").exists());
    let global_config = fs::read_to_string(tmp.path().join("thruput/config.toml")).unwrap();
    assert!(global_config.contains("default_profile = \"local\""));
    assert!(global_config.contains("[profile.local]"));
    assert!(global_config.contains("network = \"local\""));
    assert!(global_config.contains("futurenet"));
    assert!(global_config.contains("NATIVE_TOKEN = \"11111111111111111111111111111112\""));
    let local = thruput_cli::config::resolve_profile(tmp.path(), Some("local")).unwrap();
    assert_eq!(local.network, Some(thruput_cli::config::Network::Local));
    assert!(local.rpc_url.is_none());
    assert_eq!(
        local.contracts.get("NATIVE_TOKEN").map(String::as_str),
        Some("11111111111111111111111111111112")
    );
    let futurenet = thruput_cli::config::resolve_profile(tmp.path(), Some("futurenet")).unwrap();
    assert_eq!(futurenet.name, "futurenet");
    assert_eq!(
        futurenet.network,
        Some(thruput_cli::config::Network::Futurenet)
    );
    assert_eq!(
        futurenet.rpc_url.as_deref(),
        Some("https://rpc.thruput.network")
    );
    assert_eq!(
        futurenet.faucet_url.as_deref(),
        Some("https://faucet.thruput.network")
    );
    assert_eq!(
        futurenet.contracts.get("NATIVE_TOKEN").map(String::as_str),
        Some("11111111111111111111111111111112")
    );
}

#[test]
#[serial]
fn init_preserves_existing_global_profile_fields() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        tmp.path().join("thruput/config.toml"),
        r#"
default_profile = "devnet"
default_key = "alice"
plugin = "keep"

[executor]
username = "test"
password = "pass"

[profile.local]
rpc_url = "http://127.0.0.1:9999"
plugin_field = "keep"

[profile.devnet]
rpc_url = "https://custom-rpc.devnet.example"
faucet_url = "https://custom-faucet.devnet.example"

[profile.local.wallets.alice]
address = "0x1111111111111111111111111111111111111111111111111111111111111111"
owner_public_key = "0x2222222222222222222222222222222222222222222222222222222222222222"
"#,
    )
    .unwrap();
    thruput_cli::keys::generate(
        &Some("alice".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();

    thruput_cli::init::run(tmp.path(), &InitOptions::default(), "init", &quiet()).unwrap();

    let global_config = fs::read_to_string(tmp.path().join("thruput/config.toml")).unwrap();
    assert!(global_config.contains("default_profile = \"devnet\""));
    assert!(global_config.contains("default_key = \"alice\""));
    assert!(global_config.contains("plugin = \"keep\""));
    assert!(global_config.contains("rpc_url = \"http://127.0.0.1:9999\""));
    assert!(global_config.contains("plugin_field = \"keep\""));
    assert!(global_config.contains("rpc_url = \"https://custom-rpc.devnet.example\""));
    assert!(global_config.contains("faucet_url = \"https://custom-faucet.devnet.example\""));
    assert!(global_config.contains("[profile.local.wallets.alice]"));
    assert!(global_config.contains(
        "address = \"0x1111111111111111111111111111111111111111111111111111111111111111\""
    ));
    assert!(global_config.contains("devnet"));
    let devnet = thruput_cli::config::resolve_profile(tmp.path(), Some("devnet")).unwrap();
    assert_eq!(devnet.name, "devnet");
    assert_eq!(devnet.network, Some(thruput_cli::config::Network::Devnet));
    assert_eq!(
        devnet.rpc_url.as_deref(),
        Some("https://custom-rpc.devnet.example")
    );
    assert_eq!(
        devnet.faucet_url.as_deref(),
        Some("https://custom-faucet.devnet.example")
    );
    assert_eq!(
        devnet.contracts.get("NATIVE_TOKEN").map(String::as_str),
        Some("11111111111111111111111111111112")
    );
    let future = thruput_cli::config::resolve_profile(tmp.path(), Some("futurenet")).unwrap();
    assert_eq!(
        future.network,
        Some(thruput_cli::config::Network::Futurenet)
    );
}

#[test]
#[serial]
fn init_preserves_ignored_legacy_executor_config() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let legacy = "[executor]\nusername = \"legacy\"\npassword = \"private-password\"\n";
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(tmp.path().join("thruput/config.toml"), legacy).unwrap();

    thruput_cli::keys::generate(
        &Some("default".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();

    thruput_cli::init::run(tmp.path(), &InitOptions::default(), "init", &quiet()).unwrap();

    let contents = fs::read_to_string(tmp.path().join("thruput/config.toml")).unwrap();
    assert!(contents.contains(legacy));
}

#[test]
#[serial]
fn quiet_init_does_not_generate_a_key_without_consent() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    assert!(thruput_cli::init::run(tmp.path(), &InitOptions::default(), "init", &quiet()).is_err());

    let contents = fs::read_to_string(tmp.path().join("thruput/config.toml")).unwrap();
    assert!(!contents.contains("[executor]"));
    let keys = thruput_cli::keys::load_key_summaries().unwrap();
    assert!(keys.is_empty());
    assert!(!tmp.path().join("my-contract").exists());
}

#[test]
#[serial]
fn init_rejects_dangling_project_key_despite_global_default() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let project = tmp.path().join("proj");

    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("thruput.toml"),
        "default_profile = \"default\"\n[profile.default]\ndefault_key = \"ghost\"\n",
    )
    .unwrap();

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    thruput_cli::keys::generate(
        &Some("alice".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();

    assert!(thruput_cli::init::run(&project, &InitOptions::default(), "init", &quiet()).is_err());
}

#[test]
#[serial_test::parallel]
fn new_rejects_reserved_name_without_creating_directory() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("test");

    let result = thruput_cli::new::run_in(&dir, "test", Some("counter"), "new", &quiet());

    assert!(result.is_err());
    assert!(
        !dir.exists(),
        "a rejected project name must not leave a directory behind"
    );
}
