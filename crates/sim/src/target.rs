use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

use crate::error::SimError;
use crate::manifest::{ChaosEventKind, ChaosEventSpec, SimulationManifest};
use crate::result::ChaosEventState;
use crate::util::{command_on_path, write_json_pretty_atomic};
use crate::validation::{
    ValidationError, required_bootstrap_accounts, validate_existing_target_flags,
    validate_local_target_flags,
};

const DEFAULT_LOCAL_STARTUP_TIMEOUT_SECS: u64 = 30;
const LOCAL_CLEANUP_GRACE: Duration = Duration::from_secs(10);
const LOCAL_RESTART_RELAUNCH_ATTEMPTS: usize = 3;
const LOCAL_RESTART_RELAUNCH_RETRY_DELAY: Duration = Duration::from_millis(250);
const LOCAL_STARTUP_EXIT_STDOUT_GRACE: Duration = Duration::from_millis(100);
const LOCAL_STARTUP_STDERR_CAPTURE_LIMIT_BYTES: usize = 64 * 1024;

/// Boxed future returned by target-provider lifecycle hooks.
pub type TargetProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Target provider configuration stored in a simulation manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct TargetProviderSpec {
    /// Provider implementation to use for this scenario.
    pub provider: TargetProviderKind,
    /// Public JSON-RPC URL for the `existing` provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    /// Runtime manifest path for the `existing` provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_manifest: Option<PathBuf>,
    /// `hogd-dev` binary path reserved for the Phase 2 `local` provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hogd_dev_binary: Option<PathBuf>,
}

/// Target provider selected by a simulation manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "kebab-case")]
pub enum TargetProviderKind {
    Local,
    Existing,
    RemoteSsh,
}

/// Capabilities advertised by a target provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct TargetCapabilities {
    /// Chaos event kinds this provider can execute.
    pub chaos: Vec<ChaosEventKind>,
}

impl TargetCapabilities {
    /// Return the static capabilities currently known for a provider kind.
    pub fn for_provider(provider: TargetProviderKind) -> Self {
        let chaos = match provider {
            TargetProviderKind::Local => vec![ChaosEventKind::RestartTarget],
            TargetProviderKind::Existing | TargetProviderKind::RemoteSsh => Vec::new(),
        };
        Self { chaos }
    }

    /// Return whether this capability set includes a chaos event kind.
    pub fn supports_chaos(&self, kind: ChaosEventKind) -> bool {
        self.chaos.contains(&kind)
    }
}

/// Ownership and cleanup policy for a resolved target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct TargetOwnership {
    /// Provider kind that owns or resolved the target.
    pub provider: TargetProviderKind,
    /// Whether this simulation process started the target.
    pub started_by_sim: bool,
    /// Whether this simulation process may clean up the target.
    pub cleanup_allowed: bool,
    /// Process identity for simulator-owned local targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<OwnedProcess>,
}

/// Process identity captured for a simulator-owned local target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct OwnedProcess {
    /// Operating-system process identifier for the `groundhog node` process.
    pub pid: u32,
    /// Unix process-group identifier for the node and supervised backend.
    pub pgid: u32,
    /// Public JSON-RPC port resolved from node startup JSON.
    pub port: u16,
    /// Opaque process start-time token read from `ps` and compared verbatim.
    pub start_time: String,
    /// Short command marker expected to appear in the live process command.
    pub command: String,
}

impl TargetOwnership {
    /// Return ownership metadata for an externally managed target.
    pub fn existing() -> Self {
        Self {
            provider: TargetProviderKind::Existing,
            started_by_sim: false,
            cleanup_allowed: false,
            process: None,
        }
    }

    /// Return ownership metadata for a simulator-owned local target.
    pub fn local(process: Option<OwnedProcess>) -> Self {
        Self {
            provider: TargetProviderKind::Local,
            started_by_sim: true,
            cleanup_allowed: true,
            process,
        }
    }
}

/// Command-line target overrides supplied outside the simulation manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TargetOverrides {
    /// Public JSON-RPC URL to use for the run.
    pub rpc_url: Option<String>,
    /// Runtime manifest path to use for the run.
    pub runtime_manifest: Option<PathBuf>,
    /// `groundhog` binary path for the `local` provider.
    pub groundhog_binary: Option<PathBuf>,
    /// `hogd-dev` binary path for the `local` provider.
    pub hogd_dev_binary: Option<PathBuf>,
    /// `hogd-dev` x23 intro protocol token for the `local` provider.
    pub hogd_protocol: Option<String>,
    /// Public JSON-RPC port requested from the `local` provider.
    pub port: Option<u16>,
    /// Startup timeout in seconds for the `local` provider.
    pub startup_timeout_secs: Option<u64>,
}

/// Per-run context supplied to target providers during acquisition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TargetProviderContext {
    /// Directory where providers may write target-owned logs for this run.
    pub logs_dir: PathBuf,
    /// Path to the run's target ownership artifact.
    pub ownership_path: PathBuf,
}

impl TargetOverrides {
    fn first_local_launch_override(&self) -> Option<&'static str> {
        if self.groundhog_binary.is_some() {
            Some("--groundhog-binary")
        } else if self.hogd_dev_binary.is_some() {
            Some("--hogd-dev-binary")
        } else if self.hogd_protocol.is_some() {
            Some("--hogd-protocol")
        } else if self.port.is_some() {
            Some("--port")
        } else if self.startup_timeout_secs.is_some() {
            Some("--startup-timeout-secs")
        } else {
            None
        }
    }
}

/// Concrete target endpoint and metadata acquired for one simulation run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResolvedTarget {
    /// Provider kind that produced this target.
    pub provider: TargetProviderKind,
    /// Runtime capabilities declared by the provider.
    pub capabilities: TargetCapabilities,
    /// Ownership and cleanup policy for the target.
    pub ownership: TargetOwnership,
    /// Public JSON-RPC URL used by workload execution.
    pub rpc_url: String,
    /// Runtime manifest used for validation and account derivation.
    pub runtime_manifest: PathBuf,
}

/// Result of executing one chaos event against a provider-owned target.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChaosEventOutcome {
    /// Final state the provider observed for the event.
    pub state: ChaosEventState,
    /// Optional human-readable detail for failures or provider-specific notes.
    pub detail: Option<String>,
}

/// Target-provider lifecycle boundary used by the simulator runner.
pub trait TargetProvider {
    /// Return this provider's kind.
    fn kind(&self) -> TargetProviderKind;

