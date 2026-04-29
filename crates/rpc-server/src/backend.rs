use async_trait::async_trait;
use groundhog_rpc_api_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    events::IdentifiedEvent,
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};

use crate::error::RpcServerError;

/// A transaction that has passed shared RPC-layer validation.
///
/// Obtain instances via `tx::validate_raw_transaction`.
/// No public constructor is provided so callers do not bypass validation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ValidatedTransaction {
    /// Raw signed transaction XDR bytes supplied by the client.
    pub bytes: Vec<u8>,
    /// Canonical transaction hash over the inner `Transaction`.
    pub hash: TxHash,
}

/// Backend plane for accepted raw transaction submission.
#[async_trait]
pub trait SubmitPlane: Send + Sync + 'static {
    /// Submit one validated signed transaction.
    async fn submit_raw_transaction(&self, tx: ValidatedTransaction) -> Result<(), RpcServerError>;
}

/// Backend plane for chain metadata and block-oriented reads.
#[async_trait]
pub trait ChainPlane: Send + Sync + 'static {
    /// Return the current chain tip.
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError>;

    /// Return block data at a specific height, if available.
    async fn get_block_by_number(
        &self,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError>;

    /// Return the number of currently pending transactions.
    async fn pending_transaction_count(&self) -> Result<u64, RpcServerError>;
}

/// Backend plane for transaction and event reads.
#[async_trait]
pub trait ReadPlane: Send + Sync + 'static {
    /// Return a committed transaction receipt, if available.
    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError>;

    /// Return semantic transaction status.
    ///
    /// Backends own any source fallback needed to answer this. For example,
    /// the production gateway checks scheduler pending state before indexer
    /// committed state.
    async fn transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError>;

    /// Return contract events filtered by address and inclusive height range.
    async fn contract_events(
        &self,
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
    ) -> Result<Vec<IdentifiedEvent>, RpcServerError>;
}
