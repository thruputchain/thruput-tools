//! Check the generated guide against the CLI and project it describes.

use std::{fs, path::Path, process::Command};

fn thruput(home: &Path, directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
    command
        .current_dir(directory)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"));
    command
}

#[test]
fn generated_guides_route_to_project_files_and_optional_offline_guidance() {
    let temp = tempfile::tempdir().unwrap();
    for template in [None, Some("counter"), Some("counter-app")] {
        let template_name = template.unwrap_or("minimal");
        let name = format!("guide-{template_name}");
        let mut command = thruput(temp.path(), temp.path());
        command.args(["new", &name, "--json"]);
        if let Some(template) = template {
            command.args(["--template", template]);
        }
        let created = command.output().unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
        let project = temp.path().join(&name);
        let guide = fs::read_to_string(project.join("AGENTS.md")).unwrap();
        assert!(guide.contains("guidance is\noptional"));
        for path in [
            "README.md",
            "src/implementation.rs",
            "tests/common/mod.rs",
            "tests/test_rv32.rs",
        ] {
            assert!(guide.contains(path));
            assert!(
                project.join(path).is_file(),
                "missing guide destination {path}"
            );
        }
        for group in ["skill", "docs"] {
            assert!(guide.contains(&format!("thruput {group} --help")));
            let help = thruput(temp.path(), &project)
                .env("PATH", "")
                .args([group, "--help"])
                .output()
                .unwrap();
            assert!(
                help.status.success(),
                "{}",
                String::from_utf8_lossy(&help.stderr)
            );
        }
        let missing = thruput(temp.path(), &project)
            .env("PATH", "")
            .args(["skill", "show", "--offline"])
            .output()
            .unwrap();
        assert!(!missing.status.success());
        assert!(String::from_utf8_lossy(&missing.stderr).contains("thruput skill install"));
        let installed = project.join(".agents/skills/thruput");
        fs::create_dir_all(installed.join("references")).unwrap();
        fs::write(installed.join("SKILL.md"), "Installed entry\n").unwrap();
        for topic in ["contracts", "cli", "typescript"] {
            fs::write(
                installed.join(format!("references/{topic}.md")),
                "Installed reference\n",
            )
            .unwrap();
        }
        assert!(guide.contains(&format!(
            "Scaffolded with Thruput {}, template `{template_name}`",
            env!("CARGO_PKG_VERSION")
        )));
        for topic in [None, Some("contracts"), Some("cli"), Some("typescript")] {
            let mut args = vec!["skill", "show"];
            if let Some(topic) = topic {
                args.push(topic);
            }
            // Reading an installed skill needs no Node, Git, Cargo, or network.
            let output = thruput(temp.path(), &project)
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
            let expected = if topic.is_some() {
                "Installed reference\n"
            } else {
                "Installed entry\n"
            };
            assert_eq!(output.stdout, expected.as_bytes());
        }
        assert!(!guide.contains("thruput --skill"));
        assert!(!guide.contains("{{"));
        assert_eq!(guide.contains("npm run"), template == Some("counter-app"));
    }
}

#[test]
fn optional_docs_export_works_outside_the_generated_project() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    thruput_cli::scaffold::generate(&project, "project", Some("counter")).unwrap();
    let guide = fs::read_to_string(project.join("AGENTS.md")).unwrap();
    assert!(guide.contains("thruput docs --help"));
    let destination = temp.path().join("docs");
    let exported = thruput(temp.path(), &project)
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
        destination
            .canonicalize()
            .unwrap()
            .join("index.md")
            .to_str()
            .unwrap()
    );
    assert!(destination.join("sections/contracts.md").is_file());
    assert!(!project.join("docs").exists());
}