    /// Return this provider's runtime capabilities.
    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::for_provider(self.kind())
    }

    /// Acquire or resolve the target endpoint for one simulation run.
    fn acquire<'a>(
        &'a self,
        manifest: &'a SimulationManifest,
        overrides: TargetOverrides,
        context: TargetProviderContext,
    ) -> TargetProviderFuture<'a, Result<ResolvedTarget, SimError>>;

    /// Return the chaos controller for providers that can execute chaos events.
    fn chaos_controller(&self) -> Option<&dyn ChaosController> {
        None
    }

    /// Clean up target resources acquired for a run.
    ///
    /// Implementations must be idempotent because future managed providers may
    /// call cleanup from both normal and error paths.
    fn cleanup<'a>(
        &'a self,
        _target: &'a ResolvedTarget,
    ) -> TargetProviderFuture<'a, Result<(), SimError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Optional target fault controller for providers that support chaos events.
pub trait ChaosController {
    /// Execute one manifest chaos event against the provider-owned target.
    fn execute<'a>(
        &'a self,
        event: &'a ChaosEventSpec,
    ) -> TargetProviderFuture<'a, Result<ChaosEventOutcome, SimError>>;
}

/// Provider for an already-running target supplied by URL and runtime manifest.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExistingTargetProvider;

impl TargetProvider for ExistingTargetProvider {
    fn kind(&self) -> TargetProviderKind {
        TargetProviderKind::Existing
    }

    fn acquire<'a>(
        &'a self,
        manifest: &'a SimulationManifest,
        overrides: TargetOverrides,
        _context: TargetProviderContext,
    ) -> TargetProviderFuture<'a, Result<ResolvedTarget, SimError>> {
        Box::pin(async move {
            if let Some(flag) = overrides.first_local_launch_override() {
                return Err(ValidationError::ExistingRejectsLocalLaunchOverride { flag }.into());
            }
            validate_existing_target_flags(
                manifest,
                overrides.rpc_url.as_deref(),
                overrides.runtime_manifest.is_some(),
            )?;
            let rpc_url = overrides
                .rpc_url
                .or_else(|| manifest.target.rpc_url.clone())
                .ok_or(ValidationError::MissingRpcUrl)?;
            let runtime_manifest = overrides
                .runtime_manifest
                .or_else(|| manifest.target.runtime_manifest.clone())
                .ok_or(ValidationError::MissingRuntimeManifest)?;
            Ok(ResolvedTarget {
                provider: TargetProviderKind::Existing,
                capabilities: self.capabilities(),
                ownership: TargetOwnership::existing(),
                rpc_url,
                runtime_manifest,
            })
        })
    }
}

/// Provider that launches and owns a local `groundhog node` process.
#[derive(Debug, Default)]
pub struct LocalTargetProvider {
    state: Mutex<LocalTargetState>,
}

#[derive(Debug, Default)]
enum LocalTargetState {
    #[default]
    Idle,
    Acquiring,
    Acquired(Box<LocalAcquiredTarget>),
}

#[derive(Debug)]
struct LocalAcquiredTarget {
    child: Child,
    launch_config: LocalLaunchConfig,
    log_tasks: LocalLogTasks,
    rpc_url: String,
    rpc_port: u16,
    next_launch_index: u32,
}

#[derive(Debug, Clone)]
struct LocalLaunchConfig {
    groundhog_binary: PathBuf,
    hogd_dev_binary: Option<PathBuf>,
    hogd_protocol: Option<String>,
    required_accounts: u32,
    startup_timeout_secs: u64,
    logs_dir: PathBuf,
    ownership_path: PathBuf,
}

#[derive(Debug)]
struct LocalLaunchedTarget {
    child: Child,
    startup: LocalNodeStartup,
    log_tasks: LocalLogTasks,
}

#[derive(Debug)]
struct LocalLogTasks {
    stdout: JoinHandle<io::Result<()>>,
    stderr: JoinHandle<io::Result<()>>,
}

#[derive(Debug)]
struct StdoutCapture {
    startup_line: oneshot::Receiver<StartupLine>,
    task: JoinHandle<io::Result<()>>,
}

#[derive(Debug)]
struct StderrCapture {
    collected: Arc<Mutex<String>>,
    task: JoinHandle<io::Result<()>>,
}

#[derive(Debug)]
enum StartupLine {
    Line(String),
    Eof,
    Io(String),
}

impl LocalTargetProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TargetProvider for LocalTargetProvider {
    fn kind(&self) -> TargetProviderKind {
        TargetProviderKind::Local
    }

    fn acquire<'a>(
        &'a self,
        manifest: &'a SimulationManifest,
        overrides: TargetOverrides,
        context: TargetProviderContext,
    ) -> TargetProviderFuture<'a, Result<ResolvedTarget, SimError>> {
        Box::pin(async move {
            validate_local_target_flags(
                manifest,
                overrides.rpc_url.as_deref(),
                overrides.runtime_manifest.is_some(),
            )?;
            {
                let mut state = self.state.lock().await;
                if !matches!(*state, LocalTargetState::Idle) {
                    return Err(SimError::LocalNodeAlreadyAcquired);
                }
                *state = LocalTargetState::Acquiring;
            }
            let acquire_result = self.acquire_inner(manifest, overrides, context).await;
            if acquire_result.is_err() {
                *self.state.lock().await = LocalTargetState::Idle;
            }
            acquire_result
        })
    }

    fn cleanup<'a>(
        &'a self,
        _target: &'a ResolvedTarget,
    ) -> TargetProviderFuture<'a, Result<(), SimError>> {
        Box::pin(async move {
            let acquired = match std::mem::take(&mut *self.state.lock().await) {
                LocalTargetState::Acquired(acquired) => acquired,
                LocalTargetState::Idle | LocalTargetState::Acquiring => return Ok(()),
            };
            let LocalAcquiredTarget {
                mut child,
                log_tasks,
                ..
            } = *acquired;
            terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await?;
            log_tasks.finish().await
        })
    }

    fn chaos_controller(&self) -> Option<&dyn ChaosController> {
        Some(self)
    }
}

