//! Groundhog local development node.
//!
//! Embeds the SDK's `native_exec` executor behind a standard
//! `GroundhogApi` JSON-RPC interface. Same execution semantics
//! as SDK unit tests, accessible over HTTP.

pub mod accounts;
pub mod backend;
pub mod error;
pub mod rpc;
pub mod wallet;
pub mod worker;

use groundhog_rpc_api::{GroundhogApiServer, GroundhogDevApiServer};
use jsonrpsee::server::{RpcModule, Server};

use crate::accounts::{DEFAULT_ACCOUNT_COUNT, DEFAULT_STORAGE_TOKENS};
use crate::rpc::DevNodeRpc;
use crate::worker::spawn_worker;

/// Configuration for the dev node server.
#[derive(Debug, Clone)]
pub struct DevNodeConfig {
    /// Listen address (default: "127.0.0.1:9545").
    pub listen_address: String,
    /// Number of pre-funded accounts (default: 10).
    pub account_count: usize,
    /// Storage tokens per account (default: 1_000_000_000).
    pub storage_tokens: u64,
}

impl Default for DevNodeConfig {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1:9545".to_string(),
            account_count: DEFAULT_ACCOUNT_COUNT,
            storage_tokens: DEFAULT_STORAGE_TOKENS,
        }
    }
}

/// Start the dev node JSON-RPC server.
///
/// Returns a handle containing the pre-funded accounts and server
/// control. The caller can await `handle.stopped()` to block until
/// shutdown, or call `handle.stop()` to initiate shutdown.
pub async fn start_server(
    config: DevNodeConfig,
) -> Result<DevNodeHandle, Box<dyn std::error::Error + Send + Sync>> {
    let worker = spawn_worker(config.account_count, config.storage_tokens);
    let accounts = worker
        .get_accounts()
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    let server = Server::builder().build(&config.listen_address).await?;

    // DevNodeRpc implements both GroundhogApiServer and GroundhogDevApiServer.
    // jsonrpsee's into_rpc() only picks up one trait. Use RpcModule::merge
    // to combine both namespaces into a single module.
    let rpc = DevNodeRpc::new(worker);
    let mut module = RpcModule::new(());
    module.merge(GroundhogApiServer::into_rpc(rpc.clone()))?;
    module.merge(GroundhogDevApiServer::into_rpc(rpc))?;

    let addr = server.local_addr()?;
    let handle = server.start(module);

    Ok(DevNodeHandle {
        accounts,
        addr,
        handle,
    })
}

/// Running dev node server.
pub struct DevNodeHandle {
    /// Pre-funded dev accounts.
    pub accounts: Vec<groundhog_rpc_api_types::dev::DevAccount>,
    /// Actual bound address (useful when binding to port 0).
    pub addr: std::net::SocketAddr,
    handle: jsonrpsee::server::ServerHandle,
}

impl DevNodeHandle {
    /// Wait until the server stops.
    pub async fn stopped(&self) {
        self.handle.clone().stopped().await;
    }

    /// Signal the server to stop.
    pub fn stop(&self) -> Result<(), jsonrpsee::server::AlreadyStoppedError> {
        self.handle.stop()
    }
}
