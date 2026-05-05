//! Dev-only JSON-RPC methods served by the node supervisor.
//!
//! PR 4e only exposes the account list needed by CLI deploy/call. Other
//! `groundhog_dev_*` methods remain method-not-found until their backend data
//! exists in hogd-dev.

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
    async fn reset(&self) -> RpcResult<()> {
        Err(method_not_found("groundhog_dev_reset"))
    }

    async fn get_accounts(&self) -> RpcResult<Vec<DevAccount>> {
        Ok((*self.accounts).clone())
    }

    async fn get_transaction_detail(
        &self,
        _tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionDetail>> {
        Err(method_not_found("groundhog_dev_getTransactionDetail"))
    }

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
