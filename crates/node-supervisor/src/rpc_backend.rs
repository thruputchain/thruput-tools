//! hogd-dev x23 backend adapters for the public JSON-RPC facade.
//!
//! Chain and submit both use hogd-dev's advertised scheduler endpoint. Read
//! and dev-control adapters stay stubbed until hogd-dev exposes the
//! corresponding stable x23 surfaces.

use async_trait::async_trait;
use groundhog_rpc_api_types::{
    BlockHeight,
    block::{BlockInfo, ChainTip},
    hex::Hex0x,
};
use groundhog_rpc_server::{ChainPlane, RpcServerError, SubmitPlane, ValidatedTransaction};
use groundhog_scheduler::{
    DbTip, ParallelBatch, SchedulerGetTipResult, parallel::ParallelSignedTransactionBatch,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use groundhog_x23_transport::connect;
use std::{fmt, net::SocketAddr};
use xdr_common::{BytesM, VecM};

use crate::{NodeSupervisorError, genesis::ensure_dev_genesis};

// chainTip and submit share the scheduler endpoint. Keep a small pool so a
// long-running submit does not block tip checks during local-node startup and
// smoke tests.
const SCHEDULER_POOL_SIZE: usize = 2;

/// Scheduler-backed adapter for hogd-dev chain and submit planes.
#[derive(Clone)]
pub(crate) struct HogdDevSchedulerBackend {
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
}

impl fmt::Debug for HogdDevSchedulerBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HogdDevSchedulerBackend")
            .finish_non_exhaustive()
    }
}

impl HogdDevSchedulerBackend {
    pub(crate) async fn connect(endpoint: SocketAddr) -> Result<Self, NodeSupervisorError> {
        let scheduler = connect(
            "hogd-dev scheduler",
            &endpoint.to_string(),
            SCHEDULER_POOL_SIZE,
            SchedulerCoord_SchedulerCoord1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::SchedulerConnect { endpoint, source })?;
        Ok(Self { scheduler })
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn ensure_dev_genesis(&self) -> Result<(), NodeSupervisorError> {
        ensure_dev_genesis(&self.scheduler).await
    }

    /// Borrow the inner scheduler client for use by sibling backends
    /// (e.g. the read backend's pending-tx probe).
    // Used in read_backend.rs::HogdDevReadBackend::connect (T3) and server.rs (T4).
    #[allow(dead_code)]
    pub(crate) fn scheduler_client(&self) -> &SchedulerCoord_SchedulerCoord1_Client {
        &self.scheduler
    }

    /// Construct from a pre-built scheduler client. Used only in unit tests
    /// that need to pass a `HogdDevSchedulerBackend` without going through
    /// the full `connect` path.
    #[cfg(test)]
    pub(crate) fn _new_for_test(scheduler: SchedulerCoord_SchedulerCoord1_Client) -> Self {
        Self { scheduler }
    }
}

#[async_trait]
impl SubmitPlane for HogdDevSchedulerBackend {
    #[tracing::instrument(skip_all, fields(tx_hash = ?tx.hash, tx_bytes_len = tx.bytes.len()))]
    async fn submit_raw_transaction(&self, tx: ValidatedTransaction) -> Result<(), RpcServerError> {
        let batch = single_transaction_batch(&tx.bytes)?;
        self.scheduler
            .schedulercoord_submit_unvetted(&batch)
            .await
            .map_err(|error| classify_submit_rpc_error(&error))
    }
}

#[async_trait]
impl ChainPlane for HogdDevSchedulerBackend {
    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
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
                tracing::error!("hogd-dev scheduler reported no genesis after startup bootstrap");
                Err(RpcServerError::BackendUnavailable(
                    "hogd-dev scheduler has no genesis block".into(),
                ))
            }
            other => Err(RpcServerError::BackendUnavailable(format!(
                "unexpected scheduler tip result: {other:?}"
            ))),
        }
    }

    #[tracing::instrument(skip_all)]
    async fn get_block_by_number(
        &self,
        _height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBlockByNumber".into()))
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(&self) -> Result<u64, RpcServerError> {
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
