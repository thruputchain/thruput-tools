use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::BlockHeight;

/// The execution parameters a chain's network config states, as read with
/// the chain tip. Returned by `chainConfig`.
///
/// Every parameter is reported exactly as the chain states it. A field is
/// absent when the config does not state that parameter; the node, not this
/// surface, decides what value is then in force, so nothing is filled in
/// here. A config version this build cannot interpret is an error rather
/// than an empty object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainConfig {
    /// Tip the config was read with. The parameters govern the block that
    /// follows it; they change only at explicit upgrade blocks.
    pub height: BlockHeight,
    /// Concurrently open incremental SHA-256 buffers permitted per
    /// invocation frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sha256_buffers: Option<u32>,
    /// Invocation frames one transaction may have open at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cross_invoke_depth: Option<u32>,
    /// Contracts one transaction may attach in `contractsToDeploy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_new_contracts_per_tx: Option<u32>,
    /// Bytes one bytestring storage item may hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytestring_len: Option<u32>,
    /// Bytes one attached contract's code blob may hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_contract_size_bytes: Option<u32>,
    /// Storage savepoints one transaction may have open at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_savepoint_depth: Option<u32>,
    /// Bytes one incremental SHA-256 buffer may absorb between resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sha256_buffer_bytes: Option<u32>,
    /// Levels of node nesting an auth token may carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_auth_token_depth: Option<u32>,
    /// Code bytes an RV32 artifact may carry to be attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rv32_code_bytes: Option<u32>,
    /// Guest data memory bytes an RV32 artifact may declare to be attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rv32_memory_bytes: Option<u32>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::BlockHeight;

    use super::ChainConfig;

    #[test]
    fn chain_config_serializes_stated_params_and_omits_unstated_ones() {
        let config = ChainConfig {
            height: BlockHeight::from(42),
            max_contract_size_bytes: Some(1_048_576),
            max_new_contracts_per_tx: Some(4),
            max_sha256_buffers: None,
            max_cross_invoke_depth: None,
            max_bytestring_len: None,
            max_savepoint_depth: None,
            max_sha256_buffer_bytes: None,
            max_auth_token_depth: None,
            max_rv32_code_bytes: None,
            max_rv32_memory_bytes: None,
        };
        let expected = json!({
            "height": 42,
            "maxContractSizeBytes": 1_048_576,
            "maxNewContractsPerTx": 4,
        });

        let encoded = serde_json::to_value(&config).expect("bug: ChainConfig serializes");
        assert_eq!(encoded, expected);

        let decoded: ChainConfig =
            serde_json::from_value(encoded).expect("bug: ChainConfig deserializes");
        assert_eq!(decoded, config);
    }
}
