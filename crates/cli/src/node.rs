//! `groundhog node` — start the experimental hogd-dev local node.

use crate::{CliError, Print};

#[cfg(feature = "dev-node")]
use crate::result::CommandResult;
#[cfg(feature = "dev-node")]
use serde_json::json;
#[cfg(feature = "dev-node")]
use std::net::SocketAddr;
#[cfg(feature = "dev-node")]
use std::path::PathBuf;
#[cfg(feature = "dev-node")]
use std::process::ExitStatus;

/// Start the experimental hogd-dev local node server.
///
/// Requires the `dev-node` feature.
#[cfg(feature = "dev-node")]
pub fn run(
    port: u16,
    _account_count: usize,
    hogd_dev_binary: Option<PathBuf>,
    hogd_protocol: Option<String>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let default_filter = if print.is_json() {
        "groundhog_node_supervisor=warn"
    } else {
        "groundhog_node_supervisor=info"
    };
    init_tracing(default_filter);

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    rt.block_on(async {
        run_hogd_dev(
            LocalNodeOptions {
                port,
                hogd_dev_binary,
                hogd_protocol,
                ..LocalNodeOptions::default()
            },
            cmd,
            print,
        )
        .await
    })
}

/// Startup options for a local hogd-dev-backed node.
#[cfg(feature = "dev-node")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalNodeOptions {
    /// Public JSON-RPC port. Use `0` to ask the OS for a free port.
    pub port: u16,
    /// Explicit hogd-dev binary path.
    pub hogd_dev_binary: Option<PathBuf>,
    /// Override hogd protocol string for local cross-repo smoke tests.
    pub hogd_protocol: Option<String>,
    /// PID-file behavior for orphan detection.
    pub pid_file: LocalNodePidFile,
    /// Override the deterministic dev-account count.
    pub dev_account_count: Option<usize>,
}

/// PID-file behavior for local hogd-dev startup.
#[cfg(feature = "dev-node")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalNodePidFile {
    /// Use the standard cache-directory PID file.
    #[default]
    GlobalCache,
    /// Use an explicit PID-file path.
    Path(PathBuf),
    /// Do not configure a PID file.
    Disabled,
}

/// Running local node and the startup metadata callers need for orchestration.
#[cfg(feature = "dev-node")]
#[derive(Debug)]
#[must_use = "local nodes keep hogd-dev running until stop, stopped, or run_until consumes them"]
pub struct StartedLocalNode {
    handle: groundhog_node_supervisor::NodeSupervisorHandle,
    rpc_addr: SocketAddr,
    readiness: groundhog_node_supervisor::HogdDevReadiness,
    backend_planes: groundhog_node_supervisor::BackendPlanes,
}

#[cfg(feature = "dev-node")]
impl StartedLocalNode {
    /// Bound JSON-RPC listen address.
    pub fn rpc_addr(&self) -> SocketAddr {
        self.rpc_addr
    }

    /// HTTP JSON-RPC URL for the running local node.
    pub fn rpc_url(&self) -> String {
        format!("http://{}", self.rpc_addr)
    }

    /// Parsed hogd-dev readiness payload.
    pub fn readiness(&self) -> &groundhog_node_supervisor::HogdDevReadiness {
        &self.readiness
    }

    /// Backend plane status for startup reporting and feature gating.
    pub fn backend_planes(&self) -> groundhog_node_supervisor::BackendPlanes {
        self.backend_planes
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
#[cfg(feature = "dev-node")]
pub async fn start_local_node(options: LocalNodeOptions) -> Result<StartedLocalNode, CliError> {
    use groundhog_node_supervisor::NodeSupervisorHandle;

    let config = local_node_config(options)?;
    let handle = NodeSupervisorHandle::start(config)
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
    })
}

