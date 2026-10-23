//! hogd-dev adapters for submission and chain metadata.
//!
//! Submissions use `SchedGateway`; tip/configuration reads use `DbGateway`.
//! The internal `SchedulerCoord` endpoint remains available only for
//! genesis and local block driving. The read plane is implemented in
//! [`crate::read_backend`] and backed by hogd-dev's `IndexerBlockExplorer` /
//! `DbGateway` endpoints. Dev-control remains stubbed pending a hogd-dev stable
//! x23 surface.

use async_trait::async_trait;
use std::{fmt, net::SocketAddr};
use thruput_db_gateway::DbGateway_DbGateway1_Client;
use thruput_hogd_backend::{db_gateway as shared_db_gateway, intro};
use thruput_indexer::IndexerBlockExplorer_IndexerBlockExplorer1_Client;
use thruput_json_rpc_server::{
    ChainPlane, RpcRequestContext, RpcServerError, TipReading, ValidatedTransaction,
};
use thruput_json_rpc_types::{
    BlockHeight, NetworkHashText, block::BlockInfo, protocol_token::protocol_from_text,
};
use thruput_scheduler::{
    DbTip, ExpectedDBTip, sched_coord::SchedulerCoord_SchedulerCoord1_Client,
    sched_gateway::SchedGateway_SchedGateway1_Client,
};
use thruput_x23_transport::{connect_with_config, hogd_large_message_config};

use crate::{
    NodeSupervisorError,
    block_driver::BlockDriverTrigger,
    genesis::require_dev_genesis,
    request_deadline::within_request_deadline,
    service::{DB_GATEWAY, SCHEDULER_GATEWAY},
    submit_batcher::SubmitBatcher,
};

const SCHEDULER_COORD_POOL_SIZE: usize = 1;
// Public chain and submit methods share SchedGateway. Keep a small pool so a
// long-running submit does not block tip and pending probes.
const SCHEDULER_GATEWAY_POOL_SIZE: usize = 2;

/// Scheduler submission and DB-backed committed chain reads for hogd-dev.
#[derive(Clone)]
pub(crate) struct HogdDevSchedulerBackend {
    coordinator: SchedulerCoord_SchedulerCoord1_Client,
    gateway: SchedGateway_SchedGateway1_Client,
    submitter: SubmitBatcher,
    db_gateway: Option<DbGateway_DbGateway1_Client>,
    indexer: Option<IndexerBlockExplorer_IndexerBlockExplorer1_Client>,
    expected_network: Option<NetworkHashText>,
}

impl fmt::Debug for HogdDevSchedulerBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HogdDevSchedulerBackend")
            .finish_non_exhaustive()
    }
}

impl HogdDevSchedulerBackend {
    /// Reuse the read adapter's validated indexer pool for historical blocks.
    pub(crate) fn with_indexer(
        mut self,
        client: IndexerBlockExplorer_IndexerBlockExplorer1_Client,
    ) -> Self {
        self.indexer = Some(client);
        self
    }

