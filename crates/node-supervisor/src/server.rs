//! Top-level node supervisor handle.

use std::process::ExitStatus;

use crate::{NodeSupervisorError, SupervisedChild};

/// Running node supervisor instance.
///
/// The scaffold only owns a supervised child. Later PRs will add the public
/// JSON-RPC server handle and hogd-dev readiness metadata here.
#[derive(Debug)]
pub struct NodeSupervisorHandle {
    child: SupervisedChild,
}

impl NodeSupervisorHandle {
    /// Create a supervisor handle from a running child process.
    pub fn new(child: SupervisedChild) -> Self {
        Self { child }
    }

    /// Operating-system process id for the supervised child, if available.
    pub fn child_id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Wait until the supervised child exits.
    #[tracing::instrument(skip_all)]
    pub async fn stopped(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        self.child.wait().await
    }

    /// Force-stop the supervised child.
    #[tracing::instrument(skip_all)]
    pub async fn stop(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        self.child.kill_and_wait().await
    }
}
