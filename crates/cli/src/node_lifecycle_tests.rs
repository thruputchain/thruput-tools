//! Exercise OS-owned managed leases through cancellation and parent process death.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use thruput_toolchain::{Catalog, HostTarget, InstallError, Installer};

use super::{LocalNodeOptions, LocalNodePidFile, start_resolved_local_node};

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "missing marker {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let catalog = Catalog::embedded().unwrap();
    let target = HostTarget::current().unwrap();
    let version = catalog.default_version();
    let artifact = catalog.artifact(version, target).unwrap();
    let installer = Installer::new(root.path().join("cache"));
    let bundle = installer.installation_path(version, target);
    // Cache trust is deliberate: seed a complete local fixture without a public download.
    for file in artifact.files() {
        let path = bundle.join(file.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fixture").unwrap();
    }
    std::fs::write(
        bundle.join(".ok"),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "version": version, "target": target.triple(),
            "archiveSha256": artifact.sha256(), "layoutVersion": artifact.layout_version(),
        }))
        .unwrap(),
    )
    .unwrap();
    let binary = bundle.join("hog-devnode");
    std::fs::write(
        &binary,
        r"#!/usr/bin/env python3
import pathlib, signal, time
root = pathlib.Path(__file__).parent

def stop(*_):
    (root / 'stopping').touch()
    while not (root / 'allow-exit').exists():
        time.sleep(0.01)
    raise SystemExit(0)

signal.signal(signal.SIGTERM, stop)
(root / 'pid').write_text(str(__import__('os').getpid()))
(root / 'started').touch()
while True:
    time.sleep(0.01)
",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

#[test]
fn cancelled_managed_startup_keeps_bundle_busy_after_runtime_teardown() {
    let root = fixture();
    let catalog = Catalog::embedded().unwrap();
    let target = HostTarget::current().unwrap();
    let version = catalog.default_version();
    let installer = Installer::new(root.path().join("cache"));
    let bundle = installer.installation_path(version, target);
    let binary = bundle.join("hog-devnode");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let lease = installer
            .lease(&catalog, version, target)
            .await
            .unwrap()
            .unwrap();
        let options = LocalNodeOptions {
            hogd_dev_binary: Some(binary),
            data_dir: Some(root.path().join("node-data")),
            pid_file: LocalNodePidFile::Disabled,
            ..LocalNodeOptions::default()
        };
        let mut startup = Box::pin(start_resolved_local_node(options, Some(lease)));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !bundle.join("started").exists() {
            tokio::select! {
                result = &mut startup => panic!("fixture should wait for readiness: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "fixture did not start"
            );
        }
        drop(startup);
    });
    drop(runtime);
    wait_for_file(&bundle.join("stopping"));
    let refused = matches!(
        installer.clean(version, target),
        Err(InstallError::Busy { .. })
    );
    // Always allow fixture exit before asserting, so a regression does not strand it.
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("allow-exit"), b"").unwrap();
    assert!(
        refused,
        "cancelled startup released its bundle before child exit"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match installer.clean(version, target) {
            Ok(true) => break,
            Err(InstallError::Busy { .. }) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => panic!("bundle was not released after child exit: {result:?}"),
        }
    }
}

// This test doubles as the independently killable parent process. Environment
// routing avoids another fixture binary while exercising the actual CLI path.
#[test]
fn managed_startup_parent_process() {
    let Some(root) = std::env::var_os("THRUPUT_TEST_MANAGED_PARENT_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let catalog = Catalog::embedded().unwrap();
        let target = HostTarget::current().unwrap();
        let installer = Installer::new(root.join("cache"));
        let lease = installer
            .lease(&catalog, catalog.default_version(), target)
            .await
            .unwrap()
            .unwrap();
        let options = LocalNodeOptions {
            hogd_dev_binary: Some(lease.binary_path()),
            data_dir: Some(root.join("node-data")),
            pid_file: LocalNodePidFile::Disabled,
            ..LocalNodeOptions::default()
        };
        let mut startup = Box::pin(start_resolved_local_node(options, Some(lease)));
        let bundle = installer.installation_path(catalog.default_version(), target);
        loop {
            tokio::select! {
                result = &mut startup => panic!("fixture must wait for readiness: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
            if bundle.join("started").exists()
                && std::env::var_os("THRUPUT_TEST_MANAGED_PARENT_EXIT").is_some()
            {
                // Bypass Rust drops, just like an embedding caller exiting.
                std::process::exit(0);
            }
        }
    });
}

#[test]
fn managed_lease_survives_parent_exit_and_sigkill() {
    for normal_exit in [false, true] {
        let root = fixture();
        let catalog = Catalog::embedded().unwrap();
        let target = HostTarget::current().unwrap();
        let version = catalog.default_version();
        let installer = Installer::new(root.path().join("cache"));
        let bundle = installer.installation_path(version, target);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "node::lifecycle_tests::managed_startup_parent_process",
                "--nocapture",
            ])
            .env("THRUPUT_TEST_MANAGED_PARENT_ROOT", root.path());
        if normal_exit {
            command.env("THRUPUT_TEST_MANAGED_PARENT_EXIT", "1");
        }
        let mut parent = command.spawn().unwrap();
        wait_for_file(&bundle.join("started"));
        if !normal_exit {
            parent.kill().unwrap();
        }
        let status = parent.wait().unwrap();
        assert_eq!(status.success(), normal_exit);
        let pid: i32 = std::fs::read_to_string(bundle.join("pid"))
            .unwrap()
            .parse()
            .unwrap();
        let busy = matches!(
            installer.clean(version, target),
            Err(InstallError::Busy { .. })
        );
        // Release the fixture even on regression; it is now orphaned by design.
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("allow-exit"), b"").unwrap();
        // SAFETY: pid belongs to this fixture, recorded before the marker.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        assert!(
            busy,
            "parent exit released the live child's inherited bundle lease"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match installer.clean(version, target) {
                Ok(true) => break,
                Err(InstallError::Busy { .. }) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => panic!("child exit did not release inherited lease: {result:?}"),
            }
        }
    }
}