    pub(crate) async fn connect(
        coordinator_endpoint: SocketAddr,
        gateway_endpoint: SocketAddr,
        protocol_version: &str,
        expected_protocol: Option<&str>,
    ) -> Result<Self, NodeSupervisorError> {
        let coordinator = connect_with_config(
            "hogd-dev scheduler coordinator",
            &coordinator_endpoint.to_string(),
            SCHEDULER_COORD_POOL_SIZE,
            hogd_large_message_config(),
            SchedulerCoord_SchedulerCoord1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::SchedulerConnect {
            endpoint: coordinator_endpoint,
            source,
        })?;
        let gateway = connect_with_config(
            "hogd-dev scheduler gateway",
            &gateway_endpoint.to_string(),
            SCHEDULER_GATEWAY_POOL_SIZE,
            hogd_large_message_config(),
            SchedGateway_SchedGateway1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::SchedulerGatewayConnect {
            endpoint: gateway_endpoint,
            source,
        })?;
        let protocol = protocol_from_text(protocol_version)?;
        let intro_outcome = gateway
            .schedgateway_intro(&protocol)
            .await
            .map_err(|source| NodeSupervisorError::SchedulerGatewayRpc {
                op: "schedgateway_intro",
                source,
            })?;
        let intro = intro::sched_gateway(intro_outcome);
        if !intro.accepted() {
            return Err(NodeSupervisorError::intro_rejected(
                intro,
                protocol_version,
                expected_protocol,
            ));
        }
        let submitter = SubmitBatcher::spawn(gateway.clone());
        Ok(Self {
            coordinator,
            gateway,
            submitter,
            db_gateway: None,
            indexer: None,
            expected_network: None,
        })
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn require_dev_genesis(
        &self,
        expected_network_hash: &NetworkHashText,
    ) -> Result<(), NodeSupervisorError> {
        let coordinator_tip = require_dev_genesis(&self.coordinator).await?;
        validate_scheduler_network_hash(expected_network_hash, &coordinator_tip)?;

        let gateway_tip = self
            .gateway
            .schedgateway_get_tip()
            .await
            .map_err(|source| NodeSupervisorError::SchedulerGatewayRpc {
                op: "schedgateway_get_tip",
                source,
            })?;
        let gateway_tip = match gateway_tip {
            ExpectedDBTip::kRPCNone { db_tip } => db_tip,
            other => {
                return Err(NodeSupervisorError::SchedulerRejected {
                    op: "schedgateway_get_tip",
                    result: format!("{other:?}"),
                });
            }
        };
        validate_scheduler_network_hash(expected_network_hash, &gateway_tip)?;
        if coordinator_tip != gateway_tip {
            return Err(NodeSupervisorError::SchedulerPlaneTipMismatch {
                coordinator: format!("{coordinator_tip:?}"),
                gateway: format!("{gateway_tip:?}"),
            });
        }
        Ok(())
    }

    /// Borrow the internal coordinator client for genesis and block driving.
    pub(crate) fn scheduler_client(&self) -> &SchedulerCoord_SchedulerCoord1_Client {
        &self.coordinator
    }

    /// Borrow the public scheduler gateway client for pending-tx probes.
    pub(crate) fn scheduler_gateway_client(&self) -> &SchedGateway_SchedGateway1_Client {
        &self.gateway
    }

    /// Share the already-connected storage client with the chain plane.
    #[must_use]
    pub(crate) fn with_db_gateway(
        mut self,
        client: Option<DbGateway_DbGateway1_Client>,
        expected_network: NetworkHashText,
    ) -> Self {
        self.db_gateway = client;
        self.expected_network = Some(expected_network);
        self
    }

    /// Return a copy that wakes the local block driver after submit RPC success.
    pub(crate) fn with_block_driver(self, block_driver: BlockDriverTrigger) -> Self {
        self.submitter.set_block_driver(block_driver);
        self
    }
}

fn validate_scheduler_network_hash(
    expected: &NetworkHashText,
    db_tip: &DbTip,
) -> Result<(), NodeSupervisorError> {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(
        db_tip
            .value
            .proposal_header
            .network_hash
            .clone()
            .into_inner()
            .as_ref(),
    );
    let observed = NetworkHashText::from(bytes);
    if expected == &observed {
        return Ok(());
    }
    Err(NodeSupervisorError::SchedulerNetworkHashMismatch {
        expected: expected.clone(),
        observed,
    })
}

#[async_trait]
impl crate::airdrop::FundingSubmitPlane for HogdDevSchedulerBackend {
    #[tracing::instrument(skip_all, fields(tx_hash = ?tx.hash, tx_bytes_len = tx.bytes.len()))]
    async fn submit_funding(
        &self,
        context: RpcRequestContext,
        tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        self.submitter.submit(context, tx.bytes).await
    }
}

#[async_trait]
impl ChainPlane for HogdDevSchedulerBackend {
    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self, context: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        within_request_deadline(context, async {
            let client = self.db_gateway.as_ref().ok_or_else(|| {
                RpcServerError::BackendUnavailable("db_gateway not connected".into())
            })?;
            let result = client.dbgateway_get_config().await.map_err(|error| {
                RpcServerError::BackendUnavailable(format!("{DB_GATEWAY}: {error}"))
            })?;
            shared_db_gateway::tip_reading(DB_GATEWAY, result, self.expected_network.as_ref())
        })
        .await
    }

    #[tracing::instrument(skip_all)]
    async fn get_block_by_number(
        &self,
        context: RpcRequestContext,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        within_request_deadline(context, async {
            let client = self.indexer.as_ref().ok_or_else(|| {
                RpcServerError::BackendUnavailable("hogd-dev indexer is unavailable".into())
            })?;
            let result = client.get_block_by_height(&height).await.map_err(|error| {
                RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {error}"))
            })?;
            thruput_hogd_backend::indexer::block_lookup("hogd-dev indexer", &height, result)
        })
        .await
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(
        &self,
        context: RpcRequestContext,
    ) -> Result<u64, RpcServerError> {
        within_request_deadline(context, async {
            let count = self
                .gateway
                .schedgateway_pending_tx_count()
                .await
                .map_err(|error| {
                    RpcServerError::BackendUnavailable(format!(
                        "{SCHEDULER_GATEWAY} pending count failed: {error}"
                    ))
                })?;
            u64::try_from(count).map_err(|_| {
                RpcServerError::BackendUnavailable(format!(
                    "{SCHEDULER_GATEWAY} returned negative pending count: {count}"
                ))
            })
        })
        .await
    }
}

#[async_trait]
impl thruput_json_rpc_server::native::transaction::NativeSubmitPlane for HogdDevSchedulerBackend {
    #[tracing::instrument(skip_all)]
    async fn native_submit(
        &self,
        context: RpcRequestContext,
        tx: ValidatedTransaction,
        attempt: thruput_json_rpc_server::native::transaction::SubmissionAttempt,
    ) -> Result<(), RpcServerError> {
        self.submitter
            .submit_native(context, tx.bytes, attempt)
            .await
    }
}
