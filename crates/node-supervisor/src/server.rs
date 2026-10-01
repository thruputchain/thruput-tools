//! Top-level node supervisor orchestration.
//!
//! Owns hogd-dev process supervision and the public JSON-RPC server. Backend
//! adapters land by plane: chain and submit are backed by hogd-dev's
//! `SchedGateway` endpoint; receipt/event reads are backed by hogd-dev's
//! `IndexerBlockExplorer` endpoint when advertised in readiness, and
//! latest-state storage reads can be backed by hogd-dev's `DbGateway`
//! endpoint independently.

use std::{future::Future, io, net::SocketAddr, process::ExitStatus};

use groundhog_bundle_identity::BundleIdentity;
use groundhog_json_rpc_api::{AirdropApiServer, GroundhogApiServer};
use groundhog_json_rpc_server::batch::{ConcurrentBatchService, MAX_RPC_BATCH_REQUESTS};
use groundhog_json_rpc_server::{BuildInfo, GroundhogRpcServer, RpcServerConfig};
use groundhog_json_rpc_types::protocol_token::protocol_text;
use groundhog_json_rpc_types::{NetworkHash, network_info::NetworkInfo};
use groundhog_xdr::PROTOCOL_VERSION;
use jsonrpsee::server::{RpcModule, Server, ServerConfig, ServerHandle};
use serde::Serialize;

use crate::{
    BlockDriverConfig, HogdDevProcess, HogdDevReadiness, NodeSupervisorConfig, NodeSupervisorError,
    airdrop::{LocalAirdropContext, LocalAirdropRpc},
    block_driver::{BlockDriverTask, ManualBlockProducer},
    hogd_dev::{DEFAULT_HOGD_DEV_BINARY, validate_dev_accounts, validate_runtime_identity},
    read_backend::{HogdDevReadBackend, HogdDevStorageReadBackend},
    rpc_backend::HogdDevSchedulerBackend,
    stub_backend::StubBackend,
    view_backend::HogdDevViewBackend,
};

/// Why a [`NodeSupervisorHandle::stopped`] call returned.
///
/// Callers (CLI exit codes, supervisors-of-supervisors) need to distinguish
/// "hogd-dev died on us" from "we were asked to stop and hogd-dev is now
/// down because we asked it to". Both branches return an `ExitStatus`, but
/// the meaning is different.
#[derive(Debug)]
#[non_exhaustive]
pub enum SupervisorStop {
    /// hogd-dev exited on its own; the supervisor reacted by stopping the
    /// JSON-RPC server. The contained status is hogd-dev's own exit status.
    HogdExited(ExitStatus),
    /// The JSON-RPC server stopped (typically via an external `stop()`
    /// call), and the supervisor then shut hogd-dev down with the
    /// configured grace timeout. The contained status is hogd-dev's exit
    /// status from that shutdown.
    RpcStopped(ExitStatus),
    /// An external stop signal was received, and the supervisor then shut
    /// hogd-dev down with the configured grace timeout. The contained status
    /// is hogd-dev's exit status from that requested shutdown.
    StopRequested(ExitStatus),
}

/// Backend implementation currently serving a public RPC plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlaneStatus {
    /// The public plane is backed by hogd-dev's `SchedGateway` x23 endpoint.
    /// The serialized value remains `scheduler` for compatibility.
    Scheduler,
    /// The plane is backed by hogd-dev's `IndexerBlockExplorer` x23 endpoint.
    Indexer,
    /// The plane is backed by hogd-dev's `DbGateway` x23 endpoint.
    DbGateway,
    /// The plane is backed by hogd-dev's `ViewGateway` x23 endpoint.
    ViewGateway,
    /// The plane is intentionally served by the supervisor stub.
    Stub,
}

impl PlaneStatus {
    /// Returns true when this plane is backed by hogd-dev's `IndexerBlockExplorer` endpoint.
    pub const fn is_indexer_backed(self) -> bool {
        match self {
            Self::Indexer => true,
            Self::Scheduler | Self::DbGateway | Self::ViewGateway | Self::Stub => false,
        }
    }
}

/// Backend plane status advertised by the node supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BackendPlanes {
    /// Transaction submission plane.
    pub submit: PlaneStatus,
    /// Chain metadata plane.
    pub chain: PlaneStatus,
    /// Committed-state read plane.
    pub read: PlaneStatus,
    /// Read-only contract execution plane.
    pub view: PlaneStatus,
}

impl BackendPlanes {
    /// Current hogd-dev v1 wiring: scheduler-backed submit/chain with stubbed reads.
    pub const fn hogd_dev_scheduler_only() -> Self {
        Self {
            submit: PlaneStatus::Scheduler,
            chain: PlaneStatus::Scheduler,
            read: PlaneStatus::Stub,
            view: PlaneStatus::Stub,
        }
    }

    /// All three planes backed by hogd-dev's scheduler and indexer x23 endpoints.
    pub const fn hogd_dev_full(view: PlaneStatus) -> Self {
        Self {
            submit: PlaneStatus::Scheduler,
            chain: PlaneStatus::Scheduler,
            read: PlaneStatus::Indexer,
            view,
        }
    }

    /// Scheduler-backed submit/chain with DbGateway-backed latest storage reads.
    pub const fn hogd_dev_storage_only(view: PlaneStatus) -> Self {
        Self {
            submit: PlaneStatus::Scheduler,
            chain: PlaneStatus::Scheduler,
            read: PlaneStatus::DbGateway,
            view,
        }
    }
}

/// Running node supervisor.
///
/// Owns the supervised hogd-dev child and the JSON-RPC server. The handle
/// does no proactive supervision in the background: callers are responsible
/// for awaiting [`NodeSupervisorHandle::stopped`] (the supervisor-level
/// "run until something ends" entry point) or driving [`NodeSupervisorHandle::stop`]
/// directly. Both consume the handle so a single instance cannot be torn
/// down twice.
#[derive(Debug)]
pub struct NodeSupervisorHandle {
    hogd: HogdDevProcess,
    rpc_handle: ServerHandle,
    rpc_addr: SocketAddr,
    backend_planes: BackendPlanes,
    manual_block_producer: Option<ManualBlockProducer>,
    block_driver: Option<BlockDriverTask>,
}

impl NodeSupervisorHandle {
    /// Spawn hogd-dev, wait for readiness, connect the scheduler x23 client,
    /// and start the JSON-RPC server.
    ///
    /// On any failure after hogd-dev has started, the child is stopped before
    /// the error is returned, so a failed `start` does not leave a hogd-dev
    /// process behind.
    #[tracing::instrument(skip_all, fields(rpc_listen = %config.rpc_listen_address))]
    pub async fn start(config: NodeSupervisorConfig) -> Result<Self, NodeSupervisorError> {
        Self::start_with_inherited_file(config, None).await
    }

    /// Start a node with one inherited file descriptor, such as a locked bundle
    /// lease. The child and its descendants retain the open file description
    /// independently of this handle and process, including after SIGKILL.
    /// The executable must preserve inherited descriptors for this guarantee.
    pub async fn start_with_inherited_file(
        config: NodeSupervisorConfig,
        inherited_file: Option<std::fs::File>,
    ) -> Result<Self, NodeSupervisorError> {
        if let Some(identity) = &config.bundle_identity {
            tracing::info!(
                component_version = identity.build_version().crate_version(),
                source_commit = ?identity.build_version().source_commit(),
                release_version = ?identity.release_version(),
                bundle_manifest_digest = ?identity.bundle_manifest_digest(),
                bundle_manifest_mismatch_reason = ?identity.mismatch_reason(),
                "Groundhog release identity",
            );
        }
        config
            .local_airdrop
            .validate()
            .map_err(|reason| NodeSupervisorError::InvalidLocalAirdropConfig { reason })?;
        let mut hogd =
            HogdDevProcess::start_with_inherited_file(&config.hogd_dev, inherited_file).await?;

        // Text, not the wire array: this is checked against the runtime
        // manifest's `protocol` JSON field and carried in error messages.
        let default_protocol = protocol_text(&PROTOCOL_VERSION);
        let protocol_version = config
            .hogd_protocol_override
            .as_deref()
            .unwrap_or(&default_protocol);

        match validate_runtime_identity(hogd.readiness(), protocol_version)
            .and_then(|()| validate_dev_accounts(hogd.readiness(), &config.hogd_dev))
        {
            Ok(()) => {}
            Err(error) => {
                if let Err(stop_err) = hogd.stop().await {
                    tracing::warn!(
                        error = %stop_err,
                        "failed to stop hogd-dev after readiness validation error",
                    );
                }
                return Err(error);
            }
        }

        match start_rpc_server(&config, hogd.readiness(), protocol_version).await {
            Ok(started) => {
                tracing::info!(rpc_addr = %started.addr, "node supervisor JSON-RPC ready");
                Ok(Self {
                    hogd,
                    rpc_handle: started.handle,
                    rpc_addr: started.addr,
                    backend_planes: started.backend_planes,
                    manual_block_producer: started.block_driver.manual_producer(),
                    block_driver: Some(started.block_driver),
                })
            }
            Err(error) => {
                // Roll back the hogd-dev child so a failed RPC bind does not
                // leak a child process.
                if let Err(stop_err) = hogd.stop().await {
                    tracing::warn!(
                        error = %stop_err,
                        "failed to stop hogd-dev after JSON-RPC startup error",
                    );
                }
                Err(error)
            }
        }
    }

    /// Bound JSON-RPC listen address. Differs from the configured address when
    /// the caller asked for port `0`.
    pub fn rpc_addr(&self) -> SocketAddr {
        self.rpc_addr
    }

    /// Parsed hogd-dev readiness payload.
    pub fn hogd_readiness(&self) -> &HogdDevReadiness {
        self.hogd.readiness()
    }

    /// Backend plane status for startup reporting.
    pub fn backend_planes(&self) -> BackendPlanes {
        self.backend_planes
    }

    /// Operating-system process id for hogd-dev, if available.
    pub fn hogd_child_id(&self) -> Option<u32> {
        self.hogd.child_id()
    }

