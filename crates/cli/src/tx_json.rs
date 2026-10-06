use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::{TransactionRef, TransactionStatus, TxHash};
use groundhog_json_rpc_types::{BlockHeight, ContractAddress, events::PaginationCursor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const TX_SHOW_JSON_SCHEMA_VERSION: u32 = 3;
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
    #[serde(with = "groundhog_json_rpc_types::decimal")]
    #[schemars(with = "groundhog_json_rpc_types::decimal::Decimal<BlockHeight>")]
    pub latest_block: BlockHeight,
    #[serde(with = "groundhog_json_rpc_types::decimal")]
    #[schemars(with = "groundhog_json_rpc_types::decimal::Decimal<BlockHeight>")]
    pub oldest_block: BlockHeight,
}
