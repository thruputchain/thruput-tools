use std::process::Command;

use groundhog_toolchain::Catalog;

fn default_version() -> String {
    Catalog::embedded()
        .expect("bug: embedded catalog should validate")
        .default_version()
        .to_owned()
}

fn thruput(config: &std::path::Path, cache: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
    command
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_CACHE_HOME", cache)
        .env_remove("THRUPUT_OFFLINE")
        .env_remove("THRUPUT_TOOLCHAIN_BASE_URL");
    command
}

#[test]
fn toolchain_use_persists_an_exact_catalog_version() {
    let version = default_version();
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let config = root.path().join("config");
    let cache = root.path().join("cache");

    let use_output = thruput(&config, &cache)
        .args(["--json", "toolchain", "use", version.as_str()])
        .output()
        .expect("bug: thruput should run");
    assert!(use_output.status.success());
    let use_json: serde_json::Value =
        serde_json::from_slice(&use_output.stdout).expect("bug: use output should be JSON");
    assert_eq!(use_json["command"], "toolchain.use");
    assert_eq!(use_json["data"]["schemaVersion"], 1);
    assert_eq!(use_json["data"]["version"], version);
    assert_eq!(use_json["data"]["active"], true);
    assert_eq!(use_json["data"]["installed"], false);
    assert_eq!(use_json["data"]["changed"], true);

    let config_text = std::fs::read_to_string(config.join("thruput/config.toml"))
        .expect("bug: global config should exist");
    assert!(config_text.contains("[toolchain]"));
    assert!(config_text.contains(&format!("hog_devnode_version = \"{version}\"")));

    let list_output = thruput(&config, &cache)
        .args(["--json", "toolchain", "list"])
        .output()
        .expect("bug: thruput should run");
    assert!(list_output.status.success());
    let list_json: serde_json::Value =
        serde_json::from_slice(&list_output.stdout).expect("bug: list output should be JSON");
    assert_eq!(list_json["command"], "toolchain.list");
    assert_eq!(list_json["data"]["selectedVersion"], version);
    let toolchains = list_json["data"]["toolchains"]
        .as_array()
        .expect("bug: toolchains should be an array");
    let selected = toolchains
        .iter()
        .find(|toolchain| toolchain["version"] == version)
        .expect("bug: selected version should be listed");
    assert_eq!(selected["active"], true);
    assert!(
        toolchains
            .iter()
            .filter(|toolchain| toolchain["version"] != version)
            .all(|toolchain| toolchain["active"] == false)
    );

    let repeated_use_output = thruput(&config, &cache)
        .args(["--json", "toolchain", "use", version.as_str()])
        .output()
        .expect("bug: thruput should run");
    assert!(repeated_use_output.status.success());
    let repeated_use_json: serde_json::Value = serde_json::from_slice(&repeated_use_output.stdout)
        .expect("bug: repeated use output should be JSON");
    assert_eq!(repeated_use_json["data"]["changed"], false);
}

