//! Opt-in loopback candidate endpoint for local native RPC qualification.
//! Transport defaults differ from production; this endpoint qualifies contracts,
//! not gateway capacity or batch scheduling.

use std::{marker::PhantomData, net::SocketAddr, time::Duration};

use jsonrpsee::{
    RpcModule,
    core::RegisterMethodError,
    server::{Server, ServerConfig, ServerHandle},
};
use thruput_json_rpc_server::{
    RpcServerConfig,
    native::service::{NativeCapabilities, NativeService},
};
use thruput_json_rpc_types::NetworkHash;

use super::NodeSupervisorHandle;
use crate::{
    HogdDevEndpoints, HogdDevReadiness, NodeSupervisorError, read_backend::HogdDevReadBackend,
    rpc_backend::HogdDevSchedulerBackend, view_backend::HogdDevViewBackend,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT_MS: u64 = 30_000;

/// Failure to start a local qualification endpoint. Normal RPC keeps running.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NativeQualificationError {
    /// Required configuration is absent; no backend connections were attempted.
    #[error("native qualification requires {0}")]
    Missing(&'static str),
    /// Existing adapter validation or transport setup failed.
    #[error(transparent)]
    Backend(#[from] NodeSupervisorError),
    /// Connecting and validating the configured services exceeded the startup bound.
    #[error("native qualification startup exceeded {STARTUP_TIMEOUT:?}")]
    StartupTimeout,
    /// The native catalog could not be registered.
    #[error("native qualification registration failed: {0}")]
    Registration(#[from] RegisterMethodError),
    /// The ephemeral loopback listener could not be created.
    #[error("native qualification listener failed: {0}")]
    Listener(#[from] std::io::Error),
}

/// Owns only the candidate listener. The borrowed supervisor owns the runtime
/// and its existing block driver. Drop signals listener shutdown without waiting.
/// Call [`Self::stop`] to drain requests before stopping the node.
#[derive(Debug)]
pub struct NativeQualificationHandle<'node> {
    server: ServerHandle,
    addr: SocketAddr,
    node: PhantomData<&'node NodeSupervisorHandle>,
}

impl NativeQualificationHandle<'_> {
    /// Actual loopback address, with an operating-system assigned port.
    pub const fn rpc_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop accepting requests and wait for active requests to finish.
    #[tracing::instrument(skip_all)]
    pub async fn stop(self) {
        let _ = self.server.stop();
        self.server.clone().stopped().await;
    }
}

impl Drop for NativeQualificationHandle<'_> {
    fn drop(&mut self) {
        let _ = self.server.stop();
    }
}

impl NodeSupervisorHandle {
    /// Start the implemented native methods on a separate ephemeral loopback
    /// listener. This requires a fully configured running local node and does
    /// not alter its wire-13 endpoint. Unimplemented methods are absent.
    ///
    /// The validated runtime readiness supplies every backend endpoint. The
    /// candidate reuses the node's block driver and does not claim bundle release
    /// identity. The supplied configuration controls candidate limits independently
    /// of the wire-13 listener. An omitted request timeout becomes 30 seconds.
    #[tracing::instrument(skip_all)]
    pub async fn start_native_qualification(
        &self,
        mut config: RpcServerConfig,
    ) -> Result<NativeQualificationHandle<'_>, NativeQualificationError> {
        let readiness = self.hogd.readiness();
        require_endpoints(readiness)?;
        let driver = self
            .block_driver
            .as_ref()
            .ok_or(NativeQualificationError::Missing("a running block driver"))?;
        if config.request_timeout_ms.is_none() {
            config = config.with_request_timeout_ms(REQUEST_TIMEOUT_MS);
        }
        let connect = async {
            let (scheduler, read, view) = connect_backends(readiness, &config).await?;
            let scheduler = scheduler.with_block_driver(driver.trigger());
            let module = NativeService::new(
                scheduler.clone(),
                scheduler,
                read,
                view,
                NetworkHash::from(readiness.network_hash.to_network_hash().into_inner()),
                config,
                thruput_json_rpc_server::build_version!(),
                NativeCapabilities::new(true, true, true, true),
            )
            .into_module()?
            .into_rpc();
            bind(module).await
        };
        tokio::time::timeout(STARTUP_TIMEOUT, connect)
            .await
            .map_err(|_| NativeQualificationError::StartupTimeout)?
    }
}

fn require_endpoints(readiness: &HogdDevReadiness) -> Result<(), NativeQualificationError> {
    validated_protocol(readiness)?;
    if readiness.runtime_manifest.is_none() {
        return Err(NativeQualificationError::Missing(
            "a validated runtime manifest",
        ));
    }
    require_services(&readiness.endpoints)
}

fn validated_protocol(readiness: &HogdDevReadiness) -> Result<&str, NativeQualificationError> {
    readiness
        .protocol
        .as_deref()
        .ok_or(NativeQualificationError::Missing(
            "a validated runtime protocol",
        ))
}

fn require_services(endpoints: &HogdDevEndpoints) -> Result<(), NativeQualificationError> {
    for (name, endpoint) in [
        ("schedulerGateway", endpoints.scheduler_gateway),
        ("indexerBlockExplorer", endpoints.indexer_block_explorer),
        ("dbGateway", endpoints.db_gateway),
        ("viewGateway", endpoints.view_gateway),
    ] {
        required_endpoint(name, endpoint)?;
    }
    Ok(())
}

fn required_endpoint(
    name: &'static str,
    endpoint: Option<SocketAddr>,
) -> Result<SocketAddr, NativeQualificationError> {
    endpoint.ok_or(NativeQualificationError::Missing(name))
}

