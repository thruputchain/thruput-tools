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
    let deadline = time::Instant::now() + LOCK_RETRY_WINDOW;
    loop {
        let mut file = open_pid_file(&path)?;
        match try_lock(&path, &file) {
            // A releasing owner unlinks the PID file before its close drops
            // the lock, so a lock acquired through a descriptor opened before
            // that unlink can be for a file `path` no longer names. Only a
            // lock on the file currently at `path` counts; otherwise reopen
            // and retry.
            Ok(()) if locked_file_is_at_path(&path, &file)? => {
                clean_stale_pid(
                    &path,
                    &mut file,
                    expected_program,
                    grace,
                    reject_live_program_mismatch,
                )
                .await?;
                clear_pid(&path, &mut file)?;
                return Ok(PidGuard {
                    path,
                    file,
                    remove_on_drop: true,
                });
            }
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(source) => return Err(NodeSupervisorError::PidFile { path, source }),
        }

        if time::Instant::now() >= deadline {
            let existing_pid = read_pid(&path, &mut file)?;
            return Err(NodeSupervisorError::LocalNodeAlreadyRunning {
                path,
                pid: existing_pid,
            });
        }
        time::sleep(LOCK_RETRY_INTERVAL).await;
    }
}

/// Whether the locked descriptor still refers to the file at `path`, i.e. the
/// lock was not won on an inode the previous owner already unlinked.
#[cfg(unix)]
fn locked_file_is_at_path(path: &Path, file: &File) -> Result<bool, NodeSupervisorError> {
    use std::os::unix::fs::MetadataExt;

    let locked = file
        .metadata()
        .map_err(|source| NodeSupervisorError::PidFile {
            path: path.to_path_buf(),
            source,
        })?;
    match fs::symlink_metadata(path) {
        Ok(current) => Ok(current.dev() == locked.dev() && current.ino() == locked.ino()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(NodeSupervisorError::PidFile {
            path: path.to_path_buf(),
            source,
        }),
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

    crate::process::process_group_has_live_member(-target.as_raw()).unwrap_or(false)
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
mod tests;
