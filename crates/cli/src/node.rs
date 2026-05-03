//! `groundhog node` — start the experimental local dev node.

use crate::{CliError, Print};

#[cfg(feature = "dev-node")]
use crate::result::CommandResult;
#[cfg(feature = "dev-node")]
use serde_json::json;

/// Start the experimental dev node server.
///
/// Requires the `dev-node` feature.
#[cfg(feature = "dev-node")]
pub fn run(
    port: u16,
    _account_count: usize,
    hogd_dev_binary: Option<std::path::PathBuf>,
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
    rt.block_on(async { run_hogd_dev(port, hogd_dev_binary, cmd, print).await })
}

#[cfg(feature = "dev-node")]
async fn run_hogd_dev(
    port: u16,
    binary: Option<std::path::PathBuf>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    use groundhog_node_supervisor::{
        HogdDevConfig, NodeSupervisorConfig, NodeSupervisorError, NodeSupervisorHandle,
        SupervisorStop,
    };

    let hogd = binary.map_or_else(HogdDevConfig::default, |binary| {
        HogdDevConfig::default().with_binary_path(binary)
    });
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address(format!("127.0.0.1:{port}"))
        .with_hogd_dev(hogd);

    let handle = NodeSupervisorHandle::start(config)
        .await
        .map_err(|err| CliError::NodeSupervisor(err.to_string()))?;
    let rpc_addr = handle.rpc_addr();
    let readiness = handle.hogd_readiness().clone();
    let backend_planes = handle.backend_planes();

    if print.is_json() {
        print.json(&CommandResult::new(
            cmd,
            hogd_dev_startup_payload(rpc_addr, &readiness, backend_planes),
        ))?;
    } else {
        print.status("Groundhog Local Node (experimental)");
        print.status(format_args!("Chain: {}", readiness.network_id));
        if backend_planes.read.is_indexer_backed() {
            print.warn("backend: hogd-dev (chainTip, submit, and reads live)");
        } else {
            print.warn(
                "backend: hogd-dev (chainTip and submit live; read methods return NOT_IMPLEMENTED)",
            );
        }
        print.status(format_args!("Listening on http://{rpc_addr}"));
        print.status(format_args!(
            "hogd-dev scheduler: {}",
            readiness.endpoints.scheduler
        ));
        if let Some(read) = readiness.endpoints.read {
            print.status(format_args!("hogd-dev read: {read}"));
        }
        if let Some(control) = readiness.endpoints.control {
            print.status(format_args!("hogd-dev control: {control}"));
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

    let outcome = handle
        .run_until(stop_signal)
        .await
        .map_err(|err| CliError::NodeSupervisor(err.to_string()))?;
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

#[cfg(not(feature = "dev-node"))]
pub fn run(
    _port: u16,
    _account_count: usize,
    _hogd_dev_binary: Option<std::path::PathBuf>,
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
        "accounts": [],
    })
}

#[cfg(feature = "dev-node")]
fn hogd_dev_endpoints_payload(
    readiness: &groundhog_node_supervisor::HogdDevReadiness,
) -> serde_json::Value {
    let mut endpoints = serde_json::Map::new();
    endpoints.insert(
        "scheduler".into(),
        serde_json::Value::String(readiness.endpoints.scheduler.to_string()),
    );
    if let Some(read) = readiness.endpoints.read {
        endpoints.insert("read".into(), serde_json::Value::String(read.to_string()));
    }
    if let Some(control) = readiness.endpoints.control {
        endpoints.insert(
            "control".into(),
            serde_json::Value::String(control.to_string()),
        );
    }
    serde_json::Value::Object(endpoints)
}

#[cfg(all(test, feature = "dev-node"))]
mod tests {
    use super::hogd_dev_startup_payload;
    use groundhog_node_supervisor::{BackendPlanes, parse_readiness_line};

    #[test]
    fn hogd_dev_startup_payload_includes_readiness_endpoints() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346","control":"127.0.0.1:12347"}}"#,
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
            payload["backend"]["endpoints"]["scheduler"],
            "127.0.0.1:12345"
        );
        assert_eq!(payload["backend"]["endpoints"]["read"], "127.0.0.1:12346");
        assert_eq!(
            payload["backend"]["endpoints"]["control"],
            "127.0.0.1:12347"
        );
        assert!(payload["accounts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hogd_dev_startup_payload_allows_scheduler_only_readiness() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345"}}"#,
        )
        .expect("bug: v1 readiness should parse");

        let payload =
            hogd_dev_startup_payload(addr, &readiness, BackendPlanes::hogd_dev_scheduler_only());
        assert_eq!(
            payload["backend"]["endpoints"]["scheduler"],
            "127.0.0.1:12345"
        );
        assert!(payload["backend"]["endpoints"].get("read").is_none());
        assert!(payload["backend"]["endpoints"].get("control").is_none());
    }

    #[test]
    fn hogd_dev_startup_payload_reports_indexer_read_plane() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346"}}"#,
        )
        .expect("bug: readiness should parse");

        let payload = hogd_dev_startup_payload(addr, &readiness, BackendPlanes::hogd_dev_full());
        let expected_planes =
            serde_json::to_value(BackendPlanes::hogd_dev_full()).expect("bug: planes serialize");
        assert_eq!(payload["backend"]["planes"], expected_planes);
        assert!(payload["backend"].get("limitations").is_none());
    }
}
