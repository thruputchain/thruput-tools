//! `groundhog node` — start the hog-devnode local node.

use crate::{CliError, Print};

use crate::result::CommandResult;
use groundhog_node_supervisor::ConfigParamName;
use serde_json::json;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

const HOG_DEVNODE_PID_FILE: &str = "hog-devnode.pid";
const LEGACY_HOGD_DEV_PID_FILE: &str = "hogd-dev.pid";
const NODE_STARTUP_SCHEMA_VERSION: u32 = 3;
pub(crate) const GENESIS_ROOT_ARTIFACT_FILENAME: &str = "genesis_root.rv32a";

#[cfg(test)]
fn test_local_runtime_state_result() -> PathBuf {
    PathBuf::from("/tmp/groundhog-config.toml")
}

/// Block production mode for the supervised local node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalNodeBlockDriverMode {
    /// Submit-plane successes wake block production immediately.
    #[default]
    Instant,
    /// A fixed timer drives block production.
    Interval,
}

/// Start the hog-devnode local node server with reusable local-node options.
pub fn run_with_options(
    options: LocalNodeOptions,
    verbose: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    init_node_tracing(verbose, print);

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    rt.block_on(async { run_hogd_dev(options, cmd, print).await })
}

/// Parse one `NAME=VALUE` genesis parameter override.
///
/// The name must be one this build knows, so a typo fails here rather than
/// after hog-devnode has started. The value is a byte size, and whether it is
/// one hog-devnode's build accepts stays hog-devnode's call.
pub fn parse_network_config_override(
    value: &str,
) -> Result<(ConfigParamName, u32), NetworkConfigOverrideError> {
    let (name, amount) = value
        .split_once('=')
        .ok_or(NetworkConfigOverrideError::NotAnAssignment)?;
    let name = name.parse::<ConfigParamName>()?;
    let amount = crate::byte_size::parse_byte_size(amount)?;
    let amount =
        u32::try_from(amount).map_err(|_| NetworkConfigOverrideError::TooLarge { name, amount })?;
    Ok((name, amount))
}

/// Why a `--network-config NAME=VALUE` override was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetworkConfigOverrideError {
    /// The argument is not in `NAME=VALUE` form.
    #[error("expected NAME=VALUE, for example max_rv32_memory_bytes=16M")]
    NotAnAssignment,
    /// The name is not one this build knows.
    #[error(transparent)]
    Name(#[from] groundhog_node_supervisor::UnknownConfigParamName),
    /// The value is not a byte count.
    #[error(transparent)]
    Amount(#[from] crate::byte_size::ByteSizeError),
    /// The value is a byte count, but wider than the parameter holds.
    #[error("{name} must fit an unsigned 32-bit integer")]
    TooLarge {
        /// The parameter being overridden.
        name: ConfigParamName,
        /// The requested value.
        amount: u64,
    },
}

/// Startup options for a local hog-devnode-backed node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalNodeOptions {
    /// Public JSON-RPC bind host. Defaults to IPv4 loopback when omitted.
    pub host: Option<IpAddr>,
    /// Public JSON-RPC port. Use `0` to ask the OS for a free port.
    pub port: u16,
    /// Explicit hog-devnode binary path.
    pub hogd_dev_binary: Option<PathBuf>,
    /// Contract artifact directories passed to hog-devnode in search order.
    pub contract_artifact_directories: Vec<PathBuf>,
    /// Exact managed hog-devnode version requested for this invocation.
    pub hogd_dev_version: Option<String>,
    /// Forbid managed network acquisition while retaining local fallback sources.
    pub offline: bool,
    /// Directory where hog-devnode persists recoverable node state.
    pub data_dir: Option<PathBuf>,
    /// Override hogd protocol string for local cross-repo smoke tests.
    pub hogd_protocol: Option<String>,
    /// Override readiness timeout while waiting for hogd-dev.
    pub startup_timeout: Option<Duration>,
    /// PID-file behavior for orphan detection.
    pub pid_file: LocalNodePidFile,
    /// Override the local-user account count, excluding the dev-0 faucet.
    pub dev_account_count: Option<usize>,
    /// Override initial raw HOG units allocated to each local-user account.
    pub dev_account_tokens: Option<u64>,
    /// Override the local block production mode.
    pub block_driver: Option<LocalNodeBlockDriverMode>,
    /// Override the interval used by timer-driven block production.
    pub block_interval: Option<Duration>,
    /// Override the public JSON-RPC simultaneous connection cap.
    pub rpc_max_connections: Option<u32>,
    /// Override active plus buffered entries per JSON-RPC array (1..=1024).
    pub rpc_batch_concurrency: Option<groundhog_node_supervisor::RpcBatchConcurrency>,
    /// Override the RAM database capacity in bytes hog-devnode reserves at startup.
    pub ramdb_bytes: Option<u64>,
    /// Genesis parameter values to use in place of the ones stock genesis
    /// states. Whether hog-devnode's build accepts a value is its own call.
    pub network_config_overrides: Vec<(ConfigParamName, u32)>,
    /// Validated release identity supplied by an embedding client process.
    /// When absent, the `groundhog` client identity is discovered while building the config.
    pub bundle_identity: Option<groundhog_bundle_identity::BundleIdentity>,
}

/// PID-file behavior for local hogd-dev startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalNodePidFile {
    /// Use the data-directory PID file for persistent nodes, otherwise the cache directory.
    #[default]
    GlobalCache,
    /// Use an explicit PID-file path.
    Path(PathBuf),
    /// Do not configure a PID file.
    Disabled,
}

/// Running local node and the startup metadata callers need for orchestration.
#[derive(Debug)]
#[must_use = "local nodes keep hogd-dev running until stop, stopped, or run_until consumes them"]
pub struct StartedLocalNode {
    handle: groundhog_node_supervisor::NodeSupervisorHandle,
    rpc_addr: SocketAddr,
    readiness: groundhog_node_supervisor::HogdDevReadiness,
    backend_planes: groundhog_node_supervisor::BackendPlanes,
    data_dir: Option<PathBuf>,
}

impl StartedLocalNode {
    /// Bound JSON-RPC listen address.
    pub fn rpc_addr(&self) -> SocketAddr {
        self.rpc_addr
    }

    /// Connectable HTTP JSON-RPC URL for the running local node.
    ///
    /// Wildcard listen addresses are mapped to loopback in the same address
    /// family because unspecified addresses are bind targets, not portable
    /// client destinations.
    pub fn rpc_url(&self) -> String {
        rpc_url_for_addr(self.rpc_addr)
    }

    /// Parsed hog-devnode readiness payload.
    pub fn readiness(&self) -> &groundhog_node_supervisor::HogdDevReadiness {
        &self.readiness
    }

    /// Backend plane status for startup reporting and feature gating.
    pub fn backend_planes(&self) -> groundhog_node_supervisor::BackendPlanes {
        self.backend_planes
    }

    /// Absolute persistent data directory, when persistence is enabled.
    pub fn data_dir(&self) -> Option<&std::path::Path> {
        self.data_dir.as_deref()
    }

    /// Wait until hogd-dev or the JSON-RPC server stops, then tear down the
    /// remaining side.
    pub async fn stopped(self) -> Result<groundhog_node_supervisor::SupervisorStop, CliError> {
        self.handle
            .stopped()
            .await
            .map_err(|err| CliError::NodeSupervisor(err.to_string()))
    }

    /// Wait until hogd-dev exits, the JSON-RPC server stops, or a caller-owned
    /// stop signal completes, then tear down the remaining side.
    pub async fn run_until<F>(
        self,
        stop_signal: F,
    ) -> Result<groundhog_node_supervisor::SupervisorStop, CliError>
    where
        F: std::future::Future<Output = Result<(), groundhog_node_supervisor::NodeSupervisorError>>,
    {
        self.handle
            .run_until(stop_signal)
            .await
            .map_err(|err| CliError::NodeSupervisor(err.to_string()))
    }

    /// Stop the JSON-RPC server and shut hogd-dev down.
    pub async fn stop(self) -> Result<ExitStatus, CliError> {
        self.handle
            .stop()
            .await
            .map_err(|err| CliError::NodeSupervisor(err.to_string()))
    }
}