impl LocalTargetProvider {
    async fn acquire_inner(
        &self,
        manifest: &SimulationManifest,
        overrides: TargetOverrides,
        context: TargetProviderContext,
    ) -> Result<ResolvedTarget, SimError> {
        let groundhog = resolve_groundhog_binary(overrides.groundhog_binary.as_deref())?;
        let required_accounts = required_bootstrap_accounts(manifest);
        let hogd_dev_binary = overrides
            .hogd_dev_binary
            .or_else(|| manifest.target.hogd_dev_binary.clone());
        let startup_timeout_secs = overrides
            .startup_timeout_secs
            .unwrap_or(DEFAULT_LOCAL_STARTUP_TIMEOUT_SECS);
        let launch_config = LocalLaunchConfig {
            groundhog_binary: groundhog,
            hogd_dev_binary,
            hogd_protocol: overrides.hogd_protocol,
            required_accounts,
            startup_timeout_secs,
            logs_dir: context.logs_dir,
            ownership_path: context.ownership_path,
        };
        let launched = launch_local_node(&launch_config, overrides.port, 0, None).await?;
        let rpc_url = launched.startup.rpc_url.clone();
        let rpc_port = match resolved_rpc_port(&rpc_url) {
            Ok(port) => port,
            Err(error) => {
                let mut child = launched.child;
                let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
                let _ = launched.log_tasks.finish().await;
                return Err(error);
            }
        };
        let process = capture_owned_process(launched.child.id(), rpc_port).await;
        let runtime_manifest = launched.startup.runtime_manifest.clone();
        *self.state.lock().await = LocalTargetState::Acquired(Box::new(LocalAcquiredTarget {
            child: launched.child,
            launch_config,
            log_tasks: launched.log_tasks,
            rpc_url: rpc_url.clone(),
            rpc_port,
            next_launch_index: 1,
        }));
        Ok(ResolvedTarget {
            provider: TargetProviderKind::Local,
            capabilities: self.capabilities(),
            ownership: TargetOwnership::local(process),
            rpc_url,
            runtime_manifest,
        })
    }
}

impl ChaosController for LocalTargetProvider {
    fn execute<'a>(
        &'a self,
        event: &'a ChaosEventSpec,
    ) -> TargetProviderFuture<'a, Result<ChaosEventOutcome, SimError>> {
        Box::pin(async move {
            match event.kind {
                ChaosEventKind::RestartTarget => self.restart_target().await,
            }
        })
    }
}

impl LocalTargetProvider {
    async fn restart_target(&self) -> Result<ChaosEventOutcome, SimError> {
        let mut acquired = match std::mem::take(&mut *self.state.lock().await) {
            LocalTargetState::Acquired(acquired) => acquired,
            LocalTargetState::Idle | LocalTargetState::Acquiring => {
                return Err(SimError::LocalNodeRestartWithoutAcquiredTarget);
            }
        };

        let old_pid = acquired.child.id();
        if let Err(error) = terminate_child(&mut acquired.child, LOCAL_CLEANUP_GRACE).await {
            *self.state.lock().await = LocalTargetState::Acquired(acquired);
            return Err(error);
        }

        let expected_rpc_url = acquired.rpc_url.clone();
        let launch_config = acquired.launch_config.clone();
        let rpc_port = acquired.rpc_port;
        let launch_index = acquired.next_launch_index;
        acquired.log_tasks.finish().await?;
        let launched =
            match relaunch_local_node_for_restart(&launch_config, rpc_port, launch_index).await {
                Ok(launched) => launched,
                Err(error) => return Err(error),
            };
        let actual_rpc_url = launched.startup.rpc_url.clone();
        if actual_rpc_url != expected_rpc_url {
            let mut child = launched.child;
            let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
            let _ = launched.log_tasks.finish().await;
            return Err(SimError::LocalNodeRestartRpcUrlChanged {
                expected: expected_rpc_url,
                actual: actual_rpc_url,
            });
        }
        let new_pid = launched.child.id();
        let new_process = capture_owned_process(new_pid, rpc_port).await;
        if let Err(error) = write_json_pretty_atomic(
            launch_config.ownership_path.clone(),
            &TargetOwnership::local(new_process),
        )
        .await
        {
            let mut child = launched.child;
            let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
            let _ = launched.log_tasks.finish().await;
            return Err(error);
        }
        *self.state.lock().await = LocalTargetState::Acquired(Box::new(LocalAcquiredTarget {
            child: launched.child,
            launch_config,
            log_tasks: launched.log_tasks,
            rpc_url: expected_rpc_url,
            rpc_port,
            next_launch_index: launch_index + 1,
        }));

        Ok(ChaosEventOutcome {
            state: ChaosEventState::Recovered,
            detail: Some(format_restart_detail(old_pid, new_pid)),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalNodeStartup {
    rpc_url: String,
    runtime_manifest: PathBuf,
}

fn local_node_args(
    required_accounts: u32,
    hogd_dev_binary: Option<&Path>,
    hogd_protocol: Option<&str>,
    port: Option<u16>,
    startup_timeout_secs: u64,
) -> Vec<String> {
    let mut args = vec![
        "node".to_string(),
        "--dev-account-count".to_string(),
        required_accounts.to_string(),
        "--json".to_string(),
        "--startup-timeout-secs".to_string(),
        startup_timeout_secs.to_string(),
    ];
    if let Some(binary) = hogd_dev_binary {
        args.push("--hogd-dev-binary".to_string());
        args.push(binary.display().to_string());
    }
    if let Some(protocol) = hogd_protocol {
        args.push("--hogd-protocol".to_string());
        args.push(protocol.to_string());
    }
    if let Some(port) = port {
        args.push("--port".to_string());
        args.push(port.to_string());
    }
    args
}

async fn launch_local_node(
    config: &LocalLaunchConfig,
    port: Option<u16>,
    launch_index: u32,
    launch_attempt: Option<usize>,
) -> Result<LocalLaunchedTarget, SimError> {
    let args = local_node_args(
        config.required_accounts,
        config.hogd_dev_binary.as_deref(),
        config.hogd_protocol.as_deref(),
        port,
        config.startup_timeout_secs,
    );
    let mut command = Command::new(&config.groundhog_binary);
    command
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|source| SimError::LocalNodeSpawn {
        binary: config.groundhog_binary.display().to_string(),
        source,
    })?;

    let logs =
        match start_child_log_capture(&mut child, &config.logs_dir, launch_index, launch_attempt)
            .await
        {
            Ok(logs) => logs,
            Err(error) => {
                let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
                return Err(error);
            }
        };
    let stderr = Arc::clone(&logs.stderr);
    let startup = match wait_for_local_startup(
        &mut child,
        config.startup_timeout_secs,
        logs.startup_line,
        Arc::clone(&stderr),
    )
    .await
    {
        Ok(startup) => startup,
        Err(error) => {
            let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
            let _ = logs.tasks.finish().await;
            return Err(error);
        }
    };

    Ok(LocalLaunchedTarget {
        child,
        startup,
        log_tasks: logs.tasks,
    })
}

async fn relaunch_local_node_for_restart(
    config: &LocalLaunchConfig,
    port: u16,
    launch_index: u32,
) -> Result<LocalLaunchedTarget, SimError> {
    let mut last_error = None;
    for attempt in 0..LOCAL_RESTART_RELAUNCH_ATTEMPTS {
        match launch_local_node(config, Some(port), launch_index, Some(attempt)).await {
            Ok(launched) => return Ok(launched),
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < LOCAL_RESTART_RELAUNCH_ATTEMPTS {
                    sleep(LOCAL_RESTART_RELAUNCH_RETRY_DELAY).await;
                }
            }
        }
    }
    Err(last_error.expect("bug: restart relaunch attempts must be nonzero"))
}

fn resolved_rpc_port(rpc_url: &str) -> Result<u16, SimError> {
    let without_scheme = rpc_url
        .strip_prefix("http://")
        .or_else(|| rpc_url.strip_prefix("https://"))
        .ok_or_else(|| SimError::LocalNodeInvalidRpcUrl {
            rpc_url: rpc_url.into(),
        })?;
    let authority =
        without_scheme
            .split('/')
            .next()
            .ok_or_else(|| SimError::LocalNodeInvalidRpcUrl {
                rpc_url: rpc_url.into(),
            })?;
    let port = authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .ok_or_else(|| SimError::LocalNodeInvalidRpcUrl {
            rpc_url: rpc_url.into(),
        })?;
    Ok(port)
}

fn format_restart_detail(old_pid: Option<u32>, new_pid: Option<u32>) -> String {
    format!(
        "restarted local target oldPid={} newPid={}",
        old_pid.map_or_else(|| "unknown".into(), |pid| pid.to_string()),
        new_pid.map_or_else(|| "unknown".into(), |pid| pid.to_string())
    )
}

fn resolve_groundhog_binary(override_path: Option<&Path>) -> Result<PathBuf, SimError> {
    if let Some(path) = override_path {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(SimError::GroundhogBinaryNotFound {
            binary: path.display().to_string(),
        });
    }

    let binary = "groundhog";
    let Some(paths) = std::env::var_os("PATH") else {
        return Err(SimError::GroundhogBinaryNotFound {
            binary: binary.to_string(),
        });
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(SimError::GroundhogBinaryNotFound {
        binary: binary.to_string(),
    })
}

#[cfg(unix)]
async fn capture_owned_process(pid: Option<u32>, port: u16) -> Option<OwnedProcess> {
    let Some(pid) = pid else {
        eprintln!("warning: local target process id was unavailable; sweep metadata omitted");
        return None;
    };
    if !command_on_path("ps") {
        eprintln!("warning: `ps` was not found on PATH; local target sweep metadata omitted");
        return None;
    }
    let identity = match read_process_identity(pid).await {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!(
                "warning: failed to capture local target process identity for pid {pid}: {error}; sweep metadata omitted"
            );
            return None;
        }
    };
    Some(OwnedProcess {
        pid,
        pgid: identity.pgid,
        port,
        start_time: identity.start_time,
        command: "groundhog node".into(),
    })
}

#[cfg(not(unix))]
async fn capture_owned_process(_pid: Option<u32>, _port: u16) -> Option<OwnedProcess> {
    None
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedProcessIdentity {
    start_time: String,
    pgid: u32,
}

#[cfg(unix)]
async fn read_process_identity(pid: u32) -> Result<CapturedProcessIdentity, io::Error> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart=,pgid="])
        .output()
        .await?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("ps reported no process for pid {pid}"),
        ));
    }
    parse_process_identity(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(unix)]
