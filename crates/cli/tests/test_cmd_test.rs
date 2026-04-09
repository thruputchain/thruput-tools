use serial_test::serial;
use std::env;
use tempfile::TempDir;

/// Verify that `test_cmd::run` fails when run outside
/// a groundhog project (no Cargo.toml with sdk dep).
#[test]
#[serial]
fn test_fails_outside_project() {
    let tmp = TempDir::new().unwrap();
    let prev_dir = env::current_dir().unwrap();

    env::set_current_dir(tmp.path()).unwrap();

    let result = groundhog_cli::test_cmd::run(&[], None, "test", &groundhog_cli::Print::quiet());

    env::set_current_dir(prev_dir).unwrap();

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not a groundhog project"),
        "expected NotAProject, got: {err}"
    );
}
