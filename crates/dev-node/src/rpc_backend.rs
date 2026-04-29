use async_trait::async_trait;
use groundhog_rpc_api_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    events::IdentifiedEvent,
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};
use groundhog_rpc_server::{
    ChainPlane, ReadPlane, RpcServerError, SubmitPlane, ValidatedTransaction,
};

use crate::{error::DevNodeError, worker::WorkerHandle};

/// Adapter from the native-exec worker to the shared RPC server backend traits.
#[derive(Clone)]
pub struct DevNodeBackend {
    worker: WorkerHandle,
}

impl DevNodeBackend {
    /// Create a backend adapter over the dev-node worker.
    pub fn new(worker: WorkerHandle) -> Self {
        Self { worker }
    }
}

#[async_trait]
impl SubmitPlane for DevNodeBackend {
    #[tracing::instrument(skip_all, fields(tx_hash = ?tx.hash, tx_bytes_len = tx.bytes.len()))]
    async fn submit_raw_transaction(&self, tx: ValidatedTransaction) -> Result<(), RpcServerError> {
        let expected_hash = tx.hash;
        let actual_hash = self
            .worker
            .send_raw_transaction(tx.bytes)
            .await
            .map_err(map_error)?;

        if actual_hash != expected_hash {
            tracing::error!(
                ?expected_hash,
                ?actual_hash,
                "dev node hash diverged from shared validation"
            );
            return Err(RpcServerError::TxRejected(
                "dev node hash diverged from shared validation".into(),
            ));
        }

        Ok(())
    }
}

#[async_trait]
impl ChainPlane for DevNodeBackend {
    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
        self.worker.chain_tip().await.map_err(map_error)
    }

    #[tracing::instrument(skip_all)]
    async fn get_block_by_number(
        &self,
        _height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getBlockByNumber is not yet implemented in the dev node".into(),
        ))
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(&self) -> Result<u64, RpcServerError> {
        Ok(0)
    }
}

#[async_trait]
impl ReadPlane for DevNodeBackend {
    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        self.worker
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(map_error)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        self.worker
            .get_transaction_status(tx_hash)
            .await
            .map_err(map_error)
    }

    #[tracing::instrument(skip_all, fields(?address, ?from_height, ?to_height))]
    async fn contract_events(
        &self,
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
    ) -> Result<Vec<IdentifiedEvent>, RpcServerError> {
        self.worker
            .get_contract_events(address, from_height, to_height)
            .await
            .map_err(map_error)
    }
}

fn map_error(err: DevNodeError) -> RpcServerError {
    match err {
        DevNodeError::TxInvalid(msg) | DevNodeError::AuthFailed(msg) => {
            RpcServerError::TxInvalid(msg)
        }
        DevNodeError::ExecFailed(msg) => RpcServerError::TxRejected(msg),
        DevNodeError::TxNotFound => RpcServerError::TxNotFound,
        DevNodeError::WorkerInit(msg) | DevNodeError::SandboxInit(msg) => {
            RpcServerError::BackendUnavailable(msg)
        }
        DevNodeError::WorkerStopped => {
            RpcServerError::BackendUnavailable("dev node worker stopped".into())
        }
    }
}
