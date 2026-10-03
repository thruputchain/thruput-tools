use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn skill_topics_and_complete_output_are_available_offline() {
    let list = cli(&["skill", "list", "--offline", "--json"]);
    assert!(list.status.success());
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list["data"]["schemaVersion"], 1);
    for topic in list["data"]["topics"].as_array().unwrap() {
        let result = cli(&["skill", "show", topic.as_str().unwrap(), "--offline"]);
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        let text = String::from_utf8(result.stdout).unwrap();
        assert!(text.contains("groundhog-cli-version:"));
        assert!(text.contains("skill-source:"));
    }
    assert_eq!(
        cli(&["skill", "show", "--all"]).stdout,
        groundhog_cli::skill::render().as_bytes()
    );
    let entry = cli(&["skill", "show"]);
    assert!(entry.stdout.starts_with(b"---\nname: groundhog\n"));
    assert!(entry.stdout.len() < 6 * 1024);
}

#[test]
fn skill_errors_use_stdout_envelopes_without_installer_execution() {
    for args in [
        vec!["skill", "show", "missing", "--json"],
        vec!["skill", "install", "--non-interactive", "--json"],
        vec!["skill", "install", "--non-interactive", "--yes", "--json"],
        vec![
            "skill",
            "install",
            "--yes",
            "--agent",
            "opencode",
            "--offline",
            "--json",
        ],
        vec!["skill", "install", "--yes", "--agent=--all", "--json"],
    ] {
        let result = cli(&args);
        assert!(!result.status.success(), "{args:?}");
        assert!(
            result.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["error"]["code"], "skill_error");
        assert_eq!(report["data"]["schemaVersion"], 1);
    }
}

#[cfg(unix)]
mod installer {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (name, script) in [
            ("node", "#!/bin/sh\nprintf 'v22.20.0\\n'\n"),
            ("npm", "#!/bin/sh\nexit 0\n"),
            ("git", "#!/bin/sh\nexit 0\n"),
            (
                "npx",
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$SKILL_ARGS\"\nif [ \"$3\" = list ]; then\n  printf '%s\\n' '[{\"name\":\"groundhog\",\"source\":\"groundhogchain/agent-skills\"}]'\nelse\n  printf 'installer output\\n'\n  if read -r answer; then exit 99; fi\nfi\nexit \"${SKILL_EXIT:-0}\"\n",
            ),
        ] {
            let path = root.path().join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        root
    }

    fn command(root: &tempfile::TempDir) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_groundhog"));
        cmd.env("PATH", root.path())
            .env("SKILL_ARGS", root.path().join("args"))
            .args(["skill"]);
        cmd
    }

    #[test]
    fn unattended_install_passes_explicit_target_and_keeps_json_clean() {
        let root = fixture();
        let result = command(&root)
            .args([
                "install",
                "--global",
                "--agent",
                "opencode",
                "--yes",
                "--non-interactive",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["command"], "skill install");
        assert_eq!(report["data"]["scope"], "global");
        assert!(String::from_utf8_lossy(&result.stderr).contains("installer output"));
        assert_eq!(
            fs::read_to_string(root.path().join("args")).unwrap(),
            "--yes\nskills@1.7.0\nadd\ngroundhogchain/agent-skills\n--skill\ngroundhog\n--global\n--agent\nopencode\n--json\n--yes\n--yes\nskills@1.7.0\nlist\n--json\n--global\n"
        );
    }

    #[test]
    fn update_is_named_and_project_scoped_and_propagates_failure() {
        let root = fixture();
        let result = command(&root)
            .args(["update", "--yes", "--non-interactive", "--json"])
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            fs::read_to_string(root.path().join("args")).unwrap(),
            "--yes\nskills@1.7.0\nlist\n--json\n--yes\nskills@1.7.0\nupdate\ngroundhog\n--project\n--yes\n"
        );
        let failure = command(&root)
            .env("SKILL_EXIT", "7")
            .args(["install", "--agent", "opencode", "--yes", "--json"])
            .output()
            .unwrap();
        assert!(!failure.status.success());
        let report: serde_json::Value = serde_json::from_slice(&failure.stdout).unwrap();
        assert_eq!(report["ok"], false);
        assert!(report["error"]["message"].as_str().unwrap().contains('7'));
    }

    #[test]
    fn missing_prerequisite_and_old_node_produce_actionable_errors() {
        let root = fixture();
        fs::write(root.path().join("node"), "#!/bin/sh\nprintf 'v20.0.0\\n'\n").unwrap();
        let old = command(&root)
            .args(["install", "--yes", "--agent", "opencode", "--json"])
            .output()
            .unwrap();
        assert!(!old.status.success());
        assert!(String::from_utf8_lossy(&old.stdout).contains("22.20"));
        fs::remove_file(root.path().join("npx")).unwrap();
        let missing = command(&root)
            .args(["install", "--yes", "--agent", "opencode", "--json"])
            .output()
            .unwrap();
        assert!(!missing.status.success());
        assert!(String::from_utf8_lossy(&missing.stdout).contains("npx is not on PATH"));
        assert!(!root.path().join("args").exists());
    }

    #[test]
    fn update_requires_an_installation_from_the_maintained_source() {
        let root = fixture();
        for installed in [
            "[]",
            r#"[{"name":"groundhog","source":"someone/another-skill"}]"#,
        ] {
            fs::write(
                root.path().join("npx"),
                format!("#!/bin/sh\nprintf '%s\\n' '{installed}'\n"),
            )
            .unwrap();
            let result = command(&root)
                .args(["update", "--yes", "--json"])
                .output()
                .unwrap();
            assert!(!result.status.success());
            let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert!(
                report["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("not installed from")
            );
        }
    }

    #[test]
    fn zero_exit_without_an_installed_skill_is_not_reported_as_success() {
        let root = fixture();
        fs::write(
            root.path().join("npx"),
            "#!/bin/sh\nif [ \"$3\" = list ]; then printf '[]\\n'; fi\nexit 0\n",
        )
        .unwrap();
        let result = command(&root)
            .args(["install", "--yes", "--agent", "opencode", "--json"])
            .output()
            .unwrap();
        assert!(!result.status.success());
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["ok"], false);
        assert!(
            report["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not installed from")
        );
    }
}