#[cfg(feature = "dev-node")]
async fn run_hogd_dev(
    options: LocalNodeOptions,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    use groundhog_node_supervisor::{NodeSupervisorError, SupervisorStop};

    let started = start_local_node(options).await?;

    if print.is_json() {
        print.json(&CommandResult::new(
            cmd,
            hogd_dev_startup_payload(
                started.rpc_addr(),
                started.readiness(),
                started.backend_planes(),
            ),
        ))?;
    } else {
        print.status("Groundhog Local Node (experimental)");
        print.status(format_args!("Chain: {}", started.readiness().network_id));
        if started.backend_planes().read.is_indexer_backed() {
            print.warn("backend: hogd-dev (chainTip, submit, and reads live)");
        } else {
            print.warn(
                "backend: hogd-dev (chainTip and submit live; read methods return NOT_IMPLEMENTED)",
            );
        }
        print.status(format_args!("Listening on {}", started.rpc_url()));
        print.status(format_args!(
            "hogd-dev SchedulerCoord: {}",
            started.readiness().endpoints.scheduler_coord
        ));
        if let Some(indexer) = started.readiness().endpoints.indexer_block_explorer {
            print.status(format_args!("hogd-dev IndexerBlockExplorer: {indexer}"));
        }
        if let Some(db_gateway) = started.readiness().endpoints.db_gateway {
            print.status(format_args!("hogd-dev DbGateway: {db_gateway}"));
        }
        if let Some(view_gateway) = started.readiness().endpoints.view_gateway {
            print.status(format_args!("hogd-dev ViewGateway: {view_gateway}"));
        }
        if let Some(dev_control) = started.readiness().endpoints.dev_control {
            print.status(format_args!("hogd-dev dev control: {dev_control}"));
        }
    }

    let stop_signal = async {
        tokio::signal::ctrl_c()
            .await
            .map_err(|source| NodeSupervisorError::StopSignal { source })?;
        if !print.is_json() {
            print.status("");
            print.status("Shutting down...");
        }
        Ok(())
    };

    let outcome = started.run_until(stop_signal).await?;
    match outcome {
        SupervisorStop::HogdExited(status) => {
            if !print.is_json() {
                print.status(format_args!("hogd-dev exited with status {status}"));
            }
            if status.success() {
                Ok(())
            } else {
                Err(CliError::NodeSupervisor(format!(
                    "hogd-dev exited with status {status}"
                )))
            }
        }
        SupervisorStop::RpcStopped(_) | SupervisorStop::StopRequested(_) => Ok(()),
        other => Err(CliError::NodeSupervisor(format!(
            "unexpected supervisor stop state: {other:?}"
        ))),
    }
}

#[cfg(feature = "dev-node")]
fn local_node_config(
    options: LocalNodeOptions,
) -> Result<groundhog_node_supervisor::NodeSupervisorConfig, CliError> {
    use groundhog_node_supervisor::{HogdDevConfig, NodeSupervisorConfig};

    let hogd = options
        .hogd_dev_binary
        .map_or_else(HogdDevConfig::default, |binary| {
            HogdDevConfig::default().with_binary_path(binary)
        });
    let hogd = match options.pid_file {
        LocalNodePidFile::GlobalCache => {
            hogd.with_pid_file(crate::config::cache_dir()?.join("hogd-dev.pid"))
        }
        LocalNodePidFile::Path(path) => hogd.with_pid_file(path),
        LocalNodePidFile::Disabled => hogd,
    };
    let hogd = match options.dev_account_count {
        Some(count) => hogd.with_dev_account_count(count),
        None => hogd,
    };
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address(format!("127.0.0.1:{}", options.port))
        .with_hogd_dev(hogd);
    Ok(match options.hogd_protocol {
        Some(protocol) => config.with_hogd_protocol_override(protocol),
        None => config,
    })
}

#[cfg(feature = "dev-node")]
fn map_node_supervisor_error(err: &groundhog_node_supervisor::NodeSupervisorError) -> CliError {
    let message = err.to_string();
    match err {
        groundhog_node_supervisor::NodeSupervisorError::LocalNodeAlreadyRunning { .. } => {
            CliError::LocalNodeAlreadyRunning(message)
        }
        _ => CliError::NodeSupervisor(message),
    }
}

