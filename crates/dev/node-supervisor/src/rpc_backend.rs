//! hogd-dev scheduler-backed adapters for the submit and chain planes.
//!
//! Both planes use hogd-dev's advertised `SchedulerCoord` endpoint. The read
//! plane is implemented in [`crate::read_backend`] and backed by hogd-dev's
//! `IndexerBlockExplorer` / `DbGateway` endpoints. Dev-control remains stubbed
//! pending a hogd-dev stable x23 surface.

use async_trait::async_trait;
use groundhog_json_rpc_server::{
    ChainPlane, RpcRequestContext, RpcServerError, SubmitPlane, ValidatedTransaction,
};
use groundhog_json_rpc_types::{
    BlockHeight,
    block::{BlockInfo, ChainTip},
    hex::Hex0x,
};
use groundhog_scheduler::{
    DbTip, ParallelBatch, SchedulerGetTipResult, parallel::ParallelSignedTransactionBatch,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use groundhog_x23_transport::{connect_with_config, hogd_large_message_config};
use std::{fmt, net::SocketAddr};
use xdr_common::{BytesM, VecM};

use crate::{
    NodeSupervisorError, block_driver::BlockDriverTrigger, genesis::ensure_dev_genesis,
    request_deadline::within_request_deadline,
};

// chainTip and submit share the SchedulerCoord endpoint. Keep a small pool so a
// long-running submit does not block tip checks during local-node startup and
// smoke tests.
const SCHEDULER_POOL_SIZE: usize = 2;

/// Scheduler-backed adapter for hogd-dev chain and submit planes.
#[derive(Clone)]
pub(crate) struct HogdDevSchedulerBackend {
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
    block_driver: Option<BlockDriverTrigger>,
}

impl fmt::Debug for HogdDevSchedulerBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HogdDevSchedulerBackend")
            .finish_non_exhaustive()
    }
}

impl HogdDevSchedulerBackend {
    pub(crate) async fn connect(endpoint: SocketAddr) -> Result<Self, NodeSupervisorError> {
        let scheduler = connect_with_config(
            "hogd-dev scheduler",
            &endpoint.to_string(),
            SCHEDULER_POOL_SIZE,
            hogd_large_message_config(),
            SchedulerCoord_SchedulerCoord1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::SchedulerConnect { endpoint, source })?;
        Ok(Self {
            scheduler,
            block_driver: None,
        })
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn ensure_dev_genesis(&self) -> Result<(), NodeSupervisorError> {
        ensure_dev_genesis(&self.scheduler).await
    }

    /// Borrow the inner scheduler client for use by sibling backends
    /// (e.g. the read backend's pending-tx probe).
    pub(crate) fn scheduler_client(&self) -> &SchedulerCoord_SchedulerCoord1_Client {
        &self.scheduler
    }

    /// Return a copy that wakes the local block driver after submit RPC success.
    pub(crate) fn with_block_driver(mut self, block_driver: BlockDriverTrigger) -> Self {
        self.block_driver = Some(block_driver);
        self
    }

    /// Construct from a pre-built scheduler client. Used only in unit tests
    /// that need to pass a `HogdDevSchedulerBackend` without going through
    /// the full `connect` path.
    #[cfg(test)]
    pub(crate) fn _new_for_test(scheduler: SchedulerCoord_SchedulerCoord1_Client) -> Self {
        Self {
            scheduler,
            block_driver: None,
        }
    }
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
            self.scheduler
                .schedulercoord_submit_unvetted(&batch)
                .await
                .map_err(|error| classify_submit_rpc_error(&error))?;
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
                .scheduler
                .schedcoord_get_tip()
                .await
                .map_err(|error| RpcServerError::BackendUnavailable(error.to_string()))?;
            match result {
                SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => {
                    Ok(chain_tip_from_db_tip(db_tip))
                }
                SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => {
                    tracing::error!(
                        "hogd-dev scheduler reported no genesis after startup bootstrap"
                    );
                    Err(RpcServerError::BackendUnavailable(
                        "hogd-dev scheduler has no genesis block".into(),
                    ))
                }
                other => Err(RpcServerError::BackendUnavailable(format!(
                    "unexpected scheduler tip result: {other:?}"
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
        _context: RpcRequestContext,
    ) -> Result<u64, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "pendingTransactionCount".into(),
        ))
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
    // protocol-rs currently exposes `submit_unvetted` as `void`, and the
    // generated dispatcher maps server-side `ProgramError` into a generic
    // x23 `SystemError`. Until the XDR grows a typed submit result, we cannot
    // distinguish semantic rejection from backend failure here.
    // TODO(protocol-rs-submit-result): map typed scheduler rejection to
    // `TxRejected` once submit returns an accepted/rejected result.
    RpcServerError::BackendUnavailable(format!("hogd-dev scheduler submit failed: {error}"))
}

fn chain_tip_from_db_tip(
    DbTip {
        height,
        block_hash,
        events_hash: _,
        db_hash: _,
        id: _,
    }: DbTip,
) -> ChainTip {
    ChainTip {
        height,
        block_hash: Hex0x(block_hash.into_inner()),
    }
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
}
