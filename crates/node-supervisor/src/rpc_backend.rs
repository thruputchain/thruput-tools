//! hogd-dev x23 backend adapters for the public JSON-RPC facade.
//!
//! PR 4a intentionally starts with the scheduler-backed chain plane only.
//! Submit, read, and dev-control adapters stay stubbed until hogd-dev exposes
//! the corresponding stable x23 surfaces.

use async_trait::async_trait;
use groundhog_rpc_api_types::{
    BlockHeight,
    block::{BlockInfo, ChainTip},
    hex::Hex0x,
};
use groundhog_rpc_server::{ChainPlane, RpcServerError};
use groundhog_scheduler::{
    DbTip, SchedulerGetTipResult, sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use groundhog_x23_transport::connect;
use std::{fmt, net::SocketAddr};

use crate::{NodeSupervisorError, genesis::ensure_dev_genesis};

// chainTip only needs one lane today; two gives the near-term PR 4b/4c
// scheduler-sharing path room without making PR 4a a pool-tuning exercise.
const SCHEDULER_POOL_SIZE: usize = 2;

/// Chain-plane adapter backed by hogd-dev's scheduler endpoint.
#[derive(Clone)]
pub(crate) struct HogdDevChainBackend {
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
}

impl fmt::Debug for HogdDevChainBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HogdDevChainBackend")
            .finish_non_exhaustive()
    }
}

impl HogdDevChainBackend {
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
}

#[async_trait]
impl ChainPlane for HogdDevChainBackend {
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
