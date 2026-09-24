//! Minimal one-child process supervision.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::error::{ChildSignal, NodeSupervisorError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildStdio {
    Null,
    Piped,
    Inherit,
}

impl ChildStdio {
    fn into_stdio(self) -> Stdio {
        match self {
            Self::Null => Stdio::null(),
            Self::Piped => Stdio::piped(),
            Self::Inherit => Stdio::inherit(),
        }
    }
}

/// Command specification for a supervised child process.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChildProcessSpec {
    program: OsString,
    args: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    current_dir: Option<PathBuf>,
    stdin: ChildStdio,
    stdout: ChildStdio,
    stderr: ChildStdio,
    kill_on_drop: bool,
    #[cfg(unix)]
    process_group: Option<i32>,
}

impl ChildProcessSpec {
    /// Create a new child process specification.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
            current_dir: None,
            stdin: ChildStdio::Null,
            stdout: ChildStdio::Inherit,
            stderr: ChildStdio::Inherit,
            kill_on_drop: true,
            #[cfg(unix)]
            process_group: None,
        }
    }

    /// Add one command-line argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple command-line arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Add one environment variable override.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    /// Set the child process working directory.
    #[must_use]
    pub fn current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(current_dir.into());
        self
    }

    /// Pipe child stdin so the supervisor can write to it after spawn.
    #[must_use]
    pub fn stdin_piped(mut self) -> Self {
        self.stdin = ChildStdio::Piped;
        self
    }

    /// Pipe child stdout so the supervisor can read readiness output.
    #[must_use]
    pub fn stdout_piped(mut self) -> Self {
        self.stdout = ChildStdio::Piped;
        self
    }

    /// Pipe child stderr so the supervisor can forward log output.
    #[must_use]
    pub fn stderr_piped(mut self) -> Self {
        self.stderr = ChildStdio::Piped;
        self
    }

    /// Configure whether Tokio force-kills the direct child when its handle is dropped.
    #[must_use]
    pub fn kill_on_drop(mut self, kill_on_drop: bool) -> Self {
        self.kill_on_drop = kill_on_drop;
        self
    }

    /// Run the child in the requested Unix process group.
    #[cfg(unix)]
    #[must_use]
    pub fn process_group(mut self, process_group: i32) -> Self {
        self.process_group = Some(process_group);
        self
    }

    /// Program path or name.
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// Command-line arguments.
    pub fn program_args(&self) -> &[OsString] {
        &self.args
    }

    /// Environment variable overrides.
    #[cfg(test)]
    pub(crate) fn program_envs(&self) -> &[(OsString, OsString)] {
        &self.envs
    }

    fn program_label(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(self.stdin.into_stdio())
            .stdout(self.stdout.into_stdio())
            .stderr(self.stderr.into_stdio())
            .kill_on_drop(self.kill_on_drop);

        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            command.process_group(process_group);
        }

        for (key, value) in &self.envs {
            command.env(key, value);
        }

        if let Some(current_dir) = &self.current_dir {
            command.current_dir(current_dir);
        }

        command
    }
}

/// One-child supervisor for a configured process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSupervisor {
    spec: ChildProcessSpec,
}

impl ChildSupervisor {
    /// Create a supervisor for one child process.
    pub fn new(spec: ChildProcessSpec) -> Self {
        Self { spec }
    }

    /// Spawn the configured child process.
    #[tracing::instrument(skip_all, fields(program = %self.spec.program_label()))]
    pub fn spawn(&self) -> Result<SupervisedChild, NodeSupervisorError> {
        self.spawn_with_inherited_file(None)
    }

    pub(crate) fn spawn_with_inherited_file(
        &self,
        inherited_file: Option<File>,
    ) -> Result<SupervisedChild, NodeSupervisorError> {
        let program = self.spec.program_label();
        let mut command = self.spec.command();
        if let Some(file) = inherited_file {
            inherit_file(&mut command, file).map_err(|source| NodeSupervisorError::Spawn {
                program: program.clone(),
                source,
            })?;
        }
        let child = command
            .spawn()
            .map_err(|source| NodeSupervisorError::Spawn {
                program: program.clone(),
                source,
            })?;
        #[cfg(unix)]
        let process_group = resolve_process_group(self.spec.process_group, child.id(), &program)?;

        Ok(SupervisedChild {
            program,
            child,
            #[cfg(unix)]
            process_group,
        })
    }
}

#[cfg(unix)]
fn inherit_file(command: &mut Command, file: File) -> std::io::Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};

    // A caller may have closed standard input/output before opening the file.
    // Move inheritance above stdio so Command's pipe/null setup cannot replace it.
    // SAFETY: file is live; fcntl returns a new descriptor owned below.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh successful duplication and is owned exactly once.
    let inherited = unsafe { File::from_raw_fd(fd) };
    drop(file);

    // SAFETY: pre_exec runs after fork. The closure calls only async-signal-safe
    // fcntl and constructs an OS error without allocating. Owning the file keeps
    // its descriptor valid until spawn completes. Only the child's descriptor
    // loses CLOEXEC; unrelated commands in this process cannot inherit it.
    unsafe {
        command.pre_exec(move || {
            let fd = inherited.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn inherit_file(_command: &mut Command, _file: File) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "inherited installation leases require Unix file descriptors",
    ))
}