/// Start a reusable local node without taking over the terminal.
pub async fn start_local_node(options: LocalNodeOptions) -> Result<StartedLocalNode, CliError> {
    let (options, lease) = resolve_local_node_options(options, None).await?;
    start_resolved_local_node(options, lease).await
}

pub(crate) async fn start_local_node_with_print(
    options: LocalNodeOptions,
    print: &Print,
) -> Result<StartedLocalNode, CliError> {
    let (options, lease) = resolve_local_node_options(options, Some(print)).await?;
    start_resolved_local_node(options, lease).await
}

async fn start_resolved_local_node(
    options: LocalNodeOptions,
    lease: Option<groundhog_toolchain::InstallationLease>,
) -> Result<StartedLocalNode, CliError> {
    use groundhog_node_supervisor::NodeSupervisorHandle;

    let config = local_node_config(options)?;
    let data_dir = config.hogd_dev.data_dir.clone();
    let handle = NodeSupervisorHandle::start_with_inherited_file(
        config,
        lease.map(groundhog_toolchain::InstallationLease::into_lock_file),
    )
    .await
    .map_err(|e| map_node_supervisor_error(&e))?;
    let rpc_addr = handle.rpc_addr();
    let readiness = handle.hogd_readiness().clone();
    let backend_planes = handle.backend_planes();

    Ok(StartedLocalNode {
        handle,
        rpc_addr,
        readiness,
        backend_planes,
        data_dir,
    })
}

fn report_acquisition_interrupted(cmd: &'static str, print: &Print) -> Result<(), CliError> {
    if print.is_json() {
        print.json(&CommandResult::failure_with_error(
            cmd,
            json!({
                "schemaVersion": NODE_STARTUP_SCHEMA_VERSION,
                "started": false
            }),
            crate::result::CommandError::new(
                "node_acquisition_interrupted",
                "local node acquisition interrupted before startup",
                json!({}),
            ),
        ))?;
    } else {
        print.status("Local node acquisition interrupted before startup");
    }
    Err(CliError::CommandReportedFailure)
}

async fn run_hogd_dev(
    options: LocalNodeOptions,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    use groundhog_node_supervisor::{NodeSupervisorError, SupervisorStop};

    let shutdown_signal = prepare_stop_signal()
        .map_err(|source| map_node_supervisor_error(&NodeSupervisorError::StopSignal { source }))?;
    tokio::pin!(shutdown_signal);
    let (options, lease) = tokio::select! {
        result = resolve_local_node_options(options, Some(print)) => result?,
        result = &mut shutdown_signal => {
            result.map_err(|source| map_node_supervisor_error(&NodeSupervisorError::StopSignal { source }))?;
            return report_acquisition_interrupted(cmd, print);
        }
    };
    let runtime_artifacts_dir = local_node_runtime_artifacts_dir(&options);
    print.phase("Starting Groundhog local node");
    let started = start_resolved_local_node(options, lease).await?;
    let local_runtime_state_path = match write_started_local_runtime_state(
        &started,
        runtime_artifacts_dir.as_deref(),
    ) {
        Ok(path) => path,
        Err(error) => {
            if !print.is_json() {
                print.warn(format_args!(
                    "local runtime state was not updated; stopping local node: {error}"
                ));
            }
            if let Err(stop_error) = started.stop().await {
                return Err(CliError::NodeSupervisor(format!(
                    "failed to stop local node after local runtime state update failed: {stop_error}; local runtime state update failed: {error}"
                )));
            }
            return Err(error);
        }
    };
    let local_runtime_state_result = Ok(local_runtime_state_path);
    let cleanup_local_runtime_state =
        local_runtime_state_cleanup(&started, runtime_artifacts_dir.as_deref());

    if print.is_json() {
        if let Err(error) = print.json(&CommandResult::new(
            cmd,
            hogd_dev_startup_payload(
                started.rpc_addr(),
                started.readiness(),
                started.backend_planes(),
                &local_runtime_state_result,
                started.data_dir(),
            ),
        )) {
            let stop_result = started.stop().await;
            cleanup_written_local_runtime_state(cleanup_local_runtime_state, print);
            if let Err(stop_error) = stop_result {
                return Err(CliError::NodeSupervisor(format!(
                    "failed to stop local node after startup output failed: {stop_error}; startup output failed: {error}"
                )));
            }
            return Err(error);
        }
    } else {
        print_started_local_node(&started, &local_runtime_state_result, print);
    }

    let stop_signal = async {
        shutdown_signal
            .await
            .map_err(|source| NodeSupervisorError::StopSignal { source })?;
        if !print.is_json() {
            print.status("");
            print.note("Shutting down");
        }
        Ok(())
    };

    let outcome = started.run_until(stop_signal).await;
    cleanup_written_local_runtime_state(cleanup_local_runtime_state, print);
    let outcome = outcome?;
    match outcome {
        SupervisorStop::HogdExited(status) => {
            if status.success() {
                print.group(
                    crate::print::Outcome::Success,
                    "Local node stopped",
                    |print| {
                        print.note(format_args!("hog-devnode exited with status {status}"));
                    },
                );
                Ok(())
            } else {
                Err(CliError::NodeSupervisor(format!(
                    "hog-devnode exited with status {status}"
                )))
            }
        }
        SupervisorStop::RpcStopped(_) | SupervisorStop::StopRequested(_) => {
            print.group(crate::print::Outcome::Success, "Local node stopped", |_| {});
            Ok(())
        }
        other => Err(CliError::NodeSupervisor(format!(
            "unexpected supervisor stop state: {other:?}"
        ))),
    }
}

struct LocalRuntimeStateCleanup {
    state: crate::config::LocalRuntimeState,
}

fn local_runtime_state_cleanup(
    started: &StartedLocalNode,
    runtime_artifacts_dir: Option<&Path>,
) -> Option<LocalRuntimeStateCleanup> {
    Some(LocalRuntimeStateCleanup {
        state: local_runtime_state(started, runtime_artifacts_dir)?,
    })
}

fn cleanup_written_local_runtime_state(cleanup: Option<LocalRuntimeStateCleanup>, print: &Print) {
    let Some(cleanup) = cleanup else {
        return;
    };
    let result = crate::config::clear_local_runtime_state_if_matches(&cleanup.state);

    if let Err(error) = result
        && !print.is_json()
    {
        print.warn(format_args!("local runtime state was not cleared: {error}"));
    }
}

#[cfg(unix)]
pub(crate) fn prepare_stop_signal()
-> Result<impl Future<Output = Result<(), std::io::Error>>, std::io::Error> {
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    Ok(async move {
        tokio::select! {
            _ = sigint.recv() => Ok(()),
            _ = sigterm.recv() => Ok(()),
        }
    })
}

#[cfg(not(unix))]
pub(crate) fn prepare_stop_signal()
-> Result<impl Future<Output = Result<(), std::io::Error>>, std::io::Error> {
    Ok(tokio::signal::ctrl_c())
}

