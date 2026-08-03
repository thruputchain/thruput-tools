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

#[cfg(unix)]
use nix::unistd::Pid;

/// Guard that owns the hogd-dev PID file for the supervisor lifetime.
#[derive(Debug)]
pub(crate) struct PidGuard {
    path: PathBuf,
    file: File,
    remove_on_drop: bool,
}

impl PidGuard {
    /// Acquire PID-file ownership, cleaning up stale or orphaned hogd-dev processes first.
    pub(crate) async fn acquire(
        path: PathBuf,
        expected_program: &OsStr,
        grace: Duration,
    ) -> Result<Self, NodeSupervisorError> {
        Self::acquire_with_policy(path, expected_program, grace, false).await
    }

    /// Acquire PID-file ownership without accepting an unverifiable live process.
    pub(crate) async fn acquire_persistent(
        path: PathBuf,
        expected_program: &OsStr,
        grace: Duration,
    ) -> Result<Self, NodeSupervisorError> {
        Self::acquire_with_policy(path, expected_program, grace, true).await
    }

    async fn acquire_with_policy(
        path: PathBuf,
        expected_program: &OsStr,
        grace: Duration,
        reject_live_program_mismatch: bool,
    ) -> Result<Self, NodeSupervisorError> {
        #[cfg(unix)]
        {
            acquire_unix(path, expected_program, grace, reject_live_program_mismatch).await
        }

        #[cfg(not(unix))]
        {
            let file = open_pid_file(&path)?;
            Ok(Self {
                path,
                file,
                remove_on_drop: true,
            })
        }
    }

    /// Write the supervised child PID into the already-owned PID file.
    pub(crate) fn write(&mut self, pid: u32) -> Result<(), NodeSupervisorError> {
        write_pid(&self.path, &mut self.file, pid)?;
        self.remove_on_drop = false;
        Ok(())
    }

