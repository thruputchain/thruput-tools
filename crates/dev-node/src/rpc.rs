//! Dev-only JSON-RPC trait implementation.
//!
//! The public `GroundhogApiServer` implementation lives in
//! `groundhog-rpc-server`; this module only exposes the `groundhog_dev_*`
//! namespace over the native-exec worker.

use groundhog_rpc_api::GroundhogDevApiServer;
use groundhog_rpc_api_types::ContractAddress;
use groundhog_rpc_api_types::dev::{DevAccount, TransactionDetail};
use groundhog_rpc_api_types::hex::HexBytes;
use groundhog_rpc_api_types::state::StorageItemRead;
use groundhog_rpc_api_types::transaction::TxHash;
use jsonrpsee::core::RpcResult;
use jsonrpsee::types::ErrorObjectOwned;

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

    async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: HexBytes,
    ) -> RpcResult<StorageItemRead> {
        let key: [u8; 32] = key.0.as_slice().try_into().map_err(|_| {
            ErrorObjectOwned::owned(
                groundhog_rpc_api_types::error::codes::INVALID_PARAMS,
                "storage key must be exactly 32 bytes",
                None::<()>,
            )
        })?;

        Ok(self.worker.get_storage_at(address, key).await?)
    }
}
