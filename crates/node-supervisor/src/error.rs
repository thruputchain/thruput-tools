//! Error types for the node supervisor.

use std::io;

/// Errors returned by the node supervisor.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NodeSupervisorError {
    /// The supervisor could not start the configured child process.
    #[error("failed to spawn child process `{program}`: {source}")]
    Spawn {
        /// Program path or name.
        program: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The supervisor could not query the child process state.
    #[error("failed to query child process `{program}` status: {source}")]
    Status {
        /// Program path or name.
        program: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The supervisor could not wait for the child process to exit.
    #[error("failed to wait for child process `{program}`: {source}")]
    Wait {
        /// Program path or name.
        program: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The supervisor could not request child process termination.
    #[error("failed to stop child process `{program}`: {source}")]
    Stop {
        /// Program path or name.
        program: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}
