use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thruput_json_rpc_types::responses::{TransactionReceipt, TransactionStatus};
use thruput_json_rpc_types::transaction::{TransactionRef, TxHash};
use thruput_json_rpc_types::{BlockHeight, ContractAddress, events::PaginationCursor};

pub const TX_SHOW_JSON_SCHEMA_VERSION: u32 = 5;
pub const TX_LIST_JSON_SCHEMA_VERSION: u32 = 2;

/// Stable JSON data for a one-shot public transaction lookup.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TxShowReport {
    pub schema_version: u32,
    pub profile: String,
    pub rpc_url: String,
    pub tx_hash: TxHash,
    pub status: TransactionStatus,
    pub receipt: Option<TransactionReceipt>,
    /// Failed advisory lookup retained alongside an authoritative receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_error: Option<serde_json::Value>,
    #[serde(flatten)]
    pub abort: crate::abort_json::AbortReport,
}

/// Stable JSON data for one page of root-invocation transaction history.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TxListReport {
    pub schema_version: u32,
    pub profile: String,
    pub rpc_url: String,
    pub target_input: String,
    pub address: ContractAddress,
    pub refs: Vec<TransactionRef>,
    pub cursor: PaginationCursor,
    #[serde(with = "thruput_json_rpc_types::decimal")]
    #[schemars(with = "thruput_json_rpc_types::decimal::Decimal<BlockHeight>")]
    pub latest_block: BlockHeight,
    #[serde(with = "thruput_json_rpc_types::decimal")]
    #[schemars(with = "thruput_json_rpc_types::decimal::Decimal<BlockHeight>")]
    pub oldest_block: BlockHeight,
}
