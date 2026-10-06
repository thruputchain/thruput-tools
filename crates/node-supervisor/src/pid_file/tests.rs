use std::{fs, time::Duration};

use tempfile::TempDir;

use super::{PidGuard, matches_expected_program};
use crate::{
    ChildProcessSpec, ChildSupervisor, NodeSupervisorError,
    test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
};

#[tokio::test]
async fn acquire_no_file_is_clean() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");

    let guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: acquiring missing PID file should succeed");

    assert!(path.exists());
    drop(guard);
    assert!(!path.exists());
}

#[tokio::test]
async fn write_creates_parent_dirs() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("nested").join("hogd-dev.pid");
    let mut guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: acquiring PID file should create parents");

    guard.write(1234).expect("bug: PID write should succeed");
    assert_eq!(
        fs::read_to_string(&path).expect("bug: PID file should be readable"),
        "1234\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path.parent().expect("bug: PID file has a parent"))
            .expect("bug: PID parent metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path)
            .expect("bug: PID file metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[tokio::test]
async fn written_pid_file_survives_drop_until_explicit_release() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");
    let stale_pid = i32::MAX as u32;
    let mut guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: PID file should be acquired");
    guard
        .write(stale_pid)
        .expect("bug: PID write should succeed");

    drop(guard);
    assert_eq!(
        fs::read_to_string(&path).expect("bug: orphan recovery PID should remain"),
        format!("{stale_pid}\n")
    );

    let guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: stale PID should be reacquired");
    guard.release();
    assert!(!path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn acquire_rejects_symlinked_pid_file() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("bug: temp dir should be created");
    let target = temp.path().join("target.pid");
    let path = temp.path().join("hogd-dev.pid");
    fs::write(&target, "").expect("bug: PID target should be written");
    symlink(&target, &path).expect("bug: PID symlink should be created");

    let err = PidGuard::acquire(
        path,
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect_err("bug: symlinked PID file should fail closed");
    assert!(matches!(err, NodeSupervisorError::PidFile { .. }));
}

#[tokio::test]
async fn drop_removes_file() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");
    let guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: acquiring PID file should succeed");

    assert!(path.exists());
    drop(guard);
    assert!(!path.exists());
}

#[tokio::test]
async fn acquire_stale_pid_removes_file_contents() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");
    fs::write(&path, "999999\n").expect("bug: stale PID should be written");

    let mut guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: stale PID should not block acquire");

    guard.write(7).expect("bug: PID write should succeed");
    assert_eq!(
        fs::read_to_string(&path).expect("bug: PID file should be readable"),
        "7\n"
    );
}

#[tokio::test]
async fn acquire_malformed_pid_removes_file_contents() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");
    fs::write(&path, "not-a-pid\n").expect("bug: malformed PID should be written");

    let mut guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: malformed PID should not block acquire");

    guard.write(8).expect("bug: PID write should succeed");
    assert_eq!(
        fs::read_to_string(&path).expect("bug: PID file should be readable"),
        "8\n"
    );
}

#[tokio::test]
async fn contended_acquire_rebinds_to_the_file_currently_at_path() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");
    let holder = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: holder should acquire the PID file");

    let contender_path = path.clone();
    let contender = tokio::spawn(async move {
        PidGuard::acquire(
            contender_path,
            std::ffi::OsStr::new("hogd-dev"),
            Duration::from_millis(10),
        )
        .await
    });
    // Dropping the holder unlinks the path before the close releases the
    // lock, so a contender that keeps its pre-unlink descriptor would win
    // the lock on an unreachable inode and write its PID there. Release
    // from a separate OS thread so an overloaded single-threaded Tokio
    // executor cannot delay this past the contender's retry deadline.
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        drop(holder);
    });

    let mut guard = contender
        .await
        .expect("bug: contender task should finish")
        .expect("bug: contender should acquire after the holder releases");
    release
        .join()
        .expect("bug: holder release thread should finish");
    guard.write(4321).expect("bug: PID write should succeed");
    assert_eq!(
        fs::read_to_string(&path).expect("bug: written PID should be visible at the path"),
        "4321\n"
    );
}

