use super::{
    Deserialize, Deserializer, EmittedEvent, JsonSchema, ResponseData, Serialize, UnknownVariant,
    Value,
    fields::{height_schema, hex_schema, read_height, read_hex},
};
use crate::{BlockHeight, hex::HexBytes};

response_enum! {
/// Invocation outcome. This does not validate a signed transaction envelope.
InvocationOutput, "result" {
    Return("return", true) {
        #[serde(deserialize_with = "read_hex")]
        #[schemars(schema_with = "hex_schema")]
        return_data: HexBytes,
    },
    Abort("abort", true) {
        #[serde(deserialize_with = "read_hex")]
        #[schemars(schema_with = "hex_schema")]
        abort_data: HexBytes,
    },
    Fault("fault", true) {},
}
}

response_object! {
/// One invocation's outcome within a shared-snapshot batch.
InvocationResult {
    output: InvocationOutput,
    events: Vec<EmittedEvent>,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    gas_used: u64,
}
}

response_object! {
/// Simulation at committed state H, executed with context H+1.
SimulationResult {
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    state_block_height: BlockHeight,
    output: InvocationOutput,
    events: Vec<EmittedEvent>,
    /// Metered work, not a fee estimate.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    gas_used: u64,
}
}

response_object! {
/// Input-ordered results from independent invocations at the same snapshot.
SimulationResults {
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    state_block_height: BlockHeight,
    results: Vec<InvocationResult>,
}
}