fn parse_process_identity(value: &str) -> Result<CapturedProcessIdentity, io::Error> {
    let line = value.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    let mut start_tokens = Vec::with_capacity(5);
    for _ in 0..5 {
        let Some(token) = parts.next() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ps returned an incomplete process start time",
            ));
        };
        start_tokens.push(token);
    }
    let process_group = parts.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "ps returned an empty process group",
        )
    })?;
    let pgid = process_group.parse::<u32>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("ps returned invalid process group `{process_group}`: {error}"),
        )
    })?;
    Ok(CapturedProcessIdentity {
        start_time: start_tokens.join(" "),
        pgid,
    })
}

struct StartedChildLogCapture {
    startup_line: oneshot::Receiver<StartupLine>,
    stderr: Arc<Mutex<String>>,
    tasks: LocalLogTasks,
}

async fn start_child_log_capture(
    child: &mut Child,
    logs_dir: &Path,
    launch_index: u32,
    launch_attempt: Option<usize>,
) -> Result<StartedChildLogCapture, SimError> {
    tokio::fs::create_dir_all(logs_dir).await?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SimError::Executor("local node stdout was not piped".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SimError::Executor("local node stderr was not piped".into()))?;
    let stdout_log = tokio::fs::File::create(local_log_path(
        logs_dir,
        launch_index,
        launch_attempt,
        "stdout",
    ))
    .await?;
    let stderr_log = tokio::fs::File::create(local_log_path(
        logs_dir,
        launch_index,
        launch_attempt,
        "stderr",
    ))
    .await?;
    let stdout_capture = spawn_stdout_capture(stdout, stdout_log);
    let stderr_capture = spawn_stderr_capture(stderr, stderr_log);
    Ok(StartedChildLogCapture {
        startup_line: stdout_capture.startup_line,
        stderr: Arc::clone(&stderr_capture.collected),
        tasks: LocalLogTasks {
            stdout: stdout_capture.task,
            stderr: stderr_capture.task,
        },
    })
}

fn local_log_path(
    logs_dir: &Path,
    launch_index: u32,
    launch_attempt: Option<usize>,
    stream: &str,
) -> PathBuf {
    let attempt = launch_attempt.map_or_else(String::new, |attempt| format!(".attempt-{attempt}"));
    logs_dir.join(format!("local-node-{launch_index}{attempt}.{stream}.log"))
}

#[cfg(test)]
async fn start_stdout_capture<R>(stdout: R, log_path: PathBuf) -> Result<StdoutCapture, SimError>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let log = tokio::fs::File::create(log_path).await?;
    Ok(spawn_stdout_capture(stdout, log))
}

fn spawn_stdout_capture<R>(stdout: R, mut log: tokio::fs::File) -> StdoutCapture
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (startup_sender, startup_line) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let mut startup_sender = Some(startup_sender);
        loop {
            line.clear();
            let read = match reader.read_line(&mut line).await {
                Ok(read) => read,
                Err(error) => {
                    send_startup_line(&mut startup_sender, StartupLine::Io(error.to_string()));
                    return Err(error);
                }
            };
            if read == 0 {
                send_startup_line(&mut startup_sender, StartupLine::Eof);
                return log.flush().await;
            }
            if let Err(error) = log.write_all(line.as_bytes()).await {
                send_startup_line(&mut startup_sender, StartupLine::Io(error.to_string()));
                return Err(error);
            }
            if !line.trim().is_empty() {
                send_startup_line(&mut startup_sender, StartupLine::Line(line.clone()));
            }
        }
    });
    StdoutCapture { startup_line, task }
}

