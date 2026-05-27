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
            TargetProviderKind::Existing
            | TargetProviderKind::Local
            | TargetProviderKind::RemoteSsh => Vec::new(),
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
    Acquired(Child),
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
            let child = match std::mem::take(&mut *self.state.lock().await) {
                LocalTargetState::Acquired(child) => child,
                LocalTargetState::Idle | LocalTargetState::Acquiring => return Ok(()),
            };
            let mut child = child;
            terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await
        })
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
        let args = local_node_args(
            required_accounts,
            overrides
                .hogd_dev_binary
                .as_deref()
                .or(manifest.target.hogd_dev_binary.as_deref()),
            overrides.port,
        );
        let mut child = Command::new(&groundhog)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| SimError::LocalNodeSpawn {
                binary: groundhog.display().to_string(),
                source,
            })?;

        let stderr = collect_child_stderr(&mut child);
        let timeout_secs = overrides
            .startup_timeout_secs
            .unwrap_or(DEFAULT_LOCAL_STARTUP_TIMEOUT_SECS);
        let startup = match wait_for_local_startup(&mut child, timeout_secs, stderr.clone()).await {
            Ok(startup) => startup,
            Err(error) => {
                let _ = terminate_child(&mut child, LOCAL_CLEANUP_GRACE).await;
                return Err(error);
            }
        };
        *self.state.lock().await = LocalTargetState::Acquired(child);
        Ok(ResolvedTarget {
            provider: TargetProviderKind::Local,
            capabilities: self.capabilities(),
            ownership: TargetOwnership::local(),
            rpc_url: startup.rpc_url,
            runtime_manifest: startup.runtime_manifest,
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
        signal_process(pid, libc::SIGTERM)?;
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
    child.wait().await?;
    Ok(())
}

fn signal_process(pid: u32, signal: libc::c_int) -> Result<(), SimError> {
    let rc = unsafe { libc::kill(pid.cast_signed(), signal) };
    if rc == 0 {
        Ok(())
    } else {
        Err(SimError::LocalNodeSignal {
            pid,
            source: std::io::Error::last_os_error(),
        })
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
}