#[test]
fn toolchain_use_accepts_the_previous_release() {
    // Users who saved the previous default must keep a selectable version.
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let config = root.path().join("config");
    let cache = root.path().join("cache");

    let output = thruput(&config, &cache)
        .args(["--json", "toolchain", "use", "0.1.30-rc.1"])
        .output()
        .expect("bug: thruput should run");
    assert!(output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("bug: use output should be JSON");
    assert_eq!(json["data"]["version"], "0.1.30-rc.1");
    assert_ne!(json["data"]["version"], default_version());
}

#[test]
fn offline_install_fails_without_touching_the_network() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let config = root.path().join("config");
    let cache = root.path().join("cache");

    let output = thruput(&config, &cache)
        .args(["--json", "--offline", "toolchain", "install"])
        .output()
        .expect("bug: thruput should run");

    assert!(!output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("bug: error output should be JSON");
    assert_eq!(json["command"], "toolchain.install");
    assert_eq!(json["error"]["code"], "toolchain_offline_missing");
    assert!(
        !cache
            .join("thruput/toolchain/hog-devnode")
            .join(default_version())
            .exists()
    );
}

#[test]
fn toolchain_which_is_local_and_reports_a_missing_installation() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let output = thruput(&root.path().join("config"), &root.path().join("cache"))
        .args(["--json", "toolchain", "which"])
        .output()
        .expect("bug: thruput should run");

    assert!(!output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("bug: error output should be JSON");
    assert_eq!(json["command"], "toolchain.which");
    assert_eq!(json["error"]["code"], "toolchain_not_installed");
}

#[test]
fn toolchain_clean_is_a_lock_aware_noop_when_absent() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let output = thruput(&root.path().join("config"), &root.path().join("cache"))
        .args(["--json", "toolchain", "clean"])
        .output()
        .expect("bug: thruput should run");

    assert!(output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("bug: clean output should be JSON");
    assert_eq!(json["command"], "toolchain.clean");
    assert_eq!(json["data"]["changed"], false);
    assert_eq!(json["data"]["installed"], false);
}

#[test]
fn toolchain_use_rejects_a_version_outside_the_embedded_catalog() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let output = thruput(&root.path().join("config"), &root.path().join("cache"))
        .args(["--json", "toolchain", "use", "0.1.12"])
        .output()
        .expect("bug: thruput should run");

    assert!(!output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("bug: error output should be JSON");
    assert_eq!(json["error"]["code"], "toolchain_catalog_error");
}

#[test]
fn groundhog_offline_environment_blocks_a_missing_install() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let output = thruput(&root.path().join("config"), &root.path().join("cache"))
        .env("THRUPUT_OFFLINE", "1")
        .args(["--json", "toolchain", "install"])
        .output()
        .expect("bug: thruput should run");

    assert!(!output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("bug: error output should be JSON");
    assert_eq!(json["error"]["code"], "toolchain_offline_missing");
}

#[test]
fn toolchain_list_human_output_is_stdout_data() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let output = thruput(&root.path().join("config"), &root.path().join("cache"))
        .args(["toolchain", "list"])
        .output()
        .expect("bug: thruput should run");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("bug: list output should be UTF-8");
    assert!(stdout.contains(&format!("{} (active) — available", default_version())));
}

#[test]
fn status_never_creates_or_downloads_managed_toolchain_state() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let cache = root.path().join("cache");
    let output = thruput(&root.path().join("config"), &cache)
        .env("THRUPUT_TOOLCHAIN_BASE_URL", "not a URL")
        .args(["--json", "status"])
        .output()
        .expect("bug: thruput should run");

    assert!(!output.status.success());
    assert!(!cache.join("thruput/toolchain").exists());
}

#[test]
fn stale_selection_can_be_inspected_replaced_and_cleaned() {
    let root = tempfile::tempdir().expect("bug: tempdir");
    let config = root.path().join("config");
    let cache = root.path().join("cache");
    std::fs::create_dir_all(config.join("thruput")).expect("bug: config dir");
    std::fs::write(
        config.join("thruput/config.toml"),
        "[toolchain]\nhog_devnode_version = \"0.1.13\"\n",
    )
    .expect("bug: config");
    let list = thruput(&config, &cache)
        .args(["--json", "toolchain", "list"])
        .output()
        .expect("bug: list");
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&list.stdout).expect("bug: JSON");
    assert_eq!(report["data"]["selectedVersion"], "0.1.13");
    assert_eq!(report["data"]["toolchains"][0]["active"], false);
    let target = groundhog_toolchain::HostTarget::current().expect("bug: target");
    let retired = cache
        .join("thruput/toolchain/hog-devnode/0.1.13")
        .join(target.triple());
    std::fs::create_dir_all(&retired).expect("bug: retired dir");
    std::fs::write(retired.join("hog-devnode"), "old binary").expect("bug: retired binary");
    let clean = thruput(&config, &cache)
        .args(["--json", "toolchain", "clean", "0.1.13"])
        .output()
        .expect("bug: clean");
    assert!(
        clean.status.success(),
        "{}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert!(!retired.exists());
    let catalog = groundhog_toolchain::Catalog::embedded().expect("bug: catalog");
    let repair = thruput(&config, &cache)
        .args(["--json", "toolchain", "use", catalog.default_version()])
        .output()
        .expect("bug: use");
    assert!(
        repair.status.success(),
        "{}",
        String::from_utf8_lossy(&repair.stderr)
    );
}

