//! Shared JSON-RPC trait definitions for the Groundhog RPC surface.
//!
//! Both the production gateway and local node implement
//! `GroundhogApiServer`. The client crate uses `GroundhogApiClient`.

pub use groundhog_json_rpc_types as types;

use groundhog_open_rpc_macros::open_rpc;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;

use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress,
    airdrop::{AirdropAttemptResponse, AirdropRequest},
    balance::BalanceResponse,
    block::{BlockInfo, ChainTip},
    events::{EventPage, EventQuery},
    hex::HexBytes,
    network_config::NetworkConfigParams,
    network_info::NetworkInfo,
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash},
    version::VersionInfo,
    view::{ViewRequest, ViewResponse},
};

/// Wire-format `SemVer` for the JSON-RPC surface.
///
/// See the "`OpenRPC` Wire Version" section in `CLAUDE.md` for bump
/// rules.
pub const WIRE_FORMAT_VERSION: &str = "5.3.0";

/// Optional faucet-backed API registered only when a server supports airdrops.
#[open_rpc(namespace = "groundhog", tag = "Airdrop API")]
#[rpc(client, server, namespace = "groundhog")]
pub trait AirdropApi {
    /// Request an idempotent native HOG airdrop.
    ///
    /// Servers register this method only when an airdrop backend is configured;
    /// other deployments return standard JSON-RPC `Method not found`.
    #[method(name = "requestAirdrop")]
    async fn request_airdrop(&self, request: AirdropRequest) -> RpcResult<AirdropAttemptResponse>;
}

/// Phase 1: Core API — submit, query, poll.
#[open_rpc(namespace = "groundhog", tag = "Core API")]
#[rpc(client, server, namespace = "groundhog")]
pub trait GroundhogApi {
    /// Submit a signed transaction (0x-prefixed hex XDR). Returns the canonical tx hash.
    /// Transactions whose `blockExpiryHeight` is at or below the current chain tip can
    /// never commit and are rejected with `TxRejected` when the tip is available.
    /// Transactions that break an executor pre-execution bound (too many attached
    /// contracts for the chain config, an oversized contract, an RV32 artifact declaring
    /// more guest memory or code than the chain admits, or a duplicate attachment) are
    /// rejected with `TxInvalid` carrying a `TxInvalidDetail` in the error `data`.
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(&self, tx_hex: String) -> RpcResult<TxHash>;

    /// Full receipt for a committed transaction.
    #[method(name = "getTransactionReceipt")]
    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>>;

    /// Lightweight status check (mempool -> indexer -> unknown). `unknown` is only
    /// returned after both the mempool and the indexer answered without a match; if
    /// the mempool could not be consulted and the indexer has no record, the call
    /// fails with `BackendUnavailable` because the transaction may still be pending.
    #[method(name = "getTransactionStatus")]
    async fn get_transaction_status(&self, tx_hash: TxHash) -> RpcResult<TransactionStatus>;

    /// Current chain tip: height, block hash, and the protocol it executed under.
    #[method(name = "chainTip")]
    async fn chain_tip(&self) -> RpcResult<ChainTip>;

    /// The execution parameters the chain's network config states. Only stated
    /// parameters are reported; an unstated one is absent. Served from a tip reading
    /// refreshed at most once per cache window. Fails with `BackendUnavailable` when no
    /// reading is available or the config version is unreadable.
    #[method(name = "chainConfig")]
    async fn chain_config(&self) -> RpcResult<NetworkConfigParams>;

    /// Immutable protocol and genesis identity of the network this endpoint serves.
    #[method(name = "networkInfo")]
    async fn network_info(&self) -> RpcResult<NetworkInfo>;

    /// Block data at a given height.
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(&self, height: BlockHeight) -> RpcResult<Option<BlockInfo>>;

    /// Number of transactions in the mempool.
    #[method(name = "pendingTransactionCount")]
    async fn pending_transaction_count(&self) -> RpcResult<u64>;

    /// Paginated, filterable contract events. See `EventQuery` /
    /// `EventPage` for the Soroban-shaped filter / cursor / topic
    /// surface, with RPC-native `0x`-prefixed address and topic
    /// projections at the JSON boundary.
    #[method(name = "getEvents")]
    async fn get_events(&self, query: EventQuery) -> RpcResult<EventPage>;

    /// Paginated transaction history for a single address: refs to
    /// transactions whose root invocation targets the address, walked
    /// in the requested order. Hydrate full bodies via
    /// `getTransactionReceipt`.
    #[method(name = "getTransactionsByAddress")]
    async fn get_transactions_by_address(
        &self,
        query: AddressTransactionQuery,
    ) -> RpcResult<TransactionRefPage>;

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

    /// Native HOG balance for any address at the latest committed snapshot.
    #[method(name = "getBalance")]
    async fn get_balance(&self, address: ContractAddress) -> RpcResult<BalanceResponse>;

    /// Execute a sandboxed contract simulation against committed state.
    #[method(name = "view")]
    async fn view(&self, request: ViewRequest) -> RpcResult<ViewResponse>;

    /// Version dimensions for this JSON-RPC server.
    #[method(name = "version")]
    async fn version(&self) -> RpcResult<VersionInfo>;
}
