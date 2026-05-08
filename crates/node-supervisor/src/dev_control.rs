//! Dev-only JSON-RPC methods served by the node supervisor.
//!
//! `groundhog_dev_getAccounts` is live. Transaction detail is reserved until
//! its backend data exists in hogd-dev.

use std::sync::Arc;

use groundhog_rpc_api::GroundhogDevApiServer;
use groundhog_rpc_api_types::{
    dev::{DevAccount, TransactionDetail},
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
}

fn method_not_found(method: &'static str) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(
        jsonrpsee::types::error::METHOD_NOT_FOUND_CODE,
        format!("{method} is not available from hogd-dev yet"),
        None::<()>,
    )
}
