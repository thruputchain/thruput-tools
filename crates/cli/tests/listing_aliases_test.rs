use std::process::Command;

#[test]
fn listing_aliases_keep_json_command_names() {
    let directory = tempfile::tempdir().unwrap();
    for (group, primary, alias, expected) in [
        ("keys", "ls", "list", "keys.ls"),
        ("templates", "list", "ls", "templates.list"),
    ] {
        for spelling in [primary, alias] {
            let output = Command::new(env!("CARGO_BIN_EXE_thruput"))
                .args(["--json", group, spelling])
                .current_dir(directory.path())
                .env("HOME", directory.path())
                .env("XDG_CONFIG_HOME", directory.path().join("config"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{group} {spelling}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["command"], expected, "{group} {spelling}");
        }
    }
}
