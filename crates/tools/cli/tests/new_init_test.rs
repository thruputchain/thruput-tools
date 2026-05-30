use groundhog_cli::Print;
use serial_test::serial;
use std::env;
use std::fs;
use tempfile::TempDir;

fn quiet() -> Print {
    Print::quiet()
}

#[test]
fn new_creates_subdirectory_and_scaffolds() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-project");

    groundhog_cli::new::run_in(&dir, "my-project", "counter", "new", &quiet()).unwrap();

    assert!(dir.join("Cargo.toml").exists());
    assert!(dir.join("groundhog.toml").exists());
    assert!(dir.join("src/lib.rs").exists());
    let config = fs::read_to_string(dir.join("groundhog.toml")).unwrap();
    assert!(config.contains("[profile.default.contracts]"));
}

#[test]
fn new_fails_if_dir_exists() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("existing");
    fs::create_dir(&dir).unwrap();

    let result = groundhog_cli::new::run_in(&dir, "existing", "counter", "new", &quiet());
    assert!(result.is_err());
}

#[test]
#[serial]
fn init_configures_registry_and_succeeds() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    unsafe {
        env::set_var("CARGO_HOME", &cargo_home);
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    // Pre-store credentials and a key so the wizard does not
    // try to run interactive login or key generation.
    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "test".into(),
        password: "pass".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();
    groundhog_cli::keys::generate(&Some("default".into()), false, "keys.generate", &quiet())
        .unwrap();

    let result = groundhog_cli::init::run(tmp.path(), "status", &quiet());

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }

    result.unwrap();
    let cargo_config = cargo_home.join("config.toml");
    let contents = fs::read_to_string(cargo_config).unwrap();
    assert!(contents.contains("[registries.groundhog]"));
}

#[test]
#[serial]
fn init_with_credentials_skips_login_hint() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    unsafe {
        env::set_var("CARGO_HOME", &cargo_home);
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    groundhog_cli::auth::ensure_cargo_registry(&cargo_home.join("config.toml")).unwrap();

    // Pre-populate executor credentials.
    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "test".into(),
        password: "pass".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();

    groundhog_cli::keys::generate(&Some("default".into()), false, "keys.generate", &quiet())
        .unwrap();

    groundhog_cli::init::run(tmp.path(), "status", &quiet()).unwrap();

    let loaded = groundhog_cli::config::load_executor_credentials().unwrap();
    assert!(loaded.is_some());

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }
}

#[test]
#[serial]
fn init_uses_global_default_when_project_key_is_dangling() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let project = tmp.path().join("proj");

    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("groundhog.toml"),
        "[profile.default]\ndefault_key = \"ghost\"\n",
    )
    .unwrap();

    unsafe {
        env::set_var("CARGO_HOME", &cargo_home);
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    groundhog_cli::auth::ensure_cargo_registry(&cargo_home.join("config.toml")).unwrap();

    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "test".into(),
        password: "pass".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();

    groundhog_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();

    groundhog_cli::init::run(&project, "status", &quiet()).unwrap();

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }
}
