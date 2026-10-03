//! Error types for the node supervisor.

use std::{fmt, io, path::PathBuf, process::ExitStatus, time::Duration};

use groundhog_hogd_backend::IntroOutcome;
use groundhog_json_rpc_types::NetworkHashHex;
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
    /// The chain's private faucet state cannot be safely loaded or persisted.
    #[error("invalid local funding state at {}: {reason}", path.display())]
    LocalFundingState {
        /// File or directory whose state failed validation.
        path: PathBuf,
        /// Diagnostic without private material.
        reason: String,
    },
    /// Local airdrop resource or transfer configuration is invalid.
    #[error("invalid local airdrop configuration: {reason}")]
    InvalidLocalAirdropConfig {
        /// Human-readable validation failure.
        reason: &'static str,
    },
    /// The configured protocol override is not the wire width of a token.
    #[error("invalid protocol override: {source}")]
    InvalidProtocolOverride {
        /// Why the text cannot name a protocol token.
        #[from]
        source: groundhog_json_rpc_types::protocol_token::ProtocolTokenWidthError,
    },
    /// A configured binary path does not exist.
    #[error("configured hog-devnode binary path does not exist: {}", path.display())]
    ConfiguredPathMissing {
        /// Missing configured path.
        path: PathBuf,
    },

    /// A configured binary path exists but is not executable.
    #[error("configured hog-devnode binary path is not executable: {}", path.display())]
    ConfiguredPathNotExecutable {
        /// Non-executable configured path.
        path: PathBuf,
    },

    /// The supervisor could not locate a devnode binary.
    #[error("hog-devnode binary not found")]
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

    /// The configured hogd-dev bootstrap manifest request could not be represented.
    #[error("invalid hogd-dev bootstrap manifest request: {reason}")]
    InvalidBootstrapManifestConfig {
        /// Human-readable reason.
        reason: String,
    },

    /// The supervisor could not create a private bootstrap-manifest directory.
    #[error("failed to create hogd-dev bootstrap manifest temp dir: {source}")]
    BootstrapManifestTempDir {
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The supervisor could not serialize the bootstrap manifest.
    #[error("failed to serialize hogd-dev bootstrap manifest: {source}")]
    BootstrapManifestSerialize {
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },

    /// The supervisor could not write the bootstrap manifest file.
    #[error("failed to write hogd-dev bootstrap manifest {}: {source}", path.display())]
    BootstrapManifestWrite {
        /// Manifest path.
        path: PathBuf,
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

    /// The child process wrote a readiness line with an unsupported schema.
    #[error(
        "invalid readiness schema version from child process `{program}`: expected {expected}, got {actual}"
    )]
    InvalidReadinessSchemaVersion {
        /// Program path or name.
        program: String,
        /// Expected schema version.
        expected: u32,
        /// Actual schema version.
        actual: u32,
    },

    /// The child process wrote a readiness line with an unexpected status.
    #[error("invalid readiness status from child process `{program}`: {status}")]
    InvalidReadinessStatus {
        /// Program path or name.
        program: String,
        /// Status value from the readiness line.
        status: String,
    },

    /// The supervisor could not read the readiness runtime manifest.
    #[error(
        "failed to read runtime manifest `{path}` advertised by child process `{program}`: {source}"
    )]
    RuntimeManifestRead {
        /// Program path or name.
        program: String,
        /// Manifest path advertised in readiness.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The advertised runtime manifest path or permissions are unsafe.
    #[error("unsafe runtime manifest `{path}` advertised by child process `{program}`: {reason}")]
    RuntimeManifestUnsafePermissions {
        /// Program path or name.
        program: String,
        /// Manifest path advertised in readiness.
        path: String,
        /// Validation failure.
        reason: String,
    },

    /// The child process advertised a malformed runtime manifest hash.
    #[error("invalid runtime manifest hash from child process `{program}`: {hash}")]
    InvalidRuntimeManifestHash {
        /// Program path or name.
        program: String,
        /// Advertised hash.
        hash: String,
    },

    /// The runtime manifest bytes did not match the advertised hash.
    #[error(
        "runtime manifest hash mismatch from child process `{program}` for `{path}`: expected {expected}, got {actual}"
    )]
    RuntimeManifestHashMismatch {
        /// Program path or name.
        program: String,
        /// Manifest path advertised in readiness.
        path: String,
        /// Hash advertised in readiness.
        expected: String,
        /// Hash computed from manifest bytes.
        actual: String,
    },

    /// The runtime manifest could not be parsed or projected.
    #[error("invalid runtime manifest from child process `{program}`: {reason}")]
    InvalidRuntimeManifest {
        /// Program path or name.
        program: String,
        /// Validation failure.
        reason: String,
    },

    /// The runtime manifest advertised a different network than the bootstrap request.
    #[error(
        "hogd-dev runtime manifest network hash mismatch: bootstrap requested {expected}, runtime advertised {observed}"
    )]
    RuntimeNetworkHashMismatch {
        /// Network hash requested in the bootstrap manifest.
        expected: NetworkHashHex,
        /// Network hash advertised by the runtime manifest.
        observed: NetworkHashHex,
    },

    /// The runtime manifest advertised a different protocol token than requested.
    #[error(
        "hogd-dev runtime manifest protocol mismatch: supervisor requested {expected}, runtime advertised {observed:?}"
    )]
    RuntimeProtocolMismatch {
        /// Protocol token requested by the supervisor.
        expected: String,
        /// Protocol token advertised by the runtime manifest, if present.
        observed: Option<String>,
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

    /// The supervisor could not raise the process file-descriptor limit for JSON-RPC.
    #[cfg(unix)]
    #[error("failed to raise JSON-RPC nofile limit toward {target}: {source}")]
    RpcNofileLimit {
        /// Desired soft nofile limit.
        target: u64,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// The configured JSON-RPC connection budget exceeds the hard file-descriptor limit.
    #[cfg(unix)]
    #[error("JSON-RPC nofile hard limit {hard} is below requested target {target}")]
    RpcNofileHardLimit {
        /// Desired soft nofile limit.
        target: u64,
        /// Current hard nofile limit.
        hard: u64,
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

    /// The supervisor could not connect to hogd-dev's `SchedGateway` x23 endpoint.
    #[error("failed to connect hogd-dev SchedGateway endpoint {endpoint}: {source}")]
    SchedulerGatewayConnect {
        /// `SchedGateway` endpoint advertised by hogd-dev readiness.
        endpoint: std::net::SocketAddr,
        /// x23 connection error.
        #[source]
        source: ConnectError,
    },

    /// The scheduler tip belongs to a different network than the runtime manifest.
    #[error(
        "hogd-dev scheduler network hash mismatch: runtime manifest expected {expected}, scheduler reported {observed}"
    )]
    SchedulerNetworkHashMismatch {
        /// Network hash advertised by the runtime manifest.
        expected: NetworkHashHex,
        /// Network hash observed in the scheduler tip proposal header.
        observed: NetworkHashHex,
    },

    /// The coordinator and public scheduler gateway advertised different startup tips.
    #[error(
        "hogd-dev scheduler planes disagree at startup: coordinator {coordinator}, gateway {gateway}"
    )]
    SchedulerPlaneTipMismatch {
        /// Debug representation of the coordinator tip.
        coordinator: String,
        /// Debug representation of the scheduler gateway tip.
        gateway: String,
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

    /// A hogd-dev scheduler gateway RPC failed during startup.
    #[error("hogd-dev scheduler gateway RPC `{op}` failed: {source}")]
    SchedulerGatewayRpc {
        /// Scheduler gateway RPC operation name.
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

    /// The supervisor could not connect to hogd-dev's `ViewGateway` x23 endpoint.
    #[error("failed to connect hogd-dev ViewGateway endpoint {endpoint}: {source}")]
    ViewGatewayConnect {
        /// `ViewGateway` endpoint advertised by hogd-dev readiness.
        endpoint: std::net::SocketAddr,
        /// x23 connection error.
        #[source]
        source: ConnectError,
    },

    /// A hogd-dev `ViewGateway` RPC failed during startup probing.
    #[error("hogd-dev ViewGateway RPC `{op}` failed: {error}")]
    ViewGatewayRpc {
        /// `ViewGateway` RPC operation name.
        op: &'static str,
        /// x23 RPC error message.
        error: String,
    },

    /// The `ViewGateway` rejected the intro handshake with a non-success outcome.
    #[error("hogd-dev ViewGateway intro rejected with outcome {outcome}")]
    ViewGatewayIntroRejected {
        /// Raw outcome discriminant from `ViewGateway`.
        outcome: i32,
    },

    /// An x23 service rejected the intro handshake with protocol details.
    #[error(
        "{}",
        intro_rejected_message(
            service,
            *outcome,
            outcome_name,
            *is_protocol_mismatch,
            protocol,
            expected_protocol.as_deref()
        )
    )]
    ProtocolIntroRejected {
        /// x23 service that rejected the intro handshake.
        service: &'static str,
        /// Raw outcome discriminant from the x23 service.
        outcome: i32,
        /// Decoded intro outcome name when known.
        outcome_name: &'static str,
        /// Whether the intro outcome is a protocol mismatch.
        is_protocol_mismatch: bool,
        /// Protocol token sent by the Rust supervisor.
        protocol: String,
        /// Protocol token advertised by readiness, when available.
        expected_protocol: Option<String>,
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

    /// Forced shutdown did not conclusively stop the supervised process group.
    #[error(
        "child process `{program}` did not fully stop after SIGKILL (process group {process_group:?}); preserving its PID file to prevent unsafe restart"
    )]
    ShutdownIncomplete {
        /// Program path or name used to spawn the child.
        program: String,
        /// Process group that may still be alive.
        process_group: Option<i32>,
    },

    /// Persistent local-node lifecycle guarantees require Unix process groups.
    #[error("persistent hog-devnode state is only supported on Unix platforms")]
    PersistentStateUnsupported,

    /// The supervisor could not prepare a private persistent data directory.
    #[error("hog-devnode data directory operation failed at {}: {source}", path.display())]
    DataDir {
        /// Persistent data directory involved in the failed operation.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

impl NodeSupervisorError {
    /// Report an x23 service that did not accept the protocol this build
    /// speaks.
    ///
    /// `expected_protocol` is what hogd-dev advertised in its readiness
    /// payload, when the supervisor has it: naming both sides turns "intro
    /// rejected" into an actionable version mismatch.
    pub(crate) fn intro_rejected(
        outcome: IntroOutcome,
        protocol: &str,
        expected_protocol: Option<&str>,
    ) -> Self {
        Self::ProtocolIntroRejected {
            service: outcome.service(),
            outcome: outcome.code(),
            outcome_name: outcome.name(),
            is_protocol_mismatch: outcome.mismatched_protocol(),
            protocol: protocol.to_string(),
            expected_protocol: expected_protocol.map(str::to_string),
        }
    }
}

fn intro_rejected_message(
    service: &str,
    outcome: i32,
    outcome_name: &str,
    is_protocol_mismatch: bool,
    protocol: &str,
    expected_protocol: Option<&str>,
) -> String {
    if is_protocol_mismatch {
        return match expected_protocol {
            Some(expected_protocol) => format!(
                "{service} intro mismatch: groundhog node speaks protocol {protocol}, \
                 hogd-dev expects {expected_protocol} ({outcome_name}; binary is built against \
                 a different protocol commit)"
            ),
            None => format!(
                "{service} intro mismatch: groundhog node speaks protocol {protocol}, \
                 but hogd-dev rejected it ({outcome_name})"
            ),
        };
    }

    format!(
        "hogd-dev {service} intro rejected with outcome {outcome_name} \
         (discriminant {outcome}); groundhog node speaks protocol {protocol}"
    )
}
