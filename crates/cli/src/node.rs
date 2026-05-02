//! `groundhog node` — start the experimental local dev node.

use crate::{CliError, Print};

#[cfg(feature = "dev-node")]
use crate::result::CommandResult;
#[cfg(feature = "dev-node")]
use groundhog_rpc_api_types::dev::DevAccount;
#[cfg(feature = "dev-node")]
use serde_json::json;

/// Start the experimental dev node server.
///
/// Requires the `dev-node` feature.
#[cfg(feature = "dev-node")]
pub fn run(
    port: u16,
    account_count: usize,
    hogd_dev_binary: Option<std::path::PathBuf>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let default_filter = if hogd_dev_binary.is_some() {
        if print.is_json() {
            "groundhog_node_supervisor=warn"
        } else {
            "groundhog_node_supervisor=info"
        }
    } else if print.is_json() {
        "groundhog_dev_node=warn"
    } else {
        "groundhog_dev_node=info"
    };
    init_tracing(default_filter);

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;

    rt.block_on(async {
        if let Some(binary) = hogd_dev_binary {
            return run_hogd_dev(port, binary, cmd, print).await;
        }

        run_native_exec(port, account_count, cmd, print).await
    })
}

#[cfg(feature = "dev-node")]
async fn run_native_exec(
    port: u16,
    account_count: usize,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    use groundhog_dev_node::{DevNodeConfig, start_server};

    let config = DevNodeConfig {
        listen_address: format!("127.0.0.1:{port}"),
        account_count,
        ..Default::default()
    };

    {
        let handle = start_server(config).await.map_err(map_start_error)?;

        if print.is_json() {
            print.json(&CommandResult::new(
                cmd,
                native_startup_payload(handle.addr, &handle.accounts),
            ))?;
        } else {
            // Print startup banner
            print.status("Groundhog Dev Node v0.1.0 (experimental, legacy prototype)");
            print.status("Chain: groundhog-local (dev mode)");
            print.warn(
                "prototype backend: native_exec/test_ffi. \
                 Latest-state reads are supported; snapshot, revert, \
                 and time travel are not yet supported.",
            );
            print.status("");
            print.status("Available Accounts");
            print.status("==================");
            for (i, account) in handle.accounts.iter().enumerate() {
                let addr = serde_json::to_string(&account.address)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string();
                print.status(format_args!(
                    "({i}) {addr}  ({} storage tokens)",
                    account.storage_tokens
                ));
            }
            print.status("");
            print.status("Private Keys");
            print.status("==================");
            for (i, account) in handle.accounts.iter().enumerate() {
                let key = format!("0x{}", hex::encode(&account.private_key.0));
                print.status(format_args!("({i}) {key}"));
            }
            print.status("");
            print.warn(
                "These keys are for LOCAL DEVELOPMENT ONLY. \
                 Do not use on a real network.",
            );
            print.status("");
            print.status(format_args!("Listening on http://{}", handle.addr));
        }

        // Block until Ctrl-C
        tokio::signal::ctrl_c()
            .await
            .map_err(|e| CliError::RuntimeInit(e.to_string()))?;

        let stats = handle.stats().await.ok();
        if !print.is_json() {
            print.status("");
            print.status("Shutting down...");
        }
        let _ = handle.stop();
        handle.stopped().await;
        if let Some(stats) = stats
            && !print.is_json()
        {
            print.status(format_args!(
                "Summary: {} blocks, {} committed transactions",
                stats.block_count, stats.tx_count
            ));
        }
        Ok(())
    }
}

#[cfg(feature = "dev-node")]
async fn run_hogd_dev(
    port: u16,
    binary: std::path::PathBuf,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    use groundhog_node_supervisor::{
        HogdDevConfig, NodeSupervisorConfig, NodeSupervisorError, NodeSupervisorHandle,
        SupervisorStop,
    };

    let hogd = HogdDevConfig::default().with_binary_path(binary);
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
        if backend_planes.read.serves_committed_reads() {
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
fn map_start_error(err: Box<dyn std::error::Error + Send + Sync>) -> CliError {
    use groundhog_dev_node::error::DevNodeError;

    match err.downcast::<DevNodeError>() {
        Ok(devnode) => match *devnode {
            DevNodeError::SandboxInit(msg) => CliError::DevNodeSandbox(msg),
            other => CliError::DevNode(other.to_string()),
        },
        Err(boxed) => CliError::DevNode(boxed.to_string()),
    }
}

#[cfg(feature = "dev-node")]
fn native_startup_payload(
    addr: std::net::SocketAddr,
    accounts: &[DevAccount],
) -> serde_json::Value {
    json!({
        "chain": "groundhog-local",
        "mode": "dev",
        "experimental": true,
        "backend": {
            "kind": "native_exec_test_ffi",
            "stability": "prototype",
            "limitations": [
                "latest-state reads only",
                "snapshot/revert not supported",
                "time travel not supported"
            ],
        },
        "rpc_url": format!("http://{addr}"),
        "accounts": accounts,
    })
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
            "limitations": hogd_dev_limitations(backend_planes),
            "endpoints": hogd_dev_endpoints_payload(readiness),
        },
        "rpc_url": format!("http://{addr}"),
        "accounts": [],
    })
}

