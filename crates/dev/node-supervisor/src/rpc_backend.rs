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
    ChainPlane, RpcRequestContext, RpcServerError, SubmitPlane, ValidatedTransaction,
};
use groundhog_json_rpc_types::{
    BlockHeight, NetworkHashHex,
    block::{BlockInfo, ChainTip, consensus_value_hash},
    protocol_token::protocol_from_text,
};
use groundhog_scheduler::{
    DbTip, ExpectedDBTip, ParallelBatch, RPCError, SchedGatewayIntroOutcome,
    SchedGatewaySubmitOutcome, SchedGatewaySubmitOutcomeBatch,
    parallel::ParallelSignedTransactionBatch, sched_coord::SchedulerCoord_SchedulerCoord1_Client,
    sched_gateway::SchedGateway_SchedGateway1_Client,
};
use groundhog_x23_transport::{connect_with_config, hogd_large_message_config};
use std::{fmt, net::SocketAddr};
use xdr_common::{BytesM, VecM};

use crate::{
    NodeSupervisorError, block_driver::BlockDriverTrigger, genesis::require_dev_genesis,
    request_deadline::within_request_deadline,
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
    block_driver: Option<BlockDriverTrigger>,
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
        Ok(Self {
            coordinator,
            gateway,
            block_driver: None,
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
    pub(crate) fn with_block_driver(mut self, block_driver: BlockDriverTrigger) -> Self {
        self.block_driver = Some(block_driver);
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
        within_request_deadline(context, async {
            let batch = single_transaction_batch(&tx.bytes)?;
            let outcomes = self
                .gateway
                .schedgateway_submit_tx_batch_with_outcomes(&batch)
                .await
                .map_err(|error| classify_submit_rpc_error(&error))?;
            result_for_single_submit(outcomes)?;
            if let Some(block_driver) = &self.block_driver {
                block_driver.trigger_after_submit();
            }
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl ChainPlane for HogdDevSchedulerBackend {
    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self, context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
        within_request_deadline(context, async {
            let result = self
                .gateway
                .schedgateway_get_tip()
                .await
                .map_err(|error| RpcServerError::BackendUnavailable(error.to_string()))?;
            match result {
                ExpectedDBTip::kRPCNone { db_tip } => chain_tip_from_db_tip(db_tip),
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

fn single_transaction_batch(
    tx_bytes: &[u8],
) -> Result<ParallelSignedTransactionBatch, RpcServerError> {
    let tx_len = u32::try_from(tx_bytes.len())
        .map_err(|_| RpcServerError::TxInvalid("transaction too large".into()))?;
    Ok(ParallelSignedTransactionBatch::new(ParallelBatch {
        segment_tokens: VecM(vec![1]),
        segment_bytes: VecM(vec![tx_len]),
        xdr: BytesM(tx_bytes.to_vec()),
    }))
}

fn classify_submit_rpc_error(error: &rpc_driver::RpcError) -> RpcServerError {
    // An x23 failure carries no request-scoped admission outcome. The caller
    // must treat admission as indeterminate and must not assume rejection.
    RpcServerError::BackendUnavailable(format!(
        "hogd-dev scheduler gateway submit outcome is indeterminate: {error}"
    ))
}

fn result_for_single_submit(
    outcomes: SchedGatewaySubmitOutcomeBatch,
) -> Result<(), RpcServerError> {
    let mut outcomes = outcomes.0.into_iter();
    let Some(outcome) = outcomes.next() else {
        return Err(RpcServerError::BackendUnavailable(
            "scheduler gateway returned no admission outcome for one transaction".into(),
        ));
    };
    if outcomes.next().is_some() {
        return Err(RpcServerError::BackendUnavailable(
            "scheduler gateway returned multiple admission outcomes for one transaction".into(),
        ));
    }
    match outcome {
        SchedGatewaySubmitOutcome::SchedGatewaySubmitAccepted => Ok(()),
        SchedGatewaySubmitOutcome::SchedGatewaySubmitRejected => Err(RpcServerError::TxRejected(
            "scheduler rejected the signed transaction".into(),
        )),
        SchedGatewaySubmitOutcome::SchedGatewaySubmitRetryable => Err(RpcServerError::Overloaded(
            "scheduler could not admit the transaction; retry with backoff".into(),
        )),
        SchedGatewaySubmitOutcome::SchedGatewaySubmitInternalError => Err(
            RpcServerError::BackendUnavailable("scheduler admission dependency failed".into()),
        ),
        unknown => Err(RpcServerError::BackendUnavailable(format!(
            "scheduler returned unknown submission outcome {}",
            unknown.0
        ))),
    }
}

fn chain_tip_from_db_tip(DbTip { value, .. }: DbTip) -> Result<ChainTip, RpcServerError> {
    let block_hash = consensus_value_hash(&value).map_err(|error| {
        RpcServerError::BackendUnavailable(format!(
            "hogd-dev scheduler returned an unencodable consensus value: {error}"
        ))
    })?;
    Ok(ChainTip {
        height: value.proposal_header.block_height.clone(),
        block_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_transaction_batch_packs_one_segment() {
        let tx_bytes = vec![0xaa, 0xbb, 0xcc];
        let batch = single_transaction_batch(&tx_bytes).expect("bug: test tx should pack");

        assert_eq!(batch.segment_tokens.0, vec![1]);
        assert_eq!(batch.segment_bytes.0, vec![3]);
        assert_eq!(batch.xdr.0, tx_bytes);
    }

    #[test]
    fn single_submit_requires_exactly_one_outcome() {
        assert!(matches!(
            result_for_single_submit(VecM(vec![])),
            Err(RpcServerError::BackendUnavailable(_))
        ));
        assert!(matches!(
            result_for_single_submit(VecM(vec![
                SchedGatewaySubmitOutcome::SchedGatewaySubmitAccepted,
                SchedGatewaySubmitOutcome::SchedGatewaySubmitAccepted,
            ])),
            Err(RpcServerError::BackendUnavailable(_))
        ));
    }

    #[test]
    fn single_submit_maps_each_gateway_outcome() {
        let accepted = result_for_single_submit(VecM(vec![
            SchedGatewaySubmitOutcome::SchedGatewaySubmitAccepted,
        ]));
        assert!(accepted.is_ok());

        let rejected = result_for_single_submit(VecM(vec![
            SchedGatewaySubmitOutcome::SchedGatewaySubmitRejected,
        ]));
        assert!(matches!(rejected, Err(RpcServerError::TxRejected(_))));

        let retryable = result_for_single_submit(VecM(vec![
            SchedGatewaySubmitOutcome::SchedGatewaySubmitRetryable,
        ]));
        assert!(matches!(retryable, Err(RpcServerError::Overloaded(_))));

        let internal = result_for_single_submit(VecM(vec![
            SchedGatewaySubmitOutcome::SchedGatewaySubmitInternalError,
        ]));
        assert!(matches!(
            internal,
            Err(RpcServerError::BackendUnavailable(_))
        ));

        let unknown = result_for_single_submit(VecM(vec![SchedGatewaySubmitOutcome(99)]));
        assert!(matches!(
            unknown,
            Err(RpcServerError::BackendUnavailable(_))
        ));
    }
}
