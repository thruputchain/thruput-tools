//! Shared JSON-RPC trait definitions for the Groundhog RPC surface.
//!
//! Both the production gateway and local node implement
//! `GroundhogApiServer`. The client crate uses `GroundhogApiClient`.

pub use groundhog_rpc_api_types as types;

use groundhog_open_rpc_macros::open_rpc;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;

use groundhog_rpc_api_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    dev::{DevAccount, TransactionDetail},
    events::{EventPage, EventQuery},
    hex::HexBytes,
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{TransactionStatus, TxHash},
};

/// Phase 1: Core API — submit, query, poll.
#[open_rpc(namespace = "groundhog", tag = "Core API")]
#[rpc(client, server, namespace = "groundhog")]
pub trait GroundhogApi {
    /// Submit a signed transaction (0x-prefixed hex XDR). Returns the canonical tx hash.
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(&self, tx_hex: String) -> RpcResult<TxHash>;

    /// Full receipt for a committed transaction.
    #[method(name = "getTransactionReceipt")]
    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>>;

    /// Lightweight status check (mempool -> indexer -> unknown).
    #[method(name = "getTransactionStatus")]
    async fn get_transaction_status(&self, tx_hash: TxHash) -> RpcResult<TransactionStatus>;

    /// Current chain tip: height + block hash.
    #[method(name = "chainTip")]
    async fn chain_tip(&self) -> RpcResult<ChainTip>;

    /// Block data at a given height.
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(&self, height: BlockHeight) -> RpcResult<Option<BlockInfo>>;

    /// Number of transactions in the mempool.
    #[method(name = "pendingTransactionCount")]
    async fn pending_transaction_count(&self) -> RpcResult<u64>;

    /// Paginated, filterable contract events.  See `EventQuery` /
    /// `EventPage` (re-exported from `groundhog_indexer`) for the
    /// Soroban-shaped filter / cursor / topic surface.
    #[method(name = "getEvents")]
    async fn get_events(&self, query: EventQuery) -> RpcResult<EventPage>;

    /// Read the latest committed storage item at an address/key pair.
    /// `block_height` in the returned `StorageItemRead` always
    /// reflects the snapshot the read was performed against — even
    /// when no item is present at the key — so wallets can compare
    /// against `chainTip` to gauge freshness.
    #[method(name = "getStorageAt")]
    async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: HexBytes,
    ) -> RpcResult<StorageItemRead>;
}

/// Dev-only RPC methods. Only implemented by the local node.
#[open_rpc(namespace = "groundhog_dev", tag = "Dev API")]
#[rpc(client, server, namespace = "groundhog_dev")]
pub trait GroundhogDevApi {
    /// Reset the local node to its initial state.
    #[method(name = "reset")]
    async fn reset(&self) -> RpcResult<()>;

    /// List pre-funded dev accounts with addresses and keys.
    #[method(name = "getAccounts")]
    async fn get_accounts(&self) -> RpcResult<Vec<DevAccount>>;

    /// Full retained execution detail for a committed transaction.
    #[method(name = "getTransactionDetail")]
    async fn get_transaction_detail(&self, tx_hash: TxHash)
    -> RpcResult<Option<TransactionDetail>>;
}
