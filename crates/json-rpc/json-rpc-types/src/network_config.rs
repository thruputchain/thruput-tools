use std::sync::Arc;

use groundhog_xdr::network_config::{ConfigParam, NetworkConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Chain configuration with an optional authoritative execution reading.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChainConfig {
    /// Explicitly stated fields retain their original omission semantics.
    #[serde(flatten)]
    pub stated: NetworkConfigParams,
    /// Absent when the backend cannot supply authoritative execution rules.
    // Shared, not owned: a server answering from a reading it already holds
    // must not copy the storage schedule on every request. Kept off the doc
    // comment so this note stays out of the published schema description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<Arc<ExecutionConfig>>,
}

impl From<NetworkConfigParams> for ChainConfig {
    fn from(stated: NetworkConfigParams) -> Self {
        Self {
            stated,
            execution: None,
        }
    }
}

/// Rules read with the committed tip named in `observedTip`, which may be a
/// recent reading this endpoint already held rather than one taken during this
/// request. Never a lapsed reading: without one recent enough the request
/// reads the backend, and a failure surfaces rather than an old price.
/// Apply to execution following `observedTip`; later inclusion can use newer rules.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ExecutionConfig {
    /// Committed snapshot read together with these rules.
    pub observed_tip: crate::block::ChainTip,
    /// Consensus commitment to the stated configuration and storage model.
    pub config_hash: crate::SHA256Hash,
    /// Runtime-resolved limits; omissions are unavailable, never zero.
    pub effective_limits: NetworkConfigParams,
    /// Active storage collateral schedule in native-HOG base units.
    pub storage_costs: StorageCosts,
}

/// Integer native-HOG base units (10,000,000 per HOG), encoded as decimal strings.
/// Storage collateral is separate from gas capacity. A first credit pays the
/// recipient's balance deposit; code storage is charged to the deployed address.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct StorageCosts {
    /// Base charge for an ordinary data item; balance and deployment have separate bases.
    pub base_data_item_cost: String,
    /// Added to the ordinary base for each byte of a bytestring value (u32).
    pub bytestring_cost_per_byte: String,
    /// RV32 deployment base; does not include the new address's balance deposit.
    pub base_contract_deployment_cost: String,
    /// Added per byte of ContractBlob.data, including the full RV32 artifact.
    pub contract_deployment_cost_per_byte: String,
    /// Added to the ordinary base for each authorization key.
    pub auth_per_key_cost: String,
    /// Added to the ordinary base per live queue entry; an empty queue costs zero.
    pub rcq_cost_per_entry: String,
    /// Added to the ordinary base per replay-cache entry, charged to its owner.
    pub replay_cache_cost_per_entry: String,
    /// Entire balance-item deposit, deducted once from the first credit (no ordinary base).
    pub balance_cost: String,
}

impl From<&groundhog_xdr::storage_cost::StorageCostModelXdr> for StorageCosts {
    fn from(value: &groundhog_xdr::storage_cost::StorageCostModelXdr) -> Self {
        Self {
            base_data_item_cost: value.base_data_item_cost.to_string(),
            bytestring_cost_per_byte: value.bytestring_cost_per_byte.to_string(),
            base_contract_deployment_cost: value.base_contract_deployment_cost.to_string(),
            contract_deployment_cost_per_byte: value.contract_deployment_cost_per_byte.to_string(),
            auth_per_key_cost: value.auth_per_key_cost.to_string(),
            rcq_cost_per_entry: value.rcq_cost_per_entry.to_string(),
            replay_cache_cost_per_entry: value.replay_cache_cost_per_entry.to_string(),
            balance_cost: value.balance_cost.to_string(),
        }
    }
}

impl ExecutionConfig {
    /// Bind prices and resolved limits to their shared committed reading.
    pub fn new(
        observed_tip: crate::block::ChainTip,
        config_hash: crate::SHA256Hash,
        effective_limits: NetworkConfigParams,
        storage_costs: StorageCosts,
    ) -> Self {
        Self {
            observed_tip,
            config_hash,
            effective_limits,
            storage_costs,
        }
    }
}

impl ChainConfig {
    /// Attach the authoritative reading without changing stated-field meanings.
    pub fn with_execution(
        stated: NetworkConfigParams,
        execution: impl Into<Arc<ExecutionConfig>>,
    ) -> Self {
        Self {
            stated,
            execution: Some(execution.into()),
        }
    }
}

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
