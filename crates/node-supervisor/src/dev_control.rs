//! Dev-only JSON-RPC methods served by the node supervisor.
//!
//! `groundhog_dev_getAccounts` is live; other `groundhog_dev_*` methods return
//! method-not-found until their backend data exists in hogd-dev.

use std::sync::Arc;

use groundhog_rpc_api::GroundhogDevApiServer;
use groundhog_rpc_api_types::{
    ContractAddress,
    dev::{DevAccount, StorageItemInfo, TransactionDetail},
    hex::HexBytes,
    transaction::TxHash,
};
use jsonrpsee::{core::RpcResult, types::ErrorObjectOwned};

/// Supervisor-owned dev RPC state.
#[derive(Debug, Clone)]
pub(crate) struct SupervisorDevRpc {
    accounts: Arc<Vec<DevAccount>>,
}

impl SupervisorDevRpc {
    /// Create a dev RPC handler with the advertised local-dev accounts.
    pub(crate) fn new(accounts: Vec<DevAccount>) -> Self {
        Self {
            accounts: Arc::new(accounts),
        }
    }
}

#[async_trait::async_trait]
impl GroundhogDevApiServer for SupervisorDevRpc {
    #[tracing::instrument(skip_all, fields(method = "groundhog_dev_reset"))]
    async fn reset(&self) -> RpcResult<()> {
        Err(method_not_found("groundhog_dev_reset"))
    }

    #[tracing::instrument(
        skip_all,
        fields(method = "groundhog_dev_getAccounts", account_count = self.accounts.len())
    )]
    async fn get_accounts(&self) -> RpcResult<Vec<DevAccount>> {
        Ok((*self.accounts).clone())
    }

    #[tracing::instrument(skip_all, fields(method = "groundhog_dev_getTransactionDetail"))]
    async fn get_transaction_detail(
        &self,
        _tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionDetail>> {
        Err(method_not_found("groundhog_dev_getTransactionDetail"))
    }

    #[tracing::instrument(skip_all, fields(method = "groundhog_dev_getStorageAt"))]
    async fn get_storage_at(
        &self,
        _address: ContractAddress,
        _key: HexBytes,
    ) -> RpcResult<Option<StorageItemInfo>> {
        Err(method_not_found("groundhog_dev_getStorageAt"))
    }
}

fn method_not_found(method: &'static str) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(
        jsonrpsee::types::error::METHOD_NOT_FOUND_CODE,
        format!("{method} is not available from hogd-dev yet"),
        None::<()>,
    )
}
