use std::{fs, process::Command};
use tempfile::TempDir;

fn cli(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(args)
        .args(["--json", "--non-interactive"])
        .env("XDG_CONFIG_HOME", root.join("unconfigured"))
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn discovery_preserves_human_data_on_stdout_in_quiet_mode() {
    for args in [
        vec!["templates", "list"],
        vec!["templates", "show", "counter"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .args(args)
            .arg("--quiet")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(String::from_utf8_lossy(&output.stdout).contains("counter"));
    }
}

#[test]
fn minimal_scaffold_is_offline_and_has_no_template() {
    let tmp = TempDir::new().unwrap();
    let output = cli(tmp.path(), &["new", "minimal"]);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data"]["schemaVersion"], 2);
    assert_eq!(report["data"]["template"], serde_json::Value::Null);
    let source = fs::read_to_string(tmp.path().join("minimal/src/implementation.rs")).unwrap();
    assert!(source.contains("pub fn initialize(&mut self) {}"));
    assert!(!source.contains("Counter"));
    assert!(!source.contains("Error"));
}

#[test]
fn discovery_and_generation_share_the_catalog() {
    let tmp = TempDir::new().unwrap();
    let output = cli(tmp.path(), &["templates", "list"]);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = report["data"]["templates"].as_array().unwrap();
    assert_eq!(entries.len(), groundhog_cli::templates::CATALOG.len());
    for entry in entries {
        let id = entry["id"].as_str().unwrap();
        assert_ne!(id, "stub");
        let show = cli(tmp.path(), &["templates", "show", id]);
        assert!(show.status.success());
        let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
        assert_eq!(&show["data"]["template"], entry);
        let output = cli(
            tmp.path(),
            &["new", &format!("example-{id}"), "--template", id],
        );
        assert!(output.status.success(), "{output:?}");
    }
}

#[test]
fn invalid_template_leaves_no_directory_and_has_structured_error() {
    let tmp = TempDir::new().unwrap();
    let output = cli(tmp.path(), &["new", "example", "--template", "missing"]);
    assert!(!output.status.success());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "unknown_template");
    assert!(!tmp.path().join("example").exists());
}

#[test]
fn explicit_destination_accepts_empty_directory_and_protects_existing_files() {
    let tmp = TempDir::new().unwrap();
    let output = cli(tmp.path(), &["new", "my-contract", "--path", "."]);
    assert!(output.status.success(), "{output:?}");
    let manifest = fs::read(tmp.path().join("Cargo.toml")).unwrap();
    let output = cli(tmp.path(), &["new", "another", "--path", "."]);
    assert!(!output.status.success());
    assert_eq!(fs::read(tmp.path().join("Cargo.toml")).unwrap(), manifest);
}

#[test]
fn project_reports_resolve_parent_components_in_destination_paths() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("workspace");
    fs::create_dir(&cwd).unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let output = cli(&cwd, &["new", "example", "--path", "../example"]);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["data"]["path"],
        root.join("example").to_str().unwrap()
    );
    assert_eq!(
        report["data"]["configPath"],
        root.join("example/groundhog.toml").to_str().unwrap()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(["new", "human", "--path", "../human", "--non-interactive"])
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let destination = root.join("human");
    assert!(stderr.contains(destination.to_str().unwrap()), "{stderr}");
    assert!(
        stderr.contains(destination.join("groundhog.toml").to_str().unwrap()),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("cd {}", destination.display())),
        "{stderr}"
    );
    assert!(!stderr.contains("/../"), "{stderr}");
}

#[test]
fn missing_name_is_actionable_without_a_terminal() {
    let tmp = TempDir::new().unwrap();
    let output = cli(tmp.path(), &["new"]);
    assert!(!output.status.success());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("pass a project name")
    );
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
}