#[test]
fn which_reports_the_executable_and_offline_reuse_needs_no_source() {
    use groundhog_toolchain::{Catalog, HostTarget};
    let root = tempfile::tempdir().expect("bug: tempdir");
    let config = root.path().join("config");
    let cache = root.path().join("cache");
    let catalog = Catalog::embedded().expect("bug: catalog");
    let target = HostTarget::current().expect("bug: target");
    let version = catalog.default_version();
    let artifact = catalog.artifact(version, target).expect("bug: artifact");
    let bundle = cache
        .join("thruput/toolchain/hog-devnode")
        .join(version)
        .join(target.triple());
    for file in artifact.files() {
        let path = bundle.join(file.path());
        std::fs::create_dir_all(path.parent().expect("bug: parent")).expect("bug: dirs");
        std::fs::write(path, "fixture").expect("bug: file");
    }
    std::fs::write(
        bundle.join(".ok"),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "version": version, "target": target.triple(),
            "archiveSha256": artifact.sha256(), "layoutVersion": artifact.layout_version(),
        }))
        .expect("bug: serialize"),
    )
    .expect("bug: completion");
    let which = thruput(&config, &cache)
        .args(["toolchain", "which"])
        .output()
        .expect("bug: which");
    assert!(which.status.success());
    assert_eq!(
        String::from_utf8(which.stdout).expect("bug: utf8").trim(),
        bundle.join("hog-devnode").to_str().expect("bug: utf8 path")
    );
    let reuse = thruput(&config, &cache)
        .env("THRUPUT_TOOLCHAIN_BASE_URL", "invalid source")
        .args(["--json", "--offline", "toolchain", "install"])
        .output()
        .expect("bug: reuse");
    assert!(
        reuse.status.success(),
        "{}",
        String::from_utf8_lossy(&reuse.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&reuse.stdout).expect("bug: JSON");
    assert_eq!(
        report["data"]["path"],
        bundle.join("hog-devnode").to_str().expect("bug: utf8 path")
    );
    assert_eq!(report["data"]["sourceKind"], "managedCache");
    assert_eq!(report["data"]["changed"], false);
}

#[test]
fn toolchain_clean_invalid_version_preserves_catalog_error_and_hint() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let output = thruput(&root.path().join("config"), &root.path().join("cache"))
        .args(["--json", "toolchain", "clean", "../0.1.29-rc.4"])
        .output()
        .expect("bug: thruput should run");
    assert!(!output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("bug: error output should be JSON");
    assert_eq!(json["error"]["code"], "toolchain_catalog_error");
    assert_eq!(
        json["error"]["hint"],
        "use a hog-devnode version and target present in this thruput release's embedded catalog"
    );
}

#[test]
fn toolchain_install_rejects_invalid_proxy_without_exposing_credentials() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let output = thruput(&root.path().join("config"), &root.path().join("cache"))
        .env("HTTPS_PROXY", "ftp://review-user:review-secret@localhost:9")
        .args(["--json", "toolchain", "install"])
        .output()
        .expect("bug: thruput should run");
    assert!(!output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("bug: error output should be JSON");
    assert_eq!(json["error"]["code"], "proxy_configuration_invalid");
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("review-user"));
        assert!(!text.contains("review-secret"));
    }
}

#[test]
fn toolchain_clean_accepts_safe_retired_version_names() {
    let root = tempfile::tempdir().expect("bug: tempdir should create");
    let output = thruput(&root.path().join("config"), &root.path().join("cache"))
        .args(["--json", "toolchain", "clean", "v0.1.29-rc.4"])
        .output()
        .expect("bug: thruput should run");
    assert!(output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("bug: clean output should be JSON");
    assert_eq!(json["data"]["version"], "v0.1.29-rc.4");
    assert_eq!(json["data"]["changed"], false);
}

#[test]
fn toolchain_url_errors_never_expose_supplied_values() {
    for source in [
        "https://review-user:review-secret@example.invalid",
        "https://example.invalid/?token=review-secret",
        "https://example.invalid/#review-secret",
        "ftp://review-user:review-secret@example.invalid",
        "https://review-user:review-secret@[broken",
    ] {
        for json in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut command = thruput(&root.path().join("config"), &root.path().join("cache"));
            if json {
                command.arg("--json");
            }
            for key in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                command.env_remove(key);
            }
            let output = command
                .args(["toolchain", "install"])
                .env("THRUPUT_TOOLCHAIN_BASE_URL", source)
                .output()
                .unwrap();
            assert!(!output.status.success());
            let stderr = String::from_utf8(output.stderr).unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(stderr.contains("artifact base URL"), "{stderr}");
            for sensitive in ["review-user", "review-secret", "example.invalid", "broken"] {
                assert!(!stderr.contains(sensitive), "{stderr}");
                assert!(!stdout.contains(sensitive), "{stdout}");
            }
            if json {
                let report: serde_json::Value = serde_json::from_str(&stderr).unwrap();
                assert_eq!(report["ok"], false);
            }
        }
    }
}