    /// In-process block production control when the supervisor runs in manual mode.
    pub fn manual_block_producer(&self) -> Option<ManualBlockProducer> {
        self.manual_block_producer.clone()
    }

    /// Wait until either hogd-dev exits or the JSON-RPC server stops, then
    /// tear the other side down.
    ///
    /// If hogd-dev dies after readiness, the public JSON-RPC facade keeps
    /// answering but every backend call would fail; this method observes
    /// that and shuts the facade down so the CLI sees a clean stop instead
    /// of silent malfunction. If the JSON-RPC server stops first (e.g. via
    /// an external [`NodeSupervisorHandle::stop`] call), hogd-dev is shut
    /// down with the configured grace timeout.
    ///
    /// Returns a [`SupervisorStop`] describing which side ended first.
    #[tracing::instrument(skip_all)]
    pub async fn stopped(mut self) -> Result<SupervisorStop, NodeSupervisorError> {
        self.run_until_inner(std::future::pending::<Result<(), NodeSupervisorError>>())
            .await
    }

    /// Wait until hogd-dev exits, the JSON-RPC server stops, or an external
    /// stop signal completes, then tear the remaining side down.
    ///
    /// This is the CLI-friendly variant of [`NodeSupervisorHandle::stopped`]:
    /// callers pass a future for Ctrl-C or another stop request without
    /// losing the ability to shut hogd-dev down cleanly.
    #[tracing::instrument(skip_all)]
    pub async fn run_until<F>(
        mut self,
        stop_signal: F,
    ) -> Result<SupervisorStop, NodeSupervisorError>
    where
        F: Future<Output = Result<(), NodeSupervisorError>>,
    {
        self.run_until_inner(stop_signal).await
    }

    async fn run_until_inner<F>(
        &mut self,
        stop_signal: F,
    ) -> Result<SupervisorStop, NodeSupervisorError>
    where
        F: Future<Output = Result<(), NodeSupervisorError>>,
    {
        let rpc_done = self.rpc_handle.clone().stopped();
        tokio::select! {
            result = self.hogd.stopped() => {
                match &result {
                    Ok(status) => tracing::warn!(?status, "hogd-dev exited; stopping JSON-RPC server"),
                    Err(error) => tracing::error!(%error, "hog-devnode cleanup failed; stopping JSON-RPC server"),
                }
                if let Err(error) = self.rpc_handle.stop() {
                    tracing::debug!(%error, "JSON-RPC server already stopped");
                }
                self.stop_block_driver().await;
                self.rpc_handle.clone().stopped().await;
                result.map(SupervisorStop::HogdExited)
            }
            () = rpc_done => {
                tracing::info!("JSON-RPC server stopped; stopping hogd-dev");
                let status = self.stop_inner().await?;
                Ok(SupervisorStop::RpcStopped(status))
            }
            result = stop_signal => {
                if let Err(error) = result {
                    tracing::warn!(%error, "stop signal failed; stopping node supervisor");
                    if let Err(stop_error) = self.stop_inner().await {
                        tracing::error!(%stop_error, "failed to stop node supervisor after stop signal error");
                    }
                    return Err(error);
                }
                tracing::info!("stop signal received; stopping node supervisor");
                let status = self.stop_inner().await?;
                Ok(SupervisorStop::StopRequested(status))
            }
        }
    }

    /// Stop the JSON-RPC server and shut hogd-dev down with the configured
    /// grace timeout. Consumes the handle.
    #[tracing::instrument(skip_all)]
    pub async fn stop(mut self) -> Result<ExitStatus, NodeSupervisorError> {
        self.stop_inner().await
    }

    async fn stop_inner(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        if let Err(error) = self.rpc_handle.stop() {
            tracing::debug!(%error, "JSON-RPC server already stopped");
        }
        self.stop_block_driver().await;
        self.rpc_handle.clone().stopped().await;
        self.hogd.stop().await
    }

    async fn stop_block_driver(&mut self) {
        if let Some(block_driver) = self.block_driver.take() {
            block_driver.stop().await;
        }
    }
}

async fn start_rpc_server(
    config: &NodeSupervisorConfig,
    readiness: &HogdDevReadiness,
    protocol_version: &str,
) -> Result<StartedRpcServer, NodeSupervisorError> {
    let listen_address = &config.rpc_listen_address;
    let manifest = readiness.runtime_manifest.as_ref().ok_or_else(|| {
        NodeSupervisorError::InvalidRuntimeManifest {
            program: DEFAULT_HOGD_DEV_BINARY.to_owned(),
            reason: "hog-devnode readiness must advertise a runtime manifest".to_owned(),
        }
    })?;
    let network_hash = manifest.network_hash.to_network_hash().into_inner();
    let network_info = NetworkInfo::new(NetworkHash::from(network_hash));
    let scheduler_backend = connect_scheduler_backend(readiness, protocol_version).await?;

    let view = HogdDevViewBackend::connect_optional(
        readiness.endpoints.view_gateway,
        protocol_version,
        readiness.protocol.as_deref(),
        config.view_timeout,
    )
    .await?;
    let view_status = view.plane_status();

    // Prefer the full indexer-backed read plane when available. If hogd-dev
    // only advertises DbGateway, still serve latest-state storage reads.
    if let Some(indexer_addr) = readiness.endpoints.indexer_block_explorer {
        let local_airdrop = validated_local_airdrop_context(manifest, config)?;
        tracing::debug!(%indexer_addr, "connecting to hogd-dev IndexerBlockExplorer");
        let read = HogdDevReadBackend::connect(
            indexer_addr,
            readiness.endpoints.db_gateway,
            scheduler_backend.scheduler_gateway_client().clone(),
            protocol_version,
            readiness.protocol.as_deref(),
        )
        .await?;
        start_rpc_server_with_read(
            listen_address,
            scheduler_backend,
            read,
            view,
            rpc_server_options(
                config,
                network_info,
                Some(local_airdrop),
                BackendPlanes::hogd_dev_full(view_status),
            ),
        )
        .await
    } else if let Some(db_gateway_addr) = readiness.endpoints.db_gateway {
        tracing::debug!(%db_gateway_addr, "connecting to hogd-dev DbGateway for storage reads");
        let read = HogdDevStorageReadBackend::connect(
            db_gateway_addr,
            protocol_version,
            readiness.protocol.as_deref(),
        )
        .await?;
        start_rpc_server_with_read(
            listen_address,
            scheduler_backend,
            read,
            view,
            rpc_server_options(
                config,
                network_info,
                None,
                BackendPlanes::hogd_dev_storage_only(view_status),
            ),
        )
        .await
    } else {
        tracing::warn!(
            "hogd-dev did not advertise indexer_block_explorer or db_gateway endpoints; read plane is stubbed \
             (getTransactionReceipt / getTransactionStatus / getContractEvents / \
              getStorageAt return NotImplemented)"
        );
        start_rpc_server_with_read(
            listen_address,
            scheduler_backend,
            StubBackend::new(),
            view,
            rpc_server_options(
                config,
                network_info,
                None,
                BackendPlanes {
                    view: view_status,
                    ..BackendPlanes::hogd_dev_scheduler_only()
                },
            ),
        )
        .await
    }
}

async fn connect_scheduler_backend(
    readiness: &HogdDevReadiness,
    protocol_version: &str,
) -> Result<HogdDevSchedulerBackend, NodeSupervisorError> {
    let scheduler_gateway = readiness.endpoints.scheduler_gateway.ok_or_else(|| {
        NodeSupervisorError::InvalidRuntimeManifest {
            program: DEFAULT_HOGD_DEV_BINARY.to_owned(),
            reason: "hog-devnode runtime manifest is missing endpoints.schedulerGateway".to_owned(),
        }
    })?;
    let scheduler_backend = HogdDevSchedulerBackend::connect(
        readiness.endpoints.scheduler_coord,
        scheduler_gateway,
        protocol_version,
        readiness.protocol.as_deref(),
    )
    .await?;
    scheduler_backend
        .require_dev_genesis(&readiness.network_hash)
        .await?;
    Ok(scheduler_backend)
}

fn rpc_server_options(
    config: &NodeSupervisorConfig,
    network_info: NetworkInfo,
    local_airdrop: Option<LocalAirdropContext>,
    backend_planes: BackendPlanes,
) -> RpcServerOptions {
    RpcServerOptions {
        network_info,
        local_airdrop,
        backend_planes,
        max_view_gas: config.max_view_gas,
        block_driver_config: config.block_driver,
        rpc_max_connections: config.rpc_max_connections,
        rpc_batch_concurrency: config.rpc_batch_concurrency,
        bundle_identity: config.bundle_identity.clone(),
    }
}

fn validated_local_airdrop_context(
    manifest: &groundhog_json_rpc_types::runtime_manifest::RuntimeManifest,
    config: &NodeSupervisorConfig,
) -> Result<LocalAirdropContext, NodeSupervisorError> {
    LocalAirdropContext::from_validated_runtime(manifest, config).map_err(|reason| {
        NodeSupervisorError::InvalidRuntimeManifest {
            program: DEFAULT_HOGD_DEV_BINARY.to_owned(),
            reason,
        }
    })
}

struct RpcServerOptions {
    network_info: NetworkInfo,
    local_airdrop: Option<LocalAirdropContext>,
    backend_planes: BackendPlanes,
    max_view_gas: u64,
    block_driver_config: BlockDriverConfig,
    rpc_max_connections: u32,
    rpc_batch_concurrency: groundhog_json_rpc_server::batch::RpcBatchConcurrency,
    bundle_identity: Option<BundleIdentity>,
}

