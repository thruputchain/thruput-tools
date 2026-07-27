use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const TX_SHOW_JSON_SCHEMA_VERSION: u32 = 1;

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
