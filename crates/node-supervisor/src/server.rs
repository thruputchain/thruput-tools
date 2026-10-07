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
use groundhog_json_rpc_api::{AirdropApiServer, GroundhogApiServer, LocalFundingApiServer};
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
    hogd_dev::{DEFAULT_HOGD_DEV_BINARY, validate_runtime_identity},
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

        match validate_runtime_identity(
            hogd.readiness(),
            protocol_version,
            hogd.expected_network_hash(),
        ) {
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

        let started = async {
            start_rpc_server(
                &config,
                hogd.readiness(),
                protocol_version,
                hogd.faucet_signer()?,
                hogd.funding_journal()?,
                hogd.funding_lock_lease(),
            )
            .await
        }
        .await;
        match started {
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
    signer: std::sync::Arc<groundhog_signing::LocalSigner>,
    journal: std::sync::Arc<std::sync::Mutex<crate::funding_journal::FundingJournal>>,
    lock_lease: Option<std::sync::Arc<std::fs::File>>,
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
        let local_airdrop =
            validated_local_airdrop_context(manifest, config, signer, journal, lock_lease)?;
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
        view_gas_limit: config.view_gas_limit,
        view_batch: config.view_batch,
        view_timeout_ms: u64::try_from(config.view_timeout.as_millis()).unwrap_or(u64::MAX),
        block_driver_config: config.block_driver,
        rpc_max_connections: config.rpc_max_connections,
        rpc_batch_concurrency: config.rpc_batch_concurrency,
        bundle_identity: config.bundle_identity.clone(),
    }
}

fn validated_local_airdrop_context(
    manifest: &groundhog_json_rpc_types::runtime_manifest::RuntimeManifest,
    config: &NodeSupervisorConfig,
    signer: std::sync::Arc<groundhog_signing::LocalSigner>,
    journal: std::sync::Arc<std::sync::Mutex<crate::funding_journal::FundingJournal>>,
    lock_lease: Option<std::sync::Arc<std::fs::File>>,
) -> Result<LocalAirdropContext, NodeSupervisorError> {
    LocalAirdropContext::from_validated_runtime(manifest, config, signer, journal, lock_lease)
        .map_err(|reason| NodeSupervisorError::InvalidRuntimeManifest {
            program: DEFAULT_HOGD_DEV_BINARY.to_owned(),
            reason,
        })
}

struct RpcServerOptions {
    view_batch: groundhog_json_rpc_server::ViewBatchConfig,
    view_timeout_ms: u64,
    network_info: NetworkInfo,
    local_airdrop: Option<LocalAirdropContext>,
    backend_planes: BackendPlanes,
    view_gas_limit: u64,
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
        view_batch,
        view_timeout_ms,
        network_info,
        local_airdrop,
        backend_planes,
        view_gas_limit,
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
        RpcServerConfig::default()
            .with_view_gas_limit(view_gas_limit)
            .with_view_timeout_ms(view_timeout_ms)
            .with_view_batch(view_batch),
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
            .merge(LocalFundingApiServer::into_rpc(local_airdrop.clone()))
            .expect("bug: local funding methods register without conflicts");
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
mod tests;