    /// Remove the PID file after the supervised process group is confirmed stopped.
    pub(crate) fn release(mut self) {
        self.remove_on_drop = true;
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if !self.remove_on_drop {
            return;
        }
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
        create_pid_parent(parent)?;
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|source| NodeSupervisorError::PidFile {
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| NodeSupervisorError::PidFile {
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(file)
}

#[cfg(unix)]
fn create_pid_parent(parent: &Path) -> Result<(), NodeSupervisorError> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(parent)
        .map_err(|source| NodeSupervisorError::PidFile {
            path: parent.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn create_pid_parent(parent: &Path) -> Result<(), NodeSupervisorError> {
    fs::create_dir_all(parent).map_err(|source| NodeSupervisorError::PidFile {
        path: parent.to_path_buf(),
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

/// How long to retry a contended PID-file lock before treating the holder as
/// a live supervisor. A child process spawned concurrently by another thread
/// briefly co-owns this process's file descriptors (and their `flock`s)
/// between fork and exec, so an instantly contended lock does not yet prove
/// another supervisor is running.
#[cfg(unix)]
const LOCK_RETRY_WINDOW: Duration = Duration::from_millis(500);
#[cfg(unix)]
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(unix)]
async fn acquire_unix(
    path: PathBuf,
    expected_program: &OsStr,
    grace: Duration,
    reject_live_program_mismatch: bool,
) -> Result<PidGuard, NodeSupervisorError> {
    let mut file = open_pid_file(&path)?;

    match try_lock_with_retry(&path, &file).await {
        Ok(()) => {
            clean_stale_pid(
                &path,
                &mut file,
                expected_program,
                grace,
                reject_live_program_mismatch,
            )
            .await?;
            clear_pid(&path, &mut file)?;
            Ok(PidGuard {
                path,
                file,
                remove_on_drop: true,
            })
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
async fn try_lock_with_retry(path: &Path, file: &File) -> Result<(), io::Error> {
    let deadline = time::Instant::now() + LOCK_RETRY_WINDOW;
    loop {
        match try_lock(path, file) {
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && time::Instant::now() < deadline =>
            {
                time::sleep(LOCK_RETRY_INTERVAL).await;
            }
            result => return result,
        }
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
    reject_live_program_mismatch: bool,
) -> Result<(), NodeSupervisorError> {
    let Some(pid) = read_pid(path, file)? else {
        return Ok(());
    };

    let Some(info) = process_info(pid) else {
        if process_exists(pid) {
            tracing::warn!(
                pid,
                path = %path.display(),
                "PID file points at a live process, but this platform could not verify \
                 its identity; refusing to signal it"
            );
            return Err(NodeSupervisorError::LocalNodeAlreadyRunning {
                path: path.to_path_buf(),
                pid: Some(pid),
            });
        }
        if process_group_exists(pid) {
            tracing::warn!(
                pid,
                process_group = pid,
                path = %path.display(),
                "PID file's process-group leader exited but descendants remain; refusing startup because their identity cannot be verified safely"
            );
            return Err(NodeSupervisorError::LocalNodeAlreadyRunning {
                path: path.to_path_buf(),
                pid: Some(pid),
            });
        }
        tracing::debug!(pid, path = %path.display(), "removing stale hogd-dev PID file");
        return Ok(());
    };

    if !matches_expected_program(&info.program, expected_program) {
        tracing::warn!(
            pid,
            program = %info.program,
            path = %path.display(),
            "PID file points at a different program (likely PID reuse); ignoring"
        );
        if reject_live_program_mismatch {
            return Err(NodeSupervisorError::LocalNodeAlreadyRunning {
                path: path.to_path_buf(),
                pid: Some(pid),
            });
        }
        return Ok(());
    }

    if info.process_group != pid {
        // The supervisor always spawns hogd-dev as its own process-group
        // leader (`process_group(0)`), so a recorded PID that is not its
        // group's leader means signalling the group would hit the wrong
        // processes. Refuse to clean up.
        tracing::warn!(
            pid,
            pgid = info.process_group,
            path = %path.display(),
            "PID file's hogd-dev is not the leader of its process group; refusing to signal"
        );
        return Err(NodeSupervisorError::LocalNodeAlreadyRunning {
            path: path.to_path_buf(),
            pid: Some(pid),
        });
    }

    // Lock ownership already proved no live supervisor holds this PID file,
    // so the recorded child is orphaned no matter where it got reparented.
    // Don't gate on `ppid == 1` — subreapers (systemd --user, Docker/tini,
    // dev-container launchers) reparent orphans to themselves, not init.
    tracing::info!(
        pid,
        ppid = ?info.parent_pid,
        "stopping prior hogd-dev process group before startup"
    );
    kill_process_group(path, pid, grace).await
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessInfo {
    parent_pid: Option<u32>,
    process_group: u32,
    /// Kernel-set process name, truncated on some platforms.
    program: String,
}

#[cfg(unix)]
fn process_info(target_pid: u32) -> Option<ProcessInfo> {
    let pid = pid_from_u32(target_pid)?;
    let process_group = u32::try_from(nix::unistd::getpgid(Some(pid)).ok()?.as_raw()).ok()?;

    #[cfg(target_os = "linux")]
    let (parent_pid, program) = linux_process_parent_and_comm(target_pid)?;

    #[cfg(target_os = "macos")]
    let (parent_pid, program) = (None, macos_process_comm(target_pid)?);

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let (parent_pid, program) = {
        let _ = target_pid;
        return None;
    };

    Some(ProcessInfo {
        parent_pid,
        process_group,
        program,
    })
}

#[cfg(all(unix, target_os = "linux"))]
fn linux_process_parent_and_comm(target_pid: u32) -> Option<(Option<u32>, String)> {
    let stat = fs::read_to_string(format!("/proc/{target_pid}/stat")).ok()?;
    parse_linux_stat(&stat)
}

#[cfg(all(unix, target_os = "linux"))]
fn parse_linux_stat(stat: &str) -> Option<(Option<u32>, String)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    if close <= open {
        return None;
    }

    let program = stat[open + 1..close].to_string();
    if program.is_empty() {
        return None;
    }

    let mut fields = stat[close + 1..].split_whitespace();
    let _state = fields.next()?;
    let parent_pid = fields.next()?.parse::<u32>().ok()?;

    Some((Some(parent_pid), program))
}

#[cfg(all(unix, target_os = "macos"))]
fn macos_process_comm(target_pid: u32) -> Option<String> {
    let pid = i32::try_from(target_pid).ok()?;
    let mut buffer = [0 as libc::c_char; 1024];
    let len = unsafe {
        libc::proc_name(
            pid,
            buffer.as_mut_ptr().cast::<libc::c_void>(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if len <= 0 {
        return None;
    }

    let program = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
        .to_str()
        .ok()?
        .to_string();
    if program.is_empty() {
        return None;
    }

    Some(program)
}

#[cfg(unix)]
fn matches_expected_program(program: &str, expected_program: &OsStr) -> bool {
    let expected = Path::new(expected_program)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();

    !expected.is_empty()
        && (program == expected || (expected == "hog-devnode" && program == "hogd-dev"))
}

#[cfg(unix)]
async fn kill_process_group(
    path: &Path,
    process_group: u32,
    grace: Duration,
) -> Result<(), NodeSupervisorError> {
    /// Hard cap on the post-SIGKILL drain. Bounds startup latency while
    /// giving the kernel a window to release sockets the dying child held
    /// (otherwise the new hogd-dev can race the old one for the listening
    /// port and fail to bind).
    const POST_KILL_DRAIN: Duration = Duration::from_millis(500);

    signal_process_group(path, process_group, libc::SIGTERM)?;
    let term_deadline = time::Instant::now() + grace;
    let mut kill_deadline: Option<time::Instant> = None;

    loop {
        if !process_group_exists(process_group) {
            return Ok(());
        }

        let now = time::Instant::now();

        if kill_deadline.is_none() && now >= term_deadline {
            tracing::warn!(
                pid = process_group,
                process_group,
                path = %path.display(),
                shutdown_grace = ?grace,
                orphan_cleanup = true,
                "prior hog-devnode process group did not stop within its graceful shutdown deadline; sending SIGKILL; persistent state may require recovery"
            );
            signal_process_group(path, process_group, libc::SIGKILL)?;
            kill_deadline = Some(now + POST_KILL_DRAIN);
        }

        if let Some(deadline) = kill_deadline
            && now >= deadline
        {
            tracing::warn!(
                process_group,
                path = %path.display(),
                orphan_cleanup = true,
                "hogd-dev process group still present after SIGKILL drain; \
                 refusing startup while the prior node may still own persistent state"
            );
            return Err(NodeSupervisorError::LocalNodeAlreadyRunning {
                path: path.to_path_buf(),
                pid: Some(process_group),
            });
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
    let target = process_group_signal_target(path, process_group)?;

    let result = unsafe { libc::kill(target.as_raw(), signal) };
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
    let Ok(target) = process_group_signal_target(Path::new("<process-group>"), process_group)
    else {
        return false;
    };

    let result = unsafe { libc::kill(target.as_raw(), 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    let Some(target) = pid_from_u32(pid) else {
        return false;
    };

    let result = unsafe { libc::kill(target.as_raw(), 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn process_group_signal_target(
    path: &Path,
    process_group: u32,
) -> Result<Pid, NodeSupervisorError> {
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

    Ok(Pid::from_raw(target))
}

#[cfg(unix)]
fn pid_from_u32(pid: u32) -> Option<Pid> {
    i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .map(Pid::from_raw)
}

#[cfg(test)]
mod tests {
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
}
