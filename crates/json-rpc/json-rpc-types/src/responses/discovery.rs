use super::{
    Deserialize, Deserializer, JsonSchema, ResponseData, Serialize,
    fields::{
        decimal_u32_schema, height_schema, hex_list_schema, present, read_decimal_u32, read_height,
        read_hex_list, u32_schema, write_decimal_u32,
    },
};
use crate::{
    BlockHash, BlockHeight, NetworkHash, Protocol, SHA256Hash, StorageHash, TimePoint, ValidatorID,
    hex::HexBytes,
};

response_object! {
/// Native wire compatibility and configured capability metadata. Diagnostic
/// release fields are not codec selectors and are omitted when unavailable.
#[non_exhaustive]
VersionInfo {
    wire_format: String,
    supported_methods: Vec<String>,
    implementation: String,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(with = "String")]
    commit_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(with = "String")]
    platform_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(with = "String")]
    release_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(with = "String")]
    bundle_manifest_digest: Option<String>,
}
}

response_object! {
/// Signing identity; this is not a chain-reset instance identifier.
NetworkInfo { network_hash: NetworkHash }
}

response_object! {
/// Committed tip identity, also used for configuration observation provenance.
ChainTip {
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    block_height: BlockHeight,
    block_hash: SHA256Hash,
}
}

response_object! {
/// Complete committed block metadata, preserving the existing consensus fields.
/// Identity, coverage and request-height checks belong to the block adapter.
BlockInfo {
    block_hash: BlockHash,
    #[schemars(schema_with = "u32_schema")]
    protocol: Protocol,
    network_hash: NetworkHash,
    config_hash: SHA256Hash,
    proposer: ValidatorID,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<TimePoint>")]
    block_timepoint: TimePoint,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    block_height: BlockHeight,
    txs_hash: SHA256Hash,
    storage_hash: StorageHash,
    events_hash: SHA256Hash,
    prev_block_hash: BlockHash,
    proposal_txs_hash: SHA256Hash,
    #[serde(deserialize_with = "read_hex_list")]
    #[schemars(schema_with = "hex_list_schema")]
    upgrades: Vec<HexBytes>,
}
}

/// Null is a successful metadata lookup outside authoritative block coverage.
pub type BlockResult = Option<BlockInfo>;

response_object! {
/// Stated chain rules and optional authoritative execution provenance.
#[non_exhaustive]
ChainConfig {
    stated: ConfigLimits,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(with = "ExecutionConfig")]
    execution: Option<ExecutionConfig>,
}
}

response_object! {
/// Limits and storage prices read with one tip. Does not guarantee later rules.
ExecutionConfig {
    observed_tip: ChainTip,
    config_hash: SHA256Hash,
    effective_limits: ConfigLimits,
    storage_costs: StorageCosts,
}
}

response_object! {
/// Explicit configuration values. Omission means unavailable or unspecified.
#[derive(Default)]
ConfigLimits {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_sha256_buffers: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_cross_invoke_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_new_contracts_per_tx: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_bytestring_len: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_contract_size_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_savepoint_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_auth_token_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_rv32_code_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(schema_with = "u32_schema")]
    max_rv32_memory_bytes: Option<u32>,
}
}

response_object! {
/// Storage collateral in native-HOG base units. Each decimal amount retains
/// the width of its protocol field; these prices are unrelated to gas burning.
StorageCosts {
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    base_data_item_cost: u64,
    #[serde(serialize_with = "write_decimal_u32", deserialize_with = "read_decimal_u32")]
    #[schemars(schema_with = "decimal_u32_schema")]
    bytestring_cost_per_byte: u32,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    base_contract_deployment_cost: u64,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    contract_deployment_cost_per_byte: u64,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    auth_per_key_cost: u64,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    rcq_cost_per_entry: u64,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    replay_cache_cost_per_entry: u64,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    balance_cost: u64,
}
}

response_object! {
/// Configured per-endpoint request budgets, not a complete ingress-limit list.
#[non_exhaustive]
ServerLimits {
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    max_simulation_gas: u64,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    max_simulation_calldata_bytes: u64,
    #[schemars(schema_with = "u32_schema")]
    max_events_per_page: u32,
    #[schemars(schema_with = "u32_schema")]
    max_transactions_per_page: u32,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    max_cursor_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[schemars(with = "SimulationBatchLimits")]
    simulation_batch: Option<SimulationBatchLimits>,
}
}

response_object! {
/// Aggregate budgets for a shared-snapshot simulation batch.
SimulationBatchLimits {
    #[schemars(schema_with = "u32_schema")]
    max_invocations: u32,
    #[schemars(schema_with = "u32_schema")]
    max_calldata_bytes: u32,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    max_gas: u64,
    #[schemars(schema_with = "u32_schema")]
    max_backend_response_bytes: u32,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    timeout_ms: u64,
}
}