#[cfg(unix)]
fn resolve_process_group(
    configured_process_group: Option<i32>,
    child_id: Option<u32>,
    program: &str,
) -> Result<Option<i32>, NodeSupervisorError> {
    match configured_process_group {
        Some(0) => child_id_to_i32(child_id, program).map(Some),
        Some(process_group) => Ok(Some(process_group)),
        None => Ok(None),
    }
}

#[cfg(unix)]
fn child_id_to_i32(child_id: Option<u32>, program: &str) -> Result<i32, NodeSupervisorError> {
    let Some(child_id) = child_id else {
        return Err(NodeSupervisorError::Status {
            program: program.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "child process id is unavailable",
            ),
        });
    };

    i32::try_from(child_id).map_err(|_| NodeSupervisorError::Status {
        program: program.to_string(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "child pid does not fit in pid_t",
        ),
    })
}

/// Running child process owned by the supervisor.
#[derive(Debug)]
pub struct SupervisedChild {
    program: String,
    child: Child,
    #[cfg(unix)]
    process_group: Option<i32>,
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Err(error) = self.terminate_process_group() {
            tracing::debug!(
                program = %self.program,
                %error,
                "failed to SIGTERM supervised child process group during drop"
            );
        }
    }
}

impl SupervisedChild {
    /// Program path or name used to spawn the child.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Operating-system process id, if the child has started and is still known.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Take the child's stdin pipe.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// Take the child's stdout pipe.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Take the child's stderr pipe.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Return the child exit status if it has already exited.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, NodeSupervisorError> {
        self.child
            .try_wait()
            .map_err(|source| NodeSupervisorError::Status {
                program: self.program.clone(),
                source,
            })
    }

    /// Wait for the child to exit.
    #[tracing::instrument(skip_all, fields(program = %self.program))]
    pub async fn wait(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        self.child
            .wait()
            .await
            .map_err(|source| NodeSupervisorError::Wait {
                program: self.program.clone(),
                source,
            })
    }

    /// Force-stop the child and wait for it to exit.
    ///
    /// PR 2 should replace this direct-child kill with the agreed process-group
    /// shutdown contract once hogd-dev process semantics are settled.
    #[tracing::instrument(skip_all, fields(program = %self.program))]
    pub async fn kill_and_wait(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        if let Some(status) = self.try_wait()? {
            return Ok(status);
        }

        self.child
            .start_kill()
            .map_err(|source| NodeSupervisorError::Stop {
                program: self.program.clone(),
                source,
            })?;
        self.wait().await
    }

    /// Send SIGTERM to the child's process group.
    #[cfg(unix)]
    pub fn terminate_process_group(&self) -> Result<(), NodeSupervisorError> {
        self.signal_process_group(libc::SIGTERM)
    }

    /// Send SIGKILL to the child's process group.
    #[cfg(unix)]
    pub fn kill_process_group(&self) -> Result<(), NodeSupervisorError> {
        self.signal_process_group(libc::SIGKILL)
    }

    /// Return true when the child's process group still has live members.
    #[cfg(unix)]
    pub fn process_group_exists(&self) -> Result<bool, NodeSupervisorError> {
        let Some(target) = self.process_group_signal_target()? else {
            return Ok(false);
        };

        process_group_has_live_member(-target).map_err(|source| NodeSupervisorError::Signal {
            program: self.program.clone(),
            signal: ChildSignal::from_raw(0),
            source,
        })
    }

    /// Unix process-group id used for child lifecycle signals.
    #[cfg(unix)]
    pub(crate) fn process_group_id(&self) -> Option<i32> {
        self.process_group
    }

    #[cfg(unix)]
    fn signal_process_group(&self, signal: i32) -> Result<(), NodeSupervisorError> {
        let Some(target) = self.process_group_signal_target()? else {
            return Ok(());
        };

        // SAFETY: kill(2) is called with the child's process-group id and a
        // signal constant. It does not alias memory or retain pointers.
        let result = unsafe { libc::kill(target, signal) };
        if result == 0 {
            return Ok(());
        }

        let source = std::io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }

        Err(NodeSupervisorError::Signal {
            program: self.program.clone(),
            signal: ChildSignal::from_raw(signal),
            source,
        })
    }

    #[cfg(unix)]
    fn process_group_signal_target(&self) -> Result<Option<i32>, NodeSupervisorError> {
        let process_group = match self.process_group {
            Some(process_group) => process_group,
            None => match self.id() {
                Some(_) => child_id_to_i32(self.id(), &self.program)?,
                None => return Ok(None),
            },
        };

        Ok(Some(-process_group))
    }
}

