//! Minimal one-child process supervision.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};

use tokio::process::{Child, Command};

use crate::NodeSupervisorError;

/// Command specification for a supervised child process.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChildProcessSpec {
    program: OsString,
    args: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    current_dir: Option<PathBuf>,
}

impl ChildProcessSpec {
    /// Create a new child process specification.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
            current_dir: None,
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

    /// Program path or name.
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    fn program_label(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

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
        let program = self.spec.program_label();
        let child = self
            .spec
            .command()
            .spawn()
            .map_err(|source| NodeSupervisorError::Spawn {
                program: program.clone(),
                source,
            })?;

        Ok(SupervisedChild { program, child })
    }
}

/// Running child process owned by the supervisor.
#[derive(Debug)]
pub struct SupervisedChild {
    program: String,
    child: Child,
}

impl SupervisedChild {
    /// Operating-system process id, if the child has started and is still known.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
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
}

#[cfg(test)]
mod tests {
    use super::{ChildProcessSpec, ChildSupervisor};

    #[tokio::test]
    async fn wait_reports_child_exit_status() {
        let spec = ChildProcessSpec::new("/bin/sh").args(["-c", "exit 7"]);
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
        let spec = ChildProcessSpec::new("/bin/sleep").arg("60");
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
