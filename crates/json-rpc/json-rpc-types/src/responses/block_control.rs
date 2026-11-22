//! Local block production and block time results.

use super::{
    Deserialize, Deserializer, JsonSchema, ResponseData, Serialize,
    fields::{height_schema, read_height},
};
use crate::{BlockHeight, TimePoint};

response_object! {
/// A block the local node produced on request.
ProducedBlock {
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    block_height: BlockHeight,
    /// Unix nanoseconds, as contracts read it.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<TimePoint>")]
    block_timepoint: TimePoint,
}
}

response_object! {
/// Blocks produced by one request, in height order.
ProducedBlocks {
    /// Produced blocks, lowest height first.
    blocks: Vec<ProducedBlock>,
}
}

response_object! {
/// The block clock after a time change.
BlockClock {
    /// Unix nanoseconds the next block receives if it is produced now.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<TimePoint>")]
    next_block_timepoint: TimePoint,
}
}

response_object! {
/// The block clock after a step change.
BlockTimeStep {
    /// Nanoseconds from each block to the next unpinned one. Absent while
    /// blocks follow the wall clock.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::decimal::option::serialize",
        deserialize_with = "read_optional_step"
    )]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    step_nanoseconds: Option<u64>,
    /// Unix nanoseconds the next block receives if it is produced now.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<TimePoint>")]
    next_block_timepoint: TimePoint,
}
}

fn read_optional_step<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    crate::decimal::Decimal::<u64>::deserialize(d).map(|value| Some(value.0))
}

response_strings! {
/// How the local node produces blocks.
ActiveBlockDriverMode {
    Instant("instant", true),
    Interval("interval", true),
    Manual("manual", true),
}
}

response_object! {
/// The local node's block production settings.
BlockDriverStatus {
    /// How the node produces blocks now.
    mode: ActiveBlockDriverMode,
    /// The interval used in interval mode.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    interval_milliseconds: u64,
}
}
