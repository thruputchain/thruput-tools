//! Minimal one-child process supervision.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

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
        self.spawn_with_lifetime_guard(None)
    }

    pub(crate) fn spawn_with_lifetime_guard(
        &self,
        lifetime_guard: Option<LifetimeGuard>,
    ) -> Result<SupervisedChild, NodeSupervisorError> {
        let program = self.spec.program_label();
        let child = self
            .spec
            .command()
            .spawn()
            .map_err(|source| NodeSupervisorError::Spawn {
                program: program.clone(),
                source,
            })?;
        #[cfg(unix)]
        let process_group = resolve_process_group(self.spec.process_group, child.id(), &program)?;

        Ok(SupervisedChild {
            program,
            child: Some(child),
            lifetime_guard,
            #[cfg(unix)]
            process_group,
        })
    }
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
    child: Option<Child>,
    lifetime_guard: Option<LifetimeGuard>,
    #[cfg(unix)]
    process_group: Option<i32>,
}

/// A caller-owned resource that must outlive the supervised process group.
pub(crate) struct LifetimeGuard {
    _resource: Box<dyn Send + Sync>,
    shutdown_grace: Duration,
}

impl std::fmt::Debug for LifetimeGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifetimeGuard").finish_non_exhaustive()
    }
}

impl LifetimeGuard {
    pub(crate) fn new(resource: Box<dyn Send + Sync>, shutdown_grace: Duration) -> Self {
        Self {
            _resource: resource,
            shutdown_grace,
        }
    }
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Err(error) = self.terminate_process_group() {
            tracing::debug!(program = %self.program, %error,
                "failed to SIGTERM supervised child process group during drop");
        }
        let Some(guard) = self.lifetime_guard.take() else {
            return;
        };
        // Completed, awaited shutdown can release the resource synchronously.
        let reaped = matches!(self.try_wait(), Ok(Some(_)));
        #[cfg(unix)]
        let group_gone = matches!(self.process_group_exists(), Ok(false));
        #[cfg(not(unix))]
        let group_gone = true;
        if reaped && group_gone {
            return;
        }

        let child = self
            .child
            .take()
            .expect("bug: supervised child is present until drop");
        let pending = std::sync::Arc::new(std::sync::Mutex::new(Some((child, guard))));
        let handoff = pending.clone();
        #[cfg(unix)]
        let process_group = self.process_group;
        let reap = move |pending: &std::sync::Mutex<Option<(Child, LifetimeGuard)>>| {
            let (mut child, guard) = pending
                .lock()
                .expect("bug: reaper handoff is not poisoned")
                .take()
                .expect("bug: reaper handoff is consumed once");
            let deadline = Instant::now() + guard.shutdown_grace;
            let mut killed = false;
            loop {
                let reaped = matches!(child.try_wait(), Ok(Some(_)));
                #[cfg(unix)]
                let group_gone = process_group.is_none_or(|pgid| {
                    // SAFETY: signal zero observes only the owned child's group.
                    (unsafe { libc::kill(-pgid, 0) }) == -1
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                });
                #[cfg(not(unix))]
                let group_gone = true;
                if reaped && group_gone {
                    break;
                }
                if !killed && Instant::now() >= deadline {
                    #[cfg(unix)]
                    if let Some(pgid) = process_group {
                        // SAFETY: this is the process group assigned at spawn.
                        unsafe {
                            libc::kill(-pgid, libc::SIGKILL);
                        }
                    }
                    if !reaped {
                        let _ = child.start_kill();
                    }
                    killed = true;
                }
                // Errors and lingering processes retain the resource, even beyond
                // the kill deadline. Releasing it would permit destructive cleanup.
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(guard);
        };
        // A Tokio task can be cancelled with its runtime. An OS thread owns the
        // child and resource independently, including during runtime teardown.
        if let Err(error) = std::thread::Builder::new()
            .name("devnode-reaper".into())
            .spawn(move || reap(&handoff))
        {
            tracing::warn!(%error, "failed to spawn child reaper; waiting synchronously");
            reap(&pending);
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
        self.child.as_ref().expect("bug: child is present").id()
    }

    /// Take the child's stdin pipe.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child
            .as_mut()
            .expect("bug: child is present")
            .stdin
            .take()
    }

    /// Take the child's stdout pipe.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child
            .as_mut()
            .expect("bug: child is present")
            .stdout
            .take()
    }

    /// Take the child's stderr pipe.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child
            .as_mut()
            .expect("bug: child is present")
            .stderr
            .take()
    }

    /// Return the child exit status if it has already exited.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, NodeSupervisorError> {
        self.child
            .as_mut()
            .expect("bug: child is present")
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
            .as_mut()
            .expect("bug: child is present")
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
            .as_mut()
            .expect("bug: child is present")
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

        // SAFETY: kill(2) with signal 0 performs existence/permission checks
        // for the process group and does not deliver a signal.
        let result = unsafe { libc::kill(target, 0) };
        if result == 0 {
            return Ok(true);
        }

        let source = std::io::Error::last_os_error();
        match source.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(NodeSupervisorError::Signal {
                program: self.program.clone(),
                signal: ChildSignal::from_raw(0),
                source,
            }),
        }
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

#[cfg(all(test, unix))]
mod tests {
    use crate::test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary};

    use super::{ChildProcessSpec, ChildSupervisor};

    struct DropNotice(std::sync::mpsc::Sender<()>);

    impl Drop for DropNotice {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    async fn guarded_delayed_child() -> (super::SupervisedChild, std::sync::mpsc::Receiver<()>) {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (tx, rx) = std::sync::mpsc::channel();
        let spec = ChildProcessSpec::new(fake_hogd_dev_binary())
            .env(FAKE_HOGD_DEV_MODE_ENV, "delayed-shutdown")
            .stdout_piped()
            .process_group(0);
        let mut child = ChildSupervisor::new(spec)
            .spawn_with_lifetime_guard(Some(super::LifetimeGuard::new(
                Box::new(DropNotice(tx)),
                std::time::Duration::from_secs(1),
            )))
            .expect("bug: fixture child spawns");
        let mut line = String::new();
        BufReader::new(child.take_stdout().expect("bug: stdout is piped"))
            .read_line(&mut line)
            .await
            .expect("bug: fixture readiness");
        assert_eq!(line, "ready\n");
        (child, rx)
    }

    #[tokio::test]
    async fn lifetime_guard_survives_dropped_child_until_delayed_exit() {
        let (child, rx) = guarded_delayed_child().await;
        drop(child);
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .expect("bug: guard released after child and group exit");
    }

    #[test]
    fn lifetime_guard_survives_original_runtime_teardown() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("bug: fixture runtime");
        let (child, rx) = runtime.block_on(guarded_delayed_child());
        // No runtime may be required by drop or by the thread's try_wait loop.
        drop(runtime);
        drop(child);
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .expect("bug: runtime-independent reaper releases guard");
    }

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
}