fn print_started_local_node(
    started: &StartedLocalNode,
    local_runtime_state_result: &Result<PathBuf, CliError>,
    print: &Print,
) {
    print.group(crate::print::Outcome::Ready, "Local node", |print| {
        print.success("Groundhog local node ready");
        print.detail("chain", &started.readiness().network_hash);
        if let Some(chain_state) = started.readiness().chain_state {
            print.detail("chain state", chain_state);
        }
        if started.backend_planes().read.is_indexer_backed() {
            print.detail("backend", "hog-devnode (chainTip, submit, and reads live)");
        } else {
            print.warn(
            "backend: hog-devnode (chainTip and submit live; read methods return NOT_IMPLEMENTED)",
        );
        }
        print.detail("rpc", started.rpc_url());
        if started.rpc_addr().ip().is_unspecified() {
            print.detail("rpc listen", started.rpc_addr());
        }
        if let Some(data_dir) = started.data_dir() {
            print.detail("data dir", data_dir.display());
        }
        print.detail("scheduler", started.readiness().endpoints.scheduler_coord);
        if let Some(indexer) = started.readiness().endpoints.indexer_block_explorer {
            print.detail("indexer", indexer);
        }
        if let Some(db_gateway) = started.readiness().endpoints.db_gateway {
            print.detail("db gateway", db_gateway);
        }
        if let Some(view_gateway) = started.readiness().endpoints.view_gateway {
            print.detail("view gateway", view_gateway);
        }
        if let Some(dev_control) = started.readiness().endpoints.dev_control {
            print.detail("dev control", dev_control);
        }
        if let Some(path) = &started.readiness().manifest_path {
            print.detail("runtime manifest", path.display());
            match local_runtime_state_result {
                Ok(path) => print.detail("config", format_args!("updated {}", path.display())),
                Err(error) => print.warn(format!("local runtime state was not updated: {error}")),
            }
            print.detail(
                "dev-key manifest",
                format_args!(
                    "export GROUNDHOG_RUNTIME_MANIFEST={}",
                    crate::terminal::render_shell_arg(&path.to_string_lossy()),
                ),
            );
        }
    });
}

fn write_started_local_runtime_state(
    started: &StartedLocalNode,
    runtime_artifacts_dir: Option<&Path>,
) -> Result<PathBuf, CliError> {
    let state = local_runtime_state(started, runtime_artifacts_dir).ok_or_else(|| {
        CliError::NodeSupervisor(
            "hog-devnode readiness did not include runtime manifest path".into(),
        )
    })?;
    crate::config::save_local_runtime_state(&state)
}

fn local_runtime_state(
    started: &StartedLocalNode,
    runtime_artifacts_dir: Option<&Path>,
) -> Option<crate::config::LocalRuntimeState> {
    Some(crate::config::LocalRuntimeState {
        node_id: started.rpc_url(),
        rpc_url: started.rpc_url(),
        runtime_manifest: started.readiness().manifest_path.clone()?,
        runtime_artifacts_dir: runtime_artifacts_dir.map(Path::to_path_buf),
    })
}

/// A read-only observation of the default node selection.
pub(crate) enum LocalNodeDiscovery {
    Local(PathBuf),
    /// Managed selection is retained even when it cannot currently be executed.
    Selected {
        version: String,
        reason: String,
    },
}

enum DevnodeSelection {
    Local(PathBuf),
    Managed {
        catalog: groundhog_toolchain::Catalog,
        version: String,
        explicit: bool,
    },
}

/// Plan selection without acquiring, locking, or downloading an installation.
fn resolve_devnode_selection(options: &LocalNodeOptions) -> Result<DevnodeSelection, CliError> {
    use groundhog_node_supervisor::{HogdDevConfig, resolve_hogd_dev_binary_before_path};
    let hogd = options
        .hogd_dev_binary
        .clone()
        .map_or_else(HogdDevConfig::default, |binary| {
            HogdDevConfig::default().with_binary_path(binary)
        });
    if options.hogd_dev_version.is_none()
        && let Some(binary) = resolve_hogd_dev_binary_before_path(&hogd)
            .map_err(|error| map_node_supervisor_error(&error))?
    {
        return Ok(DevnodeSelection::Local(binary));
    }
    let catalog = groundhog_toolchain::Catalog::embedded()?;
    let configured = crate::config::load_toolchain_version()?;
    let requested = options
        .hogd_dev_version
        .as_deref()
        .or(configured.as_deref());
    // Keep stale saved selections visible to inspection instead of discarding intent.
    let version = match requested {
        Some(version) => version.to_owned(),
        None => catalog.resolve_version(None)?.to_owned(),
    };
    Ok(DevnodeSelection::Managed {
        catalog,
        version,
        explicit: requested.is_some(),
    })
}

fn managed_target(
    catalog: &groundhog_toolchain::Catalog,
    version: &str,
) -> Result<groundhog_toolchain::HostTarget, CliError> {
    catalog.resolve_version(Some(version))?;
    let target = groundhog_toolchain::HostTarget::current()?;
    catalog
        .artifact(version, target)?
        .validate_current_host(target)?;
    Ok(target)
}

/// Inspect default selection without creating cache directories or downloading artifacts.
pub(crate) fn find_local_node_binary() -> Result<LocalNodeDiscovery, CliError> {
    use groundhog_node_supervisor::resolve_hogd_dev_binary_on_path;
    let selection = resolve_devnode_selection(&LocalNodeOptions::default())?;
    let (catalog, version, explicit) = match selection {
        DevnodeSelection::Local(path) => return Ok(LocalNodeDiscovery::Local(path)),
        DevnodeSelection::Managed {
            catalog,
            version,
            explicit,
        } => (catalog, version, explicit),
    };
    let installed = managed_target(&catalog, &version).and_then(|target| {
        groundhog_toolchain::Installer::new(crate::config::cache_dir()?)
            .find(&catalog, &version, target)
            .map_err(CliError::from)
    });
    if let Ok(Some(installation)) = &installed {
        return Ok(LocalNodeDiscovery::Local(installation.binary_path()));
    }
    if !explicit && let Some(path) = resolve_hogd_dev_binary_on_path() {
        return Ok(LocalNodeDiscovery::Local(path));
    }
    let reason = match installed {
        Ok(None) => format!(
            "hog-devnode {version} is selected, not installed; node startup will install it unless offline"
        ),
        Err(error) => format!("hog-devnode {version} is selected but unavailable: {error}"),
        Ok(Some(_)) => unreachable!(),
    };
    Ok(LocalNodeDiscovery::Selected { version, reason })
}

async fn resolve_local_node_options(
    mut options: LocalNodeOptions,
    print: Option<&Print>,
) -> Result<
    (
        LocalNodeOptions,
        Option<groundhog_toolchain::InstallationLease>,
    ),
    CliError,
> {
    use groundhog_node_supervisor::resolve_hogd_dev_binary_on_path;
    // Host probes and read-only filesystem discovery must not block signal handling.
    let planning_options = options.clone();
    let (selection, target) = tokio::task::spawn_blocking(move || {
        let selection = resolve_devnode_selection(&planning_options)?;
        let target = match &selection {
            DevnodeSelection::Managed {
                catalog, version, ..
            } => Some(managed_target(catalog, version)),
            DevnodeSelection::Local(_) => None,
        };
        Ok::<_, CliError>((selection, target))
    })
    .await
    .map_err(|error| CliError::NodeSupervisor(format!("node selection task failed: {error}")))??;
    match selection {
        DevnodeSelection::Local(binary) => {
            options.hogd_dev_binary = Some(binary);
            Ok((options, None))
        }
        DevnodeSelection::Managed {
            catalog,
            version,
            explicit,
        } => {
            let target = match target.expect("bug: managed selection has a target result") {
                Ok(target) => target,
                Err(error) => {
                    if !explicit && let Some(binary) = resolve_hogd_dev_binary_on_path() {
                        options.hogd_dev_binary = Some(binary);
                        return Ok((options, None));
                    }
                    return Err(error);
                }
            };
            acquire_managed_node_options(options, catalog, version, target, explicit, print).await
        }
    }
}

async fn acquire_managed_node_options(
    mut options: LocalNodeOptions,
    catalog: groundhog_toolchain::Catalog,
    version: String,
    target: groundhog_toolchain::HostTarget,
    explicitly_requested: bool,
    print: Option<&Print>,
) -> Result<
    (
        LocalNodeOptions,
        Option<groundhog_toolchain::InstallationLease>,
    ),
    CliError,