#[cfg(not(feature = "dev-node"))]
pub fn run(
    _port: u16,
    _account_count: usize,
    _hogd_dev_binary: Option<std::path::PathBuf>,
    _hogd_protocol: Option<String>,
    _cmd: &'static str,
    _print: &Print,
) -> Result<(), CliError> {
    Err(CliError::Other(
        "`groundhog node` requires the dev-node feature.\n\n  \
         hint: install with `groundhogup` or build with:\n        \
         cargo install groundhog-cli --features dev-node"
            .into(),
    ))
}

#[cfg(feature = "dev-node")]
fn init_tracing(default_filter: &str) {
    use tracing_subscriber::EnvFilter;

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .try_init();
}

#[cfg(feature = "dev-node")]
fn hogd_dev_startup_payload(
    addr: std::net::SocketAddr,
    readiness: &groundhog_node_supervisor::HogdDevReadiness,
    backend_planes: groundhog_node_supervisor::BackendPlanes,
) -> serde_json::Value {
    json!({
        "chain": readiness.network_id,
        "mode": "dev",
        "experimental": true,
        "backend": {
            "kind": "hogd_dev",
            "stability": "experimental",
            "planes": backend_planes,
            "endpoints": hogd_dev_endpoints_payload(readiness),
        },
        "rpc_url": format!("http://{addr}"),
    })
}

#[cfg(feature = "dev-node")]
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

#[cfg(all(test, feature = "dev-node"))]
mod tests {
    use super::{LocalNodeOptions, LocalNodePidFile, hogd_dev_startup_payload, local_node_config};
    use groundhog_node_supervisor::{BackendPlanes, PlaneStatus, parse_readiness_line};
    use std::path::PathBuf;

    #[test]
    fn local_node_config_sets_supervisor_startup_options() {
        let binary = PathBuf::from("/tmp/hogd-dev-test");
        let config = local_node_config(LocalNodeOptions {
            port: 0,
            hogd_dev_binary: Some(binary.clone()),
            hogd_protocol: Some("test-protocol".into()),
            pid_file: LocalNodePidFile::Path(PathBuf::from("/tmp/hogd-dev-test.pid")),
            dev_account_count: Some(3),
        })
        .expect("bug: local node config should build");

        assert_eq!(config.rpc_listen_address, "127.0.0.1:0");
        assert_eq!(config.hogd_dev.binary_path, Some(binary));
        assert_eq!(
            config.hogd_protocol_override.as_deref(),
            Some("test-protocol")
        );
        assert_eq!(
            config.hogd_dev.pid_file.as_deref(),
            Some(std::path::Path::new("/tmp/hogd-dev-test.pid"))
        );
        assert_eq!(config.hogd_dev.dev_account_count, 3);
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
    fn hogd_dev_startup_payload_includes_readiness_endpoints() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler_coord":"127.0.0.1:12345","indexer_block_explorer":"127.0.0.1:12346","db_gateway":"127.0.0.1:12347","view_gateway":"127.0.0.1:12348","dev_control":"127.0.0.1:12349"}}"#,
        )
        .expect("bug: readiness should parse");

        let payload =
            hogd_dev_startup_payload(addr, &readiness, BackendPlanes::hogd_dev_scheduler_only());
        assert_eq!(payload["rpc_url"], "http://127.0.0.1:9545");
        assert_eq!(payload["chain"], "groundhog-local");
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
        assert!(payload.get("accounts").is_none());
    }

    #[test]
    fn hogd_dev_startup_payload_allows_scheduler_only_readiness() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler_coord":"127.0.0.1:12345"}}"#,
        )
        .expect("bug: v1 readiness should parse");

        let payload =
            hogd_dev_startup_payload(addr, &readiness, BackendPlanes::hogd_dev_scheduler_only());
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
            r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler_coord":"127.0.0.1:12345","indexer_block_explorer":"127.0.0.1:12346"}}"#,
        )
        .expect("bug: readiness should parse");

        let payload = hogd_dev_startup_payload(
            addr,
            &readiness,
            BackendPlanes::hogd_dev_full(PlaneStatus::Stub),
        );
        let expected_planes = serde_json::to_value(BackendPlanes::hogd_dev_full(PlaneStatus::Stub))
            .expect("bug: planes serialize");
        assert_eq!(payload["backend"]["planes"], expected_planes);
        assert!(payload["backend"].get("limitations").is_none());
    }
}
