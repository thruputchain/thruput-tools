use std::fs;
use tempfile::TempDir;

#[test]
fn ensure_cargo_registry_creates_entry() {
    let tmp = TempDir::new().unwrap();
    let cargo_config = tmp.path().join("config.toml");

    groundhog_cli::auth::ensure_cargo_registry(&cargo_config).unwrap();

    let contents = fs::read_to_string(&cargo_config).unwrap();
    assert!(contents.contains("[registries.groundhog]"));
    assert!(contents.contains("sparse+https://"));
}

#[test]
fn ensure_cargo_registry_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let cargo_config = tmp.path().join("config.toml");

    groundhog_cli::auth::ensure_cargo_registry(&cargo_config).unwrap();
    groundhog_cli::auth::ensure_cargo_registry(&cargo_config).unwrap();

    let contents = fs::read_to_string(&cargo_config).unwrap();
    assert_eq!(contents.matches("[registries.groundhog]").count(), 1);
}

#[test]
fn ensure_cargo_registry_appends_to_existing() {
    let tmp = TempDir::new().unwrap();
    let cargo_config = tmp.path().join("config.toml");
    fs::write(&cargo_config, "[build]\ntarget-dir = \"custom\"\n").unwrap();

    groundhog_cli::auth::ensure_cargo_registry(&cargo_config).unwrap();

    let contents = fs::read_to_string(&cargo_config).unwrap();
    assert!(contents.contains("target-dir = \"custom\""));
    assert!(contents.contains("[registries.groundhog]"));
}
