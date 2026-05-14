//! Error types for the node supervisor.

use std::{fmt, io, path::PathBuf, process::ExitStatus, time::Duration};

use groundhog_x23_transport::ConnectError;

/// Unix child-process signal used by the supervisor.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildSignal(i32);

#[cfg(unix)]
impl ChildSignal {
    /// Create a signal wrapper from a raw libc signal number.
    pub const fn from_raw(signal: i32) -> Self {
        Self(signal)
    }

    /// Raw libc signal number.
    pub const fn raw(self) -> i32 {
        self.0
    }

    const fn name(self) -> Option<&'static str> {
        match self.0 {
            libc::SIGTERM => Some("SIGTERM"),
            libc::SIGKILL => Some("SIGKILL"),
            _ => None,
        }
    }
}

#[cfg(unix)]
impl fmt::Display for ChildSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "signal {}", self.raw()),
        }
    }
}

/// Errors returned by the node supervisor.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NodeSupervisorError {
    /// A configured binary path does not exist.
    #[error("configured hogd-dev binary path does not exist: {}", path.display())]
    ConfiguredPathMissing {
        /// Missing configured path.
        path: PathBuf,
    },

    /// A configured binary path exists but is not executable.
    #[error("configured hogd-dev binary path is not executable: {}", path.display())]
    ConfiguredPathNotExecutable {
        /// Non-executable configured path.
        path: PathBuf,
    },

    /// The supervisor could not locate a hogd-dev binary.
    #[error("hogd-dev binary not found")]
    HogdDevNotFound,

    /// The supervisor could not determine its current executable path.
    #[error("failed to resolve current executable path: {source}")]
    CurrentExe {
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The supervisor could not start the configured child process.
    #[error("failed to spawn child process `{program}`: {source}")]
    Spawn {
        /// Program path or name.
        program: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The child process was spawned without a stdout pipe.
    #[error("child process `{program}` did not expose stdout")]
    MissingStdout {
        /// Program path or name.
        program: String,
    },

    /// The child process was spawned without a stderr pipe.
    #[error("child process `{program}` did not expose stderr")]
    MissingStderr {
        /// Program path or name.
        program: String,
    },

    /// The child process did not report readiness before the startup timeout.
    #[error("child process `{program}` did not report readiness within {timeout:?}")]
    ReadinessTimeout {
        /// Program path or name.
        program: String,
        /// Startup timeout.
        timeout: Duration,
    },

    /// The child process wrote a readiness line exceeding the configured cap.
    #[error("child process `{program}` wrote a readiness line larger than {max_bytes} bytes")]
    ReadinessTooLarge {
        /// Program path or name.
        program: String,
        /// Maximum allowed readiness line size.
        max_bytes: usize,
    },

    /// The child process exited before reporting readiness.
    #[error("child process `{program}` exited before readiness with status {status}")]
    ExitedBeforeReady {
        /// Program path or name.
        program: String,
        /// Child exit status.
        status: ExitStatus,
    },

    /// The supervisor could not read the readiness line.
    #[error("failed to read readiness from child process `{program}`: {source}")]
    ReadinessRead {
        /// Program path or name.
        program: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The child process wrote malformed readiness JSON.
    #[error("invalid readiness JSON from child process `{program}`: {source}")]
    InvalidReadinessJson {
        /// Program path or name.
        program: String,
        /// Raw readiness line.
        line: String,
        /// JSON parsing error.
        #[source]
        source: serde_json::Error,
    },

    /// The child process wrote a readiness line with an unexpected status.
    #[error("invalid readiness status from child process `{program}`: {status}")]
    InvalidReadinessStatus {
        /// Program path or name.
        program: String,
        /// Status value from the readiness line.
        status: String,
    },

    /// hogd-dev advertised a different dev-account count than requested.
    #[error(
        "hogd-dev readiness advertised {actual} dev accounts, but supervisor requested {expected}"
    )]
    DevAccountCountMismatch {
        /// Requested dev-account count.
        expected: usize,
        /// Advertised dev-account count.
        actual: usize,
    },

    /// hogd-dev advertised a dev-account entry at the wrong index.
    #[error(
        "hogd-dev readiness dev account at position {position} had index \
         {actual}, expected {expected}"
    )]
    DevAccountIndexMismatch {
        /// Position in the readiness account array.
        position: usize,
        /// Expected account index.
        expected: usize,
        /// Advertised account index.
        actual: u32,
    },

    /// hogd-dev advertised a malformed dev-account public key.
    #[error(
        "hogd-dev readiness dev account {index} public key had {actual} bytes, \
         expected 32"
    )]
    DevAccountPublicKeyLength {
        /// Dev-account index.
        index: usize,
        /// Advertised public key length.
        actual: usize,
    },

    /// hogd-dev advertised a dev-account public key that does not match the
    /// requested seed.
    #[error(
        "hogd-dev readiness dev account {index} public key mismatch: expected \
         {expected}, got {actual}"
    )]
    DevAccountPublicKeyMismatch {
        /// Dev-account index.
        index: usize,
        /// Expected 0x-prefixed public key.
        expected: String,
        /// Advertised 0x-prefixed public key.
        actual: String,
    },

    /// The supervisor could not signal the child process group.
    #[cfg(unix)]
    #[error("failed to send {signal} to child process group `{program}`: {source}")]
    Signal {
        /// Program path or name.
        program: String,
        /// Signal sent to the child process group.
        signal: ChildSignal,
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

    /// The supervisor could not bind the JSON-RPC server to its listen address.
    #[error("failed to bind JSON-RPC server to {address}: {source}")]
    RpcBind {
        /// Configured listen address.
        address: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The supervisor could not read the JSON-RPC server's bound address.
    #[error("failed to read JSON-RPC bound address: {source}")]
    RpcLocalAddr {
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The supervisor stop signal failed.
    #[error("failed to wait for supervisor stop signal: {source}")]
    StopSignal {
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The supervisor could not connect to hogd-dev's `SchedulerCoord` x23 endpoint.
    #[error("failed to connect hogd-dev SchedulerCoord endpoint {endpoint}: {source}")]
    SchedulerConnect {
        /// `SchedulerCoord` endpoint advertised by hogd-dev readiness.
        endpoint: std::net::SocketAddr,
        /// x23 connection error.
        #[source]
        source: ConnectError,
    },

    /// A hogd-dev scheduler RPC failed.
    #[error("hogd-dev scheduler RPC `{op}` failed: {source}")]
    SchedulerRpc {
        /// Scheduler RPC operation name.
        op: &'static str,
        /// x23 RPC error.
        #[source]
        source: rpc_driver::RpcError,
    },

    /// The scheduler rejected a startup operation.
    #[error("hogd-dev scheduler rejected `{op}`: {result}")]
    SchedulerRejected {
        /// Scheduler RPC operation name.
        op: &'static str,
        /// Scheduler result value.
        result: String,
    },

    /// The supervisor could not connect to hogd-dev's `IndexerBlockExplorer` x23 endpoint.
    #[error("failed to connect hogd-dev IndexerBlockExplorer endpoint {endpoint}: {source}")]
    IndexerConnect {
        /// `IndexerBlockExplorer` endpoint advertised by hogd-dev readiness.
        endpoint: std::net::SocketAddr,
        /// x23 connection error.
        #[source]
        source: ConnectError,
    },

    /// A hogd-dev indexer RPC failed during startup probing.
    #[error("hogd-dev indexer RPC `{op}` failed: {error}")]
    IndexerRpc {
        /// Indexer RPC operation name.
        op: &'static str,
        /// x23 RPC error message.
        error: String,
    },

    /// The indexer rejected the intro handshake with a non-success outcome.
    #[error("hogd-dev indexer intro rejected with outcome {outcome}")]
    IndexerIntroRejected {
        /// Raw outcome discriminant from the indexer.
        outcome: i32,
    },

    /// The supervisor could not connect to hogd-dev's `DbGateway` x23 endpoint.
    #[error("failed to connect hogd-dev DbGateway endpoint {endpoint}: {source}")]
    DbGatewayConnect {
        /// `DbGateway` endpoint advertised by hogd-dev readiness.
        endpoint: std::net::SocketAddr,
        /// x23 connection error.
        #[source]
        source: ConnectError,
    },

    /// A hogd-dev `DbGateway` RPC failed during startup probing.
    #[error("hogd-dev DbGateway RPC `{op}` failed: {error}")]
    DbGatewayRpc {
        /// `DbGateway` RPC operation name.
        op: &'static str,
        /// x23 RPC error message.
        error: String,
    },

    /// The `DbGateway` rejected the intro handshake with a non-success outcome.
    #[error("hogd-dev DbGateway intro rejected with outcome {outcome}")]
    DbGatewayIntroRejected {
        /// Raw outcome discriminant from `DbGateway`.
        outcome: i32,
    },

    /// The supervisor could not operate on the hogd-dev PID file.
    #[error("hogd-dev PID file operation failed at {}: {source}", path.display())]
    PidFile {
        /// PID file path or parent directory involved in the failed operation.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// Another managed local node appears to be running.
    #[error(
        "groundhog local node already appears to be running (PID file {}, hogd-dev pid {pid:?}); stop it before starting another node",
        path.display()
    )]
    LocalNodeAlreadyRunning {
        /// PID file path that is locked by the running node.
        path: PathBuf,
        /// Existing hogd-dev PID if it could be read from the PID file.
        pid: Option<u32>,
    },
}
