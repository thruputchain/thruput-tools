//! Default discovery must preserve explicit intent and work without a writable cache.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn isolated_cli(root: &std::path::Path) -> Command {
    let executable = root.join("thruput");
    if !executable.exists() {
        let contents = std::fs::read(env!("CARGO_BIN_EXE_thruput")).unwrap();
        std::fs::write(&executable, contents).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut command = Command::new(executable);
    command
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env_remove("THRUPUT_DEVNODE_BINARY")
        .env_remove("HOGD_DEV_BINARY")
        .env_remove("THRUPUT_OFFLINE")
        .env_remove("THRUPUT_TOOLCHAIN_BASE_URL")
        .env("PATH", root.join("bin"));
    command
}

#[test]
fn implicit_offline_node_uses_path_when_cache_is_read_only() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let binary = bin.join("hog-devnode");
    let marker = root.path().join("path-binary-started");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o555)).unwrap();
    let output = isolated_cli(root.path())
        .args(["--json", "--offline", "node", "--data-dir"])
        .arg(root.path().join("node-data"))
        .output()
        .unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        marker.exists(),
        "PATH binary was not started: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Root can write a 0555 directory; a file forces the same lease I/O branch
    // on privileged Linux CI as well as normal developer machines.
    std::fs::remove_dir_all(&cache).unwrap();
    std::fs::write(&cache, b"blocked").unwrap();
    std::fs::remove_file(&marker).unwrap();
    let output = isolated_cli(root.path())
        .args(["--json", "--offline", "node", "--data-dir"])
        .arg(root.path().join("node-data-file-cache"))
        .output()
        .unwrap();
    assert!(
        marker.exists(),
        "PATH fallback failed for unusable cache: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn saved_selection_does_not_fall_back_to_path_when_cache_is_unusable() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config/thruput");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let catalog = thruput_toolchain::Catalog::embedded().unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[toolchain]\nhog_devnode_version = \"{}\"\n",
            catalog.default_version()
        ),
    )
    .unwrap();
    // A file makes the cache unusable even when tests run as root.
    std::fs::write(root.path().join("cache"), b"blocked").unwrap();
    let marker = root.path().join("unexpected-path-start");
    let binary = bin.join("hog-devnode");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = isolated_cli(root.path())
        .args(["--json", "--offline", "node", "--data-dir"])
        .arg(root.path().join("node-data"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
    let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(report["error"]["code"], "toolchain_install_error");
}

#[test]
fn status_retains_uninstalled_and_stale_managed_selections() {
    let catalog = thruput_toolchain::Catalog::embedded().unwrap();
    for selected in [None, Some(catalog.default_version()), Some("0.0.0-retired")] {
        let version = selected.unwrap_or(catalog.default_version());
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config/thruput");
        std::fs::create_dir_all(&config).unwrap();
        if selected.is_some() {
            std::fs::write(
                config.join("config.toml"),
                format!("[toolchain]\nhog_devnode_version = \"{version}\"\n"),
            )
            .unwrap();
        }
        let output = isolated_cli(root.path())
            .args(["--json", "status"])
            .output()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let node = &json["data"]["environment"]["devnode"];
        assert_eq!(node["reachable"], false);
        assert_eq!(node["version"], version);
        assert!(node["path"].is_null());
        assert!(node["error"].as_str().unwrap().contains("selected"));
        assert!(!root.path().join("cache/thruput/toolchain").exists());
    }
}

#[test]
fn interrupted_acquisition_reports_json_failure_and_nonzero_exit() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = isolated_cli(root.path())
        .env(
            "THRUPUT_TOOLCHAIN_BASE_URL",
            format!("http://{}/", listener.local_addr().unwrap()),
        )
        .env_remove("HTTPS_PROXY")
        .env_remove("HTTP_PROXY")
        .env_remove("ALL_PROXY")
        .args(["--json", "node", "--data-dir"])
        .arg(root.path().join("node-data"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let _connection = loop {
        if let Ok(connection) = listener.accept() {
            break connection;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "CLI exited before acquisition: {status}: {:?}",
                child.wait_with_output().unwrap()
            );
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!(
                "CLI never requested artifact: {:?}",
                child.wait_with_output().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // Signal a subprocess only after observing its pending download.
    assert_eq!(
        unsafe { libc::kill(libc::pid_t::try_from(child.id()).unwrap(), libc::SIGINT) },
        0
    );
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["data"]["started"], false);
    assert_eq!(report["error"]["code"], "node_acquisition_interrupted");
}
