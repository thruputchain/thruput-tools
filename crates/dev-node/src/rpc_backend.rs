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
        let actual_hash = self.worker.send_raw_transaction(tx.bytes).await?;

        // The worker independently re-runs the same canonical hash recipe over
        // the same bytes. A divergence here is structurally impossible and
        // signals a code bug, not a runtime condition.
        assert_eq!(
            actual_hash, expected_hash,
            "bug: dev node hash diverged from shared validation"
        );

        Ok(())
    }
}

#[async_trait]
impl ChainPlane for DevNodeBackend {
    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
        self.worker.chain_tip().await.map_err(Into::into)
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
            .map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        self.worker
            .get_transaction_status(tx_hash)
            .await
            .map_err(Into::into)
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
            .map_err(Into::into)
    }
}

impl From<DevNodeError> for RpcServerError {
    fn from(err: DevNodeError) -> Self {
        match err {
            DevNodeError::TxInvalid(msg) | DevNodeError::AuthFailed(msg) => Self::TxInvalid(msg),
            DevNodeError::ExecFailed(msg) => Self::TxRejected(msg),
            DevNodeError::TxNotFound => Self::TxNotFound,
            DevNodeError::WorkerInit(msg) | DevNodeError::SandboxInit(msg) => {
                Self::BackendUnavailable(msg)
            }
            DevNodeError::WorkerStopped => {
                Self::BackendUnavailable("dev node worker stopped".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_invalid_maps_to_tx_invalid() {
        let actual: RpcServerError = DevNodeError::TxInvalid("bad xdr".into()).into();

        assert!(matches!(actual, RpcServerError::TxInvalid(msg) if msg == "bad xdr"));
    }

    #[test]
    fn auth_failed_maps_to_tx_invalid() {
        let actual: RpcServerError = DevNodeError::AuthFailed("bad sig".into()).into();

        assert!(matches!(actual, RpcServerError::TxInvalid(msg) if msg == "bad sig"));
    }

    #[test]
    fn exec_failed_maps_to_tx_rejected() {
        let actual: RpcServerError = DevNodeError::ExecFailed("revert".into()).into();

        assert!(matches!(actual, RpcServerError::TxRejected(msg) if msg == "revert"));
    }

    #[test]
    fn tx_not_found_maps_to_tx_not_found() {
        let actual: RpcServerError = DevNodeError::TxNotFound.into();

        assert!(matches!(actual, RpcServerError::TxNotFound));
    }

    #[test]
    fn worker_init_maps_to_backend_unavailable() {
        let actual: RpcServerError = DevNodeError::WorkerInit("boom".into()).into();

        assert!(matches!(actual, RpcServerError::BackendUnavailable(msg) if msg == "boom"));
    }

    #[test]
    fn sandbox_init_maps_to_backend_unavailable() {
        let actual: RpcServerError = DevNodeError::SandboxInit("denied".into()).into();

        assert!(matches!(actual, RpcServerError::BackendUnavailable(msg) if msg == "denied"));
    }

    #[test]
    fn worker_stopped_maps_to_backend_unavailable() {
        let actual: RpcServerError = DevNodeError::WorkerStopped.into();

        assert!(
            matches!(actual, RpcServerError::BackendUnavailable(msg) if msg == "dev node worker stopped")
        );
    }
}
