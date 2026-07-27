use groundhog_cli::Print;
use serial_test::serial;
use std::env;
use std::fs;
use tempfile::TempDir;

mod support;

fn quiet() -> Print {
    Print::quiet()
}

fn write_groundhog_registry_config(cargo_home: &std::path::Path) {
    fs::create_dir_all(cargo_home).unwrap();
    fs::write(
        cargo_home.join("config.toml"),
        "[registries.groundhog]\nindex = \"sparse+https://groundhog.dev/crates/api/v1/crates/\"\n",
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
#[serial]
fn status_succeeds_when_fully_configured() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let devnode = support::write_versioned_devnode(tmp.path());

    unsafe {
        env::set_var("CARGO_HOME", &cargo_home);
        env::set_var("XDG_CONFIG_HOME", tmp.path());
        env::set_var("GROUNDHOG_DEVNODE_BINARY", &devnode);
    }

    write_groundhog_registry_config(&cargo_home);

    groundhog_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();

    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "test".into(),
        password: "pass".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();

    let result = groundhog_cli::status::run(tmp.path(), None, "status", &quiet());
    assert!(result.is_ok());

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
        env::remove_var("GROUNDHOG_DEVNODE_BINARY");
    }
}

#[test]
#[serial]
fn status_returns_error_when_missing_state() {
    let tmp = TempDir::new().unwrap();

    unsafe {
        env::set_var("CARGO_HOME", tmp.path().join("cargo-home"));
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    let result = groundhog_cli::status::run(tmp.path(), None, "status", &quiet());
    assert!(result.is_err());

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }
}

#[test]
#[serial]
fn collect_report_prefers_profile_default_key_over_global_default() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let project = tmp.path().join("proj");

    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("groundhog.toml"),
        "[profile.default]\ndefault_key = \"bob\"\n",
    )
    .unwrap();

    unsafe {
        env::set_var("CARGO_HOME", &cargo_home);
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    write_groundhog_registry_config(&cargo_home);

    groundhog_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();
    groundhog_cli::keys::generate(&Some("bob".into()), false, "keys.generate", &quiet()).unwrap();

    let report = groundhog_cli::checks::collect_report(&project, None).unwrap();
    assert_eq!(report.effective_default_key.as_deref(), Some("bob"));
    assert_eq!(report.effective_default_key_source, Some("profile"));
    let summary = report.effective_default_key_summary().unwrap();
    assert_eq!(summary.name, "bob");

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }
}

#[test]
#[serial]
fn collect_report_uses_builtin_dev_key_for_local_profile() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let project = tmp.path().join("proj");

    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("groundhog.toml"),
        "default_profile = \"local\"\n\n[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();

    unsafe {
        env::set_var("CARGO_HOME", &cargo_home);
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    write_groundhog_registry_config(&cargo_home);

    let report = groundhog_cli::checks::collect_report(&project, None).unwrap();
    assert_eq!(report.effective_default_key.as_deref(), Some("dev-0"));
    assert_eq!(report.effective_default_key_source, Some("local built-in"));
    assert!(!report.findings.iter().any(|finding| {
        matches!(
            finding.code,
            "no_keys" | "no_default_key" | "default_key_not_on_disk"
        )
    }));

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }
}

#[test]
#[serial]
fn collect_report_warns_when_global_default_key_is_missing_on_disk() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    unsafe {
        env::set_var("CARGO_HOME", &cargo_home);
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    write_groundhog_registry_config(&cargo_home);

    groundhog_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();

    let alice_path = groundhog_cli::config::keys_dir()
        .unwrap()
        .join("alice.json");
    fs::remove_file(alice_path).unwrap();

    let report = groundhog_cli::checks::collect_report(tmp.path(), None).unwrap();
    assert_eq!(report.effective_default_key.as_deref(), Some("alice"));
    assert!(report.effective_default_key_summary().is_none());
    assert!(
        report
            .all_findings()
            .any(|finding| finding.code == "default_key_not_on_disk")
    );

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }
}

#[test]
#[serial]
fn collect_report_uses_effective_merged_project_profile_details() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let project = tmp.path().join("proj");
    let global_hog = format!("0x{}", "11".repeat(32));
    let project_counter = format!("0x{}", "22".repeat(32));

    fs::create_dir(&project).unwrap();
    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        tmp.path().join("groundhog/config.toml"),
        format!(
            r#"
default_profile = "local"
default_key = "alice"

[profile.local]
rpc_url = "http://global.example"

[profile.local.contracts]
HOG = "{global_hog}"
"#
        ),
    )
    .unwrap();
    fs::write(
        project.join("groundhog.toml"),
        format!(
            r#"
[profile.local]
rpc_url = "http://project.example"

[profile.local.contracts]
counter = "{project_counter}"
"#
        ),
    )
    .unwrap();

    unsafe {
        env::set_var("CARGO_HOME", &cargo_home);
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    write_groundhog_registry_config(&cargo_home);
    groundhog_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();
    let creds = groundhog_cli::config::ExecutorCredentials {
        username: "test".into(),
        password: "pass".into(),
    };
    groundhog_cli::config::save_executor_credentials(&creds).unwrap();

    let report = groundhog_cli::checks::collect_report(&project, None).unwrap();
    assert_eq!(report.project.profile_name.as_deref(), Some("local"));
    assert_eq!(
        report.project.rpc_url.as_deref(),
        Some("http://project.example")
    );
    assert_eq!(report.project.contract_count, 2);
    assert_eq!(report.effective_default_key.as_deref(), Some("alice"));
    assert_eq!(report.effective_default_key_source, Some("global"));
    assert!(report.effective_default_key_summary().is_some());

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }
}

#[test]
#[serial]
fn collect_report_shows_global_profile_details_outside_project() {
    let tmp = TempDir::new().unwrap();
    let global_hog = format!("0x{}", "11".repeat(32));
    let global_wallet = format!("0x{}", "22".repeat(32));
    let global_owner = format!("0x{}", "33".repeat(32));

    fs::create_dir_all(tmp.path().join("groundhog")).unwrap();
    fs::write(
        tmp.path().join("groundhog/config.toml"),
        format!(
            r#"
default_profile = "local"

[profile.local]
rpc_url = "http://127.0.0.1:9545"
default_wallet = "global-wallet"

[profile.local.contracts]
HOG = "{global_hog}"

[profile.local.wallets.global-wallet]
address = "{global_wallet}"
owner_public_key = "{global_owner}"
"#
        ),
    )
    .unwrap();

    unsafe {
        env::set_var("CARGO_HOME", tmp.path().join("cargo-home"));
        env::set_var("XDG_CONFIG_HOME", tmp.path());
    }

    let report = groundhog_cli::checks::collect_report(tmp.path(), None).unwrap();
    assert!(report.project.config_path.is_none());
    assert_eq!(report.project.profile_name.as_deref(), Some("local"));
    assert_eq!(
        report.project.rpc_url.as_deref(),
        Some("http://127.0.0.1:9545")
    );
    assert_eq!(report.project.contract_count, 1);
    assert!(report.project.project_default_wallet.is_none());
    assert_eq!(
        report.project.effective_default_wallet.as_deref(),
        Some("global-wallet")
    );
    assert_eq!(report.project.default_wallet_source, Some("global"));

    unsafe {
        env::remove_var("CARGO_HOME");
        env::remove_var("XDG_CONFIG_HOME");
    }
}
