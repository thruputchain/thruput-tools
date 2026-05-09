//! PID-file ownership for supervised hogd-dev.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::time;

use crate::NodeSupervisorError;

/// Guard that owns the hogd-dev PID file for the supervisor lifetime.
#[derive(Debug)]
pub(crate) struct PidGuard {
    path: PathBuf,
    file: File,
}

impl PidGuard {
    /// Acquire PID-file ownership, cleaning up stale or orphaned hogd-dev processes first.
    pub(crate) async fn acquire(
        path: PathBuf,
        expected_program: &OsStr,
        grace: Duration,
    ) -> Result<Self, NodeSupervisorError> {
        #[cfg(unix)]
        {
            acquire_unix(path, expected_program, grace).await
        }

        #[cfg(not(unix))]
        {
            let file = open_pid_file(&path)?;
            Ok(Self { path, file })
        }
    }

    /// Write the supervised child PID into the already-owned PID file.
    pub(crate) fn write(&mut self, pid: u32) -> Result<(), NodeSupervisorError> {
        write_pid(&self.path, &mut self.file, pid)
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::debug!(
                path = %self.path.display(),
                %error,
                "failed to remove hogd-dev PID file"
            );
        }
    }
}

fn open_pid_file(path: &Path) -> Result<File, NodeSupervisorError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| NodeSupervisorError::PidFile {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| NodeSupervisorError::PidFile {
            path: path.to_path_buf(),
            source,
        })
}

fn read_pid(path: &Path, file: &mut File) -> Result<Option<u32>, NodeSupervisorError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| NodeSupervisorError::PidFile {
            path: path.to_path_buf(),
            source,
        })?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|source| NodeSupervisorError::PidFile {
            path: path.to_path_buf(),
            source,
        })?;

    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Ok(pid) = trimmed.parse::<u32>() {
        Ok(Some(pid))
    } else {
        tracing::warn!(
            path = %path.display(),
            contents = %trimmed,
            "removing malformed hogd-dev PID file"
        );
        Ok(None)
    }
}

fn clear_pid(path: &Path, file: &mut File) -> Result<(), NodeSupervisorError> {
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
        .map_err(|source| NodeSupervisorError::PidFile {
            path: path.to_path_buf(),
            source,
        })
}

fn write_pid(path: &Path, file: &mut File, pid: u32) -> Result<(), NodeSupervisorError> {
    clear_pid(path, file)?;
    writeln!(file, "{pid}").map_err(|source| NodeSupervisorError::PidFile {
        path: path.to_path_buf(),
        source,
    })?;
    file.flush().map_err(|source| NodeSupervisorError::PidFile {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
async fn acquire_unix(
    path: PathBuf,
    expected_program: &OsStr,
    grace: Duration,
) -> Result<PidGuard, NodeSupervisorError> {
    let mut file = open_pid_file(&path)?;

    match try_lock(&path, &file) {
        Ok(()) => {
            clean_stale_pid(&path, &mut file, expected_program, grace).await?;
            clear_pid(&path, &mut file)?;
            Ok(PidGuard { path, file })
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            let existing_pid = read_pid(&path, &mut file)?;
            Err(NodeSupervisorError::LocalNodeAlreadyRunning {
                path,
                pid: existing_pid,
            })
        }
        Err(source) => Err(NodeSupervisorError::PidFile { path, source }),
    }
}

#[cfg(unix)]
fn try_lock(path: &Path, file: &File) -> Result<(), io::Error> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) || error.raw_os_error() == Some(libc::EAGAIN)
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("PID file is locked: {}", path.display()),
        ));
    }

    Err(error)
}

