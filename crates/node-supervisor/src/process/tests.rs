use crate::test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary};

use super::{ChildProcessSpec, ChildSupervisor};

#[tokio::test]
async fn wait_reports_child_exit_status() {
    let spec = ChildProcessSpec::new(fake_hogd_dev_binary()).env(FAKE_HOGD_DEV_MODE_ENV, "exit-7");
    let supervisor = ChildSupervisor::new(spec);
    let mut child = supervisor
        .spawn()
        .expect("bug: test child should spawn successfully");

    let status = child
        .wait()
        .await
        .expect("bug: test child should be waitable");

    assert_eq!(status.code(), Some(7));
}

#[tokio::test]
async fn kill_and_wait_stops_running_child() {
    let spec = ChildProcessSpec::new(fake_hogd_dev_binary()).env(FAKE_HOGD_DEV_MODE_ENV, "hang");
    let supervisor = ChildSupervisor::new(spec);
    let mut child = supervisor
        .spawn()
        .expect("bug: test child should spawn successfully");

    assert!(child.id().is_some());

    let status = child
        .kill_and_wait()
        .await
        .expect("bug: test child should be killable");

    assert!(!status.success());
}

#[test]
#[serial_test::serial]
fn process_group_with_running_member_is_live() {
    use std::os::unix::process::CommandExt;

    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .expect("bug: sleep should spawn");
    let process_group = i32::try_from(child.id()).expect("bug: pid should fit pid_t");

    let live = super::process_group_has_live_member(process_group);

    child.kill().expect("bug: sleep should be killable");
    child.wait().expect("bug: sleep should be reaped");
    assert!(live.expect("bug: process group should be inspectable"));
}

#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial]
fn process_group_of_only_zombies_is_gone() {
    use std::os::unix::process::CommandExt;

    let mut child = std::process::Command::new("true")
        .process_group(0)
        .spawn()
        .expect("bug: true should spawn");
    let pid = child.id();
    let process_group = i32::try_from(pid).expect("bug: pid should fit pid_t");
    // Leave the exited child unreaped so it stays in its group as a zombie.
    let stat_path = std::path::PathBuf::from(format!("/proc/{pid}/stat"));
    let mut zombie = false;
    for _ in 0..500 {
        zombie = matches!(
            super::read_linux_stat(&stat_path),
            super::LinuxStat::Read { state: 'Z', .. }
        );
        if zombie {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // SAFETY: signal 0 only checks that the group is present.
    let kernel_reports_group = unsafe { libc::kill(-process_group, 0) } == 0;

    let live = super::process_group_has_live_member(process_group);

    child.wait().expect("bug: true should be reaped");
    assert!(zombie, "bug: exited child should be a zombie until reaped");
    assert!(kernel_reports_group);
    assert!(!live.expect("bug: process group should be inspectable"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_stat_parse_counts_fields_after_the_last_paren() {
    let stat = "4242 (hog) (dev node) Z 1 4240 4240 0 -1 4194560";

    assert_eq!(
        super::parse_linux_stat_state_and_group(stat),
        Some(('Z', 4240))
    );
    assert_eq!(super::parse_linux_stat_state_and_group("4242 hog"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_stat_read_failure_is_vanished_only_when_the_process_is_gone() {
    use std::io::{Error, ErrorKind};

    use super::{LinuxStat, classify_linux_stat_read};

    assert_eq!(
        classify_linux_stat_read(Err(Error::from(ErrorKind::NotFound))),
        LinuxStat::Vanished
    );
    assert_eq!(
        classify_linux_stat_read(Err(Error::from_raw_os_error(libc::ESRCH))),
        LinuxStat::Vanished
    );
    assert_eq!(
        classify_linux_stat_read(Err(Error::from(ErrorKind::PermissionDenied))),
        LinuxStat::Unreadable
    );
    assert_eq!(
        classify_linux_stat_read(Ok("4242 hog".to_string())),
        LinuxStat::Unreadable
    );
    assert_eq!(
        classify_linux_stat_read(Ok("4242 (hog) S 1 4240 4240".to_string())),
        LinuxStat::Read {
            state: 'S',
            process_group: 4240
        }
    );
}