async fn start_rpc_server_with_read<R, V>(
    listen_address: &str,
    scheduler_backend: HogdDevSchedulerBackend,
    read: R,
    view: V,
    options: RpcServerOptions,
) -> Result<StartedRpcServer, NodeSupervisorError>
where
    R: groundhog_json_rpc_server::ReadPlane + Clone,
    V: groundhog_json_rpc_server::ViewPlane,
{
    let RpcServerOptions {
        network_info,
        local_airdrop,
        backend_planes,
        max_view_gas,
        block_driver_config,
        rpc_max_connections,
        rpc_batch_concurrency,
        bundle_identity,
    } = options;
    ensure_rpc_nofile_limit(rpc_max_connections)?;
    let max_response_body_size = jsonrpsee::core::TEN_MB_SIZE_BYTES;
    let server_config = ServerConfig::builder()
        .max_connections(rpc_max_connections)
        .max_response_body_size(max_response_body_size)
        .set_batch_request_config(jsonrpsee::server::BatchRequestConfig::Limit(
            MAX_RPC_BATCH_REQUESTS,
        ))
        .build();
    let rpc_middleware =
        jsonrpsee::core::middleware::RpcServiceBuilder::new().layer_fn(move |inner| {
            ConcurrentBatchService::new(inner, max_response_body_size, rpc_batch_concurrency)
        });
    let server = Server::builder()
        .set_config(server_config)
        .set_rpc_middleware(rpc_middleware)
        .build(listen_address)
        .await
        .map_err(|source| NodeSupervisorError::RpcBind {
            address: listen_address.to_string(),
            source,
        })?;
    let rpc_addr = server
        .local_addr()
        .map_err(|source| NodeSupervisorError::RpcLocalAddr { source })?;

    let block_driver = BlockDriverTask::start(
        scheduler_backend.scheduler_client().clone(),
        block_driver_config,
    );
    let block_driver_trigger = block_driver.trigger();
    let scheduler_backend = scheduler_backend.with_block_driver(block_driver_trigger.clone());
    let local_airdrop = local_airdrop.map(|context| {
        LocalAirdropRpc::new(
            context,
            scheduler_backend.clone(),
            read.clone(),
            block_driver_trigger,
        )
    });
    let core_rpc = GroundhogRpcServer::new(
        scheduler_backend.clone(),
        scheduler_backend,
        read,
        view,
        network_info,
        RpcServerConfig::default().with_max_view_gas(max_view_gas),
        BuildInfo::from_build_version(
            groundhog_json_rpc_server::build_version!(),
            option_env!("GIT_COMMIT"),
        ),
    );
    let core_rpc = match bundle_identity.as_ref() {
        Some(identity) => core_rpc.with_bundle_identity(identity),
        None => core_rpc,
    };
    let mut module = RpcModule::new(());
    module
        .merge(GroundhogApiServer::into_rpc(core_rpc))
        .expect("bug: json-rpc-server methods register without conflicts");
    if let Some(local_airdrop) = local_airdrop {
        module
            .merge(AirdropApiServer::into_rpc(local_airdrop))
            .expect("bug: local airdrop methods register without conflicts");
    }

    let rpc_handle = server.start(module);
    Ok(StartedRpcServer {
        handle: rpc_handle,
        addr: rpc_addr,
        backend_planes,
        block_driver,
    })
}

const RPC_NOFILE_LIMIT_FLOOR: u64 = 4_096;
const RPC_NOFILE_LIMIT_HEADROOM: u64 = 512;

fn desired_rpc_nofile_limit(rpc_max_connections: u32) -> u64 {
    u64::from(rpc_max_connections)
        .saturating_add(RPC_NOFILE_LIMIT_HEADROOM)
        .max(RPC_NOFILE_LIMIT_FLOOR)
}

#[cfg(unix)]
fn ensure_rpc_nofile_limit(rpc_max_connections: u32) -> Result<(), NodeSupervisorError> {
    let target = desired_rpc_nofile_limit(rpc_max_connections);
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limits) };
    if rc != 0 {
        return Err(NodeSupervisorError::RpcNofileLimit {
            target,
            source: io::Error::last_os_error(),
        });
    }
    let Some(new_soft) = rpc_nofile_soft_limit_update(target, limits)? else {
        return Ok(());
    };

    let updated = libc::rlimit {
        rlim_cur: new_soft,
        rlim_max: limits.rlim_max,
    };
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const updated) };
    if rc != 0 {
        return Err(NodeSupervisorError::RpcNofileLimit {
            target,
            source: io::Error::last_os_error(),
        });
    }
    tracing::debug!(
        previous = limits.rlim_cur,
        current = new_soft,
        hard = limits.rlim_max,
        target,
        "raised JSON-RPC nofile soft limit"
    );
    Ok(())
}

#[cfg(unix)]
fn rpc_nofile_soft_limit_update(
    target: u64,
    limits: libc::rlimit,
) -> Result<Option<libc::rlim_t>, NodeSupervisorError> {
    let target_rlim = libc::rlim_t::try_from(target).unwrap_or(libc::RLIM_INFINITY);
    if limits.rlim_cur >= target_rlim {
        return Ok(None);
    }
    if limits.rlim_max != libc::RLIM_INFINITY && limits.rlim_max < target_rlim {
        return Err(NodeSupervisorError::RpcNofileHardLimit {
            target,
            hard: limits.rlim_max,
        });
    }

    if limits.rlim_max == libc::RLIM_INFINITY {
        Ok(Some(target_rlim))
    } else {
        Ok(Some(target_rlim.min(limits.rlim_max)))
    }
}

#[cfg(not(unix))]
fn ensure_rpc_nofile_limit(_rpc_max_connections: u32) -> Result<(), NodeSupervisorError> {
    Ok(())
}

#[derive(Debug)]
struct StartedRpcServer {
    handle: ServerHandle,
    addr: SocketAddr,
    backend_planes: BackendPlanes,
    block_driver: BlockDriverTask,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, Instant},
    };

    use groundhog_bundle_identity::{BundleIdentity, BundleSubject};
    use groundhog_json_rpc_api::{AirdropApiClient, GroundhogApiClient};
    use groundhog_json_rpc_server::BuildVersion;
    use groundhog_json_rpc_types::{
        airdrop::{AirdropAttemptResponse, AirdropRequest, ClientNonce},
        error::codes,
        hex::Hex0x,
    };
    use jsonrpsee::core::{ClientError, client::ClientT};
    use jsonrpsee::http_client::HttpClientBuilder;
    use serial_test::serial;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::*;
    use crate::{
        BlockDriverConfig, BlockProductionError, HogdDevConfig, LocalAirdropConfig,
        test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
    };

    /// Env var the fake hogd-dev reads to write its PID before printing
    /// readiness. Tests use this to verify cleanup via `kill(pid, 0)`.
    const FAKE_HOGD_DEV_PID_FILE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_PID_FILE";
    const FAKE_COMMIT_ENTERED_FILE_ENV: &str = "GROUNDHOG_FAKE_COMMIT_ENTERED_FILE";
    // Canonical XDR/hash vector only. This transaction is structurally valid
    // and has a stable hash, but it is not auth-valid against empty hogd-dev
    // genesis.
    const CANONICAL_HASH_VECTOR_SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