> {
    use groundhog_node_supervisor::resolve_hogd_dev_binary_on_path;
    use groundhog_toolchain::Installer;

    let cache_root = crate::config::cache_dir()?;
    let cache_installer = Installer::new(&cache_root);
    loop {
        match cache_installer.lease(&catalog, &version, target).await {
            Ok(Some(lease)) => {
                options.hogd_dev_binary = Some(lease.binary_path());
                return Ok((options, Some(lease)));
            }
            Err(error @ groundhog_toolchain::InstallError::Io { .. }) if !explicitly_requested => {
                if let Some(binary) = resolve_hogd_dev_binary_on_path() {
                    options.hogd_dev_binary = Some(binary);
                    return Ok((options, None));
                }
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
            Ok(None) => {}
        }

        let mut acquisition_error = None;
        if options.offline {
            if explicitly_requested {
                return Err(CliError::ToolchainOfflineMissing {
                    version,
                    target: target.triple().to_string(),
                });
            }
        } else {
            if let Some(print) = print {
                print.status(format_args!(
                    "Installing hog-devnode {version} for {target}"
                ));
            }
            match crate::toolchain_cmd::artifact_source() {
                Ok(source) => {
                    let installer = Installer::new(&cache_root);
                    match installer.install(&catalog, &version, target, &source).await {
                        Ok(_) => {
                            // Recheck under a shared lease: cleanup may have won the lock
                            // after installation completed. Never expose an unprotected path.
                            continue;
                        }
                        Err(error) if explicitly_requested => return Err(error.into()),
                        Err(error) => acquisition_error = Some(CliError::from(error)),
                    }
                }
                Err(error) if explicitly_requested => return Err(error),
                Err(error) => acquisition_error = Some(error),
            }
        }

        if let Some(binary) = resolve_hogd_dev_binary_on_path() {
            options.hogd_dev_binary = Some(binary);
            return Ok((options, None));
        }
        if options.offline {
            return Err(CliError::ToolchainOfflineMissing {
                version,
                target: target.triple().to_string(),
            });
        }
        if let Some(error) = acquisition_error {
            return Err(error);
        }
        return Err(map_node_supervisor_error(
            &groundhog_node_supervisor::NodeSupervisorError::HogdDevNotFound,
        ));
    }
}

fn local_node_runtime_artifacts_dir(options: &LocalNodeOptions) -> Option<PathBuf> {
    use groundhog_node_supervisor::{HogdDevConfig, resolve_hogd_dev_binary};

    let artifact_config = HogdDevConfig::default()
        .with_contract_artifact_directories(&options.contract_artifact_directories);
    if let Some(root) = artifact_config
        .contract_artifact_directories
        .iter()
        .find_map(|root| contract_artifacts_dir(root))
    {
        return Some(root);
    }

    let hogd = options
        .hogd_dev_binary
        .clone()
        .map_or_else(HogdDevConfig::default, |binary| {
            HogdDevConfig::default().with_binary_path(binary)
        });
    resolve_hogd_dev_binary(&hogd)
        .ok()
        .and_then(|binary| genesis_root_artifact_for_binary(&binary))
        .and_then(|artifact| artifact.parent().map(Path::to_path_buf))
}

pub(crate) fn runtime_artifacts_dir_for_binary(binary: &Path) -> Option<PathBuf> {
    let binary = std::fs::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
    let binary_dir = binary.parent()?;
    let mut candidates = vec![binary_dir.to_path_buf()];

    if let Some(prefix) = binary_dir.parent() {
        candidates.push(prefix.join("share/groundhog"));
    }
    candidates.extend(
        binary_dir
            .ancestors()
            .take(4)
            .map(|ancestor| ancestor.join("built_contracts")),
    );

    candidates
        .into_iter()
        .find_map(|root| runtime_artifacts_dir(&root))
}

fn runtime_artifacts_dir(root: &Path) -> Option<PathBuf> {
    let root = std::fs::canonicalize(root).ok()?;
    (root
        .join("contracts")
        .join(GENESIS_ROOT_ARTIFACT_FILENAME)
        .is_file()
        || root.join(GENESIS_ROOT_ARTIFACT_FILENAME).is_file())
    .then_some(root)
}

fn contract_artifacts_dir(root: &Path) -> Option<PathBuf> {
    let root = std::fs::canonicalize(root).ok()?;
    root.join(GENESIS_ROOT_ARTIFACT_FILENAME)
        .is_file()
        .then_some(root)
}

pub(crate) fn genesis_root_artifact_for_binary(binary: &Path) -> Option<PathBuf> {
    let root = runtime_artifacts_dir_for_binary(binary)?;
    [
        root.join("contracts").join(GENESIS_ROOT_ARTIFACT_FILENAME),
        root.join(GENESIS_ROOT_ARTIFACT_FILENAME),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn with_discovered_contract_artifacts(
    mut hogd: groundhog_node_supervisor::HogdDevConfig,
) -> groundhog_node_supervisor::HogdDevConfig {
    use groundhog_node_supervisor::resolve_hogd_dev_binary;

    let Some(discovered) = resolve_hogd_dev_binary(&hogd)
        .ok()
        .and_then(|binary| genesis_root_artifact_for_binary(&binary))
        .and_then(|artifact| artifact.parent().map(Path::to_path_buf))
    else {
        return hogd;
    };

    if !hogd
        .contract_artifact_directories
        .iter()
        .any(|directory| directory == &discovered)
    {
        hogd.contract_artifact_directories.push(discovered);
    }
    hogd
}

#[expect(
    clippy::too_many_lines,
    reason = "flat CLI options map directly to one supervisor configuration"
)]
fn local_node_config(
    options: LocalNodeOptions,
) -> Result<groundhog_node_supervisor::NodeSupervisorConfig, CliError> {
    use groundhog_node_supervisor::{HogdDevConfig, NodeSupervisorConfig};

    if options
        .block_interval
        .is_some_and(|interval| interval.is_zero())
    {
        return Err(CliError::Other(
            "block interval must be at least 1 ms".into(),
        ));
    }
    if options.block_interval.is_some()
        && !matches!(
            options.block_driver,
            Some(LocalNodeBlockDriverMode::Interval)
        )
    {
        return Err(CliError::Other(
            "block interval requires block driver interval mode".into(),
        ));
    }
    if options
        .rpc_max_connections
        .is_some_and(|max_connections| max_connections == 0)
    {
        return Err(CliError::Other(
            "RPC max connections must be at least 1".into(),
        ));
    }

    let bundle_identity = options
        .bundle_identity
        .clone()
        .unwrap_or_else(crate::bundle_identity);
    let hogd = options
        .hogd_dev_binary
        .map_or_else(HogdDevConfig::default, |binary| {
            HogdDevConfig::default().with_binary_path(binary)
        })
        .with_contract_artifact_directories(options.contract_artifact_directories);
    let hogd = with_discovered_contract_artifacts(hogd);
    let hogd = match options.data_dir {
        Some(data_dir) => hogd.with_data_dir(data_dir).with_shutdown_grace(
            groundhog_node_supervisor::config::DEFAULT_HOGD_DEV_PERSISTENT_SHUTDOWN_GRACE,
        ),
        None => hogd,
    };
    let automatic_pid_file = if matches!(&options.pid_file, LocalNodePidFile::GlobalCache) {
        let path = match hogd.data_dir.as_deref() {
            Some(data_dir) => data_dir.join(HOG_DEVNODE_PID_FILE),
            None => local_node_pid_file(&crate::config::cache_dir()?),
        };
        Some(path)
    } else {
        None
    };
    let hogd = match options.pid_file {
        LocalNodePidFile::GlobalCache => hogd.with_pid_file(
            automatic_pid_file.expect("bug: automatic PID path resolved for automatic mode"),
        ),
        LocalNodePidFile::Path(path) => hogd.with_pid_file(path),
        LocalNodePidFile::Disabled => hogd,
    };
    let mut hogd = hogd;
    if let Some(count) = options.dev_account_count {
        hogd = hogd.with_dev_account_count(count);
    }
    if let Some(tokens) = options.dev_account_tokens {
        hogd = hogd.with_dev_account_tokens(tokens);
    }
    let hogd = match options.startup_timeout {
        Some(timeout) => hogd.with_startup_timeout(timeout),
        None => hogd,
    };
    let hogd = match options.ramdb_bytes {
        Some(ramdb_bytes) => hogd.with_ramdb_bytes(ramdb_bytes),
        None => hogd,
    };
    let mut network_config_overrides = std::collections::BTreeMap::new();
    for (name, value) in options.network_config_overrides {
        if network_config_overrides.insert(name, value).is_some() {
            return Err(CliError::ConfigValueInvalid {
                field: "network-config".into(),
                message: format!("parameter {name} was specified more than once"),
            });
        }
    }
    let hogd = hogd.with_network_config_overrides(network_config_overrides);
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address(rpc_listen_address(options.host, options.port))
        .with_hogd_dev(hogd)
        .with_bundle_identity(bundle_identity);
    let config = match options.rpc_max_connections {
        Some(max_connections) => config.with_rpc_max_connections(max_connections),
        None => config,
    };
    let config = match options.rpc_batch_concurrency {
        Some(concurrency) => config.with_rpc_batch_concurrency(concurrency),
        None => config,
    };
    let config = match (options.block_driver, options.block_interval) {
        (None, None) => config,
        (mode, interval) => {
            let mut block_driver = config.block_driver;
            if let Some(mode) = mode {
                block_driver = block_driver.with_mode(match mode {
                    LocalNodeBlockDriverMode::Instant => {
                        groundhog_node_supervisor::BlockDriverMode::Instant
                    }
                    LocalNodeBlockDriverMode::Interval => {
                        groundhog_node_supervisor::BlockDriverMode::Interval
                    }
                });
            }
            if let Some(interval) = interval {
                block_driver = block_driver.with_interval(interval);
            }
            config.with_block_driver(block_driver)
        }
    };
    Ok(match options.hogd_protocol {
        Some(protocol) => config.with_hogd_protocol_override(protocol),
        None => config,
    })
}

fn rpc_listen_address(host: Option<IpAddr>, port: u16) -> String {
    let host = host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    SocketAddr::new(host, port).to_string()
}

fn rpc_client_address(addr: SocketAddr) -> SocketAddr {
    let host = match addr.ip() {
        IpAddr::V4(host) if host.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(host) if host.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        host => host,
    };
    SocketAddr::new(host, addr.port())
}

fn rpc_url_for_addr(addr: SocketAddr) -> String {
    format!("http://{}", rpc_client_address(addr))
}

/// Parse an exact HOG bootstrap allocation into seven-decimal raw units.
pub fn parse_dev_account_hog(input: &str) -> Result<u64, CliError> {
    let raw = crate::hog::parse_amount(input)?;
    u64::try_from(raw).map_err(|_| CliError::InvalidHogAmount {
        input: input.to_owned(),
        message: "amount exceeds the supported raw-unit range".to_owned(),
    })
}

fn local_node_pid_file(scope: &std::path::Path) -> PathBuf {
    let legacy = scope.join(LEGACY_HOGD_DEV_PID_FILE);
    if legacy.exists() {
        return legacy;
    }
    scope.join(HOG_DEVNODE_PID_FILE)
}

fn map_node_supervisor_error(err: &groundhog_node_supervisor::NodeSupervisorError) -> CliError {
    let message = err.to_string();
    match err {
        groundhog_node_supervisor::NodeSupervisorError::LocalNodeAlreadyRunning { .. } => {
            CliError::LocalNodeAlreadyRunning(message)
        }
        groundhog_node_supervisor::NodeSupervisorError::ProtocolIntroRejected {
            expected_protocol,
            ..
        } => CliError::NodeSupervisorProtocolIntroRejected {
            message,
            expected_protocol: expected_protocol.clone(),
        },
        _ => CliError::NodeSupervisor(message),
    }
}

pub(crate) fn init_node_tracing(verbose: bool, print: &Print) {
    init_tracing(default_tracing_filter(verbose), print);
}

fn default_tracing_filter(verbose: bool) -> &'static str {
    if verbose {
        "groundhog_node_supervisor=info,hogd_dev=info"
    } else {
        "groundhog_node_supervisor=warn,hogd_dev=warn"
    }
}

