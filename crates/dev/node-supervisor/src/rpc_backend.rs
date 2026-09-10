//! hogd-dev scheduler-backed adapters for the submit and chain planes.
//!
//! Public submit and chain queries use hogd-dev's advertised `SchedGateway`
//! endpoint. The internal `SchedulerCoord` endpoint remains available only for
//! genesis and local block driving. The read plane is implemented in
//! [`crate::read_backend`] and backed by hogd-dev's `IndexerBlockExplorer` /
//! `DbGateway` endpoints. Dev-control remains stubbed pending a hogd-dev stable
//! x23 surface.

use async_trait::async_trait;
use groundhog_json_rpc_server::{
    ChainPlane, RpcRequestContext, RpcServerError, SubmitPlane, TipReading, ValidatedTransaction,
};
use groundhog_json_rpc_types::{
    BlockHeight, NetworkHashHex,
    block::{BlockInfo, ChainTip, consensus_value_hash},
    protocol_token::protocol_from_text,
};
use groundhog_scheduler::{
    DbTip, ExpectedDBTip, RPCError, SchedGatewayIntroOutcome,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
    sched_gateway::SchedGateway_SchedGateway1_Client,
};
use groundhog_x23_transport::{connect_with_config, hogd_large_message_config};
use std::{fmt, net::SocketAddr};

use crate::{
    NodeSupervisorError, block_driver::BlockDriverTrigger, genesis::require_dev_genesis,
    request_deadline::within_request_deadline, submit_batcher::SubmitBatcher,
};

const SCHEDULER_COORD_POOL_SIZE: usize = 1;
// Public chain and submit methods share SchedGateway. Keep a small pool so a
// long-running submit does not block tip and pending probes.
const SCHEDULER_GATEWAY_POOL_SIZE: usize = 2;

/// Scheduler-backed adapter for hogd-dev chain and submit planes.
#[derive(Clone)]
pub(crate) struct HogdDevSchedulerBackend {
    coordinator: SchedulerCoord_SchedulerCoord1_Client,
    gateway: SchedGateway_SchedGateway1_Client,
    submitter: SubmitBatcher,
}

impl fmt::Debug for HogdDevSchedulerBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HogdDevSchedulerBackend")
            .finish_non_exhaustive()
    }
}

impl HogdDevSchedulerBackend {
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
        if intro_outcome != SchedGatewayIntroOutcome::SchedGatewayIntroSuccess {
            return Err(NodeSupervisorError::ProtocolIntroRejected {
                service: "SchedGateway",
                outcome: intro_outcome.0,
                outcome_name: scheduler_gateway_intro_outcome_name(intro_outcome),
                is_protocol_mismatch: intro_outcome
                    == SchedGatewayIntroOutcome::SchedGatewayIntroMismatchedProtocol,
                protocol: protocol_version.to_string(),
                expected_protocol: expected_protocol.map(str::to_string),
            });
        }
        let submitter = SubmitBatcher::spawn(gateway.clone());
        Ok(Self {
            coordinator,
            gateway,
            submitter,
        })
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn require_dev_genesis(
        &self,
        expected_network_hash: &NetworkHashHex,
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

    /// Return a copy that wakes the local block driver after submit RPC success.
    pub(crate) fn with_block_driver(self, block_driver: BlockDriverTrigger) -> Self {
        self.submitter.set_block_driver(block_driver);
        self
    }
}

fn scheduler_gateway_intro_outcome_name(outcome: SchedGatewayIntroOutcome) -> &'static str {
    match outcome.0 {
        value if value == SchedGatewayIntroOutcome::SchedGatewayIntroSuccess.0 => {
            "SchedGatewayIntroSuccess"
        }
        value if value == SchedGatewayIntroOutcome::SchedGatewayIntroMismatchedProtocol.0 => {
            "SchedGatewayIntroMismatchedProtocol"
        }
        _ => "UnknownSchedGatewayIntroOutcome",
    }
}

fn validate_scheduler_network_hash(
    expected: &NetworkHashHex,
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
    let observed = NetworkHashHex::from(bytes);
    if expected == &observed {
        return Ok(());
    }
    Err(NodeSupervisorError::SchedulerNetworkHashMismatch {
        expected: expected.clone(),
        observed,
    })
}

#[async_trait]
impl SubmitPlane for HogdDevSchedulerBackend {
    #[tracing::instrument(skip_all, fields(tx_hash = ?tx.hash, tx_bytes_len = tx.bytes.len()))]
    async fn submit_raw_transaction(
        &self,
        context: RpcRequestContext,
        tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        self.submitter.submit(context, tx.bytes).await
    }
}

impl HogdDevSchedulerBackend {
    /// Read the scheduler gateway's committed db tip.
    async fn db_tip(&self) -> Result<DbTip, RpcServerError> {
        let result = self
            .gateway
            .schedgateway_get_tip()
            .await
            .map_err(|error| RpcServerError::BackendUnavailable(error.to_string()))?;
        match result {
            ExpectedDBTip::kRPCNone { db_tip } => Ok(db_tip),
            ExpectedDBTip::Default { error } if error == RPCError::kRPCNotFound.0 => {
                tracing::error!(
                    "hogd-dev scheduler gateway reported no genesis after startup bootstrap"
                );
                Err(RpcServerError::BackendUnavailable(
                    "hogd-dev scheduler gateway has no genesis block".into(),
                ))
            }
            other => Err(RpcServerError::BackendUnavailable(format!(
                "unexpected scheduler gateway tip result: {other:?}"
            ))),
        }
    }
}

#[async_trait]
impl ChainPlane for HogdDevSchedulerBackend {
    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self, context: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        within_request_deadline(context, async {
            tip_reading_from_db_tip(self.db_tip().await?)
        })
        .await
    }

    #[tracing::instrument(skip_all)]
    async fn get_block_by_number(
        &self,
        _context: RpcRequestContext,
        _height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBlockByNumber".into()))
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
                        "hogd-dev scheduler gateway pending count failed: {error}"
                    ))
                })?;
            u64::try_from(count).map_err(|_| {
                RpcServerError::BackendUnavailable(format!(
                    "hogd-dev scheduler gateway returned negative pending count: {count}"
                ))
            })
        })
        .await
    }
}

fn tip_reading_from_db_tip(
    DbTip {
        value,
        network_config,
        ..
    }: DbTip,
) -> Result<TipReading, RpcServerError> {
    let block_hash = consensus_value_hash(&value).map_err(|error| {
        RpcServerError::BackendUnavailable(format!(
            "hogd-dev scheduler returned an unencodable consensus value: {error}"
        ))
    })?;
    Ok(TipReading::new(
        ChainTip {
            height: value.proposal_header.block_height.clone(),
            block_hash,
        },
        network_config,
    ))
}
