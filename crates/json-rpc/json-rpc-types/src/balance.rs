use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{BlockHeight, ContractAddress};

/// Native HOG balance read from committed chain state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    /// Address whose native HOG balance was read.
    pub address: ContractAddress,
    /// Raw native HOG balance as a decimal string.
    pub raw_balance: String,
    /// Committed block height used for the read.
    pub height: BlockHeight,
}
