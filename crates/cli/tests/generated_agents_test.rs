//! Check the generated guide against the CLI and project it describes.

use std::{fs, path::Path, process::Command};

fn groundhog(home: &Path, directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
    command
        .current_dir(directory)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"));
    command
}

#[test]
fn generated_guides_identify_their_template_and_supply_working_offline_context_commands() {
    let temp = tempfile::tempdir().unwrap();
    for template in ["counter", "stub"] {
        let name = format!("guide-{template}");
        let created = groundhog(temp.path(), temp.path())
            .args(["new", &name, "--template", template, "--json"])
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
        let project = temp.path().join(&name);
        let guide = fs::read_to_string(project.join("AGENTS.md")).unwrap();
        assert!(guide.contains(&format!(
            "Scaffolded with Groundhog {}, template `{template}`",
            env!("CARGO_PKG_VERSION")
        )));
        let manifest: toml::Value =
            toml::from_str(&fs::read_to_string(project.join("Cargo.toml")).unwrap()).unwrap();
        for feature in manifest["features"].as_table().unwrap().keys() {
            assert!(
                guide.contains(&format!("`{feature}`")),
                "missing scaffold feature {feature}"
            );
        }
        for topic in [None, Some("contracts"), Some("cli"), Some("typescript")] {
            let mut args = vec!["skill", "show"];
            if let Some(topic) = topic {
                args.push(topic);
            }
            assert!(guide.contains(&format!("groundhog {}", args.join(" "))));
            // Reading the bundled skill must work without Node, Git, Cargo, or
            // an installed external skill, even from a fresh project.
            let output = groundhog(temp.path(), &project)
                .env("PATH", "")
                .args(&args)
                .arg("--offline")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            assert!(String::from_utf8_lossy(&output.stdout).contains("groundhog-cli-version:"));
        }
        assert!(!guide.contains("groundhog --skill"));
        assert!(!guide.contains("{{"));
        assert!(!guide.contains("npm run"));
    }
}

#[test]
fn the_documented_docs_export_works_outside_the_generated_project() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    groundhog_cli::scaffold::generate(&project, "project", "counter").unwrap();
    let guide = fs::read_to_string(project.join("AGENTS.md")).unwrap();
    assert!(guide.contains("groundhog docs export --output <new-directory>"));
    let destination = temp.path().join("docs");
    let exported = groundhog(temp.path(), &project)
        .args(["docs", "export", "--output"])
        .arg(&destination)
        .args(["--offline", "--json"])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(
        report["data"]["index"],
        destination.join("index.md").to_str().unwrap()
    );
    assert!(destination.join("sections/contracts.md").is_file());
    assert!(!project.join("docs").exists());
}