async fn connect_backends(
    readiness: &HogdDevReadiness,
    config: &RpcServerConfig,
) -> Result<
    (
        HogdDevSchedulerBackend,
        HogdDevReadBackend,
        HogdDevViewBackend,
    ),
    NativeQualificationError,
> {
    let HogdDevEndpoints {
        scheduler_coord,
        scheduler_gateway,
        indexer_block_explorer,
        db_gateway,
        view_gateway,
        ..
    } = readiness.endpoints;
    let protocol = validated_protocol(readiness)?;
    let scheduler = HogdDevSchedulerBackend::connect(
        scheduler_coord,
        required_endpoint("schedulerGateway", scheduler_gateway)?,
        protocol,
        Some(protocol),
    )
    .await?;
    scheduler
        .validate_running_network(&readiness.network_hash)
        .await?;
    let read = HogdDevReadBackend::connect(
        required_endpoint("indexerBlockExplorer", indexer_block_explorer)?,
        db_gateway,
        scheduler.scheduler_gateway_client().clone(),
        protocol,
        Some(protocol),
    )
    .await?;
    let scheduler = scheduler
        .with_indexer(read.indexer_client().clone())
        .with_db_gateway(
            read.db_gateway_client().cloned(),
            readiness.network_hash.clone(),
        );
    let view = HogdDevViewBackend::connect_optional(
        view_gateway,
        protocol,
        Some(protocol),
        Duration::from_millis(config.view_timeout_ms),
    )
    .await?;
    Ok((scheduler, read, view))
}

async fn bind<'node, T: Send + Sync + 'static>(
    module: RpcModule<T>,
) -> Result<NativeQualificationHandle<'node>, NativeQualificationError> {
    let config = ServerConfig::builder()
        .set_batch_request_config(jsonrpsee::server::BatchRequestConfig::Limit(
            thruput_json_rpc_server::batch::MAX_RPC_BATCH_REQUESTS,
        ))
        .build();
    let server = Server::builder()
        .set_config(config)
        .build((std::net::Ipv4Addr::LOCALHOST, 0))
        .await?;
    let addr = server.local_addr()?;
    Ok(NativeQualificationHandle {
        server: server.start(module),
        addr,
        node: PhantomData,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_required_service_is_checked_before_connecting() {
        let endpoints: HogdDevEndpoints = serde_json::from_value(serde_json::json!({
            "scheduler_coord": "127.0.0.1:1",
            "scheduler_gateway": "127.0.0.1:2",
            "indexer_block_explorer": "127.0.0.1:3",
            "db_gateway": "127.0.0.1:4",
            "view_gateway": "127.0.0.1:5"
        }))
        .unwrap();
        assert!(require_services(&endpoints).is_ok());
        for name in [
            "schedulerGateway",
            "indexerBlockExplorer",
            "dbGateway",
            "viewGateway",
        ] {
            let mut missing = endpoints.clone();
            match name {
                "schedulerGateway" => missing.scheduler_gateway = None,
                "indexerBlockExplorer" => missing.indexer_block_explorer = None,
                "dbGateway" => missing.db_gateway = None,
                "viewGateway" => missing.view_gateway = None,
                _ => unreachable!(),
            }
            assert!(matches!(require_services(&missing),
                Err(NativeQualificationError::Missing(actual)) if actual == name));
        }
    }

    fn readiness() -> HogdDevReadiness {
        HogdDevReadiness {
            status: "ready".into(),
            chain_state: None,
            network_hash: thruput_json_rpc_types::NetworkHashText::from([0; 32]),
            protocol: Some("hogp/test00000000000".into()),
            endpoints: HogdDevEndpoints {
                scheduler_coord: "127.0.0.1:1".parse().unwrap(),
                scheduler_gateway: None,
                indexer_block_explorer: None,
                db_gateway: None,
                view_gateway: None,
                dev_control: None,
            },
            dev_accounts: vec![],
            runtime_manifest: None,
            manifest_path: None,
            manifest_hash: None,
        }
    }

    #[test]
    fn readiness_without_validated_manifest_cannot_start_candidate() {
        let readiness = readiness();
        assert!(matches!(
            require_endpoints(&readiness),
            Err(NativeQualificationError::Missing(
                "a validated runtime manifest"
            ))
        ));
    }

    #[test]
    fn missing_protocol_fails_preflight_before_connections() {
        let mut readiness = readiness();
        readiness.protocol = None;
        assert!(matches!(
            require_endpoints(&readiness),
            Err(NativeQualificationError::Missing(
                "a validated runtime protocol"
            ))
        ));
    }

    #[test]
    fn candidate_uses_validated_runtime_protocol_override() {
        let readiness = readiness();
        let selected = validated_protocol(&readiness).unwrap();
        assert_eq!(selected, "hogp/test00000000000");
        assert_ne!(
            selected,
            thruput_json_rpc_types::protocol_token::protocol_text(&thruput_xdr::PROTOCOL_VERSION)
        );
    }

    #[tokio::test]
    async fn listener_is_loopback_ephemeral_and_drop_stops_it() {
        let handle = bind(RpcModule::new(())).await.unwrap();
        assert!(handle.rpc_addr().ip().is_loopback());
        assert_ne!(handle.rpc_addr().port(), 0);
        let server = handle.server.clone();
        drop(handle);
        tokio::time::timeout(Duration::from_secs(1), server.stopped())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn explicit_stop_waits_for_listener_shutdown() {
        let handle = bind(RpcModule::new(())).await.unwrap();
        let server = handle.server.clone();
        handle.stop().await;
        assert!(server.is_stopped());
    }
}
