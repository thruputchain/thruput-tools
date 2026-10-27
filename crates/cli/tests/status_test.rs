use serial_test::serial;
use std::fs;
use tempfile::TempDir;
use thruput_cli::Print;

mod support;

#[path = "support/env_guard.rs"]
mod env_guard;
use env_guard::EnvGuard;

fn quiet() -> Print {
    Print::quiet()
}

fn write_thruput_registry_config(cargo_home: &std::path::Path) {
    fs::create_dir_all(cargo_home).unwrap();
    fs::write(
        cargo_home.join("config.toml"),
        "[registries.thruput]\nindex = \"sparse+https://crates.thruput.network/api/v1/crates/\"\n",
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

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
    let _devnode_binary = EnvGuard::set("THRUPUT_DEVNODE_BINARY", &devnode);

    write_thruput_registry_config(&cargo_home);

    thruput_cli::keys::generate(
        &Some("alice".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();

    thruput_cli::config::initialize_global_profiles().unwrap();

    let result = thruput_cli::status::run(tmp.path(), None, "status", &quiet());
    assert!(
        result.is_ok(),
        "{result:?}: {:?}",
        thruput_cli::checks::collect_report(tmp.path(), None)
            .unwrap()
            .all_findings()
            .collect::<Vec<_>>()
    );
}

#[test]
#[serial]
fn status_returns_error_when_missing_state() {
    let tmp = TempDir::new().unwrap();

    let _cargo_home = EnvGuard::set("CARGO_HOME", tmp.path().join("cargo-home"));
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let result = thruput_cli::status::run(tmp.path(), None, "status", &quiet());
    assert!(result.is_err());
}

#[test]
#[serial]
fn collect_report_prefers_profile_default_key_over_global_default() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let project = tmp.path().join("proj");

    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("thruput.toml"),
        "default_profile = \"default\"\n\n[profile.default]\ndefault_key = \"bob\"\n",
    )
    .unwrap();

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    write_thruput_registry_config(&cargo_home);

    thruput_cli::keys::generate(
        &Some("alice".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();
    thruput_cli::keys::generate(
        &Some("bob".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();

    let report = thruput_cli::checks::collect_report(&project, None).unwrap();
    assert_eq!(report.effective_default_key.as_deref(), Some("bob"));
    assert_eq!(report.effective_default_key_source, Some("profile"));
    let summary = report.effective_default_key_summary().unwrap();
    assert_eq!(summary.name, "bob");
}

#[test]
#[serial]
fn collect_report_does_not_invent_builtin_key_without_active_local_runtime() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let project = tmp.path().join("proj");

    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("thruput.toml"),
        "default_profile = \"local\"\n\n[profile.local]\nnetwork = \"local\"\n",
    )
    .unwrap();

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    write_thruput_registry_config(&cargo_home);

    let report = thruput_cli::checks::collect_report(&project, None).unwrap();
    assert_eq!(report.effective_default_key, None);
    assert_eq!(report.effective_default_key_source, None);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "no_keys")
    );
}

#[test]
#[serial]
fn collect_report_warns_when_global_default_key_is_missing_on_disk() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    write_thruput_registry_config(&cargo_home);

    thruput_cli::keys::generate(
        &Some("alice".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();

    let alice_path = thruput_cli::config::keys_dir().unwrap().join("alice.json");
    fs::remove_file(alice_path).unwrap();

    let report = thruput_cli::checks::collect_report(tmp.path(), None).unwrap();
    assert_eq!(report.effective_default_key.as_deref(), Some("alice"));
    assert!(report.effective_default_key_summary().is_none());
    assert!(
        report
            .all_findings()
            .any(|finding| finding.code == "default_key_not_on_disk")
    );
}

#[test]
#[serial]
fn collect_report_uses_effective_merged_project_profile_details() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let project = tmp.path().join("proj");
    let global_token = format!("0x{}", "11".repeat(32));
    let project_counter = format!("0x{}", "22".repeat(32));

    fs::create_dir(&project).unwrap();
    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        tmp.path().join("thruput/config.toml"),
        format!(
            r#"
default_profile = "local"
default_key = "alice"

[profile.local]
rpc_url = "http://global.example"

[profile.local.contracts]
NATIVE_TOKEN = "{global_token}"
"#
        ),
    )
    .unwrap();
    fs::write(
        project.join("thruput.toml"),
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

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    write_thruput_registry_config(&cargo_home);
    thruput_cli::keys::generate(
        &Some("alice".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &quiet(),
    )
    .unwrap();

    let report = thruput_cli::checks::collect_report(&project, None).unwrap();
    assert_eq!(report.project.profile_name.as_deref(), Some("local"));
    assert_eq!(
        report.project.rpc_url.as_deref(),
        Some("http://project.example")
    );
    assert_eq!(report.project.contract_count, 2);
    assert_eq!(report.effective_default_key.as_deref(), Some("alice"));
    assert_eq!(report.effective_default_key_source, Some("global"));
    assert!(report.effective_default_key_summary().is_some());
}

#[test]
#[serial]
fn collect_report_shows_global_profile_details_outside_project() {
    let tmp = TempDir::new().unwrap();
    let global_token = format!("0x{}", "11".repeat(32));
    let global_wallet = format!("0x{}", "22".repeat(32));
    let global_owner = format!("0x{}", "33".repeat(32));

    fs::create_dir_all(tmp.path().join("thruput")).unwrap();
    fs::write(
        tmp.path().join("thruput/config.toml"),
        format!(
            r#"
default_profile = "local"

[profile.local]
rpc_url = "http://127.0.0.1:9545"
default_wallet = "global-wallet"

[profile.local.contracts]
NATIVE_TOKEN = "{global_token}"

[profile.local.wallets.global-wallet]
address = "{global_wallet}"
owner_public_key = "{global_owner}"
"#
        ),
    )
    .unwrap();

    let _cargo_home = EnvGuard::set("CARGO_HOME", tmp.path().join("cargo-home"));
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let report = thruput_cli::checks::collect_report(tmp.path(), None).unwrap();
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
}

#[test]
#[serial]
#[cfg(unix)]
fn quiet_status_exits_zero_with_no_output_when_fully_configured() {
    let tmp = TempDir::new().unwrap();
    let cargo_home = tmp.path().join("cargo-home");
    let devnode = support::write_versioned_devnode(tmp.path());

    let _cargo_home = EnvGuard::set("CARGO_HOME", &cargo_home);
    let _config_home = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    thruput_cli::keys::generate(
        &Some("alice".into()),
        thruput_cli::keys::KeyScheme::Ed25519,
        false,
        "keys.generate",
        &thruput_cli::Print::quiet(),
    )
    .unwrap();
    thruput_cli::config::initialize_global_profiles().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_thruput"))
        .arg("-q")
        .arg("status")
        .env_remove("THRUPUT_PROFILE")
        .env_remove("THRUPUT_RUNTIME_MANIFEST")
        .env_remove("THRUPUT_BUNDLE_MANIFEST")
        .env_remove("THRUPUT_OFFLINE")
        .env("CARGO_HOME", &cargo_home)
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("THRUPUT_DEVNODE_BINARY", devnode)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