fn init_tracing(default_filter: &str, print: &Print) {
    use tracing_subscriber::EnvFilter;

    let print = print.clone();

    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .with_writer(move || print.log_writer())
        .with_target(false)
        .without_time()
        .try_init();
}

fn hogd_dev_startup_payload(
    addr: std::net::SocketAddr,
    readiness: &groundhog_node_supervisor::HogdDevReadiness,
    backend_planes: groundhog_node_supervisor::BackendPlanes,
    local_runtime_state_result: &Result<PathBuf, CliError>,
    data_dir: Option<&std::path::Path>,
) -> serde_json::Value {
    json!({
        "schemaVersion": NODE_STARTUP_SCHEMA_VERSION,
        "chain": readiness.network_hash,
        "chain_state": readiness.chain_state,
        "mode": "dev",
        "experimental": false,
        "backend": {
            "kind": "hogd_dev",
            "stability": "supported",
            "planes": backend_planes,
            "endpoints": hogd_dev_endpoints_payload(readiness),
        },
        "runtime_manifest": runtime_manifest_payload(readiness),
        "local_runtime_state": local_runtime_state_payload(local_runtime_state_result),
        "data_dir": data_dir,
        "rpc_listen_address": addr.to_string(),
        "rpc_url": rpc_url_for_addr(addr),
    })
}

fn local_runtime_state_payload(
    local_runtime_state_result: &Result<PathBuf, CliError>,
) -> serde_json::Value {
    match local_runtime_state_result {
        Ok(path) => json!({
            "ok": true,
            "path": path,
        }),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
        }),
    }
}

fn runtime_manifest_payload(
    readiness: &groundhog_node_supervisor::HogdDevReadiness,
) -> serde_json::Value {
    match (&readiness.manifest_path, &readiness.manifest_hash) {
        (Some(path), Some(hash)) => json!({
            "schemaVersion": groundhog_json_rpc_types::runtime_manifest::RUNTIME_MANIFEST_SCHEMA_VERSION,
            "path": path,
            "hash": hash,
        }),
        _ => serde_json::Value::Null,
    }
}

fn hogd_dev_endpoints_payload(
    readiness: &groundhog_node_supervisor::HogdDevReadiness,
) -> serde_json::Value {
    let mut endpoints = serde_json::Map::new();
    endpoints.insert(
        "scheduler_coord".into(),
        serde_json::Value::String(readiness.endpoints.scheduler_coord.to_string()),
    );
    if let Some(indexer) = readiness.endpoints.indexer_block_explorer {
        endpoints.insert(
            "indexer_block_explorer".into(),
            serde_json::Value::String(indexer.to_string()),
        );
    }
    if let Some(db_gateway) = readiness.endpoints.db_gateway {
        endpoints.insert(
            "db_gateway".into(),
            serde_json::Value::String(db_gateway.to_string()),
        );
    }
    if let Some(view_gateway) = readiness.endpoints.view_gateway {
        endpoints.insert(
            "view_gateway".into(),
            serde_json::Value::String(view_gateway.to_string()),
        );
    }
    if let Some(dev_control) = readiness.endpoints.dev_control {
        endpoints.insert(
            "dev_control".into(),
            serde_json::Value::String(dev_control.to_string()),
        );
    }
    serde_json::Value::Object(endpoints)
}