#[cfg(unix)]
async fn clean_stale_pid(
    path: &Path,
    file: &mut File,
    expected_program: &OsStr,
    grace: Duration,
) -> Result<(), NodeSupervisorError> {
    let Some(pid) = read_pid(path, file)? else {
        return Ok(());
    };

    let Some(info) = process_info(pid) else {
        tracing::debug!(pid, path = %path.display(), "removing stale hogd-dev PID file");
        return Ok(());
    };

    if !matches_expected_program(&info.command, expected_program) || info.process_group != pid {
        tracing::warn!(
            pid,
            command = %info.command,
            path = %path.display(),
            "PID file points at a different process; ignoring stale PID"
        );
        return Ok(());
    }

    if info.parent_pid == 1 {
        tracing::info!(
            pid,
            ppid = info.parent_pid,
            "stopping prior hogd-dev process group before startup"
        );
        kill_process_group(path, pid, grace).await?;
    } else {
        tracing::warn!(
            pid,
            ppid = info.parent_pid,
            path = %path.display(),
            "PID file points at a live hogd-dev process that is not orphaned; leaving it running"
        );
    }

    Ok(())
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessInfo {
    parent_pid: u32,
    process_group: u32,
    command: String,
}

#[cfg(unix)]
fn process_info(target_pid: u32) -> Option<ProcessInfo> {
    let output = std::process::Command::new("ps")
        .args([
            "-p",
            &target_pid.to_string(),
            "-o",
            "ppid=",
            "-o",
            "pgid=",
            "-o",
            "command=",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let mut parts = line.split_whitespace();
    let parent_pid = parts.next()?.trim().parse::<u32>().ok()?;
    let process_group = parts.next()?.trim().parse::<u32>().ok()?;
    let command = parts.next()?.trim().to_string();
    if command.is_empty() {
        return None;
    }

    Some(ProcessInfo {
        parent_pid,
        process_group,
        command,
    })
}

#[cfg(unix)]
fn matches_expected_program(command: &str, expected_program: &OsStr) -> bool {
    let expected = Path::new(expected_program)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    let actual = command.split_whitespace().next().unwrap_or(command);
    let actual = Path::new(actual)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(command);

    !expected.is_empty() && actual == expected
}

#[cfg(unix)]
async fn kill_process_group(
    path: &Path,
    process_group: u32,
    grace: Duration,
) -> Result<(), NodeSupervisorError> {
    signal_process_group(path, process_group, libc::SIGTERM)?;
    let deadline = time::Instant::now() + grace;

    loop {
        if !process_group_exists(process_group) {
            return Ok(());
        }

        if time::Instant::now() >= deadline {
            signal_process_group(path, process_group, libc::SIGKILL)?;
            return Ok(());
        }

        time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
fn signal_process_group(
    path: &Path,
    process_group: u32,
    signal: i32,
) -> Result<(), NodeSupervisorError> {
    let target = i32::try_from(process_group)
        .ok()
        .filter(|process_group| *process_group > 0)
        .and_then(i32::checked_neg)
        .ok_or_else(|| NodeSupervisorError::PidFile {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "process group does not fit pid_t",
            ),
        })?;

    let result = unsafe { libc::kill(target, signal) };
    if result == 0 {
        return Ok(());
    }

    let source = io::Error::last_os_error();
    if source.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }

    Err(NodeSupervisorError::PidFile {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn process_group_exists(process_group: u32) -> bool {
    let Some(target) = i32::try_from(process_group)
        .ok()
        .filter(|process_group| *process_group > 0)
        .and_then(i32::checked_neg)
    else {
        return false;
    };

    let result = unsafe { libc::kill(target, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::TempDir;

    use super::{PidGuard, matches_expected_program};
    use crate::NodeSupervisorError;

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
        assert!(matches_expected_program(
            "/tmp/test-bin/hogd-dev",
            std::ffi::OsStr::new("hogd-dev")
        ));
        assert!(matches_expected_program(
            "/tmp/test-bin/hogd-dev --listen-host 127.0.0.1",
            std::ffi::OsStr::new("hogd-dev")
        ));
        assert!(matches_expected_program(
            "hogd-dev",
            std::ffi::OsStr::new("/usr/local/bin/hogd-dev")
        ));
        assert!(!matches_expected_program(
            "/usr/bin/sleep",
            std::ffi::OsStr::new("hogd-dev")
        ));
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
}
