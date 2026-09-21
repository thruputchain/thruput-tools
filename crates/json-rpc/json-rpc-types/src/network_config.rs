use groundhog_xdr::network_config::{ConfigParam, NetworkConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The execution parameters a chain's network config states, flattened
/// from the list that carries them. Returned by `chainConfig`.
///
/// A field is `None`, and absent from the JSON, when the config does not
/// state that parameter; the node alone decides what is then in force.
/// `Default` is the reading with nothing stated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkConfigParams {
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
    /// Bytes one attached contract's code blob may hold. The protocol fixes
    /// `kDefaultMaxContractSizeBytes` as the value in force when unstated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_contract_size_bytes: Option<u32>,
    /// Storage savepoints one transaction may have open at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_savepoint_depth: Option<u32>,
    /// Levels of node nesting an auth token may carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_auth_token_depth: Option<u32>,
    /// Code bytes an RV32 artifact may carry to be attached. The protocol
    /// fixes `kDefaultMaxRv32CodeBytes` as the value in force when unstated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rv32_code_bytes: Option<u32>,
    /// Guest data memory bytes an RV32 artifact may declare to be attached.
    /// The protocol fixes `kDefaultMaxRv32MemoryBytes` as the value in force
    /// when unstated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rv32_memory_bytes: Option<u32>,
}

impl NetworkConfigParams {
    /// Flatten the parameters a network config states.
    ///
    /// A config version this build cannot interpret is an error rather
    /// than a guess: the protocol requires a node that cannot name a
    /// version to fail, and this crate follows suit.
    pub fn from_network_config(config: &NetworkConfig) -> Result<Self, UnsupportedNetworkConfig> {
        let params = match config {
            NetworkConfig::_0 { v0 } => &v0.params,
            other => {
                return Err(UnsupportedNetworkConfig {
                    config: format!("{other:?}"),
                });
            }
        };
        let mut resolved = Self::default();
        for param in params.iter() {
            match param {
                ConfigParam::kConfigParamMaxSHA256Buffers { max_sha256_buffers } => {
                    resolved.max_sha256_buffers = Some(*max_sha256_buffers);
                }
                ConfigParam::kConfigParamMaxCrossInvokeDepth {
                    max_cross_invoke_depth,
                } => {
                    resolved.max_cross_invoke_depth = Some(*max_cross_invoke_depth);
                }
                ConfigParam::kConfigParamMaxNewContractsPerTx {
                    max_new_contracts_per_tx,
                } => {
                    resolved.max_new_contracts_per_tx = Some(*max_new_contracts_per_tx);
                }
                ConfigParam::kConfigParamMaxBytestringLen { max_bytestring_len } => {
                    resolved.max_bytestring_len = Some(*max_bytestring_len);
                }
                ConfigParam::kConfigParamMaxContractSizeBytes {
                    max_contract_size_bytes,
                } => {
                    resolved.max_contract_size_bytes = Some(*max_contract_size_bytes);
                }
                ConfigParam::kConfigParamMaxSavepointDepth {
                    max_savepoint_depth,
                } => {
                    resolved.max_savepoint_depth = Some(*max_savepoint_depth);
                }
                ConfigParam::kConfigParamMaxAuthTokenDepth {
                    max_auth_token_depth,
                } => {
                    resolved.max_auth_token_depth = Some(*max_auth_token_depth);
                }
                ConfigParam::kConfigParamMaxRv32CodeBytes {
                    max_rv32_code_bytes,
                } => {
                    resolved.max_rv32_code_bytes = Some(*max_rv32_code_bytes);
                }
                ConfigParam::kConfigParamMaxRv32MemoryBytes {
                    max_rv32_memory_bytes,
                } => {
                    resolved.max_rv32_memory_bytes = Some(*max_rv32_memory_bytes);
                }
                // `kConfigParamMaxSHA256BufferBytes` is retired; otherwise the
                // catch-all exists only because the enum is `#[non_exhaustive]`.
                _ => {}
            }
        }
        Ok(resolved)
    }
}

/// A network config whose version this build cannot interpret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported network config version: {config}")]
pub struct UnsupportedNetworkConfig {
    /// Debug rendering of the unrecognized config.
    pub config: String,
}

#[cfg(test)]
mod tests {
    use groundhog_xdr::network_config::{ConfigParam, NetworkConfig, NetworkConfigV0};
    use serde_json::json;
    use xdr_common::VecM;

    use super::NetworkConfigParams;

    fn network_config(params: Vec<ConfigParam>) -> NetworkConfig {
        NetworkConfig::_0 {
            v0: NetworkConfigV0 {
                params: VecM(params),
            },
        }
    }

    #[test]
    fn stated_params_serialize_camel_case_and_unstated_ones_are_omitted() {
        let params = NetworkConfigParams {
            max_contract_size_bytes: Some(1_048_576),
            max_new_contracts_per_tx: Some(4),
            ..NetworkConfigParams::default()
        };
        let expected = json!({
            "maxContractSizeBytes": 1_048_576,
            "maxNewContractsPerTx": 4,
        });

        let encoded = serde_json::to_value(params).expect("bug: params serialize");
        assert_eq!(encoded, expected);

        let decoded: NetworkConfigParams =
            serde_json::from_value(encoded).expect("bug: params deserialize");
        assert_eq!(decoded, params);
        assert_eq!(
            serde_json::to_value(NetworkConfigParams::default()).expect("bug: params serialize"),
            json!({}),
            "nothing is filled in for an empty config"
        );
    }

    #[test]
    fn retired_sha256_buffer_bytes_is_dropped_when_a_chain_still_states_it() {
        let config = network_config(vec![
            ConfigParam::kConfigParamMaxSHA256BufferBytes {
                max_sha256_buffer_bytes: 8192,
            },
            ConfigParam::kConfigParamMaxSavepointDepth {
                max_savepoint_depth: 9,
            },
        ]);

        let params = NetworkConfigParams::from_network_config(&config).unwrap();

        assert_eq!(
            params,
            NetworkConfigParams {
                max_savepoint_depth: Some(9),
                ..NetworkConfigParams::default()
            }
        );
        assert_eq!(
            serde_json::to_value(params).unwrap(),
            json!({ "maxSavepointDepth": 9 })
        );
    }
}
