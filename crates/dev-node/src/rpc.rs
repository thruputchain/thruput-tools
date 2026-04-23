//! JSON-RPC trait implementations.
//!
//! Thin async layer: receives RPC calls, forwards to `WorkerHandle`,
//! maps errors to JSON-RPC `ErrorObject`.

use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_api::GroundhogDevApiServer;
use groundhog_rpc_api_types::block::{BlockInfo, ChainTip};
use groundhog_rpc_api_types::dev::{DevAccount, TransactionDetail};
use groundhog_rpc_api_types::events::IdentifiedEvent;
use groundhog_rpc_api_types::receipt::TransactionReceipt;
use groundhog_rpc_api_types::transaction::{TransactionStatus, TxHash};
use groundhog_rpc_api_types::{BlockHeight, ContractAddress};
use jsonrpsee::core::RpcResult;
use jsonrpsee::types::ErrorObjectOwned;

use crate::error::DevNodeError;
use crate::worker::WorkerHandle;

/// RPC implementation backed by the dev node worker thread.
#[derive(Clone)]
pub struct DevNodeRpc {
    worker: WorkerHandle,
}

impl DevNodeRpc {
    /// Create a new RPC handler backed by the given worker.
    pub fn new(worker: WorkerHandle) -> Self {
        Self { worker }
    }
}

#[async_trait::async_trait]
impl GroundhogApiServer for DevNodeRpc {
    async fn send_raw_transaction(&self, tx_hex: String) -> RpcResult<TxHash> {
        let hex_str = tx_hex
            .strip_prefix("0x")
            .or_else(|| tx_hex.strip_prefix("0X"))
            .unwrap_or(&tx_hex);
        let tx_bytes =
            hex::decode(hex_str).map_err(|e| DevNodeError::TxInvalid(format!("bad hex: {e}")))?;
        Ok(self.worker.send_raw_transaction(tx_bytes).await?)
    }

    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>> {
        Ok(self.worker.get_transaction_receipt(tx_hash).await?)
    }

    async fn get_transaction_status(&self, tx_hash: TxHash) -> RpcResult<TransactionStatus> {
        Ok(self.worker.get_transaction_status(tx_hash).await?)
    }

    async fn chain_tip(&self) -> RpcResult<ChainTip> {
        Ok(self.worker.chain_tip().await?)
    }

    async fn get_block_by_number(&self, _height: BlockHeight) -> RpcResult<Option<BlockInfo>> {
        Err(jsonrpsee::types::ErrorObjectOwned::owned(
            groundhog_rpc_api_types::error::codes::NOT_IMPLEMENTED,
            "getBlockByNumber is not yet implemented in the dev node",
            None::<()>,
        ))
    }

    async fn pending_transaction_count(&self) -> RpcResult<u64> {
        // Dev node has no mempool — always 0 is correct, not a stub.
        Ok(0)
    }

    async fn get_contract_events(
        &self,
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
    ) -> RpcResult<Vec<IdentifiedEvent>> {
        if to_height < from_height {
            return Err(ErrorObjectOwned::owned(
                groundhog_rpc_api_types::error::codes::INVALID_PARAMS,
                "to_height must be >= from_height",
                None::<()>,
            ));
        }

        Ok(self
            .worker
            .get_contract_events(address, from_height, to_height)
            .await?)
    }
}

#[async_trait::async_trait]
impl GroundhogDevApiServer for DevNodeRpc {
    async fn reset(&self) -> RpcResult<()> {
        Ok(self.worker.reset().await?)
    }

    async fn get_accounts(&self) -> RpcResult<Vec<DevAccount>> {
        Ok(self.worker.get_accounts().await?)
    }

    async fn get_transaction_detail(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionDetail>> {
        Ok(self.worker.get_transaction_detail(tx_hash).await?)
    }
}