0000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a00000000000000\
0000000001000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695031\
c9d3fe2554e5ce62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d";
    const CANONICAL_HASH_VECTOR_TX_HASH_JSON: &str =
        "0x9dc7ad0fc4f74a43a62e047c0d6885c8e4df2d4840298ecd52cf157681157ed3";

    fn write_matching_client_manifest() -> (TempDir, BundleIdentity, String) {
        const VERSION: &str = "9.8.7-test";
        const COMMIT: &str = "373bec4b9dd9b99abae4f23b82f4e35df61273d1";
        const RELEASE: &str = "0.1.25-rc.4";

        let canonical = format!(
            "{{\"bundle\":{{\"version\":\"{RELEASE}\"}},\"clients\":{{\"groundhog\":{{\"assetRole\":\"cli\",\"component\":\"groundhog-rpc\",\"sourceCommit\":\"{COMMIT}\",\"version\":\"{VERSION}\"}}}},\"components\":{{\"groundhog-rpc\":{{\"active\":true,\"sourceCommit\":\"{COMMIT}\",\"version\":\"{VERSION}\"}}}},\"schema\":\"groundhog-bundle-v1\"}}\n"
        );
        let digest = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(canonical.as_bytes()))
        );
        let mut manifest: serde_json::Value =
            serde_json::from_str(&canonical).expect("bug: canonical manifest should parse");
        manifest["bundle"]["manifestDigest"] = serde_json::Value::String(digest.clone());

        let directory = TempDir::new().expect("bug: tempdir creates");
        let path = directory.path().join("bundle-manifest.json");
        fs::write(
            &path,
            serde_json::to_vec(&manifest).expect("bug: manifest should serialize"),
        )
        .expect("bug: manifest should write");
        let identity = BundleIdentity::from_manifest_path(
            BundleSubject::client("groundhog", "groundhog-rpc", "cli"),
            BuildVersion::new(VERSION, None).with_source_commit(Some(COMMIT)),
            &path,
        );
        assert_eq!(identity.release_version(), Some(RELEASE));
        (directory, identity, digest)
    }

    #[tokio::test]
    async fn invalid_local_airdrop_config_is_rejected_before_child_start() {
        let local_airdrop = LocalAirdropConfig {
            attempt_capacity: 0,
            ..LocalAirdropConfig::default()
        };
        let config = NodeSupervisorConfig::default()
            .with_hogd_dev_binary("/definitely/missing/hogd-dev")
            .with_local_airdrop(local_airdrop);

        let error = NodeSupervisorHandle::start(config)
            .await
            .expect_err("invalid local airdrop config must fail before resolving hogd-dev");

        assert!(matches!(
            error,
            NodeSupervisorError::InvalidLocalAirdropConfig { .. }
        ));
    }

    /// Set an env var for the duration of a test that needs the fake child
    /// to enter `ready` mode. Restored on drop. `#[serial]` ensures we are
    /// the only test mutating env at a time, per workspace convention.
    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            // SAFETY: `#[serial]` serializes all env-mutating tests in this
            // process; no other thread is reading or writing env concurrently.
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see EnvGuard::set.
            match self.prev.take() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    /// Poll `kill(pid, 0)` until ESRCH or until `deadline` passes. Returns
    /// `true` if the OS confirmed the process is gone within the deadline.
    async fn wait_pid_gone(pid: i32, deadline: Instant) -> bool {
        loop {
            // SAFETY: `kill(pid, 0)` does not deliver a signal; it only
            // tests whether `pid` is a known process. Always safe.
            let rc = unsafe { libc::kill(pid, 0) };
            if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn read_pid_file(path: &std::path::Path) -> Option<i32> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| raw.trim().parse().ok())
    }

    async fn wait_path_exists(path: &std::path::Path, deadline: Instant) -> bool {
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        path.exists()
    }

    async fn start_fake_supervisor_expect_error(mode: &str) -> NodeSupervisorError {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, mode);
        let pid_dir = TempDir::new().expect("bug: tempdir creates");
        let pid_path = pid_dir.path().join("hogd.pid");
        let _pid_guard = EnvGuard::set(
            FAKE_HOGD_DEV_PID_FILE_ENV,
            pid_path.to_str().expect("bug: tempdir path is utf-8"),
        );

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);

        let result =
            tokio::time::timeout(Duration::from_secs(10), NodeSupervisorHandle::start(config))
                .await
                .expect("bug: startup error should return promptly");

        let err = result.expect_err("bug: fake mode should fail startup");
        let pid =
            read_pid_file(&pid_path).expect("bug: fake should have written pid before readiness");
        assert!(
            wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
            "startup failure leaked fake hogd-dev pid {pid}",
        );
        err
    }

    async fn start_fake_with_block_driver(
        mode: &str,
        block_driver: BlockDriverConfig,
    ) -> (
        EnvGuard,
        NodeSupervisorHandle,
        jsonrpsee::http_client::HttpClient,
    ) {
        let mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, mode);
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd)
            .with_block_driver(block_driver);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.rpc_addr()))
            .expect("bug: jsonrpsee http client builds");
        (mode_guard, handle, client)
    }

    #[tokio::test]
    #[serial]
    async fn start_serves_json_rpc_with_scheduler_chain_backend() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
        let pid_dir = TempDir::new().expect("bug: tempdir creates");
        let pid_path = pid_dir.path().join("hogd.pid");
        let _pid_guard = EnvGuard::set(
            FAKE_HOGD_DEV_PID_FILE_ENV,
            pid_path.to_str().expect("bug: tempdir path is utf-8"),
        );

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let (_manifest_directory, bundle_identity, manifest_digest) =
            write_matching_client_manifest();
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd)
            .with_bundle_identity(bundle_identity);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");
        assert_eq!(
            handle.backend_planes(),
            BackendPlanes::hogd_dev_scheduler_only()
        );

        let url = format!("http://{}", handle.rpc_addr());
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        let network_info = client
            .network_info()
            .await
            .expect("bug: networkInfo should use immutable manifest identity");
        assert_eq!(network_info.network_hash.as_ref(), &[0; 32]);

        let version = client
            .version()
            .await
            .expect("bug: version should report the embedding client identity");
        assert_eq!(version.implementation, "9.8.7-test");
        assert_eq!(
            version.commit_hash.as_deref(),
            Some("373bec4b9dd9b99abae4f23b82f4e35df61273d1")
        );
        assert_eq!(version.release_version.as_deref(), Some("0.1.25-rc.4"));
        assert_eq!(
            version.bundle_manifest_digest.as_deref(),
            Some(manifest_digest.as_str())
        );

        let tip = client
            .chain_tip()
            .await
            .expect("bug: chainTip should dispatch to fake scheduler");
        assert_eq!(
            *tip.height, 0,
            "chainTip should report the genesis block loaded during startup"
        );
        assert_eq!(
            serde_json::to_value(tip.block_hash).expect("bug: block hash should serialize"),
            "0xe0b0468f28980b4982a63bea1018c0ec2bb12e8bd41b5f43dd1ac5fefb31cfe9"
        );
        let chain_config = client
            .chain_config()
            .await
            .expect("bug: chainConfig should dispatch to fake scheduler");
        assert_eq!(
            chain_config.max_sha256_buffers,
            Some(0),
            "chainConfig should report the parameters the fake scheduler's genesis config states"
        );
        assert_eq!(chain_config.max_cross_invoke_depth, Some(0));
        assert_eq!(
            chain_config.max_contract_size_bytes, None,
            "a parameter the genesis config does not state is absent, not defaulted"
        );
        assert_eq!(
            client
                .pending_transaction_count()
                .await
                .expect("bug: pendingTransactionCount should use SchedGateway"),
            0
        );

        let airdrop_error = client
            .request_airdrop(AirdropRequest {
                recipient: Hex0x(groundhog_xdr::types::ContractAddress::from([0x88; 32])),
                client_nonce: ClientNonce::new(vec![0x99])
                    .expect("bug: test client nonce should validate"),
            })
            .await
            .expect_err("bug: scheduler-only runtime must not expose airdrop");
        assert!(
            matches!(&airdrop_error, ClientError::Call(object) if object.code() == jsonrpsee::types::error::METHOD_NOT_FOUND_CODE),
            "scheduler-only runtime should omit airdrop without a receipt backend, got {airdrop_error:?}",
        );

        let tx_hash = client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: sendRawTransaction should dispatch to fake scheduler");
        assert_eq!(
            serde_json::to_value(tx_hash).expect("bug: tx hash should serialize"),
            CANONICAL_HASH_VECTOR_TX_HASH_JSON
        );

        let err = client
            .send_raw_transaction("not-valid-hex".into())
            .await
            .expect_err("bug: invalid hex should fail before fake scheduler dispatch");
        assert!(
            matches!(&err, ClientError::Call(object) if object.code() == codes::TX_INVALID),
            "expected TX_INVALID JSON-RPC error, got {err:?}",
        );

        let pid = read_pid_file(&pid_path).expect("bug: fake should have written pid");
        let status = handle.stop().await.expect("bug: clean shutdown");

        // Tighter than `!success() || code == 0`: a clean SIGTERM-handled
        // exit either returns code 0 or the SIGTERM signal. Anything else
        // (SIGKILL, abnormal exit) means the supervisor escalated past
        // graceful shutdown.
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let clean = status.code() == Some(0) || status.signal() == Some(libc::SIGTERM);
            assert!(
                clean,
                "fake hogd-dev should exit on code 0 or SIGTERM, got {status:?}"
            );
        }

        assert!(
            wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
            "fake hogd-dev pid {pid} still alive after stop()",
        );
    }

    #[tokio::test]
    #[serial]
    async fn scheduler_gateway_rejection_surfaces_tx_rejected_without_coordinator_fallback() {
        let (_mode_guard, handle, client) = start_fake_with_block_driver(
            "scheduler-gateway-submit-rejected",
            BlockDriverConfig::manual(),
        )
        .await;

        let error = client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect_err("bug: typed scheduler gateway rejection should fail submission");

        assert!(
            matches!(&error, ClientError::Call(object) if object.code() == codes::TX_REJECTED),
            "expected TX_REJECTED from SchedGateway without coordinator fallback, got {error:?}",
        );
        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn submit_wakes_hogd_dev_block_driver() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "automine");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");

        let url = format!("http://{}", handle.rpc_addr());
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: sendRawTransaction should dispatch to fake scheduler");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let tip = client
                .chain_tip()
                .await
                .expect("bug: chainTip should dispatch to fake scheduler");
            if *tip.height == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "block driver did not advance fake scheduler tip before timeout; tip={}",
                *tip.height
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn concurrent_public_submissions_are_batched_for_scheduler() {
        const SUBMIT_LOG_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_SUBMIT_LOG";
        let directory = tempfile::tempdir().expect("bug: tempdir");
        let submit_log = directory.path().join("submit-batches.log");
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "submit-batch-probe");
        let _log_guard = EnvGuard::set(
            SUBMIT_LOG_ENV,
            submit_log.to_str().expect("bug: tempdir path is utf-8"),
        );
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.rpc_addr()))
            .expect("bug: jsonrpsee http client builds");
        let tx_hex = format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}");

        let (a, b, c, d, e) = tokio::join!(
            client.send_raw_transaction(tx_hex.clone()),
            client.send_raw_transaction(tx_hex.clone()),
            client.send_raw_transaction(tx_hex.clone()),
            client.send_raw_transaction(tx_hex.clone()),
            client.send_raw_transaction(tx_hex),
        );

        for result in [a, b, c, d, e] {
            result.expect("bug: concurrent transaction should be submitted");
        }
        let sizes: Vec<usize> = std::fs::read_to_string(submit_log)
            .expect("bug: fake scheduler should write submit log")
            .lines()
            .map(|line| line.parse().expect("bug: submit batch size should parse"))
            .collect();
        assert!(
            sizes.iter().any(|size| *size > 1),
            "concurrent public submissions should produce a multi-transaction batch: {sizes:?}"
        );
        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn rpc_array_limit_rejects_oversized_submissions_before_dispatch() {
        use jsonrpsee::core::{client::ClientT, params::BatchRequestBuilder};

        let directory = tempfile::tempdir().unwrap();
        let submit_log = directory.path().join("submit-batches.log");
        let _mode = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "submit-batch-probe");
        let _log = EnvGuard::set(
            "GROUNDHOG_FAKE_HOGD_DEV_SUBMIT_LOG",
            submit_log.to_str().unwrap(),
        );
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let handle = NodeSupervisorHandle::start(
            NodeSupervisorConfig::default()
                .with_rpc_listen_address("127.0.0.1:0")
                .with_hogd_dev(hogd),
        )
        .await
        .unwrap();
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.rpc_addr()))
            .unwrap();
        let mut allowed = BatchRequestBuilder::new();
        for _ in 0..MAX_RPC_BATCH_REQUESTS {
            allowed
                .insert("groundhog_version", jsonrpsee::rpc_params![])
                .unwrap();
        }
        let responses = client
            .batch_request::<serde_json::Value>(allowed)
            .await
            .unwrap();
        assert_eq!(responses.len(), MAX_RPC_BATCH_REQUESTS as usize);
        assert!(responses.into_iter().all(|response| response.is_ok()));

        let oversized: Vec<_> = (0..=MAX_RPC_BATCH_REQUESTS)
            .map(|id| {
                serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "method": "groundhog_sendRawTransaction",
                    "params": [format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")],
                })
            })
            .collect();
        // Use raw HTTP: jsonrpsee's batch client expects an array and cannot
        // decode the single null-ID error returned for a rejected whole batch.
        let error = tokio::time::timeout(Duration::from_secs(5), async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let body = serde_json::to_string(&oversized).unwrap();
            let mut socket = tokio::net::TcpStream::connect(handle.rpc_addr())
                .await
                .unwrap();
            let request = format!(
                "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            socket.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).await.unwrap();
            serde_json::from_str::<serde_json::Value>(response.split_once("\r\n\r\n").unwrap().1)
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(error["error"]["code"], -32010);
        assert!(
            !submit_log.exists(),
            "oversized array must not submit any transactions"
        );
        handle.stop().await.unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn homogeneous_rpc_array_is_batched_for_scheduler() {
        use jsonrpsee::core::{client::ClientT, params::BatchRequestBuilder};

        for concurrency in [1, 4, 128] {
            let directory = tempfile::tempdir().expect("bug: tempdir");
            let submit_log = directory.path().join("submit-batches.log");
            let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "submit-batch-probe");
            let _log_guard = EnvGuard::set(
                "GROUNDHOG_FAKE_HOGD_DEV_SUBMIT_LOG",
                submit_log.to_str().expect("bug: tempdir path is utf-8"),
            );
            let hogd = HogdDevConfig::default()
                .with_binary_path(fake_hogd_dev_binary())
                .with_startup_timeout(Duration::from_secs(5))
                .with_shutdown_grace(Duration::from_secs(2));
            let config = NodeSupervisorConfig::default()
                .with_rpc_listen_address("127.0.0.1:0")
                .with_rpc_batch_concurrency(concurrency.try_into().unwrap())
                .with_hogd_dev(hogd);
            let handle = NodeSupervisorHandle::start(config)
                .await
                .expect("bug: supervisor should start against fake hogd-dev");
            let client = HttpClientBuilder::default()
                .build(format!("http://{}", handle.rpc_addr()))
                .expect("bug: jsonrpsee http client builds");
            let mut batch = BatchRequestBuilder::new();
            for _ in 0..35 {
                batch
                    .insert(
                        "groundhog_sendRawTransaction",
                        jsonrpsee::rpc_params![format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")],
                    )
                    .expect("bug: batch entry should serialize");
            }
            let responses = client
                .batch_request::<String>(batch)
                .await
                .expect("bug: submission array should succeed");
            assert_eq!(responses.len(), 35);
            assert!(responses.into_iter().all(|response| response.is_ok()));
            let sizes: Vec<usize> = std::fs::read_to_string(submit_log)
                .expect("bug: fake scheduler should write submit log")
                .lines()
                .map(|line| line.parse().expect("bug: submit batch size should parse"))
                .collect();
            assert_eq!(sizes.iter().sum::<usize>(), 35);
            if concurrency > 1 {
                assert!(
                    sizes.iter().any(|size| *size > 1),
                    "array should coalesce: {sizes:?}"
                );
            }
            assert!(sizes.iter().all(|size| *size <= concurrency));
            handle.stop().await.expect("bug: clean shutdown");
        }
    }

    #[tokio::test]
    #[serial]
    async fn block_driver_coalesces_rapid_submit_wakes() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "automine-slow-commit");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");

        let url = format!("http://{}", handle.rpc_addr());
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");
        let tx_hex = format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}");

        let (a, b, c, d, e) = tokio::join!(
            client.send_raw_transaction(tx_hex.clone()),
            client.send_raw_transaction(tx_hex.clone()),
            client.send_raw_transaction(tx_hex.clone()),
            client.send_raw_transaction(tx_hex.clone()),
            client.send_raw_transaction(tx_hex),
        );
        for result in [a, b, c, d, e] {
            result.expect("bug: sendRawTransaction should dispatch to fake scheduler");
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let tip = client
                .chain_tip()
                .await
                .expect("bug: chainTip should dispatch to fake scheduler");
            if *tip.height >= 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "block driver did not commit any block before timeout; tip={}",
                *tip.height
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
        let final_tip = client
            .chain_tip()
            .await
            .expect("bug: chainTip should dispatch to fake scheduler");
        assert!(
            *final_tip.height <= 2,
            "coalesced submit wakes should not commit one block per submit; tip={}",
            *final_tip.height
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn interval_block_driver_paces_submit_commit() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "automine");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd)
            .with_block_driver(BlockDriverConfig::interval(Duration::from_millis(500)));
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");

        let url = format!("http://{}", handle.rpc_addr());
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: sendRawTransaction should dispatch to fake scheduler");

        tokio::time::sleep(Duration::from_millis(100)).await;
        let early_tip = client
            .chain_tip()
            .await
            .expect("bug: chainTip should dispatch to fake scheduler");
        assert_eq!(
            *early_tip.height, 0,
            "interval mode should not commit immediately from a submit wake"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let tip = client
                .chain_tip()
                .await
                .expect("bug: chainTip should dispatch to fake scheduler");
            if *tip.height == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "interval block driver did not advance fake scheduler tip before timeout; tip={}",
                *tip.height
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn manual_block_driver_only_commits_on_explicit_request() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "automine");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd)
            .with_block_driver(
                BlockDriverConfig::manual().with_interval(Duration::from_millis(25)),
            );
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");

        let url = format!("http://{}", handle.rpc_addr());
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");
        let tx_hex = format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}");
        client
            .send_raw_transaction(tx_hex.clone())
            .await
            .expect("bug: first submit should reach fake scheduler");
        client
            .send_raw_transaction(tx_hex)
            .await
            .expect("bug: second submit should reach fake scheduler");

        tokio::time::sleep(Duration::from_millis(100)).await;
        let before = client
            .chain_tip()
            .await
            .expect("bug: chainTip should dispatch to fake scheduler");
        assert_eq!(*before.height, 0, "manual mode must not automine submits");

        let produced = producer
            .produce_block()
            .await
            .expect("bug: explicit production should commit one block");
        assert_eq!(produced.height, 1);
        let after = client
            .chain_tip()
            .await
            .expect("bug: chainTip should dispatch to fake scheduler");
        assert_eq!(*after.height, 1);

        let empty_error = producer
            .produce_block()
            .await
            .expect_err("bug: fake scheduler rejects nomination without pending submits");
        assert!(matches!(
            empty_error,
            BlockProductionError::SchedulerRejected {
                op: "schedulercoord_propose_block",
                ..
            }
        ));

        handle.stop().await.expect("bug: clean shutdown");
        let stopped = producer
            .produce_block()
            .await
            .expect_err("bug: producer retained after shutdown must fail");
        assert!(matches!(stopped, BlockProductionError::DriverStopped));
    }

    #[tokio::test]
    #[serial]
    async fn manual_block_requests_are_serialized_without_coalescing() {
        let (_mode_guard, handle, client) =
            start_fake_with_block_driver("automine-serialize-probe", BlockDriverConfig::manual())
                .await;
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");
        let tx_hex = format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}");
        for _ in 0..2 {
            client
                .send_raw_transaction(tx_hex.clone())
                .await
                .expect("bug: submit should reach fake scheduler");
        }

        let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(producer.produce_block(), producer.produce_block())
        })
        .await
        .expect("bug: serialized requests should complete before timeout");
        let mut heights = [
            first
                .expect("bug: first serialized request should commit")
                .height,
            second
                .expect("bug: second serialized request should commit")
                .height,
        ];
        heights.sort_unstable();
        assert_eq!(heights, [1, 2]);

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn malformed_scheduler_proposal_returns_typed_error_without_panicking_driver() {
        let (_mode_guard, handle, client) = start_fake_with_block_driver(
            "automine-malformed-proposal",
            BlockDriverConfig::manual(),
        )
        .await;
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");
        client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: submit should reach fake scheduler");

        let error = producer
            .produce_block()
            .await
            .expect_err("bug: malformed scheduler proposal should fail");
        assert!(matches!(
            error,
            BlockProductionError::MalformedSchedulerProposal { batch: "hash", .. }
        ));
        let stopped = producer
            .produce_block()
            .await
            .expect_err("bug: malformed retained proposal should stop manual producer");
        assert!(matches!(stopped, BlockProductionError::DriverStopped));

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn retained_commit_rejection_stops_manual_producer() {
        let (_mode_guard, handle, client) =
            start_fake_with_block_driver("automine-commit-rejected", BlockDriverConfig::manual())
                .await;
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");
        client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: submit should reach fake scheduler");

        let error = producer
            .produce_block()
            .await
            .expect_err("bug: retained commit rejection should fail");
        assert!(matches!(
            error,
            BlockProductionError::CommitRejected {
                block_id: 1,
                height: 1,
                ..
            }
        ));
        let stopped = producer
            .produce_block()
            .await
            .expect_err("bug: retained commit candidate should stop manual producer");
        assert!(matches!(stopped, BlockProductionError::DriverStopped));

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn manual_block_request_times_out_instead_of_hanging() {
        let (_mode_guard, handle, client) = start_fake_with_block_driver(
            "automine-hanging-commit",
            BlockDriverConfig::manual().with_manual_request_timeout(Duration::from_millis(100)),
        )
        .await;
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");
        client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: submit should reach fake scheduler");

        let error = tokio::time::timeout(Duration::from_secs(2), producer.produce_block())
            .await
            .expect("bug: configured block-production deadline should fire")
            .expect_err("bug: hanging commit should hit the configured deadline");
        assert!(matches!(error, BlockProductionError::TimedOut { .. }));
        let stopped = producer
            .produce_block()
            .await
            .expect_err("bug: timeout should make the producer terminal");
        assert!(matches!(stopped, BlockProductionError::DriverStopped));

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn shutdown_resolves_an_in_flight_manual_request() {
        let marker_dir = TempDir::new().expect("bug: tempdir creates");
        let marker_path = marker_dir.path().join("commit-entered");
        let _marker_guard = EnvGuard::set(
            FAKE_COMMIT_ENTERED_FILE_ENV,
            marker_path.to_str().expect("bug: tempdir path is utf-8"),
        );
        let (_mode_guard, handle, client) = start_fake_with_block_driver(
            "automine-hanging-commit",
            BlockDriverConfig::manual().with_manual_request_timeout(Duration::from_secs(30)),
        )
        .await;
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");
        client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: submit should reach fake scheduler");
        let request = tokio::spawn(async move { producer.produce_block().await });
        assert!(
            wait_path_exists(&marker_path, Instant::now() + Duration::from_secs(2)).await,
            "manual request never entered the fake commit RPC"
        );

        tokio::time::timeout(Duration::from_secs(2), handle.stop())
            .await
            .expect("bug: shutdown should not wait for the hanging commit")
            .expect("bug: clean shutdown");
        let error = tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .expect("bug: pending request should resolve during shutdown")
            .expect("bug: request task should join")
            .expect_err("bug: interrupted request should not report success");
        assert!(matches!(error, BlockProductionError::OutcomeUnknown));
    }

    #[tokio::test]
    #[serial]
    async fn commit_rpc_error_is_reconciled_against_the_scheduler_tip() {
        let (_mode_guard, handle, client) = start_fake_with_block_driver(
            "automine-commit-response-lost",
            BlockDriverConfig::manual(),
        )
        .await;
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");
        client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: submit should reach fake scheduler");

        let produced = producer
            .produce_block()
            .await
            .expect("bug: reconciled tip should confirm the committed block");
        assert_eq!(produced.height, 1);

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn commit_rpc_error_without_tip_confirmation_is_indeterminate() {
        let (_mode_guard, handle, client) =
            start_fake_with_block_driver("automine-commit-error", BlockDriverConfig::manual())
                .await;
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");
        client
            .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
            .await
            .expect("bug: submit should reach fake scheduler");

        let error = producer
            .produce_block()
            .await
            .expect_err("bug: unchanged tip cannot confirm commit");
        assert!(matches!(
            error,
            BlockProductionError::CommitOutcomeUnknown { .. }
        ));
        let stopped = producer
            .produce_block()
            .await
            .expect_err("bug: indeterminate commit should make the producer terminal");
        assert!(matches!(stopped, BlockProductionError::DriverStopped));

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn runtime_scheduler_rpc_failures_are_typed() {
        for mode in ["automine-runtime-get-tip-error", "automine-nominate-error"] {
            let (_mode_guard, handle, client) =
                start_fake_with_block_driver(mode, BlockDriverConfig::manual()).await;
            let producer = handle
                .manual_block_producer()
                .expect("bug: manual mode should expose a producer");
            client
                .send_raw_transaction(format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"))
                .await
                .expect("bug: submit should reach fake scheduler");

            let error = producer
                .produce_block()
                .await
                .expect_err("bug: fake runtime RPC failure should reach the caller");
            if mode == "automine-runtime-get-tip-error" {
                assert!(matches!(
                    error,
                    BlockProductionError::SchedulerRpc {
                        op: "schedcoord_get_tip",
                        ..
                    }
                ));
            } else {
                assert!(matches!(
                    error,
                    BlockProductionError::ProposalOutcomeUnknown { .. }
                ));
                let stopped = producer
                    .produce_block()
                    .await
                    .expect_err("bug: unknown nomination should make producer terminal");
                assert!(matches!(stopped, BlockProductionError::DriverStopped));
            }

            handle.stop().await.expect("bug: clean shutdown");
        }
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_scheduler_connect_fails() {
        let err = start_fake_supervisor_expect_error("dead-scheduler").await;

        assert!(
            matches!(err, NodeSupervisorError::SchedulerConnect { .. }),
            "expected SchedulerConnect, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_scheduler_gateway_connect_fails() {
        let err = start_fake_supervisor_expect_error("dead-scheduler-gateway").await;

        assert!(
            matches!(err, NodeSupervisorError::SchedulerGatewayConnect { .. }),
            "expected SchedulerGatewayConnect, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_scheduler_gateway_intro_is_rejected() {
        let err = start_fake_supervisor_expect_error("scheduler-gateway-intro-rejected").await;

        assert!(
            matches!(
                err,
                NodeSupervisorError::ProtocolIntroRejected {
                    service: "SchedGateway",
                    is_protocol_mismatch: true,
                    ..
                }
            ),
            "expected SchedGateway protocol rejection, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rejects_scheduler_gateway_from_a_different_network() {
        let err =
            start_fake_supervisor_expect_error("scheduler-gateway-network-hash-mismatch").await;

        assert!(
            matches!(
                err,
                NodeSupervisorError::SchedulerNetworkHashMismatch { .. }
            ),
            "expected scheduler gateway network mismatch, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rejects_incoherent_scheduler_plane_tips() {
        let err = start_fake_supervisor_expect_error("scheduler-gateway-tip-mismatch").await;

        assert!(
            matches!(err, NodeSupervisorError::SchedulerPlaneTipMismatch { .. }),
            "expected scheduler plane tip mismatch, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_scheduler_get_tip_rpc_fails() {
        let err = start_fake_supervisor_expect_error("scheduler-get-tip-error").await;

        assert!(
            matches!(
                err,
                NodeSupervisorError::SchedulerRpc {
                    op: "schedcoord_get_tip",
                    ..
                }
            ),
            "expected SchedulerRpc for get_tip, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_scheduler_network_hash_mismatches_manifest() {
        let err = start_fake_supervisor_expect_error("scheduler-network-hash-mismatch").await;

        assert!(
            matches!(
                &err,
                NodeSupervisorError::SchedulerNetworkHashMismatch {
                    expected,
                    observed,
                } if expected == &groundhog_json_rpc_types::NetworkHashHex::from([0; 32])
                    && observed == &groundhog_json_rpc_types::NetworkHashHex::from([0xff; 32])
            ),
            "expected SchedulerNetworkHashMismatch, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_readiness_precedes_genesis() {
        let err = start_fake_supervisor_expect_error("scheduler-no-genesis").await;

        assert!(
            matches!(
                err,
                NodeSupervisorError::SchedulerRejected {
                    op: "schedcoord_get_tip",
                    ..
                }
            ),
            "expected SchedulerRejected for readiness without genesis, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn run_until_stop_signal_error_stops_hogd_dev() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
        let pid_dir = TempDir::new().expect("bug: tempdir creates");
        let pid_path = pid_dir.path().join("hogd.pid");
        let _pid_guard = EnvGuard::set(
            FAKE_HOGD_DEV_PID_FILE_ENV,
            pid_path.to_str().expect("bug: tempdir path is utf-8"),
        );

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");
        let pid = read_pid_file(&pid_path).expect("bug: fake should have written pid");

        let err = handle
            .run_until(async {
                Err(NodeSupervisorError::StopSignal {
                    source: std::io::Error::other("ctrl-c registration failed"),
                })
            })
            .await
            .expect_err("bug: stop signal error should be returned");
        assert!(
            matches!(err, NodeSupervisorError::StopSignal { .. }),
            "expected StopSignal error, got {err:?}",
        );
        assert!(
            wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
            "fake hogd-dev pid {pid} still alive after stop signal error",
        );
    }

    #[tokio::test]
    #[serial]
    async fn stopped_observes_hogd_dev_exit_and_tears_down_rpc() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-then-exit");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");

        // Capture the bound RPC address before consuming the handle.
        let rpc_addr = handle.rpc_addr();

        // The fake reports readiness then immediately exits with code 0.
        // `stopped()` should observe the child exit, tear down the RPC
        // server, and return HogdExited.
        let outcome = tokio::time::timeout(Duration::from_secs(5), handle.stopped())
            .await
            .expect("bug: stopped() should observe hogd-dev exit promptly")
            .expect("bug: stopped() should not error on clean child exit");

        match outcome {
            SupervisorStop::HogdExited(status) => {
                assert!(
                    status.success(),
                    "fake should exit cleanly with code 0, got {status:?}",
                );
            }
            SupervisorStop::RpcStopped(_) => {
                panic!("expected HogdExited, got RpcStopped — race lost the wrong way");
            }
            SupervisorStop::StopRequested(_) => {
                panic!("expected HogdExited, got StopRequested");
            }
        }

        // RPC facade should be torn down: a fresh connect attempt must fail.
        // (HttpClientBuilder::build is lazy; the call is what hits the wire.)
        let url = format!("http://{rpc_addr}");
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");
        let err = client
            .chain_tip()
            .await
            .expect_err("bug: RPC facade should be down after hogd-dev exit");
        // Any client error here is correct (transport closed, refused, etc.).
        // We only assert it's not a successful call mapped to NotImplemented.
        assert!(
            !matches!(err, ClientError::Call(_)),
            "expected transport-level error after RPC shutdown, got JSON-RPC call error: {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_rpc_bind_fails() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
        let pid_dir = TempDir::new().expect("bug: tempdir creates");
        let pid_path = pid_dir.path().join("hogd.pid");
        let _pid_guard = EnvGuard::set(
            FAKE_HOGD_DEV_PID_FILE_ENV,
            pid_path.to_str().expect("bug: tempdir path is utf-8"),
        );

        // Occupy a loopback port so the supervisor's RPC bind hits EADDRINUSE.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: ephemeral bind succeeds");
        let occupied = listener
            .local_addr()
            .expect("bug: bound listener has local addr");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address(occupied.to_string())
            .with_hogd_dev(hogd);

        let result =
            tokio::time::timeout(Duration::from_secs(10), NodeSupervisorHandle::start(config))
                .await
                .expect("bug: start should roll back and return promptly on rpc bind failure");

        let err = result.expect_err("bug: occupied port should fail rpc bind");
        assert!(
            matches!(err, NodeSupervisorError::RpcBind { .. }),
            "expected RpcBind, got {err:?}",
        );

        // The fake wrote its PID before the readiness line; if the rollback
        // path didn't actually reap the child, the OS would still know
        // about this pid.
        let pid =
            read_pid_file(&pid_path).expect("bug: fake should have written pid before readiness");
        assert!(
            wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
            "rollback leaked fake hogd-dev pid {pid}",
        );

        drop(listener);
    }

    #[test]
    fn plane_status_scheduler_serializes_to_scheduler_string() {
        let status = PlaneStatus::Scheduler;
        let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
        assert_eq!(json, "scheduler");
    }

    #[test]
    fn plane_status_indexer_serializes_to_indexer_string() {
        let status = PlaneStatus::Indexer;
        let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
        assert_eq!(json, "indexer");
    }

    #[test]
    fn plane_status_db_gateway_serializes_to_db_gateway_string() {
        let status = PlaneStatus::DbGateway;
        let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
        assert_eq!(json, "db_gateway");
    }

    #[test]
    fn plane_status_stub_serializes_to_stub_string() {
        let status = PlaneStatus::Stub;
        let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
        assert_eq!(json, "stub");
    }

    #[test]
    fn plane_status_view_gateway_serializes_to_view_gateway_string() {
        let status = PlaneStatus::ViewGateway;
        let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
        assert_eq!(json, "view_gateway");
    }

    #[test]
    fn plane_status_read_predicates_match_known_planes() {
        assert!(!PlaneStatus::Scheduler.is_indexer_backed());
        assert!(PlaneStatus::Indexer.is_indexer_backed());
        assert!(!PlaneStatus::DbGateway.is_indexer_backed());
        assert!(!PlaneStatus::ViewGateway.is_indexer_backed());
        assert!(!PlaneStatus::Stub.is_indexer_backed());
    }

    #[test]
    fn backend_planes_hogd_dev_full_constructor() {
        let planes = BackendPlanes::hogd_dev_full(PlaneStatus::ViewGateway);
        assert_eq!(planes.submit, PlaneStatus::Scheduler);
        assert_eq!(planes.chain, PlaneStatus::Scheduler);
        assert_eq!(planes.read, PlaneStatus::Indexer);
        assert_eq!(planes.view, PlaneStatus::ViewGateway);
    }

    #[test]
    fn backend_planes_hogd_dev_full_serializes_correctly() {
        let planes = BackendPlanes::hogd_dev_full(PlaneStatus::ViewGateway);
        let json = serde_json::to_value(planes).expect("bug: BackendPlanes serializes");
        let expected = serde_json::json!({
            "submit": "scheduler",
            "chain": "scheduler",
            "read": "indexer",
            "view": "view_gateway"
        });
        assert_eq!(json, expected);
    }

    #[test]
    fn backend_planes_hogd_dev_storage_only_constructor() {
        let planes = BackendPlanes::hogd_dev_storage_only(PlaneStatus::Stub);
        assert_eq!(planes.submit, PlaneStatus::Scheduler);
        assert_eq!(planes.chain, PlaneStatus::Scheduler);
        assert_eq!(planes.read, PlaneStatus::DbGateway);
        assert_eq!(planes.view, PlaneStatus::Stub);
    }

    #[test]
    fn desired_rpc_nofile_limit_uses_floor_and_headroom() {
        assert_eq!(desired_rpc_nofile_limit(10), RPC_NOFILE_LIMIT_FLOOR);
        assert_eq!(
            desired_rpc_nofile_limit(8_192),
            8_192 + RPC_NOFILE_LIMIT_HEADROOM
        );
    }

    #[cfg(unix)]
    #[test]
    fn rpc_nofile_soft_limit_update_fails_when_hard_limit_is_too_low() {
        let err = rpc_nofile_soft_limit_update(
            8_704,
            libc::rlimit {
                rlim_cur: 4_096,
                rlim_max: 8_192,
            },
        )
        .expect_err("hard limit below target should fail");

        assert!(matches!(
            err,
            NodeSupervisorError::RpcNofileHardLimit {
                target: 8_704,
                hard: 8_192
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rpc_nofile_soft_limit_update_returns_needed_soft_limit() {
        let update = rpc_nofile_soft_limit_update(
            8_704,
            libc::rlimit {
                rlim_cur: 4_096,
                rlim_max: 16_384,
            },
        )
        .expect("hard limit permits target");

        assert_eq!(update, Some(8_704));
    }

    #[cfg(unix)]
    #[test]
    fn rpc_nofile_soft_limit_update_skips_when_soft_limit_is_high_enough() {
        let update = rpc_nofile_soft_limit_update(
            8_704,
            libc::rlimit {
                rlim_cur: 16_384,
                rlim_max: 16_384,
            },
        )
        .expect("existing soft limit should pass");

        assert_eq!(update, None);
    }

    #[test]
    fn backend_planes_hogd_dev_storage_only_serializes_correctly() {
        let planes = BackendPlanes::hogd_dev_storage_only(PlaneStatus::Stub);
        let json = serde_json::to_value(planes).expect("bug: BackendPlanes serializes");
        let expected = serde_json::json!({
            "submit": "scheduler",
            "chain": "scheduler",
            "read": "db_gateway",
            "view": "stub"
        });
        assert_eq!(json, expected);
    }

    // -----------------------------------------------------------------------
    // Full read-plane integration tests (requires `ready-with-indexer` mode)
    // -----------------------------------------------------------------------

    /// Canned tx hashes from the fake indexer's committed set.
    /// Mirrors the constants in `tests/fixtures/fake_hogd_dev.rs`.
    const CANNED_HASH_A_HEX: &str =
        "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CANNED_HASH_UNKNOWN_HEX: &str =
        "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    const CANNED_HEIGHT_A: i64 = 5;
    const CANNED_GAS_USED_A: u64 = 1_500;
    const CANNED_STORAGE_KEY_HEX: &str =
        "0x4242424242424242424242424242424242424242424242424242424242424242";
    const CANNED_STORAGE_MISSING_KEY_HEX: &str =
        "0x4343434343434343434343434343434343434343434343434343434343434343";
    const CANNED_STORAGE_VALUE_HEX: &str = "0x6c6f63616c2d6e6f64652d73746f72616765";
    const CANNED_STORAGE_HEIGHT: i64 = 12;

    #[tokio::test]
    #[serial]
    async fn start_with_indexer_reports_hogd_dev_full_backend_planes() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev with indexer");

        assert_eq!(
            handle.backend_planes(),
            BackendPlanes::hogd_dev_full(PlaneStatus::Stub),
            "supervisor with indexer should report hogd_dev_full backend planes"
        );
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.rpc_addr()))
            .expect("bug: jsonrpsee http client builds");

        let response = client
            .request_airdrop(AirdropRequest {
                recipient: Hex0x(groundhog_xdr::types::ContractAddress::from([0x88; 32])),
                client_nonce: ClientNonce::new(vec![0x99])
                    .expect("bug: test client nonce should validate"),
            })
            .await
            .expect("bug: full local runtime should expose airdrop");

        assert!(matches!(response, AirdropAttemptResponse::Pending { .. }));

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn start_with_gateway_without_indexer_reports_storage_only_backend_planes() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-gateway");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev with gateway");

        assert_eq!(
            handle.backend_planes(),
            BackendPlanes::hogd_dev_storage_only(PlaneStatus::Stub),
            "supervisor with only DbGateway should report storage-only backend planes"
        );
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.rpc_addr()))
            .expect("bug: jsonrpsee http client builds");
        let error = client
            .request_airdrop(AirdropRequest {
                recipient: Hex0x(groundhog_xdr::types::ContractAddress::from([0x88; 32])),
                client_nonce: ClientNonce::new(vec![0x99])
                    .expect("bug: test client nonce should validate"),
            })
            .await
            .expect_err("bug: storage-only runtime must not expose airdrop");
        assert!(
            matches!(&error, ClientError::Call(object) if object.code() == jsonrpsee::types::error::METHOD_NOT_FOUND_CODE),
            "storage-only runtime should omit airdrop without a receipt backend, got {error:?}",
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn transaction_status_committed_returns_committed_with_correct_height() {
        // Arrange: start supervisor against fake that has both scheduler + indexer.
        // The fake scheduler returns empty (no pending), the fake indexer has
        // CANNED_TX_HASH_A committed at CANNED_HEIGHT_A.
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start");
        let url = format!("http://{}", handle.rpc_addr());

        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        // Act: query status for a canned committed hash.
        let status: serde_json::Value = client
            .request(
                "groundhog_getTransactionStatus",
                jsonrpsee::rpc_params![CANNED_HASH_A_HEX],
            )
            .await
            .expect("bug: getTransactionStatus should succeed");

        // Assert: status field is "committed", height matches.
        assert_eq!(
            status["status"], "committed",
            "expected committed status for known tx, got {status}"
        );
        assert_eq!(
            status["height"], CANNED_HEIGHT_A,
            "committed status height should be CANNED_HEIGHT_A"
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn transaction_status_unknown_hash_returns_unknown() {
        // Arrange
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start");
        let url = format!("http://{}", handle.rpc_addr());

        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        // Act: query status for an unknown hash.
        let status: serde_json::Value = client
            .request(
                "groundhog_getTransactionStatus",
                jsonrpsee::rpc_params![CANNED_HASH_UNKNOWN_HEX],
            )
            .await
            .expect("bug: getTransactionStatus should succeed for unknown hash");

        // Assert: status field is "unknown".
        assert_eq!(
            status["status"], "unknown",
            "expected unknown status for non-canned hash, got {status}"
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn transaction_receipt_committed_returns_some() {
        // Arrange
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start");
        let url = format!("http://{}", handle.rpc_addr());

        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        // Act: request receipt for a canned committed hash.
        let receipt: serde_json::Value = client
            .request(
                "groundhog_getTransactionReceipt",
                jsonrpsee::rpc_params![CANNED_HASH_A_HEX],
            )
            .await
            .expect("bug: getTransactionReceipt should succeed");

        // Assert: receipt is non-null, tx_hash and height round-trip correctly.
        assert!(
            !receipt.is_null(),
            "receipt should be non-null for committed tx"
        );
        assert_eq!(
            receipt["height"], CANNED_HEIGHT_A,
            "receipt height should be CANNED_HEIGHT_A"
        );
        assert_eq!(
            receipt["txHash"].as_str().unwrap_or("").to_lowercase(),
            CANNED_HASH_A_HEX,
            "receipt txHash should round-trip"
        );
        assert_eq!(
            receipt["gasUsed"], CANNED_GAS_USED_A,
            "receipt should carry the gas the fake indexer's event log records"
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn contract_events_first_page_returns_two_events() {
        // Arrange
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start");
        let url = format!("http://{}", handle.rpc_addr());

        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        // Build a minimal EventQuery (first page, one named contract).
        // Serialization shapes:
        //   - `IterationOrder` serializes as "ascending" / "descending"
        //   - `BlockAnchor` serializes as a tagged object
        //   - `PaginationCursor` serializes as `0x`-prefixed hex; `0x` = empty
        let contract = format!("0x{}", "01".repeat(32));
        let query = serde_json::json!({
            "filters": [{
                "contractAddresses": [contract],
                "topics": {"topic0": [], "topic1": [], "topic2": []},
            }],
            "order": "ascending",
            "anchor": { "kind": "atBeginning" },
            "cursor": "0x",
            "limit": 10
        });

        // Act
        let page: serde_json::Value = client
            .request("groundhog_getEvents", jsonrpsee::rpc_params![query])
            .await
            .expect("bug: getEvents should succeed");

        // Assert: first page has 2 events and a non-empty cursor.
        let events = page["events"]
            .as_array()
            .expect("bug: events should be array");
        assert_eq!(events.len(), 2, "first event page should contain 2 events");
        assert!(
            events
                .iter()
                .all(|event| event["event"]["contractAddress"] == contract),
            "events should come from the filtered contract"
        );
        let cursor = page["cursor"].as_str().unwrap_or("");
        assert!(
            cursor.len() > 2,
            "first page cursor should be non-empty (got {cursor:?})"
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn get_storage_at_existing_key_returns_item_from_hogd_dev_gateway() {
        // Arrange
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start");
        let url = format!("http://{}", handle.rpc_addr());

        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");
        let address = format!("0x{}", "00".repeat(32));

        // Act
        let read: serde_json::Value = client
            .request(
                "groundhog_getStorageAt",
                jsonrpsee::rpc_params![address, CANNED_STORAGE_KEY_HEX],
            )
            .await
            .expect("bug: getStorageAt should succeed");

        // Assert
        assert_eq!(read["blockHeight"], CANNED_STORAGE_HEIGHT);
        assert_eq!(read["item"]["kind"], "bytestring");
        assert_eq!(read["item"]["bytes"], CANNED_STORAGE_VALUE_HEX);

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn get_storage_at_works_when_gateway_is_advertised_without_indexer() {
        // Arrange
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-gateway");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start");
        let url = format!("http://{}", handle.rpc_addr());

        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");
        let address = format!("0x{}", "00".repeat(32));

        // Act
        let read: serde_json::Value = client
            .request(
                "groundhog_getStorageAt",
                jsonrpsee::rpc_params![address, CANNED_STORAGE_KEY_HEX],
            )
            .await
            .expect("bug: getStorageAt should succeed with DbGateway only");

        // Assert
        assert_eq!(read["blockHeight"], CANNED_STORAGE_HEIGHT);
        assert_eq!(read["item"]["kind"], "bytestring");
        assert_eq!(read["item"]["bytes"], CANNED_STORAGE_VALUE_HEX);

        let err = client
            .request::<serde_json::Value, _>(
                "groundhog_getTransactionReceipt",
                jsonrpsee::rpc_params![CANNED_HASH_A_HEX],
            )
            .await
            .expect_err("bug: receipt reads should remain unavailable without indexer");
        assert!(
            matches!(&err, ClientError::Call(object) if object.code() == codes::NOT_IMPLEMENTED),
            "expected NOT_IMPLEMENTED JSON-RPC error, got {err:?}",
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn get_storage_at_missing_key_returns_null_item_with_snapshot_height() {
        // Arrange
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start");
        let url = format!("http://{}", handle.rpc_addr());

        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");
        let address = format!("0x{}", "00".repeat(32));

        // Act
        let read: serde_json::Value = client
            .request(
                "groundhog_getStorageAt",
                jsonrpsee::rpc_params![address, CANNED_STORAGE_MISSING_KEY_HEX],
            )
            .await
            .expect("bug: getStorageAt miss should succeed");

        // Assert
        assert_eq!(read["blockHeight"], CANNED_STORAGE_HEIGHT);
        assert!(
            read["item"].is_null(),
            "missing storage item should serialize as null"
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn get_storage_at_without_gateway_endpoint_returns_backend_unavailable() {
        // Arrange
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer-no-gateway");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start");
        let url = format!("http://{}", handle.rpc_addr());

        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");
        let address = format!("0x{}", "00".repeat(32));

        // Act
        let err = client
            .request::<serde_json::Value, _>(
                "groundhog_getStorageAt",
                jsonrpsee::rpc_params![address, CANNED_STORAGE_KEY_HEX],
            )
            .await
            .expect_err("bug: missing DbGateway should fail");

        // Assert
        assert!(
            matches!(&err, ClientError::Call(object) if object.code() == codes::BACKEND_UNAVAILABLE),
            "expected BACKEND_UNAVAILABLE JSON-RPC error, got {err:?}",
        );

        handle.stop().await.expect("bug: clean shutdown");
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_indexer_connect_fails() {
        let err = start_fake_supervisor_expect_error("indexer-connect-fail").await;

        assert!(
            matches!(err, NodeSupervisorError::IndexerConnect { .. }),
            "expected IndexerConnect error, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_indexer_rpc_fail_at_startup() {
        // The `indexer-rpc-fail` fake mode returns a ProgramError even for the
        // intro handshake. With the intro probe in HogdDevReadBackend::connect,
        // this now fails at startup rather than at the first read call.
        let err = start_fake_supervisor_expect_error("indexer-rpc-fail").await;

        assert!(
            matches!(
                err,
                NodeSupervisorError::IndexerRpc {
                    op: "indexer_block_explorer_intro",
                    ..
                }
            ),
            "expected IndexerRpc error for intro, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_db_gateway_connect_fails() {
        let err = start_fake_supervisor_expect_error("db-gateway-connect-fail").await;

        assert!(
            matches!(err, NodeSupervisorError::DbGatewayConnect { .. }),
            "expected DbGatewayConnect error, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_db_gateway_rpc_fail_at_startup() {
        let err = start_fake_supervisor_expect_error("db-gateway-rpc-fail").await;

        assert!(
            matches!(
                err,
                NodeSupervisorError::DbGatewayRpc {
                    op: "dbgateway_intro",
                    ..
                }
            ),
            "expected DbGatewayRpc error for intro, got {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_db_gateway_intro_rejected() {
        let err = start_fake_supervisor_expect_error("db-gateway-intro-rejected").await;

        let message = err.to_string();
        match err {
            NodeSupervisorError::ProtocolIntroRejected {
                service,
                outcome,
                outcome_name,
                is_protocol_mismatch,
                protocol,
                expected_protocol,
            } => {
                assert_eq!(service, "DbGateway");
                assert_eq!(outcome, 1);
                assert_eq!(outcome_name, "DbGatewayIntroMismatchedProtocol");
                assert!(is_protocol_mismatch);
                assert_eq!(protocol, protocol_text(&PROTOCOL_VERSION));
                assert_eq!(
                    expected_protocol.as_deref(),
                    Some(protocol_text(&PROTOCOL_VERSION).as_str())
                );
                assert!(message.contains("DbGateway intro mismatch"));
                assert!(message.contains(&protocol_text(&PROTOCOL_VERSION)));
                assert!(message.contains("DbGatewayIntroMismatchedProtocol"));
            }
            other => panic!("expected ProtocolIntroRejected error, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_indexer_intro_rejected() {
        let err = start_fake_supervisor_expect_error("indexer-intro-rejected").await;

        let message = err.to_string();
        match err {
            NodeSupervisorError::ProtocolIntroRejected {
                service,
                outcome,
                outcome_name,
                is_protocol_mismatch,
                protocol,
                expected_protocol,
            } => {
                assert_eq!(service, "IndexerBlockExplorer");
                assert_eq!(outcome, 1);
                assert_eq!(outcome_name, "IntroMismatchedProtocol");
                assert!(is_protocol_mismatch);
                assert_eq!(protocol, protocol_text(&PROTOCOL_VERSION));
                assert_eq!(
                    expected_protocol.as_deref(),
                    Some(protocol_text(&PROTOCOL_VERSION).as_str())
                );
                assert!(message.contains("IndexerBlockExplorer intro mismatch"));
                assert!(message.contains(&protocol_text(&PROTOCOL_VERSION)));
                assert!(message.contains("IntroMismatchedProtocol"));
            }
            other => panic!("expected ProtocolIntroRejected error, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial]
    async fn transaction_status_indexer_runtime_rpc_fail_surfaces_backend_unavailable() {
        // The `indexer-runtime-rpc-fail` fake mode lets intro succeed but
        // returns errors for all data-plane RPCs (get_transaction_info, etc).
        // Startup should succeed; JSON-RPC calls should return a backend error.
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "indexer-runtime-rpc-fail");
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);

        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start when indexer intro succeeds");

        let url = format!("http://{}", handle.rpc_addr());
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        // Act: query a tx status; the fake indexer will fail the data-plane call.
        let err = client
            .request::<serde_json::Value, _>(
                "groundhog_getTransactionStatus",
                jsonrpsee::rpc_params![CANNED_HASH_A_HEX],
            )
            .await
            .expect_err("bug: getTransactionStatus should error when indexer data-plane fails");

        // Assert: error is a JSON-RPC call error (backend unavailable or
        // internal), not a transport-level failure.
        assert!(
            matches!(&err, ClientError::Call(_)),
            "expected a JSON-RPC call error from backend failure, got {err:?}",
        );

        handle.stop().await.expect("bug: clean shutdown");
    }
}
