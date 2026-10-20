use super::{
    Deserialize, Deserializer, JsonSchema, ReceiptExecution, ResponseData, Serialize,
    UnknownVariant, Value,
    fields::{height_schema, hex_schema, present, read_height, read_hex},
};
use crate::{
    BlockHeight, ContractAddress,
    hex::HexBytes,
    requests::funding::{positive_amount_schema, read_positive_amount},
    transaction::TxHash,
};

/// Successful committed execution reported by the faucet. Known failed execution
/// cannot be interpreted as a succeeded funding attempt.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
#[serde(tag = "result", rename_all_fields = "camelCase")]
pub enum AirdropExecution {
    /// The matched receipt returned successfully, including empty return bytes.
    #[serde(rename = "return")]
    Return {
        #[schemars(schema_with = "hex_schema")]
        return_data: HexBytes,
    },
    /// Reader-only unfamiliar outcome, retained without asserting success.
    #[serde(untagged)]
    #[schemars(skip)]
    Unsupported(UnknownVariant),
}

impl<'de> Deserialize<'de> for AirdropExecution {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(d)?;
        let execution: ReceiptExecution =
            serde_json::from_value(raw.clone()).map_err(serde::de::Error::custom)?;
        match execution {
            ReceiptExecution::Return { return_data } => Ok(Self::Return { return_data }),
            ReceiptExecution::Unsupported(_) | ReceiptExecution::Other {} => {
                Ok(Self::Unsupported(UnknownVariant(raw)))
            }
            _ => Err(serde::de::Error::custom(
                "succeeded airdrop requires returned execution",
            )),
        }
    }
}

impl ResponseData for AirdropExecution {
    fn is_supported(&self) -> bool {
        matches!(self, Self::Return { .. })
    }
}

response_enum! {
/// Result of one faucet call. Pending does not establish admission; callers can
/// poll a supplied transaction hash without repeating the faucet operation.
AirdropAttempt, "attemptStatus" {
    Pending("pending", true) {
        recipient: ContractAddress,
        #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_positive_amount")]
        #[schemars(schema_with = "positive_amount_schema")]
        raw_amount: u128,
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
        #[schemars(with = "TxHash")]
        transaction_hash: Option<TxHash>,
        #[serde(with = "crate::decimal")]
        #[schemars(with = "crate::decimal::Decimal<u64>")]
        retry_after_seconds: u64,
    },
    Succeeded("succeeded", true) {
        recipient: ContractAddress,
        #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_positive_amount")]
        #[schemars(schema_with = "positive_amount_schema")]
        raw_amount: u128,
        transaction_hash: TxHash,
        #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
        #[schemars(schema_with = "height_schema")]
        block_height: BlockHeight,
        execution: AirdropExecution,
    },
}
}

response_object! {
/// Retained local funding submission or reconciliation evidence. Inspect its
/// receipt before asserting that the transfer succeeded.
FundingSubmission {
    recipient: ContractAddress,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_positive_amount")]
    #[schemars(schema_with = "positive_amount_schema")]
    raw_amount: u128,
    transaction_hash: TxHash,
    #[serde(deserialize_with = "read_hex")]
    #[schemars(schema_with = "hex_schema")]
    signed_transaction: HexBytes,
}
}

response_object! {
/// This local runtime's pending pool observation, not a network-wide count.
PendingTransactionCount {
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    count: u64,
}
}

#[cfg(test)]
mod tests;
