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
#[cfg(feature = "dev-node")]
use std::time::Duration;

/// Start the experimental hogd-dev local node server.
///
/// Requires the `dev-node` feature.
#[cfg(feature = "dev-node")]
#[allow(clippy::too_many_arguments)]
pub fn run(
    port: u16,
    verbose: bool,
    dev_account_count: Option<usize>,
    hogd_dev_binary: Option<PathBuf>,
    hogd_protocol: Option<String>,
    startup_timeout_secs: Option<u64>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    init_tracing(default_tracing_filter(verbose, print.is_json()));

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    rt.block_on(async {
        run_hogd_dev(
            LocalNodeOptions {
                port,
                hogd_dev_binary,
                hogd_protocol,
                startup_timeout: startup_timeout_secs.map(Duration::from_secs),
                dev_account_count,
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
    /// Override readiness timeout while waiting for hogd-dev.
    pub startup_timeout: Option<Duration>,
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
    let local_state_result = write_started_local_node_state(&started);
    let started_rpc_url = started.rpc_url();
    let started_manifest_path = started.readiness().manifest_path.clone();
    let local_state_path = local_state_result
        .as_ref()
        .ok()
        .and_then(|path| path.as_ref())
        .cloned();

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
        print_started_local_node(&started, &local_state_result, print);
    }

    let stop_signal = async {
        wait_for_stop_signal()
            .await
            .map_err(|source| NodeSupervisorError::StopSignal { source })?;
        if !print.is_json() {
            print.status("");
            print.status("Shutting down...");
        }
        Ok(())
    };

    let outcome = started.run_until(stop_signal).await;
    if let (Some(local_state_path), Some(started_manifest_path)) = (
        local_state_path.as_deref(),
        started_manifest_path.as_deref(),
    ) && let Err(error) = crate::runtime_manifest::remove_local_node_state_if_matches(
        local_state_path,
        &started_rpc_url,
        started_manifest_path,
    ) && !print.is_json()
    {
        print.warn(format!("local node state was not removed: {error}"));
    }
    let outcome = outcome?;
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
async fn wait_for_stop_signal() -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = sigterm.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

#[cfg(feature = "dev-node")]
fn print_started_local_node(
    started: &StartedLocalNode,
    local_state_result: &Result<Option<PathBuf>, CliError>,
    print: &Print,
) {
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
    if let Some(path) = &started.readiness().manifest_path {
        print.status(format_args!("runtime manifest: {}", path.display()));
        match local_state_result {
            Ok(Some(local_state_path)) => print.status(format_args!(
                "local node state: {}",
                local_state_path.display()
            )),
            Ok(None) => {}
            Err(error) => {
                print.warn(format!("local node state was not written: {error}"));
            }
        }
        print.status(format_args!(
            "dev-key manifest: export GROUNDHOG_RUNTIME_MANIFEST={}",
            path.display()
        ));
    }
}

#[cfg(feature = "dev-node")]
fn write_started_local_node_state(started: &StartedLocalNode) -> Result<Option<PathBuf>, CliError> {
    let manifest_path = started
        .readiness()
        .manifest_path
        .as_deref()
        .ok_or_else(|| {
            CliError::NodeSupervisor(
                "hogd-dev readiness did not include runtime manifest path".into(),
            )
        })?;
    let cwd = std::env::current_dir()?;
    crate::runtime_manifest::write_local_node_state(&cwd, &started.rpc_url(), manifest_path)
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
    let hogd = match options.startup_timeout {
        Some(timeout) => hogd.with_startup_timeout(timeout),
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

#[cfg(not(feature = "dev-node"))]
#[allow(clippy::too_many_arguments)]
pub fn run(
    _port: u16,
    _verbose: bool,
    _dev_account_count: Option<usize>,
    _hogd_dev_binary: Option<std::path::PathBuf>,
    _hogd_protocol: Option<String>,
    _startup_timeout_secs: Option<u64>,
    _cmd: &'static str,
    _print: &Print,
) -> Result<(), CliError> {
    Err(CliError::Other(
        "`groundhog node` requires the dev-node feature.\n\n  \
         hint: build the CLI with:\n        \
         cargo install --path crates/tools/cli --features dev-node"
            .into(),
    ))
}

#[cfg(feature = "dev-node")]
fn default_tracing_filter(verbose: bool, json: bool) -> &'static str {
    if verbose {
        "groundhog_node_supervisor=info,hogd_dev=info"
    } else if json {
        "groundhog_node_supervisor=warn"
    } else {
        "groundhog_node_supervisor=info"
    }
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
        "runtime_manifest": runtime_manifest_payload(readiness),
        "rpc_url": format!("http://{addr}"),
    })
}

#[cfg(feature = "dev-node")]
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
    use super::{
        LocalNodeOptions, LocalNodePidFile, default_tracing_filter, hogd_dev_startup_payload,
        local_node_config, map_node_supervisor_error,
    };
    use crate::CliError;
    use groundhog_node_supervisor::{
        BackendPlanes, NodeSupervisorError, PlaneStatus, parse_readiness_line,
    };
    use sha2::{Digest, Sha256};
    use std::{fs, path::PathBuf, time::Duration};
    use tempfile::TempDir;

    #[test]
    fn local_node_config_sets_supervisor_startup_options() {
        let binary = PathBuf::from("/tmp/hogd-dev-test");
        let config = local_node_config(LocalNodeOptions {
            port: 0,
            hogd_dev_binary: Some(binary.clone()),
            hogd_protocol: Some("test-protocol".into()),
            startup_timeout: Some(Duration::from_secs(30)),
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
        assert_eq!(config.hogd_dev.startup_timeout, Duration::from_secs(30));
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
    fn protocol_intro_rejection_maps_to_protocol_hint() {
        let err = NodeSupervisorError::ViewGatewayIntroRejected { outcome: 1 };
        let cli_error = map_node_supervisor_error(&err);

        assert!(matches!(cli_error, CliError::NodeSupervisor(_)));

        let err = NodeSupervisorError::ProtocolIntroRejected {
            service: "ViewGateway",
            outcome: 1,
            outcome_name: "ViewGatewayIntroMismatchedProtocol",
            is_protocol_mismatch: true,
            protocol: "hogp/sent".to_string(),
            expected_protocol: Some("hogp/expected".to_string()),
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
            "pass `--hogd-protocol hogp/expected` to override, or rebuild groundhog node \
             against a matching groundhog-xdr"
        );
    }

    #[test]
    fn verbose_tracing_filter_is_honored_with_json_output() {
        assert_eq!(
            default_tracing_filter(true, true),
            "groundhog_node_supervisor=info,hogd_dev=info"
        );
        assert_eq!(
            default_tracing_filter(true, false),
            "groundhog_node_supervisor=info,hogd_dev=info"
        );
        assert_eq!(
            default_tracing_filter(false, true),
            "groundhog_node_supervisor=warn"
        );
        assert_eq!(
            default_tracing_filter(false, false),
            "groundhog_node_supervisor=info"
        );
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
        assert!(payload["runtime_manifest"].is_null());
        assert!(payload.get("accounts").is_none());
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
          "schemaVersion": 1,
          "kind": "hogd-dev-runtime",
          "networkId": "groundhog-local",
          "protocol": "hogp/test",
          "genesisHash": "0x4242424242424242424242424242424242424242424242424242424242424242",
          "producer": {"binary": "hogd-dev", "version": "0.1.0", "commitHash": "abcdef0"},
          "endpoints": {"scheduler": "127.0.0.1:12345"},
          "bootstrapAccountDerivation": {
            "scheme": "groundhog-bootstrap-account-v1",
            "seed": "groundhog-dev-accounts"
          },
          "bootstrapAccounts": [],
          "bootstrap": {
            "devWallet": {
              "contentHash": "0x5151515151515151515151515151515151515151515151515151515151515151",
              "deployedCodeHash": "0x5252525252525252525252525252525252525252525252525252525252525252",
              "artifactHint": "./dev-wallet.wasm",
              "deployEntrypoint": "0xadac498222148e4f",
              "doActionEntrypoint": "0x646f416374696f6e"
            },
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
            "schemaVersion": 1,
            "status": "ready",
            "manifestPath": manifest_path,
            "manifestHash": hash,
        });
        let readiness = parse_readiness_line("hogd-dev", &pointer.to_string())
            .expect("bug: readiness pointer should parse");
        let payload = hogd_dev_startup_payload(
            "127.0.0.1:9545".parse().expect("bug: valid socket addr"),
            &readiness,
            BackendPlanes::hogd_dev_scheduler_only(),
        );

        assert_eq!(payload["runtime_manifest"]["schemaVersion"], 1);
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
