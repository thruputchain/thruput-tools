use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(args)
        .output()
        .unwrap()
}

struct ReadFixture(tempfile::TempDir);

impl ReadFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("project")).unwrap();
        Self(root)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
        command
            .current_dir(self.0.path().join("project"))
            .env("HOME", self.0.path().join("home"))
            .env("XDG_CONFIG_HOME", self.0.path().join("config"))
            .env("PATH", "")
            .args(["skill"]);
        command
    }

    fn install(&self, scope: &str, content: &str) -> PathBuf {
        let root = self.0.path().join(scope).join(".agents/skills/groundhog");
        fs::create_dir_all(root.join("references")).unwrap();
        fs::write(root.join("SKILL.md"), content).unwrap();
        root
    }
}

#[test]
fn reads_installed_files_verbatim_offline_and_sees_updates() {
    let fixture = ReadFixture::new();
    let entry = "---\nname: groundhog\n---\n\nInstalled entry\n";
    let root = fixture.install("project", entry);
    fs::write(
        root.join("references/contracts.md"),
        "# Custom contracts\r\nNo final newline",
    )
    .unwrap();
    for topic in [
        None,
        Some("SKILL.md"),
        Some("contracts"),
        Some("contracts.md"),
        Some("references/contracts.md"),
    ] {
        let mut command = fixture.command();
        command.args(["show", "--offline"]);
        if let Some(topic) = topic {
            command.arg(topic);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        let expected = if topic.is_none() || topic == Some("SKILL.md") {
            entry
        } else {
            "# Custom contracts\r\nNo final newline"
        };
        assert_eq!(output.stdout, expected.as_bytes());
    }
    fs::write(root.join("references/contracts.md"), "Updated on disk").unwrap();
    let output = fixture
        .command()
        .args(["show", "contracts", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data"]["schemaVersion"], 1);
    assert_eq!(report["data"]["content"], "Updated on disk");
    assert_eq!(report["data"]["scope"], "project");
    assert_eq!(
        report["data"]["path"],
        root.join("references/contracts.md")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(report["data"].get("provenance").is_none());
}

#[test]
fn lists_installed_references_in_sorted_order() {
    let fixture = ReadFixture::new();
    let root = fixture.install("project", "entry");
    for name in ["typescript.md", "contracts.md", "cli.md", "notes.txt"] {
        fs::write(root.join("references").join(name), "reference").unwrap();
    }
    let output = fixture
        .command()
        .args(["list", "--offline"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"cli\ncontracts\ntypescript\n");
    let output = fixture.command().args(["list", "--json"]).output().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["data"]["topics"],
        serde_json::json!(["cli", "contracts", "typescript"])
    );
}

#[test]
fn reads_project_before_global_and_supports_explicit_global_scope() {
    let fixture = ReadFixture::new();
    fixture.install("home", "global entry");
    let output = fixture
        .command()
        .args(["show", "--offline"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"global entry");
    fixture.install("project", "project entry");
    let output = fixture.command().arg("show").output().unwrap();
    assert_eq!(output.stdout, b"project entry");
    let output = fixture
        .command()
        .args(["show", "--global"])
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"global entry");
}

#[test]
fn missing_installation_requires_install_without_executing_tools() {
    let fixture = ReadFixture::new();
    for action in ["show", "list"] {
        let plain = fixture
            .command()
            .args([action, "--offline"])
            .output()
            .unwrap();
        assert!(!plain.status.success());
        assert!(plain.stdout.is_empty());
        assert!(String::from_utf8_lossy(&plain.stderr).contains("groundhog skill install"));
        let json = fixture.command().args([action, "--json"]).output().unwrap();
        assert!(!json.status.success());
        let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["error"]["code"], "skill_error");
        assert_eq!(report["data"]["path"], serde_json::Value::Null);
    }
    fixture.install("project", "project only");
    assert!(
        !fixture
            .command()
            .args(["show", "--global"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn missing_or_invalid_project_files_do_not_fall_back_to_global() {
    let fixture = ReadFixture::new();
    let global = fixture.install("home", "global entry");
    fs::write(global.join("references/contracts.md"), "global contracts").unwrap();
    let project = fixture.install("project", "project entry");
    assert!(
        !fixture
            .command()
            .args(["show", "contracts"])
            .output()
            .unwrap()
            .status
            .success()
    );
    fs::write(project.join("references/contracts.md"), [0xff]).unwrap();
    assert!(
        !fixture
            .command()
            .args(["show", "contracts"])
            .output()
            .unwrap()
            .status
            .success()
    );
    fs::remove_file(project.join("SKILL.md")).unwrap();
    let output = fixture.command().arg("show").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn reference_names_cannot_escape_the_installation() {
    let fixture = ReadFixture::new();
    fixture.install("project", "entry");
    for topic in [
        "../SKILL",
        "references/../SKILL.md",
        "/etc/passwd",
        "..\\SKILL",
        ".",
        "",
    ] {
        let output = fixture
            .command()
            .args(["show", topic, "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{topic}: {output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ok"], false);
    }
}

#[cfg(unix)]
#[test]
fn linked_installations_work_but_references_cannot_link_outside() {
    use std::os::unix::fs::symlink;
    let fixture = ReadFixture::new();
    let root = fixture.install("home", "linked entry");
    let project_skills = fixture.0.path().join("project/.agents/skills");
    fs::create_dir_all(&project_skills).unwrap();
    symlink(&root, project_skills.join("groundhog")).unwrap();
    let output = fixture.command().arg("show").output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"linked entry");
    let outside = fixture.0.path().join("outside.md");
    fs::write(&outside, "outside content").unwrap();
    symlink(outside, root.join("references/contracts.md")).unwrap();
    for args in [vec!["show", "contracts"], vec!["list"]] {
        let output = fixture.command().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn skill_errors_use_stdout_envelopes_without_installer_execution() {
    for args in [
        vec!["skill", "show", "missing", "--json"],
        vec!["skill", "install", "--non-interactive", "--json"],
        vec!["skill", "install", "--non-interactive", "--yes", "--json"],
        vec!["skill", "update", "--global", "--non-interactive", "--json"],
        vec![
            "skill",
            "update",
            "--global",
            "--yes",
            "--offline",
            "--json",
        ],
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

    fn global_update_fixture() -> tempfile::TempDir {
        let root = fixture();
        fs::write(
            root.path().join("npx"),
            r#"#!/bin/sh
printf '%s\n' "$@" >> "$SKILL_ARGS"
case "$3" in
  list)
    printf '%s\n' '[{"name":"groundhog","source":"groundhogchain/agent-skills"}]'
    ;;
  update)
    printf 'Failed to check skills\nAll global skills are up to date\n'
    ;;
  add)
    if read -r answer; then exit 99; fi
    printf 'source diagnostics\n' >&2
    printf '%s\n' "$SKILL_REPORT"
    exit "${SKILL_EXIT:-0}"
    ;;
esac
"#,
        )
        .unwrap();
        root
    }

    const REFRESHED: &str = r#"[{"name":"groundhog","status":"installed","source":"groundhogchain/agent-skills","scope":"global"}]"#;

    fn global_update(root: &tempfile::TempDir, report: &str) -> Command {
        let mut cmd = command(root);
        cmd.env("SKILL_REPORT", report).args([
            "update",
            "--global",
            "--yes",
            "--non-interactive",
            "--json",
        ]);
        cmd
    }

    #[test]
    fn global_update_requires_a_confirmed_refresh_even_when_files_are_unchanged() {
        let root = global_update_fixture();
        // Both invocations return the same report without modifying any files.
        // A successful refresh must not depend on a change in installed content.
        for _ in 0..2 {
            let result = global_update(&root, REFRESHED).output().unwrap();
            assert!(result.status.success(), "{result:?}");
            let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(report["ok"], true);
            assert_eq!(report["command"], "skill update");
            assert_eq!(report["data"]["scope"], "global");
            assert!(String::from_utf8_lossy(&result.stderr).contains("source diagnostics"));
        }
        let invocation = "--yes\nskills@1.7.0\nlist\n--json\n--global\n--yes\nskills@1.7.0\nadd\ngroundhogchain/agent-skills\n--skill\ngroundhog\n--global\n--yes\n--json\n--yes\nskills@1.7.0\nlist\n--json\n--global\n";
        assert_eq!(
            fs::read_to_string(root.path().join("args")).unwrap(),
            invocation.repeat(2)
        );
    }

    #[test]
    fn global_update_rejects_zero_exit_without_a_successful_source_refresh() {
        let root = global_update_fixture();
        for report in [
            "Failed to check skills\nAll global skills are up to date",
            "[]",
            r#"[{"name":"groundhog","status":"failed","error":"source unavailable"}]"#,
            r#"[{"name":"groundhog","status":"skipped","reason":"source unavailable"}]"#,
            r#"[{"name":"groundhog","status":"installed"}]"#,
            r#"[{"name":"groundhog","status":"installed","source":"other/source","scope":"global"}]"#,
            r#"[{"name":"groundhog","status":"installed","source":"groundhogchain/agent-skills","scope":"project"}]"#,
            r#"[{"name":"other","status":"installed","source":"groundhogchain/agent-skills","scope":"global"}]"#,
        ] {
            let result = global_update(&root, report).output().unwrap();
            assert!(!result.status.success(), "{report}: {result:?}");
            let failure: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(failure["ok"], false, "{report}");
            assert_eq!(failure["error"]["code"], "skill_error");
            assert!(!String::from_utf8_lossy(&result.stderr).contains("update complete"));
        }
    }

    #[test]
    fn global_update_propagates_refresh_exit_failure_despite_success_report() {
        let root = global_update_fixture();
        let result = global_update(&root, REFRESHED)
            .env("SKILL_EXIT", "7")
            .output()
            .unwrap();
        assert!(!result.status.success());
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
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
            for scope in [vec![], vec!["--global"]] {
                let result = command(&root)
                    .args(["update", "--yes", "--json"])
                    .args(scope)
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
