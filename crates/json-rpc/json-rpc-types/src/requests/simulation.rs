use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    fields::{
        entrypoint_schema, gas_schema, hex_schema, present, read_entrypoint, read_gas, read_hex,
        read_optional_height, u32_schema, u64_schema,
    },
    height_schema,
};
use crate::{BlockHeight, ContractAddress, PublicKeyHash, hex::HexBytes};

object_enum! {
/// Assumed authorization for simulation; no signature is verified.
AssumedAuth, "kind" {
    PubkeySignature {
        pubkey_hash: PublicKeyHash,
        /// Account derivation nonce, not the transaction replay nonce.
        #[serde(with = "crate::decimal")]
        #[schemars(schema_with = "u64_schema")]
        account_nonce: u64,
    },
    SourceAddress {
        #[schemars(schema_with = "u32_schema")]
        allowed_keys: u32,
        #[schemars(schema_with = "u32_schema")]
        required_weight: u32,
    },
}
}

object_request! {
/// One batch invocation; freshness belongs to the whole batch.
Invocation {
    address: ContractAddress,
    #[serde(deserialize_with = "read_entrypoint")]
    #[schemars(schema_with = "entrypoint_schema")]
    entrypoint: HexBytes,
    #[serde(deserialize_with = "read_hex")]
    #[schemars(schema_with = "hex_schema")]
    calldata: HexBytes,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    #[schemars(with = "ContractAddress")]
    caller: Option<ContractAddress>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    #[schemars(with = "AssumedAuth")]
    assumed_auth: Option<AssumedAuth>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::decimal::option::serialize",
        deserialize_with = "read_gas"
    )]
    #[schemars(schema_with = "gas_schema")]
    gas_limit: Option<u64>,
}
}

object_request! {
/// Simulate one invocation against committed state.
SimulateInvocationRequest {
    address: ContractAddress,
    #[serde(deserialize_with = "read_entrypoint")]
    #[schemars(schema_with = "entrypoint_schema")]
    entrypoint: HexBytes,
    #[serde(deserialize_with = "read_hex")]
    #[schemars(schema_with = "hex_schema")]
    calldata: HexBytes,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    #[schemars(with = "ContractAddress")]
    caller: Option<ContractAddress>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    #[schemars(with = "AssumedAuth")]
    assumed_auth: Option<AssumedAuth>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::decimal::option::serialize",
        deserialize_with = "read_gas"
    )]
    #[schemars(schema_with = "gas_schema")]
    gas_limit: Option<u64>,
    /// Fail-fast freshness floor; not a historical-state selector.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::decimal::option::serialize",
        deserialize_with = "read_optional_height"
    )]
    #[schemars(schema_with = "height_schema")]
    min_state_block_height: Option<BlockHeight>,
}
}

object_request! {
/// Simulate independent invocations against one common snapshot.
SimulateInvocationsRequest {
    #[serde(deserialize_with = "read_invocations")]
    #[schemars(length(min = 1))]
    invocations: Vec<Invocation>,
    /// Fail-fast freshness floor; not a historical-state selector.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::decimal::option::serialize",
        deserialize_with = "read_optional_height"
    )]
    #[schemars(schema_with = "height_schema")]
    min_state_block_height: Option<BlockHeight>,
}
}

fn read_invocations<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Invocation>, D::Error> {
    let calls = Vec::<Invocation>::deserialize(d)?;
    if calls.is_empty() {
        return Err(serde::de::Error::custom("invocations must be nonempty"));
    }
    Ok(calls)
}