#[cfg(test)]
async fn start_stderr_capture<R>(stderr: R, log_path: PathBuf) -> Result<StderrCapture, SimError>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let log = tokio::fs::File::create(log_path).await?;
    Ok(spawn_stderr_capture(stderr, log))
}

fn spawn_stderr_capture<R>(stderr: R, mut log: tokio::fs::File) -> StderrCapture
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let collected = Arc::new(Mutex::new(String::new()));
    let task_collected = Arc::clone(&collected);
    let task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buffer = [0_u8; 1024];
        loop {
            let read = tokio::io::AsyncReadExt::read(&mut reader, &mut buffer).await?;
            if read == 0 {
                return log.flush().await;
            }
            log.write_all(&buffer[..read]).await?;
            let mut output = task_collected.lock().await;
            // This in-memory snippet is only startup-error context. The durable
            // stderr artifact above preserves the raw bytes without lossy
            // UTF-8 chunk conversion.
            append_bounded_stderr(&mut output, &buffer[..read]);
        }
    });
    StderrCapture { collected, task }
}

fn send_startup_line(sender: &mut Option<oneshot::Sender<StartupLine>>, message: StartupLine) {
    if let Some(sender) = sender.take() {
        let _ = sender.send(message);
    }
}

fn append_bounded_stderr(output: &mut String, bytes: &[u8]) {
    if output.len() >= LOCAL_STARTUP_STDERR_CAPTURE_LIMIT_BYTES {
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    let remaining = LOCAL_STARTUP_STDERR_CAPTURE_LIMIT_BYTES - output.len();
    if text.len() <= remaining {
        output.push_str(&text);
        return;
    }
    let mut used = 0;
    for ch in text.chars() {
        let len = ch.len_utf8();
        if used + len > remaining {
            break;
        }
        output.push(ch);
        used += len;
    }
}

impl LocalLogTasks {
    async fn finish(self) -> Result<(), SimError> {
        let stdout_result = finish_log_task(self.stdout).await;
        let stderr_result = finish_log_task(self.stderr).await;
        stdout_result.and(stderr_result)
    }
}

async fn finish_log_task(task: JoinHandle<io::Result<()>>) -> Result<(), SimError> {
    task.await.map_err(|error| {
        SimError::Executor(format!("local log capture task panicked: {error}"))
    })??;
    Ok(())
}

async fn wait_for_local_startup(
    child: &mut Child,
    timeout_secs: u64,
    startup_line: oneshot::Receiver<StartupLine>,
    stderr: Arc<Mutex<String>>,
) -> Result<LocalNodeStartup, SimError> {
    match timeout(
        Duration::from_secs(timeout_secs),
        read_local_startup(child, startup_line, Arc::clone(&stderr)),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(SimError::LocalNodeStartupTimeout {
            timeout_secs,
            stderr: stderr.lock().await.clone(),
        }),
    }
}

async fn read_local_startup(
    child: &mut Child,
    startup_line: oneshot::Receiver<StartupLine>,
    stderr: Arc<Mutex<String>>,
) -> Result<LocalNodeStartup, SimError> {
    let mut startup_line = startup_line;
    tokio::select! {
        biased;
        line = &mut startup_line => {
            match line {
                Ok(StartupLine::Line(line)) => parse_local_startup_json(&line),
                Ok(StartupLine::Eof) | Err(_) => {
                    let status = child.wait().await?;
                    Err(SimError::LocalNodeStartupExit {
                        status: status.to_string(),
                        stderr: stderr.lock().await.clone(),
                    })
                }
                Ok(StartupLine::Io(error)) => Err(io::Error::other(error).into()),
            }
        }
        status = child.wait() => {
            let status = status?;
            if let Ok(Ok(StartupLine::Line(line))) =
                timeout(LOCAL_STARTUP_EXIT_STDOUT_GRACE, startup_line).await
            {
                return parse_local_startup_json(&line);
            }
            Err(SimError::LocalNodeStartupExit {
                status: status.to_string(),
                stderr: stderr.lock().await.clone(),
            })
        }
    }
}

fn parse_local_startup_json(line: &str) -> Result<LocalNodeStartup, SimError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|source| SimError::LocalNodeStartupJson { source })?;
    let rpc_url = value
        .pointer("/data/rpc_url")
        .and_then(serde_json::Value::as_str)
        .ok_or(SimError::LocalNodeStartupJsonMissing {
            field: "data.rpc_url",
        })?
        .to_string();
    let runtime_manifest = value
        .pointer("/data/runtime_manifest/path")
        .and_then(serde_json::Value::as_str)
        .ok_or(SimError::LocalNodeStartupJsonMissing {
            field: "data.runtime_manifest.path",
        })?;
    Ok(LocalNodeStartup {
        rpc_url,
        runtime_manifest: PathBuf::from(runtime_manifest),
    })
}

#[cfg(unix)]
async fn terminate_child(child: &mut Child, grace: Duration) -> Result<(), SimError> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    if let Some(pid) = child.id() {
        signal_process_group(pid, libc::SIGTERM)?;
    }
    let deadline = Instant::now() + grace;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    child.start_kill()?;
    if let Some(pid) = child.id() {
        let _ = signal_process_group(pid, libc::SIGKILL);
    }
    child.wait().await?;
    Ok(())
}

fn signal_process_group(pid: u32, signal: libc::c_int) -> Result<(), SimError> {
    let target = -pid.cast_signed();
    let rc = unsafe { libc::kill(target, signal) };
    if rc == 0 {
        Ok(())
    } else {
        let source = std::io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(SimError::LocalNodeSignal { pid, source })
        }
    }
}

