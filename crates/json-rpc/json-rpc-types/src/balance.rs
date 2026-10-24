use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{BlockHeight, ContractAddress};

/// Native token balance read from committed chain state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    /// Address whose native token balance was read.
    pub address: ContractAddress,
    /// Raw native token balance as a decimal string.
    pub raw_balance: String,
    /// Committed block height used for the read.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub height: BlockHeight,
}