/// Report whether `process_group` has a member that has not exited.
///
/// An exited process stays in its group as a zombie until its parent reaps it.
/// An orphaned descendant waits on PID 1 for that, and in some containers PID 1
/// reaps late or never, so `kill(-pgid, 0)` alone would report a stopped node
/// as still running. A zombie holds no ports, files, or locks, so on Linux a
/// group whose only members are zombies is reported as gone. Anything the scan
/// cannot inspect counts as a live member, so an uncertain answer is "live".
#[cfg(unix)]
pub(crate) fn process_group_has_live_member(process_group: i32) -> std::io::Result<bool> {
    // SAFETY: kill(2) with signal 0 performs existence/permission checks
    // for the process group and does not deliver a signal.
    let result = unsafe { libc::kill(-process_group, 0) };
    if result != 0 {
        let source = std::io::Error::last_os_error();
        return match source.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(source),
        };
    }

    #[cfg(target_os = "linux")]
    return Ok(linux_process_group_has_running_member(process_group));

    #[cfg(not(target_os = "linux"))]
    Ok(true)
}

/// Scan `/proc` for a member of `process_group` with a thread that has not exited.
///
/// Only a process shown to have vanished or to belong to another group is ruled
/// out. A process whose stat cannot be read might be a live member, so it makes
/// the group live; with a restricted procfs this falls back to what `kill`
/// reported.
///
/// A thread-group leader that exits before its other threads reports the zombie
/// state while those threads run, so a zombie leader is checked thread by thread.
#[cfg(target_os = "linux")]
fn linux_process_group_has_running_member(process_group: i32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };

    entries.into_iter().any(|entry| {
        let Ok(entry) = entry else {
            return true;
        };
        let is_pid = entry
            .file_name()
            .to_str()
            .is_some_and(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()));
        if !is_pid {
            return false;
        }

        match read_linux_stat(&entry.path().join("stat")) {
            LinuxStat::Vanished => false,
            LinuxStat::Unreadable => true,
            LinuxStat::Read {
                process_group: member_group,
                ..
            } if member_group != process_group => false,
            LinuxStat::Read { state, .. } if !linux_state_has_exited(state) => true,
            LinuxStat::Read { .. } => linux_exited_leader_has_running_thread(&entry.path()),
        }
    })
}

#[cfg(target_os = "linux")]
fn linux_exited_leader_has_running_thread(process_dir: &std::path::Path) -> bool {
    let tasks = match std::fs::read_dir(process_dir.join("task")) {
        Ok(tasks) => tasks,
        Err(error) => return !linux_read_error_means_vanished(&error),
    };

    tasks.into_iter().any(|task| {
        let Ok(task) = task else {
            return true;
        };
        match read_linux_stat(&task.path().join("stat")) {
            LinuxStat::Vanished => false,
            LinuxStat::Unreadable => true,
            LinuxStat::Read { state, .. } => !linux_state_has_exited(state),
        }
    })
}

/// What reading one `/proc` stat file showed about a process or thread.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum LinuxStat {
    /// The process or thread exited and was reaped before it could be read.
    Vanished,
    /// The file exists but could not be read or parsed.
    Unreadable,
    Read {
        state: char,
        process_group: i32,
    },
}

#[cfg(target_os = "linux")]
fn read_linux_stat(path: &std::path::Path) -> LinuxStat {
    classify_linux_stat_read(std::fs::read_to_string(path))
}

#[cfg(target_os = "linux")]
fn classify_linux_stat_read(read: std::io::Result<String>) -> LinuxStat {
    match read {
        Ok(stat) => parse_linux_stat_state_and_group(&stat).map_or(
            LinuxStat::Unreadable,
            |(state, process_group)| LinuxStat::Read {
                state,
                process_group,
            },
        ),
        Err(error) if linux_read_error_means_vanished(&error) => LinuxStat::Vanished,
        Err(_) => LinuxStat::Unreadable,
    }
}

#[cfg(target_os = "linux")]
fn linux_read_error_means_vanished(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

/// Parse the state and process-group fields of a `/proc/<pid>/stat` line.
///
/// The command name is parenthesized and may itself contain spaces or `)`, so
/// fields are counted from the last `)`.
#[cfg(target_os = "linux")]
fn parse_linux_stat_state_and_group(stat: &str) -> Option<(char, i32)> {
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _parent_pid = fields.next()?;
    let process_group = fields.next()?.parse().ok()?;
    Some((state, process_group))
}

#[cfg(target_os = "linux")]
fn linux_state_has_exited(state: char) -> bool {
    matches!(state, 'Z' | 'X' | 'x')
}

#[cfg(all(test, unix))]
mod tests {
    use crate::test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary};

    use super::{ChildProcessSpec, ChildSupervisor};

    #[tokio::test]
    async fn wait_reports_child_exit_status() {
        let spec =
            ChildProcessSpec::new(fake_hogd_dev_binary()).env(FAKE_HOGD_DEV_MODE_ENV, "exit-7");
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
        let spec =
            ChildProcessSpec::new(fake_hogd_dev_binary()).env(FAKE_HOGD_DEV_MODE_ENV, "hang");
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
}