#[cfg(not(unix))]
async fn terminate_child(child: &mut Child, _grace: Duration) -> Result<(), SimError> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.start_kill()?;
    child.wait().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    use super::*;

    #[tokio::test]
    async fn existing_provider_resolves_manifest_target() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545",
            "runtimeManifest": "/tmp/runtime-manifest.json"
        }));

        let target = ExistingTargetProvider
            .acquire(&manifest, TargetOverrides::default(), context())
            .await
            .expect("existing target should resolve");

        assert_eq!(target.provider, TargetProviderKind::Existing);
        assert_eq!(target.rpc_url, "http://127.0.0.1:9545");
        assert_eq!(
            target.runtime_manifest,
            PathBuf::from("/tmp/runtime-manifest.json")
        );
        assert_eq!(target.ownership, TargetOwnership::existing());
        assert!(target.capabilities.chaos.is_empty());
    }

    #[tokio::test]
    async fn existing_provider_is_dyn_compatible() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545",
            "runtimeManifest": "/tmp/runtime-manifest.json"
        }));
        let provider: &dyn TargetProvider = &ExistingTargetProvider;

        let target = provider
            .acquire(&manifest, TargetOverrides::default(), context())
            .await
            .expect("existing target should resolve through a trait object");

        assert_eq!(target.provider, TargetProviderKind::Existing);
        assert!(provider.chaos_controller().is_none());
    }

    #[test]
    fn local_provider_advertises_restart_target() {
        let provider = LocalTargetProvider::new();
        let capabilities = TargetCapabilities::for_provider(TargetProviderKind::Local);

        assert!(capabilities.supports_chaos(ChaosEventKind::RestartTarget));
        assert!(provider.chaos_controller().is_some());
        assert!(
            TargetCapabilities::for_provider(TargetProviderKind::Existing)
                .chaos
                .is_empty()
        );
        assert!(
            TargetCapabilities::for_provider(TargetProviderKind::RemoteSsh)
                .chaos
                .is_empty()
        );
    }

    #[test]
    fn ownership_process_is_additive_and_omitted_when_absent() {
        let process = OwnedProcess {
            pid: 12,
            pgid: 12,
            port: 9545,
            start_time: "Wed May 27 12:00:00 2026".into(),
            command: "groundhog node".into(),
        };
        let ownership = TargetOwnership::local(Some(process.clone()));
        let value = serde_json::to_value(&ownership).expect("ownership should serialize");

        assert_eq!(value["process"]["pid"], 12);
        assert_eq!(value["process"]["pgid"], 12);
        assert_eq!(value["process"]["port"], 9545);
        assert_eq!(value["process"]["startTime"], process.start_time);
        assert_eq!(value["process"]["command"], process.command);

        let legacy: TargetOwnership = serde_json::from_value(json!({
            "provider": "existing",
            "startedBySim": false,
            "cleanupAllowed": false
        }))
        .expect("legacy ownership should deserialize");
        assert_eq!(legacy, TargetOwnership::existing());
        let without_process =
            serde_json::to_value(TargetOwnership::local(None)).expect("ownership should encode");
        assert!(without_process.get("process").is_none());
    }

    #[test]
    fn owned_process_round_trips() {
        let process = OwnedProcess {
            pid: 12,
            pgid: 12,
            port: 9545,
            start_time: "Wed May 27 12:00:00 2026".into(),
            command: "groundhog node".into(),
        };

        let encoded = serde_json::to_vec(&process).expect("process should serialize");
        let decoded: OwnedProcess =
            serde_json::from_slice(&encoded).expect("process should deserialize");

        assert_eq!(decoded, process);
    }

    #[cfg(unix)]
    #[test]
    fn parses_process_identity_with_lstart_and_pgid() {
        let identity = parse_process_identity("Wed May  7 12:00:00 2026  5678\n")
            .expect("process identity should parse");

        assert_eq!(identity.start_time, "Wed May 7 12:00:00 2026");
        assert_eq!(identity.pgid, 5678);
    }

    #[tokio::test]
    async fn existing_provider_prefers_cli_overrides() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545",
            "runtimeManifest": "/tmp/manifest-a.json"
        }));

        let target = ExistingTargetProvider
            .acquire(
                &manifest,
                TargetOverrides {
                    rpc_url: Some("http://127.0.0.1:9546".into()),
                    runtime_manifest: Some(PathBuf::from("/tmp/manifest-b.json")),
                    ..TargetOverrides::default()
                },
                context(),
            )
            .await
            .expect("existing target should resolve");

        assert_eq!(target.rpc_url, "http://127.0.0.1:9546");
        assert_eq!(
            target.runtime_manifest,
            PathBuf::from("/tmp/manifest-b.json")
        );
    }

    #[tokio::test]
    async fn existing_provider_rejects_local_launch_overrides() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545",
            "runtimeManifest": "/tmp/manifest-a.json"
        }));

        let error = ExistingTargetProvider
            .acquire(
                &manifest,
                TargetOverrides {
                    hogd_protocol: Some("hogp/test".into()),
                    ..TargetOverrides::default()
                },
                context(),
            )
            .await
            .expect_err("existing target should reject local launch flags");

        assert!(matches!(
            error,
            SimError::Validation(ValidationError::ExistingRejectsLocalLaunchOverride {
                flag: "--hogd-protocol"
            })
        ));
    }

    #[tokio::test]
    async fn existing_provider_rejects_non_existing_manifest_provider() {
        let manifest = manifest(&json!({
            "provider": "local",
            "hogdDevBinary": "/tmp/hogd-dev"
        }));

        let error = ExistingTargetProvider
            .acquire(&manifest, TargetOverrides::default(), context())
            .await
            .expect_err("existing provider should reject local manifests");

        assert!(matches!(
            error,
            SimError::Validation(ValidationError::OnlyExistingProviderImplemented {
                provider: TargetProviderKind::Local
            })
        ));
    }

    #[tokio::test]
    async fn existing_provider_rejects_missing_runtime_manifest() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545"
        }));

        let error = ExistingTargetProvider
            .acquire(&manifest, TargetOverrides::default(), context())
            .await
            .expect_err("existing provider should require a runtime manifest");

        assert!(matches!(
            error,
            SimError::Validation(ValidationError::MissingRuntimeManifest)
        ));
    }

    #[tokio::test]
    async fn local_provider_rejects_output_overrides_before_launch() {
        let manifest = manifest(&json!({
            "provider": "local"
        }));

        let error = LocalTargetProvider::new()
            .acquire(
                &manifest,
                TargetOverrides {
                    rpc_url: Some("http://127.0.0.1:9545".into()),
                    ..TargetOverrides::default()
                },
                context(),
            )
            .await
            .expect_err("local target should reject rpc override");

        assert!(matches!(
            error,
            SimError::Validation(ValidationError::LocalRejectsRpcUrl)
        ));
    }

    #[tokio::test]
    async fn local_provider_reports_missing_groundhog_override() {
        let manifest = manifest(&json!({
            "provider": "local"
        }));
        let missing = PathBuf::from("/tmp/groundhog-sim-missing-groundhog-binary");

        let error = LocalTargetProvider::new()
            .acquire(
                &manifest,
                TargetOverrides {
                    groundhog_binary: Some(missing.clone()),
                    ..TargetOverrides::default()
                },
                context(),
            )
            .await
            .expect_err("local target should require a real groundhog binary");

        assert!(matches!(
            error,
            SimError::GroundhogBinaryNotFound { binary } if binary == missing.display().to_string()
        ));
    }

    #[test]
    fn local_node_args_include_required_account_count() {
        let args = local_node_args(
            5,
            Some(std::path::Path::new("/tmp/hogd-dev")),
            Some("hogp/test"),
            Some(0),
            45,
        );

        assert_eq!(
            args,
            vec![
                "node",
                "--dev-account-count",
                "5",
                "--json",
                "--startup-timeout-secs",
                "45",
                "--hogd-dev-binary",
                "/tmp/hogd-dev",
                "--hogd-protocol",
                "hogp/test",
                "--port",
                "0"
            ]
        );
    }

    #[test]
    fn resolved_rpc_port_extracts_startup_port() {
        assert_eq!(
            resolved_rpc_port("http://127.0.0.1:49152").expect("port should parse"),
            49152
        );
    }

    #[test]
    fn restart_args_use_resolved_port() {
        let args = local_node_args(5, None, None, Some(49152), 30);

        assert_eq!(
            args,
            vec![
                "node",
                "--dev-account-count",
                "5",
                "--json",
                "--startup-timeout-secs",
                "30",
                "--port",
                "49152"
            ]
        );
    }

    #[test]
    fn local_log_paths_are_deterministic_by_launch_index_and_attempt() {
        let logs_dir = PathBuf::from("/tmp/sim-run/logs");

        assert_eq!(
            local_log_path(&logs_dir, 0, None, "stdout"),
            PathBuf::from("/tmp/sim-run/logs/local-node-0.stdout.log")
        );
        assert_eq!(
            local_log_path(&logs_dir, 1, Some(0), "stderr"),
            PathBuf::from("/tmp/sim-run/logs/local-node-1.attempt-0.stderr.log")
        );
        assert_eq!(
            local_log_path(&logs_dir, 1, None, "stderr"),
            PathBuf::from("/tmp/sim-run/logs/local-node-1.stderr.log")
        );
    }

    #[tokio::test]
    async fn stdout_capture_preserves_startup_json_while_parsing_it() {
        let dir = tempfile::tempdir().expect("temp dir should be created");
        let log_path = dir.path().join("local-node-0.stdout.log");
        let (mut writer, reader) = tokio::io::duplex(1024);
        let capture = start_stdout_capture(reader, log_path.clone())
            .await
            .expect("stdout capture should start");
        let startup = r#"{"ok":true,"data":{"rpc_url":"http://127.0.0.1:9545","runtime_manifest":{"path":"/tmp/runtime-manifest.json"}}}"#;

        writer
            .write_all(b"\n")
            .await
            .expect("blank line should write");
        writer
            .write_all(startup.as_bytes())
            .await
            .expect("startup line should write");
        writer
            .write_all(b"\nafter-startup\n")
            .await
            .expect("post-startup line should write");
        drop(writer);

        match capture
            .startup_line
            .await
            .expect("startup line should send")
        {
            StartupLine::Line(line) => assert_eq!(line.trim_end(), startup),
            other => panic!("expected startup line, got {other:?}"),
        }
        capture
            .task
            .await
            .expect("stdout task should join")
            .expect("stdout task should succeed");
        let log = tokio::fs::read_to_string(log_path)
            .await
            .expect("stdout log should read");
        assert!(log.contains(startup));
        assert!(log.contains("after-startup"));
    }

    #[tokio::test]
    async fn stderr_capture_writes_log_and_keeps_startup_error_context() {
        let dir = tempfile::tempdir().expect("temp dir should be created");
        let log_path = dir.path().join("local-node-0.stderr.log");
        let (mut writer, reader) = tokio::io::duplex(1024);
        let capture = start_stderr_capture(reader, log_path.clone())
            .await
            .expect("stderr capture should start");

        writer
            .write_all(b"startup failed\n")
            .await
            .expect("stderr should write");
        drop(writer);
        capture
            .task
            .await
            .expect("stderr task should join")
            .expect("stderr task should succeed");

        assert_eq!(&*capture.collected.lock().await, "startup failed\n");
        let log = tokio::fs::read_to_string(log_path)
            .await
            .expect("stderr log should read");
        assert_eq!(log, "startup failed\n");
    }

    #[test]
    fn bounded_stderr_capture_respects_exact_byte_boundary() {
        let mut output = "x".repeat(LOCAL_STARTUP_STDERR_CAPTURE_LIMIT_BYTES - 1);

        append_bounded_stderr(&mut output, b"y");
        append_bounded_stderr(&mut output, b"z");

        assert_eq!(output.len(), LOCAL_STARTUP_STDERR_CAPTURE_LIMIT_BYTES);
        assert!(output.ends_with('y'));
        assert!(!output.ends_with('z'));
    }

    #[test]
    fn bounded_stderr_capture_does_not_split_multibyte_utf8() {
        let mut output = "x".repeat(LOCAL_STARTUP_STDERR_CAPTURE_LIMIT_BYTES - 1);

        append_bounded_stderr(&mut output, "é".as_bytes());

        assert_eq!(output.len(), LOCAL_STARTUP_STDERR_CAPTURE_LIMIT_BYTES - 1);
        assert!(!output.contains('é'));
    }

    #[test]
    fn parse_local_startup_json_extracts_rpc_and_runtime_manifest() {
        let startup = parse_local_startup_json(
            r#"{"ok":true,"command":"node","warnings":[],"data":{"rpc_url":"http://127.0.0.1:9545","runtime_manifest":{"path":"/tmp/runtime-manifest.json"}}}"#,
        )
        .expect("startup json should parse");

        assert_eq!(startup.rpc_url, "http://127.0.0.1:9545");
        assert_eq!(
            startup.runtime_manifest,
            PathBuf::from("/tmp/runtime-manifest.json")
        );
    }

    #[test]
    fn parse_local_startup_json_rejects_missing_fields() {
        let error = parse_local_startup_json(r#"{"ok":true,"data":{}}"#)
            .expect_err("missing rpc url should fail");

        assert!(matches!(
            error,
            SimError::LocalNodeStartupJsonMissing {
                field: "data.rpc_url"
            }
        ));
    }

    #[test]
    fn parse_local_startup_json_rejects_missing_runtime_manifest_path() {
        let error = parse_local_startup_json(
            r#"{"ok":true,"data":{"rpc_url":"http://127.0.0.1:9545","runtime_manifest":{}}}"#,
        )
        .expect_err("missing runtime manifest path should fail");

        assert!(matches!(
            error,
            SimError::LocalNodeStartupJsonMissing {
                field: "data.runtime_manifest.path"
            }
        ));
    }

    #[test]
    fn parse_local_startup_json_rejects_malformed_json() {
        let error =
            parse_local_startup_json("{not-json").expect_err("malformed startup json should fail");

        assert!(matches!(error, SimError::LocalNodeStartupJson { .. }));
    }

    #[tokio::test]
    async fn existing_provider_cleanup_is_noop() {
        let target = ResolvedTarget {
            provider: TargetProviderKind::Existing,
            capabilities: TargetCapabilities::for_provider(TargetProviderKind::Existing),
            ownership: TargetOwnership::existing(),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest: PathBuf::from("/tmp/runtime-manifest.json"),
        };

        ExistingTargetProvider
            .cleanup(&target)
            .await
            .expect("existing cleanup should be a no-op");
    }

    #[tokio::test]
    async fn local_provider_cleanup_is_idempotent_without_child() {
        let provider = LocalTargetProvider::new();
        let target = ResolvedTarget {
            provider: TargetProviderKind::Local,
            capabilities: TargetCapabilities::for_provider(TargetProviderKind::Local),
            ownership: TargetOwnership::local(None),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest: PathBuf::from("/tmp/runtime-manifest.json"),
        };

        provider
            .cleanup(&target)
            .await
            .expect("first cleanup should be a no-op");
        provider
            .cleanup(&target)
            .await
            .expect("second cleanup should be a no-op");
    }

    #[tokio::test]
    async fn local_restart_rejects_missing_acquired_child() {
        let provider = LocalTargetProvider::new();
        let event = ChaosEventSpec {
            at: crate::manifest::DurationSpec("1s".into()),
            kind: ChaosEventKind::RestartTarget,
        };

        let error = provider
            .execute(&event)
            .await
            .expect_err("restart should require acquired child");

        assert!(matches!(
            error,
            SimError::LocalNodeRestartWithoutAcquiredTarget
        ));
    }

    #[tokio::test]
    async fn local_restart_rejects_acquiring_state() {
        let provider = LocalTargetProvider::new();
        *provider.state.lock().await = LocalTargetState::Acquiring;
        let event = ChaosEventSpec {
            at: crate::manifest::DurationSpec("1s".into()),
            kind: ChaosEventKind::RestartTarget,
        };

        let error = provider
            .execute(&event)
            .await
            .expect_err("restart should require acquired child");

        assert!(matches!(
            error,
            SimError::LocalNodeRestartWithoutAcquiredTarget
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn local_restart_relaunches_on_resolved_port() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir should be created");
        let args_log = dir.path().join("args.log");
        let runtime_manifest = dir.path().join("runtime-manifest.json");
        let script = dir.path().join("groundhog");
        let script_body = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{}"
port=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--port" ]; then
    port="$arg"
  fi
  prev="$arg"
done
if [ -z "$port" ] || [ "$port" = "0" ]; then
  port="49152"
fi
printf '{{"ok":true,"data":{{"rpc_url":"http://127.0.0.1:%s","runtime_manifest":{{"path":"{}"}}}}}}\n' "$port"
trap 'exit 0' TERM INT
while :; do sleep 1; done
"#,
            args_log.display(),
            runtime_manifest.display(),
        );
        std::fs::write(&script, script_body).expect("fake groundhog should write");
        let mut permissions = std::fs::metadata(&script)
            .expect("fake groundhog metadata should read")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions)
            .expect("fake groundhog should be executable");

        let provider = LocalTargetProvider::new();
        let manifest = manifest(&json!({"provider": "local"}));
        let target = provider
            .acquire(
                &manifest,
                TargetOverrides {
                    groundhog_binary: Some(script),
                    port: Some(0),
                    startup_timeout_secs: Some(5),
                    ..TargetOverrides::default()
                },
                TargetProviderContext {
                    logs_dir: dir.path().join("logs"),
                    ownership_path: dir.path().join("target-ownership.json"),
                },
            )
            .await
            .expect("fake local target should acquire");
        assert_eq!(target.rpc_url, "http://127.0.0.1:49152");
        let initial_process = target
            .ownership
            .process
            .as_ref()
            .expect("local acquire should capture process metadata");

        let event = ChaosEventSpec {
            at: crate::manifest::DurationSpec("1s".into()),
            kind: ChaosEventKind::RestartTarget,
        };
        let outcome = provider
            .execute(&event)
            .await
            .expect("fake restart should succeed");
        assert_eq!(outcome.state, ChaosEventState::Recovered);
        let detail = outcome.detail.expect("restart detail should report pids");
        assert_restart_detail_has_distinct_pids(&detail);
        let new_pid = restart_detail_pid(&detail, "newPid");
        let refreshed: TargetOwnership = serde_json::from_slice(
            &std::fs::read(dir.path().join("target-ownership.json"))
                .expect("refreshed ownership should read"),
        )
        .expect("refreshed ownership should parse");
        let refreshed_process = refreshed
            .process
            .expect("restart should refresh ownership process metadata");
        assert_eq!(refreshed_process.pid.to_string(), new_pid);
        assert_ne!(refreshed_process.pid, initial_process.pid);
        assert_eq!(refreshed_process.port, 49152);

        provider
            .cleanup(&target)
            .await
            .expect("fake target should clean up");

        let calls = std::fs::read_to_string(args_log).expect("args log should read");
        let lines: Vec<_> = calls.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("--port 0"));
        assert!(lines[1].contains("--port 49152"));
        assert!(dir.path().join("logs/local-node-0.stdout.log").exists());
        assert!(dir.path().join("logs/local-node-0.stderr.log").exists());
        assert!(
            dir.path()
                .join("logs/local-node-1.attempt-0.stdout.log")
                .exists()
        );
        assert!(
            dir.path()
                .join("logs/local-node-1.attempt-0.stderr.log")
                .exists()
        );
    }

    #[tokio::test]
    async fn local_provider_rejects_acquire_while_acquiring() {
        let provider = LocalTargetProvider::new();
        *provider.state.lock().await = LocalTargetState::Acquiring;
        let manifest = manifest(&json!({
            "provider": "local"
        }));

        let error = provider
            .acquire(&manifest, TargetOverrides::default(), context())
            .await
            .expect_err("provider should reject concurrent acquire");

        assert!(matches!(error, SimError::LocalNodeAlreadyAcquired));
    }

    fn manifest(target: &serde_json::Value) -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "target-test",
            "seed": "seed",
            "target": target,
            "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}]
        }))
        .expect("manifest should parse")
    }

    fn context() -> TargetProviderContext {
        TargetProviderContext {
            logs_dir: PathBuf::from("/tmp/groundhog-sim-target-test-logs"),
            ownership_path: PathBuf::from("/tmp/groundhog-sim-target-test-ownership.json"),
        }
    }

    #[cfg(unix)]
    fn assert_restart_detail_has_distinct_pids(detail: &str) {
        let old_pid = restart_detail_pid(detail, "oldPid");
        let new_pid = restart_detail_pid(detail, "newPid");

        assert_ne!(old_pid, "unknown");
        assert_ne!(new_pid, "unknown");
        assert_ne!(old_pid, new_pid);
    }

    #[cfg(unix)]
    fn restart_detail_pid<'a>(detail: &'a str, key: &str) -> &'a str {
        let prefix = format!("{key}=");
        detail
            .split_whitespace()
            .find_map(|part| part.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("{key} should be present"))
    }
}