#[cfg(test)]
mod tests {
    use super::{
        LocalNodeBlockDriverMode, LocalNodeOptions, LocalNodePidFile, NODE_STARTUP_SCHEMA_VERSION,
        default_tracing_filter, genesis_root_artifact_for_binary, hogd_dev_startup_payload,
        local_node_config, local_node_pid_file, local_node_runtime_artifacts_dir,
        local_runtime_state_payload, map_node_supervisor_error, rpc_client_address,
        runtime_artifacts_dir_for_binary, test_local_runtime_state_result,
    };
    use crate::CliError;
    use groundhog_node_supervisor::{
        BackendPlanes, ConfigParamName, NodeSupervisorError, PlaneStatus, parse_readiness_line,
    };
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        path::PathBuf,
        time::Duration,
    };
    use tempfile::TempDir;

    #[test]
    fn network_config_override_parsing_and_duplicate_rejection() {
        use super::parse_network_config_override;
        assert_eq!(
            parse_network_config_override("max_rv32_memory_bytes=16M"),
            Ok((ConfigParamName::MaxRv32MemoryBytes, 16_777_216))
        );
        assert_eq!(
            parse_network_config_override("max_bytestring_len=0x8000"),
            Ok((ConfigParamName::MaxBytestringLen, 32_768))
        );
        // An unknown name, a missing or malformed value, and a value past
        // u32 all fail before hog-devnode is started.
        for invalid in [
            "",
            "no-value",
            "=16",
            "max_rv32_memmory_bytes=2",
            "max_bytestring_len=",
            "max_bytestring_len=-1",
            "max_bytestring_len=4G",
        ] {
            assert!(parse_network_config_override(invalid).is_err(), "{invalid}");
        }
        assert!(
            parse_network_config_override("max_rv32_memmory_bytes=2")
                .unwrap_err()
                .to_string()
                .contains("max_rv32_memory_bytes")
        );

        let error = local_node_config(LocalNodeOptions {
            network_config_overrides: vec![
                (ConfigParamName::MaxBytestringLen, 1),
                (ConfigParamName::MaxBytestringLen, 2),
            ],
            ..LocalNodeOptions::default()
        })
        .unwrap_err();

        assert!(error.to_string().contains("specified more than once"));
    }

    #[test]
    fn local_node_config_sets_supervisor_startup_options() {
        let binary = PathBuf::from("/tmp/hog-devnode-test");
        let supplied_identity = groundhog_bundle_identity::BundleIdentity::unbundled(
            groundhog_build_info::BuildVersion::new("9.8.7-test", None)
                .with_source_commit(Some("373bec4b9dd9b99abae4f23b82f4e35df61273d1")),
        );
        let config = local_node_config(LocalNodeOptions {
            host: None,
            port: 0,
            hogd_dev_binary: Some(binary.clone()),
            contract_artifact_directories: vec![PathBuf::from("/tmp/contracts")],
            hogd_dev_version: None,
            offline: false,
            data_dir: Some(PathBuf::from("/tmp/hog-devnode-data")),
            hogd_protocol: Some("test-protocol".into()),
            startup_timeout: Some(Duration::from_secs(30)),
            pid_file: LocalNodePidFile::Path(PathBuf::from("/tmp/hog-devnode-test.pid")),
            dev_account_count: Some(3),
            dev_account_tokens: Some(12_345_678),
            block_driver: Some(LocalNodeBlockDriverMode::Interval),
            block_interval: Some(Duration::from_millis(250)),
            rpc_max_connections: Some(512),
            rpc_batch_concurrency: Some(17.try_into().unwrap()),
            ramdb_bytes: Some(2_147_483_648),
            network_config_overrides: vec![(ConfigParamName::MaxRv32MemoryBytes, 16_777_216)],
            bundle_identity: Some(supplied_identity.clone()),
        })
        .expect("bug: local node config should build");

        assert_eq!(config.rpc_listen_address, "127.0.0.1:0");
        assert_eq!(config.hogd_dev.binary_path, Some(binary));
        assert_eq!(
            config.hogd_dev.contract_artifact_directories,
            [PathBuf::from("/tmp/contracts")]
        );
        assert_eq!(
            config.hogd_dev.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/hog-devnode-data"))
        );
        assert_eq!(
            config.hogd_protocol_override.as_deref(),
            Some("test-protocol")
        );
        assert_eq!(
            config.hogd_dev.pid_file.as_deref(),
            Some(std::path::Path::new("/tmp/hog-devnode-test.pid"))
        );
        assert_eq!(config.hogd_dev.dev_account_count, 3);
        assert_eq!(config.hogd_dev.dev_account_tokens, 12_345_678);
        assert_eq!(config.hogd_dev.ramdb_bytes, Some(2_147_483_648));
        assert_eq!(
            config
                .hogd_dev
                .network_config_overrides
                .get(&ConfigParamName::MaxRv32MemoryBytes),
            Some(&16_777_216)
        );
        assert_eq!(config.hogd_dev.startup_timeout, Duration::from_secs(30));
        assert_eq!(
            config.hogd_dev.shutdown_grace,
            groundhog_node_supervisor::config::DEFAULT_HOGD_DEV_PERSISTENT_SHUTDOWN_GRACE
        );
        assert_eq!(
            config.block_driver.mode,
            groundhog_node_supervisor::BlockDriverMode::Interval
        );
        assert_eq!(config.block_driver.interval, Duration::from_millis(250));
        assert_eq!(config.rpc_max_connections, 512);
        assert_eq!(config.rpc_batch_concurrency.get(), 17);
        assert_eq!(config.bundle_identity.as_ref(), Some(&supplied_identity));
    }

    #[test]
    fn local_node_config_sets_public_rpc_host_without_changing_hogd_host() {
        let config = local_node_config(LocalNodeOptions {
            host: Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            port: 9545,
            ..LocalNodeOptions::default()
        })
        .expect("bug: public RPC host should configure");

        assert_eq!(config.rpc_listen_address, "0.0.0.0:9545");
        assert_eq!(
            config.hogd_dev.listen_host,
            groundhog_node_supervisor::config::DEFAULT_HOGD_DEV_LISTEN_HOST
        );
    }

    #[test]
    fn local_node_config_formats_ipv6_rpc_host() {
        let config = local_node_config(LocalNodeOptions {
            host: Some(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
            port: 9545,
            ..LocalNodeOptions::default()
        })
        .expect("bug: IPv6 public RPC host should configure");

        assert_eq!(config.rpc_listen_address, "[::1]:9545");
    }

    #[test]
    fn wildcard_rpc_addresses_use_loopback_client_addresses() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 9545);
        assert_eq!(
            rpc_client_address(ipv4),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9545)
        );

        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 9545);
        assert_eq!(
            rpc_client_address(ipv6),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9545)
        );

        let explicit = "192.0.2.1:9545".parse().expect("bug: valid socket addr");
        assert_eq!(rpc_client_address(explicit), explicit);
    }

    #[test]
    fn dev_account_hog_parser_preserves_seven_decimal_places() {
        assert_eq!(
            super::parse_dev_account_hog("12.3456789").unwrap(),
            123_456_789
        );
    }

    #[test]
    fn dev_account_hog_parser_rejects_more_than_seven_decimal_places() {
        let error = super::parse_dev_account_hog("1.00000001")
            .expect_err("more than seven decimal places should fail");

        assert_eq!(error.code(), "invalid_hog_amount");
    }

    #[test]
    fn dev_account_hog_parser_rejects_values_above_u64_raw_units() {
        let error = super::parse_dev_account_hog("1844674407370.9551616")
            .expect_err("raw allocation above u64 should fail");

        assert!(error.to_string().contains("raw-unit range"));
    }

    #[test]
    fn persistent_nodes_scope_automatic_pid_files_to_their_data_dirs() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let first_data_dir = tmp.path().join("first");
        let second_data_dir = tmp.path().join("second");
        fs::create_dir(&first_data_dir).expect("bug: first data dir should be created");
        fs::write(first_data_dir.join("hogd-dev.pid"), "")
            .expect("bug: legacy cache-era PID fixture should be written");

        let first = local_node_config(LocalNodeOptions {
            data_dir: Some(first_data_dir.clone()),
            ..LocalNodeOptions::default()
        })
        .expect("bug: first persistent config should build");
        let second = local_node_config(LocalNodeOptions {
            data_dir: Some(second_data_dir.clone()),
            ..LocalNodeOptions::default()
        })
        .expect("bug: second persistent config should build");

        assert_eq!(
            first.hogd_dev.pid_file.as_deref(),
            Some(first_data_dir.join("hog-devnode.pid").as_path())
        );
        assert_eq!(
            second.hogd_dev.pid_file.as_deref(),
            Some(second_data_dir.join("hog-devnode.pid").as_path())
        );
        assert_ne!(first.hogd_dev.pid_file, second.hogd_dev.pid_file);
    }

    #[test]
    fn runtime_artifacts_dir_resolves_release_bundle_layout() {
        let tmp = TempDir::new().expect("bug: tempdir should be created");
        let binary = tmp.path().join("hog-devnode");
        fs::write(&binary, []).expect("bug: binary fixture should be written");
        let contracts = tmp.path().join("contracts");
        fs::create_dir(&contracts).expect("bug: contracts fixture should be created");
        fs::write(contracts.join("genesis_root.rv32a"), [])
            .expect("bug: artifact fixture should be written");

        assert_eq!(
            runtime_artifacts_dir_for_binary(&binary),
            Some(fs::canonicalize(tmp.path()).expect("bug: fixture root should resolve"))
        );
    }

    #[test]
    fn runtime_artifacts_dir_resolves_cmake_build_layout() {
        let tmp = TempDir::new().expect("bug: tempdir should be created");
        let binary_dir = tmp.path().join("build/hogd/devnode");
        fs::create_dir_all(&binary_dir).expect("bug: binary dir should be created");
        let binary = binary_dir.join("hog-devnode");
        fs::write(&binary, []).expect("bug: binary fixture should be written");
        let contracts = tmp.path().join("build/built_contracts");
        fs::create_dir_all(&contracts).expect("bug: contracts fixture should be created");
        fs::write(contracts.join("genesis_root.rv32a"), [])
            .expect("bug: artifact fixture should be written");

        assert_eq!(
            runtime_artifacts_dir_for_binary(&binary),
            Some(fs::canonicalize(contracts).expect("bug: fixture root should resolve"))
        );
    }

    #[test]
    fn runtime_artifacts_dir_resolves_installed_package_layout() {
        let tmp = TempDir::new().expect("bug: tempdir should be created");
        let binary_dir = tmp.path().join("usr/bin");
        fs::create_dir_all(&binary_dir).expect("bug: binary dir should be created");
        let binary = binary_dir.join("hog-devnode");
        fs::write(&binary, []).expect("bug: binary fixture should be written");
        let artifacts_root = tmp.path().join("usr/share/groundhog");
        let contracts = artifacts_root.join("contracts");
        fs::create_dir_all(&contracts).expect("bug: contracts fixture should be created");
        fs::write(contracts.join("genesis_root.rv32a"), [])
            .expect("bug: artifact fixture should be written");

        assert_eq!(
            runtime_artifacts_dir_for_binary(&binary),
            Some(fs::canonicalize(artifacts_root).expect("bug: fixture root should resolve"))
        );
        assert_eq!(
            genesis_root_artifact_for_binary(&binary),
            Some(
                fs::canonicalize(contracts.join("genesis_root.rv32a"))
                    .expect("bug: artifact fixture should resolve")
            )
        );
    }

    #[test]
    fn local_node_config_forwards_discovered_installed_contracts_after_user_directories() {
        let tmp = TempDir::new().expect("bug: tempdir should be created");
        let binary_dir = tmp.path().join("usr/bin");
        fs::create_dir_all(&binary_dir).expect("bug: binary dir should be created");
        let binary = binary_dir.join("hog-devnode");
        fs::write(&binary, []).expect("bug: binary fixture should be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&binary)
                .expect("bug: binary fixture metadata should be readable")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary, permissions)
                .expect("bug: binary fixture should be executable");
        }

        let contracts = tmp.path().join("usr/share/groundhog/contracts");
        fs::create_dir_all(&contracts).expect("bug: contracts fixture should be created");
        fs::write(contracts.join("genesis_root.rv32a"), [])
            .expect("bug: artifact fixture should be written");

        let user_directory = tmp.path().join("user-contracts");
        assert_eq!(
            local_node_runtime_artifacts_dir(&LocalNodeOptions {
                hogd_dev_binary: Some(binary.clone()),
                contract_artifact_directories: vec![user_directory.clone()],
                ..LocalNodeOptions::default()
            }),
            Some(fs::canonicalize(&contracts).expect("bug: contracts fixture should resolve"))
        );
        let config = local_node_config(LocalNodeOptions {
            hogd_dev_binary: Some(binary),
            contract_artifact_directories: vec![user_directory.clone()],
            ..LocalNodeOptions::default()
        })
        .expect("bug: installed local node config should build");

        assert_eq!(
            config.hogd_dev.contract_artifact_directories,
            [
                user_directory,
                fs::canonicalize(contracts).expect("bug: contracts fixture should resolve"),
            ]
        );
    }

    #[test]
    fn explicit_contract_artifact_directory_drives_runtime_discovery() {
        let tmp = TempDir::new().expect("bug: tempdir should be created");
        let contracts = tmp.path().join("custom-contracts");
        fs::create_dir(&contracts).expect("bug: contracts fixture should be created");
        fs::write(contracts.join("genesis_root.rv32a"), [])
            .expect("bug: artifact fixture should be written");

        let resolved = local_node_runtime_artifacts_dir(&LocalNodeOptions {
            contract_artifact_directories: vec![contracts.clone()],
            ..LocalNodeOptions::default()
        });

        assert_eq!(
            resolved,
            Some(fs::canonicalize(contracts).expect("bug: fixture root should resolve"))
        );
    }

    #[test]
    fn local_node_config_rejects_zero_rpc_max_connections() {
        let err = local_node_config(LocalNodeOptions {
            rpc_max_connections: Some(0),
            ..LocalNodeOptions::default()
        })
        .expect_err("zero connection cap should be rejected");

        assert!(matches!(err, CliError::Other(message) if message.contains("max connections")));
    }

    #[test]
    fn local_node_config_can_disable_pid_file() {
        let config = local_node_config(LocalNodeOptions {
            pid_file: LocalNodePidFile::Disabled,
            ..LocalNodeOptions::default()
        })
        .expect("bug: local node config should build");

        assert!(config.hogd_dev.pid_file.is_none());
    }

    #[test]
    fn automatic_pid_file_prefers_legacy_path_during_migration() {
        let tmp = TempDir::new().expect("bug: temp dir should be created");
        let new_path = tmp.path().join("hog-devnode.pid");
        let legacy_path = tmp.path().join("hogd-dev.pid");

        assert_eq!(local_node_pid_file(tmp.path()), new_path);

        fs::write(&legacy_path, "").expect("bug: legacy pid file should write");
        assert_eq!(local_node_pid_file(tmp.path()), legacy_path);
    }

    #[test]
    fn local_node_config_rejects_zero_block_interval() {
        let err = local_node_config(LocalNodeOptions {
            block_driver: Some(LocalNodeBlockDriverMode::Interval),
            block_interval: Some(Duration::ZERO),
            ..LocalNodeOptions::default()
        })
        .expect_err("zero interval should be rejected");

        assert!(matches!(err, CliError::Other(message) if message.contains("block interval")));
    }

    #[test]
    fn local_node_config_rejects_block_interval_without_interval_driver() {
        let err = local_node_config(LocalNodeOptions {
            block_interval: Some(Duration::from_millis(250)),
            ..LocalNodeOptions::default()
        })
        .expect_err("interval without interval mode should be rejected");

        assert!(
            matches!(err, CliError::Other(message) if message.contains("requires block driver interval"))
        );

        let err = local_node_config(LocalNodeOptions {
            block_driver: Some(LocalNodeBlockDriverMode::Instant),
            block_interval: Some(Duration::from_millis(250)),
            ..LocalNodeOptions::default()
        })
        .expect_err("instant mode interval should be rejected");

        assert!(
            matches!(err, CliError::Other(message) if message.contains("requires block driver interval"))
        );
    }

    #[test]
    fn protocol_intro_rejection_maps_to_protocol_hint() {
        let err = NodeSupervisorError::ViewGatewayIntroRejected { outcome: 1 };
        let cli_error = map_node_supervisor_error(&err);

        assert!(matches!(cli_error, CliError::NodeSupervisor(_)));

        let err = NodeSupervisorError::ProtocolIntroRejected {
            service: "ViewGateway",
            outcome: 1,
            outcome_name: "ViewGatewayIntroMismatchedProtocol",
            is_protocol_mismatch: true,
            protocol: "hogp/sent00000000000".to_string(),
            expected_protocol: Some("hogp/expected0000000".to_string()),
        };
        let cli_error = map_node_supervisor_error(&err);

        assert!(matches!(
            cli_error,
            CliError::NodeSupervisorProtocolIntroRejected { .. }
        ));
        assert_eq!(cli_error.code(), "protocol_intro_rejected");
        assert_eq!(
            cli_error
                .hint()
                .expect("intro rejection should have a specific hint"),
            "pass `--hogd-protocol=hogp/expected0000000` to override, or rebuild groundhog node \
             against a matching groundhog-xdr"
        );
    }

    #[test]
    fn verbose_tracing_expands_default_service_logs() {
        assert_eq!(
            default_tracing_filter(true),
            "groundhog_node_supervisor=info,hogd_dev=info"
        );
        assert_eq!(
            default_tracing_filter(false),
            "groundhog_node_supervisor=warn,hogd_dev=warn"
        );
    }

    #[test]
    fn hogd_dev_startup_payload_includes_readiness_endpoints() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler_coord":"127.0.0.1:12345","indexer_block_explorer":"127.0.0.1:12346","db_gateway":"127.0.0.1:12347","view_gateway":"127.0.0.1:12348","dev_control":"127.0.0.1:12349"}}"#,
        )
        .expect("bug: readiness should parse");

        let local_runtime_state = Ok(test_local_runtime_state_result());
        let payload = hogd_dev_startup_payload(
            addr,
            &readiness,
            BackendPlanes::hogd_dev_scheduler_only(),
            &local_runtime_state,
            Some(std::path::Path::new("/tmp/hog-devnode-data")),
        );
        assert_eq!(payload["rpc_url"], "http://127.0.0.1:9545");
        assert_eq!(payload["rpc_listen_address"], "127.0.0.1:9545");
        assert_eq!(payload["chain"], "00".repeat(32));
        assert!(payload["chain_state"].is_null());
        assert_eq!(payload["backend"]["kind"], "hogd_dev");
        let expected_planes = serde_json::to_value(BackendPlanes::hogd_dev_scheduler_only())
            .expect("bug: backend planes should serialize");
        assert_eq!(payload["backend"]["planes"], expected_planes);
        assert!(payload["backend"].get("limitations").is_none());
        assert_eq!(
            payload["backend"]["endpoints"]["scheduler_coord"],
            "127.0.0.1:12345"
        );
        assert_eq!(
            payload["backend"]["endpoints"]["indexer_block_explorer"],
            "127.0.0.1:12346"
        );
        assert_eq!(
            payload["backend"]["endpoints"]["db_gateway"],
            "127.0.0.1:12347"
        );
        assert_eq!(
            payload["backend"]["endpoints"]["view_gateway"],
            "127.0.0.1:12348"
        );
        assert_eq!(
            payload["backend"]["endpoints"]["dev_control"],
            "127.0.0.1:12349"
        );
        assert!(payload["runtime_manifest"].is_null());
        assert_eq!(payload["local_runtime_state"]["ok"], true);
        assert_eq!(payload["data_dir"], "/tmp/hog-devnode-data");
        assert_eq!(
            payload["local_runtime_state"]["path"],
            "/tmp/groundhog-config.toml"
        );
        assert!(payload.get("accounts").is_none());
    }

    #[test]
    fn local_runtime_state_payload_reports_write_failure() {
        let payload =
            local_runtime_state_payload(&Err(CliError::Other("config is read-only".into())));

        assert_eq!(payload["ok"], false);
        assert_eq!(payload["error"], "config is read-only");
        assert!(payload.get("path").is_none());
    }

    #[test]
    fn hogd_dev_startup_payload_includes_runtime_manifest_pointer() {
        let temp = TempDir::new().expect("bug: temp dir should be created");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))
                .expect("bug: temp dir permissions should be set");
        }
        let manifest_path = temp.path().join("runtime-manifest.json");
        let manifest = br#"{
          "schemaVersion": 3,
          "kind": "hogd-dev-runtime",
          "chainState": "fresh",
          "networkHash": "0000000000000000000000000000000000000000000000000000000000000000",
          "protocol": "hogp/test00000000000",
          "genesisHash": "0x4242424242424242424242424242424242424242424242424242424242424242",
          "producer": {"binary": "hog-devnode", "version": "0.1.0", "commitHash": "abcdef0"},
          "endpoints": {"scheduler": "127.0.0.1:12345", "schedulerGateway": "127.0.0.1:12346"},
          "bootstrapAccountDerivation": {
            "scheme": "groundhog-bootstrap-account-v1",
            "seed": "groundhog-dev-accounts"
          },
          "bootstrapAccounts": [],
          "bootstrap": {
            "storageCostModel": {
              "hash": "0x5353535353535353535353535353535353535353535353535353535353535353",
              "baseDataItemCost": "12000",
              "bytestringCostPerByte": "10",
              "baseContractDeploymentCost": "12000",
              "contractDeploymentCostPerByte": "10",
              "authPerKeyCost": "1111",
              "rcqCostPerEntry": "8",
              "replayCacheCostPerEntry": "7",
              "balanceCost": "1234"
            }
          }
        }"#;
        fs::write(&manifest_path, manifest).expect("bug: manifest should be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600))
                .expect("bug: manifest permissions should be set");
        }
        let hash = format!("0x{}", hex::encode(Sha256::digest(manifest)));
        let pointer = serde_json::json!({
            "schemaVersion": 3,
            "status": "ready",
            "manifestPath": manifest_path,
            "manifestHash": hash,
        });
        let readiness = parse_readiness_line("hogd-dev", &pointer.to_string())
            .expect("bug: readiness pointer should parse");
        let local_runtime_state = Ok(test_local_runtime_state_result());
        let payload = hogd_dev_startup_payload(
            "127.0.0.1:9545".parse().expect("bug: valid socket addr"),
            &readiness,
            BackendPlanes::hogd_dev_scheduler_only(),
            &local_runtime_state,
            None,
        );

        assert_eq!(payload["schemaVersion"], NODE_STARTUP_SCHEMA_VERSION);
        assert_eq!(payload["chain_state"], "fresh");
        assert_eq!(payload["runtime_manifest"]["schemaVersion"], 3);
        assert_eq!(
            payload["runtime_manifest"]["path"],
            manifest_path.display().to_string()
        );
        assert_eq!(payload["runtime_manifest"]["hash"], hash);
    }

    #[test]
    fn hogd_dev_startup_payload_allows_scheduler_only_readiness() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler_coord":"127.0.0.1:12345"}}"#,
        )
        .expect("bug: v1 readiness should parse");

        let local_runtime_state = Ok(test_local_runtime_state_result());
        let payload = hogd_dev_startup_payload(
            addr,
            &readiness,
            BackendPlanes::hogd_dev_scheduler_only(),
            &local_runtime_state,
            None,
        );
        assert_eq!(
            payload["backend"]["endpoints"]["scheduler_coord"],
            "127.0.0.1:12345"
        );
        assert!(
            payload["backend"]["endpoints"]
                .get("indexer_block_explorer")
                .is_none()
        );
        assert!(payload["backend"]["endpoints"].get("db_gateway").is_none());
        assert!(
            payload["backend"]["endpoints"]
                .get("view_gateway")
                .is_none()
        );
        assert!(payload["backend"]["endpoints"].get("dev_control").is_none());
    }

    #[test]
    fn hogd_dev_startup_payload_reports_indexer_read_plane() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler_coord":"127.0.0.1:12345","indexer_block_explorer":"127.0.0.1:12346"}}"#,
        )
        .expect("bug: readiness should parse");

        let local_runtime_state = Ok(test_local_runtime_state_result());
        let payload = hogd_dev_startup_payload(
            addr,
            &readiness,
            BackendPlanes::hogd_dev_full(PlaneStatus::Stub),
            &local_runtime_state,
            None,
        );
        let expected_planes = serde_json::to_value(BackendPlanes::hogd_dev_full(PlaneStatus::Stub))
            .expect("bug: planes serialize");
        assert_eq!(payload["backend"]["planes"], expected_planes);
        assert!(payload["backend"].get("limitations").is_none());
    }
}

#[cfg(all(test, unix))]
#[path = "node_lifecycle_tests.rs"]
mod lifecycle_tests;