#[tokio::test]
async fn acquire_locked_pid_file_reports_running_node() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");
    let _guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect("bug: first acquire should succeed");

    let err = PidGuard::acquire(
        path,
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(10),
    )
    .await
    .expect_err("bug: second acquire should fail while locked");

    assert!(
        matches!(err, NodeSupervisorError::LocalNodeAlreadyRunning { .. }),
        "unexpected locked PID file error: {err:?}"
    );
}

#[test]
fn matches_expected_program_compares_basenames() {
    // `program` is the comm-style basename produced by `ps -o comm=`,
    // so it never contains arguments or path components.
    assert!(matches_expected_program(
        "hogd-dev",
        std::ffi::OsStr::new("hogd-dev")
    ));
    assert!(matches_expected_program(
        "hogd-dev",
        std::ffi::OsStr::new("/usr/local/bin/hogd-dev")
    ));
    assert!(!matches_expected_program(
        "sleep",
        std::ffi::OsStr::new("hogd-dev")
    ));
    // Truncated comm column must not collide with a longer expected
    // basename.
    assert!(!matches_expected_program(
        "hogd-de",
        std::ffi::OsStr::new("hogd-dev")
    ));
}

#[test]
fn matches_expected_program_accepts_legacy_devnode_name() {
    assert!(matches_expected_program(
        "hogd-dev",
        std::ffi::OsStr::new("hog-devnode")
    ));
    assert!(matches_expected_program(
        "hogd-dev",
        std::ffi::OsStr::new("/usr/local/bin/hog-devnode")
    ));
    assert!(!matches_expected_program(
        "hogd-dev",
        std::ffi::OsStr::new("hog-mempool")
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn parse_linux_stat_handles_spaces_and_parens_in_comm() {
    let stat = "123 (hogd dev) S 42 123 123 0 -1 4194560";
    assert_eq!(
        super::parse_linux_stat(stat),
        Some((Some(42), "hogd dev".to_string()))
    );

    let stat = "123 (hogd)dev) S 42 123 123 0 -1 4194560";
    assert_eq!(
        super::parse_linux_stat(stat),
        Some((Some(42), "hogd)dev".to_string()))
    );
}

#[tokio::test]
async fn acquire_stops_recorded_running_process() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");

    // Spawn an orphan-shaped child: own process group, long-running, so
    // `acquire` will find it live, verify the expected basename, and
    // clean it up. We don't try to detach it from the test process — the
    // test just needs `flock`-based cleanup to work even though `ppid`
    // is the test runner (not 1). That's exactly the subreaper case.
    let binary = fake_hogd_dev_binary();
    let spec = ChildProcessSpec::new(&binary)
        .env(FAKE_HOGD_DEV_MODE_ENV, "hang")
        .process_group(0);
    let mut child = ChildSupervisor::new(spec)
        .spawn()
        .expect("bug: fake hogd-dev should spawn");
    let pid = child.id().expect("bug: fake hogd-dev pid should be known");
    fs::write(&path, format!("{pid}\n")).expect("bug: pid file write");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let wait_task = tokio::spawn(async move {
        child
            .wait()
            .await
            .expect("bug: stopped fake hogd-dev should be reaped by the test")
    });

    let guard = PidGuard::acquire(path.clone(), binary.as_os_str(), Duration::from_secs(1))
        .await
        .expect("bug: acquire should stop the recorded fake hogd-dev");

    let status = wait_task
        .await
        .expect("bug: fake hogd-dev wait task should finish");
    assert!(
        !status.success(),
        "expected signal exit from stopped fake hogd-dev, got {status:?}"
    );

    drop(guard);
}

#[tokio::test]
async fn acquire_fails_closed_for_leaderless_process_group() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");
    let binary = fake_hogd_dev_binary();
    let spec = ChildProcessSpec::new(&binary)
        .env(FAKE_HOGD_DEV_MODE_ENV, "spawn-helper-then-exit")
        .process_group(0);
    let mut child = ChildSupervisor::new(spec)
        .spawn()
        .expect("bug: fake hogd-dev should spawn");
    let pid = child.id().expect("bug: fake hogd-dev pid should be known");
    fs::write(&path, format!("{pid}\n")).expect("bug: pid file should be written");
    let status = child
        .wait()
        .await
        .expect("bug: fake leader should be reaped");
    assert!(status.success());
    assert!(super::process_group_exists(pid));

    let err = PidGuard::acquire(path.clone(), binary.as_os_str(), Duration::from_millis(10))
        .await
        .expect_err("bug: leaderless process group should block startup");
    assert!(matches!(
        err,
        NodeSupervisorError::LocalNodeAlreadyRunning {
            pid: Some(found),
            ..
        } if found == pid
    ));

    let target = -i32::try_from(pid).expect("bug: pid should fit pid_t");
    let _ = unsafe { libc::kill(target, libc::SIGKILL) };
    for _ in 0..50 {
        if !super::process_group_exists(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!super::process_group_exists(pid));
    fs::remove_file(path).expect("bug: test should remove preserved PID file");
}

#[tokio::test]
async fn acquire_leaves_unrelated_program_alone() {
    use std::os::unix::process::CommandExt;

    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");

    let mut child = std::process::Command::new(sleep_command())
        .arg("2")
        .process_group(0)
        .spawn()
        .expect("bug: sleep should spawn");
    let pid = child.id();
    fs::write(&path, format!("{pid}\n")).expect("bug: pid file write");

    // Expected program is "hogd-dev" but the recorded PID is "sleep" —
    // the basename mismatch should make acquire treat this as PID
    // reuse and leave the process running.
    let guard = PidGuard::acquire(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(50),
    )
    .await
    .expect("bug: acquire should succeed past mismatched basename");

    // Confirm the sleep is still alive.
    assert!(
        child
            .try_wait()
            .expect("bug: try_wait should succeed")
            .is_none(),
        "sleep should still be running after acquire ignored it"
    );

    // Clean up so the test doesn't linger.
    let _ = child.kill();
    let _ = child.wait();
    drop(guard);
}

#[tokio::test]
async fn persistent_acquire_rejects_unrelated_live_program() {
    use std::os::unix::process::CommandExt;

    let temp = TempDir::new().expect("bug: temp dir should be created");
    let path = temp.path().join("hogd-dev.pid");
    let mut child = std::process::Command::new(sleep_command())
        .arg("2")
        .process_group(0)
        .spawn()
        .expect("bug: sleep should spawn");
    let pid = child.id();
    fs::write(&path, format!("{pid}\n")).expect("bug: pid file write");

    let err = PidGuard::acquire_persistent(
        path.clone(),
        std::ffi::OsStr::new("hogd-dev"),
        Duration::from_millis(50),
    )
    .await
    .expect_err("bug: persistent startup should reject an unverifiable live process");
    assert!(matches!(
        err,
        NodeSupervisorError::LocalNodeAlreadyRunning {
            pid: Some(found),
            ..
        } if found == pid
    ));
    assert!(
        child
            .try_wait()
            .expect("bug: try_wait should succeed")
            .is_none(),
        "sleep should remain running after fail-closed acquisition"
    );

    let _ = child.kill();
    let _ = child.wait();
    fs::remove_file(path).expect("bug: test should remove preserved PID file");
}

#[test]
fn process_group_zero_is_rejected() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let err = super::signal_process_group(temp.path(), 0, libc::SIGTERM)
        .expect_err("bug: process group zero should be rejected");

    assert!(
        matches!(err, NodeSupervisorError::PidFile { .. }),
        "unexpected process group zero error: {err:?}"
    );
    assert!(!super::process_group_exists(0));
}

#[cfg(unix)]
fn sleep_command() -> &'static str {
    if std::path::Path::new("/bin/sleep").exists() {
        "/bin/sleep"
    } else {
        "sleep"
    }
}
