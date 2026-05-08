//! Dev-only JSON-RPC methods served by the node supervisor.
//!
//! The trait surface is intentionally minimal pending the options paper at
//! `docs/cli/specs/2026-05-07-transaction-detail-options.md`.

use std::sync::Arc;

use groundhog_rpc_api::GroundhogDevApiServer;
use groundhog_rpc_api_types::dev::DevAccount;
use jsonrpsee::core::RpcResult;

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
}