#[cfg(feature = "dev-node")]
fn hogd_dev_limitations(
    backend_planes: groundhog_node_supervisor::BackendPlanes,
) -> Vec<&'static str> {
    let mut limitations = vec![
        "snapshot/revert not supported",
        "time travel not supported",
        "state is ephemeral; tmp dirs are removed on shutdown",
    ];
    if backend_planes.read.returns_not_implemented() {
        limitations.push("read methods return NOT_IMPLEMENTED until hogd-dev exposes read");
    }
    limitations
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
    use super::{hogd_dev_startup_payload, map_start_error, native_startup_payload};
    use crate::CliError;
    use groundhog_dev_node::error::DevNodeError;
    use groundhog_node_supervisor::{BackendPlanes, parse_readiness_line};
    use groundhog_rpc_api_types::dev::DevAccount;
    use groundhog_rpc_api_types::hex::{Hex0x, HexBytes};
    use groundhog_xdr::types::ContractAddress;

    fn backend_limitations(payload: &serde_json::Value) -> &Vec<serde_json::Value> {
        payload["backend"]["limitations"]
            .as_array()
            .expect("bug: limitations should be an array")
    }

    fn has_backend_limitation(payload: &serde_json::Value, expected: &str) -> bool {
        backend_limitations(payload)
            .iter()
            .any(|value| value.as_str().is_some_and(|text| text == expected))
    }

    #[test]
    fn map_start_error_routes_sandbox_init_to_typed_variant() {
        let err: Box<dyn std::error::Error + Send + Sync> =
            Box::new(DevNodeError::SandboxInit("boom".into()));
        match map_start_error(err) {
            CliError::DevNodeSandbox(msg) => assert_eq!(msg, "boom"),
            other => panic!("expected DevNodeSandbox, got {other:?}"),
        }
    }

    #[test]
    fn map_start_error_routes_other_devnode_errors_to_devnode_variant() {
        let err: Box<dyn std::error::Error + Send + Sync> =
            Box::new(DevNodeError::WorkerInit("bad".into()));
        match map_start_error(err) {
            CliError::DevNode(msg) => assert!(msg.contains("bad")),
            other => panic!("expected DevNode, got {other:?}"),
        }
    }

    #[test]
    fn map_start_error_falls_back_for_non_devnode_errors() {
        let err: Box<dyn std::error::Error + Send + Sync> = "io oops".into();
        match map_start_error(err) {
            CliError::DevNode(msg) => assert_eq!(msg, "io oops"),
            other => panic!("expected DevNode fallback, got {other:?}"),
        }
    }

    #[test]
    fn startup_payload_includes_rpc_url_and_accounts() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let account = DevAccount {
            address: Hex0x(ContractAddress::from([7_u8; 32])),
            public_key: HexBytes::from(vec![8_u8; 32]),
            private_key: HexBytes::from(vec![9_u8; 32]),
            storage_tokens: 42,
        };

        let payload = native_startup_payload(addr, &[account]);
        assert_eq!(payload["rpc_url"], "http://127.0.0.1:9545");
        assert_eq!(payload["experimental"], true);
        assert_eq!(payload["backend"]["kind"], "native_exec_test_ffi");
        assert_eq!(
            payload["accounts"]
                .as_array()
                .expect("bug: accounts array")
                .len(),
            1
        );
    }

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
        assert!(has_backend_limitation(
            &payload,
            "snapshot/revert not supported"
        ));
        assert!(has_backend_limitation(
            &payload,
            "time travel not supported"
        ));
        assert!(has_backend_limitation(
            &payload,
            "state is ephemeral; tmp dirs are removed on shutdown"
        ));
        assert!(backend_limitations(&payload).iter().any(|value| {
            value
                .as_str()
                .is_some_and(|text| text.contains("NOT_IMPLEMENTED"))
        }));
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
    fn hogd_dev_startup_payload_omits_read_stub_limitation_for_indexer_plane() {
        let addr = "127.0.0.1:9545".parse().expect("bug: valid socket addr");
        let readiness = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346"}}"#,
        )
        .expect("bug: readiness should parse");

        let payload = hogd_dev_startup_payload(addr, &readiness, BackendPlanes::hogd_dev_full());
        assert!(has_backend_limitation(
            &payload,
            "snapshot/revert not supported"
        ));
        assert!(has_backend_limitation(
            &payload,
            "time travel not supported"
        ));
        assert!(has_backend_limitation(
            &payload,
            "state is ephemeral; tmp dirs are removed on shutdown"
        ));
        assert!(backend_limitations(&payload).iter().all(|value| {
            !value
                .as_str()
                .is_some_and(|text| text.contains("NOT_IMPLEMENTED"))
        }));
    }
}
