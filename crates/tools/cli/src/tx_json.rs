use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::{TransactionRef, TransactionStatus, TxHash};
use groundhog_json_rpc_types::{BlockHeight, ContractAddress, events::PaginationCursor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const TX_SHOW_JSON_SCHEMA_VERSION: u32 = 1;
pub const TX_LIST_JSON_SCHEMA_VERSION: u32 = 1;

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
    pub latest_block: BlockHeight,
    pub oldest_block: BlockHeight,
}
