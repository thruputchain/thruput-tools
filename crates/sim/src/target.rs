use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep, timeout};

use crate::error::SimError;
use crate::manifest::{ChaosEventKind, ChaosEventSpec, SimulationManifest};
use crate::result::ChaosEventState;
use crate::validation::{
    ValidationError, required_bootstrap_accounts, validate_existing_target_flags,
    validate_local_target_flags,
};

const DEFAULT_LOCAL_STARTUP_TIMEOUT_SECS: u64 = 30;
const LOCAL_CLEANUP_GRACE: Duration = Duration::from_secs(10);
const LOCAL_RESTART_RELAUNCH_ATTEMPTS: usize = 3;
const LOCAL_RESTART_RELAUNCH_RETRY_DELAY: Duration = Duration::from_millis(250);

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
}

impl TargetOwnership {
    /// Return ownership metadata for an externally managed target.
    pub fn existing() -> Self {
        Self {
            provider: TargetProviderKind::Existing,
            started_by_sim: false,
            cleanup_allowed: false,
        }
    }

    /// Return ownership metadata for a simulator-owned local target.
    pub fn local() -> Self {
        Self {
            provider: TargetProviderKind::Local,
            started_by_sim: true,
            cleanup_allowed: true,
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
    /// Public JSON-RPC port requested from the `local` provider.
    pub port: Option<u16>,
    /// Startup timeout in seconds for the `local` provider.
    pub startup_timeout_secs: Option<u64>,
}

impl TargetOverrides {
    fn first_local_launch_override(&self) -> Option<&'static str> {
        if self.groundhog_binary.is_some() {
            Some("--groundhog-binary")
        } else if self.hogd_dev_binary.is_some() {
            Some("--hogd-dev-binary")
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
    rpc_url: String,
    rpc_port: u16,
    _runtime_manifest: PathBuf,
}

#[derive(Debug, Clone)]
struct LocalLaunchConfig {
    groundhog_binary: PathBuf,
    hogd_dev_binary: Option<PathBuf>,
    required_accounts: u32,
    startup_timeout_secs: u64,
}

#[derive(Debug)]
struct LocalLaunchedTarget {
    child: Child,
    startup: LocalNodeStartup,
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
            let acquire_result = self.acquire_inner(manifest, overrides).await;
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
            let mut child = acquired.child;
            terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await
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
            required_accounts,
            startup_timeout_secs,
        };
        let launched = launch_local_node(&launch_config, overrides.port).await?;
        let rpc_url = launched.startup.rpc_url.clone();
        let rpc_port = match resolved_rpc_port(&rpc_url) {
            Ok(port) => port,
            Err(error) => {
                let mut child = launched.child;
                let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
                return Err(error);
            }
        };
        let runtime_manifest = launched.startup.runtime_manifest.clone();
        *self.state.lock().await = LocalTargetState::Acquired(Box::new(LocalAcquiredTarget {
            child: launched.child,
            launch_config,
            rpc_url: rpc_url.clone(),
            rpc_port,
            _runtime_manifest: runtime_manifest.clone(),
        }));
        Ok(ResolvedTarget {
            provider: TargetProviderKind::Local,
            capabilities: self.capabilities(),
            ownership: TargetOwnership::local(),
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
        let launched = match relaunch_local_node_for_restart(&launch_config, rpc_port).await {
            Ok(launched) => launched,
            Err(error) => return Err(error),
        };
        let actual_rpc_url = launched.startup.rpc_url.clone();
        if actual_rpc_url != expected_rpc_url {
            let mut child = launched.child;
            let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
            return Err(SimError::LocalNodeRestartRpcUrlChanged {
                expected: expected_rpc_url,
                actual: actual_rpc_url,
            });
        }
        let runtime_manifest = launched.startup.runtime_manifest.clone();
        let new_pid = launched.child.id();
        *self.state.lock().await = LocalTargetState::Acquired(Box::new(LocalAcquiredTarget {
            child: launched.child,
            launch_config,
            rpc_url: expected_rpc_url,
            rpc_port,
            _runtime_manifest: runtime_manifest,
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
    port: Option<u16>,
) -> Vec<String> {
    let mut args = vec![
        "node".to_string(),
        "--dev-account-count".to_string(),
        required_accounts.to_string(),
        "--json".to_string(),
    ];
    if let Some(binary) = hogd_dev_binary {
        args.push("--hogd-dev-binary".to_string());
        args.push(binary.display().to_string());
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
) -> Result<LocalLaunchedTarget, SimError> {
    let args = local_node_args(
        config.required_accounts,
        config.hogd_dev_binary.as_deref(),
        port,
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

    let stderr = collect_child_stderr(&mut child);
    let startup =
        match wait_for_local_startup(&mut child, config.startup_timeout_secs, stderr.clone()).await
        {
            Ok(startup) => startup,
            Err(error) => {
                let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
                return Err(error);
            }
        };

    Ok(LocalLaunchedTarget { child, startup })
}

async fn relaunch_local_node_for_restart(
    config: &LocalLaunchConfig,
    port: u16,
) -> Result<LocalLaunchedTarget, SimError> {
    let mut last_error = None;
    for attempt in 0..LOCAL_RESTART_RELAUNCH_ATTEMPTS {
        match launch_local_node(config, Some(port)).await {
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

fn collect_child_stderr(child: &mut Child) -> Arc<Mutex<String>> {
    let collected = Arc::new(Mutex::new(String::new()));
    let Some(stderr) = child.stderr.take() else {
        return collected;
    };
    let output = Arc::clone(&collected);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buffer = [0_u8; 1024];
        loop {
            let Ok(read) = tokio::io::AsyncReadExt::read(&mut reader, &mut buffer).await else {
                return;
            };
            if read == 0 {
                return;
            }
            let mut output = output.lock().await;
            output.push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    });
    collected
}

async fn wait_for_local_startup(
    child: &mut Child,
    timeout_secs: u64,
    stderr: Arc<Mutex<String>>,
) -> Result<LocalNodeStartup, SimError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SimError::Executor("local node stdout was not piped".into()))?;
    match timeout(
        Duration::from_secs(timeout_secs),
        read_local_startup(child, stdout, Arc::clone(&stderr)),
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

async fn read_local_startup<R>(
    child: &mut Child,
    stdout: R,
    stderr: Arc<Mutex<String>>,
) -> Result<LocalNodeStartup, SimError>
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line? {
                    Some(line) if line.trim().is_empty() => {}
                    Some(line) => return parse_local_startup_json(&line),
                    None => {
                        let status = child.wait().await?;
                        return Err(SimError::LocalNodeStartupExit {
                            status: status.to_string(),
                            stderr: stderr.lock().await.clone(),
                        });
                    }
                }
            }
            status = child.wait() => {
                return Err(SimError::LocalNodeStartupExit {
                    status: status?.to_string(),
                    stderr: stderr.lock().await.clone(),
                });
            }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn existing_provider_resolves_manifest_target() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545",
            "runtimeManifest": "/tmp/runtime-manifest.json"
        }));

        let target = ExistingTargetProvider
            .acquire(&manifest, TargetOverrides::default())
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
            .acquire(&manifest, TargetOverrides::default())
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
                    port: Some(0),
                    ..TargetOverrides::default()
                },
            )
            .await
            .expect_err("existing target should reject local launch flags");

        assert!(matches!(
            error,
            SimError::Validation(ValidationError::ExistingRejectsLocalLaunchOverride {
                flag: "--port"
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
            .acquire(&manifest, TargetOverrides::default())
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
            .acquire(&manifest, TargetOverrides::default())
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
        let args = local_node_args(5, Some(std::path::Path::new("/tmp/hogd-dev")), Some(0));

        assert_eq!(
            args,
            vec![
                "node",
                "--dev-account-count",
                "5",
                "--json",
                "--hogd-dev-binary",
                "/tmp/hogd-dev",
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
        let args = local_node_args(5, None, Some(49152));

        assert_eq!(
            args,
            vec![
                "node",
                "--dev-account-count",
                "5",
                "--json",
                "--port",
                "49152"
            ]
        );
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
            ownership: TargetOwnership::local(),
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
            )
            .await
            .expect("fake local target should acquire");
        assert_eq!(target.rpc_url, "http://127.0.0.1:49152");

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

        provider
            .cleanup(&target)
            .await
            .expect("fake target should clean up");

        let calls = std::fs::read_to_string(args_log).expect("args log should read");
        let lines: Vec<_> = calls.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("--port 0"));
        assert!(lines[1].contains("--port 49152"));
    }

    #[tokio::test]
    async fn local_provider_rejects_acquire_while_acquiring() {
        let provider = LocalTargetProvider::new();
        *provider.state.lock().await = LocalTargetState::Acquiring;
        let manifest = manifest(&json!({
            "provider": "local"
        }));

        let error = provider
            .acquire(&manifest, TargetOverrides::default())
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

    #[cfg(unix)]
    fn assert_restart_detail_has_distinct_pids(detail: &str) {
        let old_pid = detail
            .split_whitespace()
            .find_map(|part| part.strip_prefix("oldPid="))
            .expect("old pid should be present");
        let new_pid = detail
            .split_whitespace()
            .find_map(|part| part.strip_prefix("newPid="))
            .expect("new pid should be present");

        assert_ne!(old_pid, "unknown");
        assert_ne!(new_pid, "unknown");
        assert_ne!(old_pid, new_pid);
    }
}
