//! Local block production and block time requests. Only a local node serves
//! them; hosted networks do not register these methods.

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{TimePoint, decimal::Decimal};

use super::fields::{u32_schema, u64_schema};

/// Largest block count that one `dev_produceBlocks` request accepts.
pub const MAX_PRODUCE_BLOCKS: u32 = 1_000;

object_request! {
/// Produce one block now; it can be empty. With a timepoint, the block takes
/// exactly that time and later blocks continue from it.
ProduceBlockRequest {
    /// Unix nanoseconds, at or after the chain tip's timepoint.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::decimal::option::serialize",
        deserialize_with = "read_optional_timepoint"
    )]
    #[schemars(with = "Decimal<TimePoint>")]
    block_timepoint: Option<TimePoint>,
}
}

object_request! {
/// Produce several blocks now, in order. With an interval, each block after
/// the first is exactly that much later than the one before; without one,
/// every block follows the block clock.
ProduceBlocksRequest {
    /// Number of blocks, from 1 to 1000.
    #[serde(deserialize_with = "read_block_count")]
    #[schemars(schema_with = "block_count_schema")]
    count: u32,
    /// Nanoseconds between consecutive blocks.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::decimal::option::serialize",
        deserialize_with = "read_optional_u64"
    )]
    #[schemars(schema_with = "u64_schema")]
    interval_nanoseconds: Option<u64>,
}
}

object_request! {
/// Give the next block exactly this timepoint. Later blocks continue from it.
SetNextBlockTimepointRequest {
    /// Unix nanoseconds, at or after the chain tip's timepoint.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "Decimal<TimePoint>")]
    block_timepoint: TimePoint,
}
}

object_request! {
/// Move the block clock forward for every later block.
IncreaseTimeRequest {
    /// Nanoseconds to add to the block clock.
    #[serde(with = "crate::decimal")]
    #[schemars(schema_with = "u64_schema")]
    nanoseconds: u64,
}
}

/// How a local node produces blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BlockDriverMode {
    /// One block for each accepted transaction submission.
    Instant,
    /// One block at a fixed interval.
    Interval,
    /// Blocks only on request.
    Manual,
}

object_request! {
/// Change how the local node produces blocks while it runs.
SetBlockDriverRequest {
    /// How to produce blocks from now on.
    mode: BlockDriverMode,
    /// Replaces the interval used in interval mode.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::decimal::option::serialize",
        deserialize_with = "read_optional_interval_ms"
    )]
    #[schemars(schema_with = "interval_ms_schema")]
    interval_milliseconds: Option<u64>,
}
}

fn read_optional_timepoint<'de, D: Deserializer<'de>>(d: D) -> Result<Option<TimePoint>, D::Error> {
    Decimal::<TimePoint>::deserialize(d).map(|value| Some(value.0))
}

fn read_optional_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Decimal::<u64>::deserialize(d).map(|value| Some(value.0))
}

fn read_optional_interval_ms<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    let value = Decimal::<u64>::deserialize(d)?.0;
    if value == 0 {
        return Err(serde::de::Error::custom(
            "intervalMilliseconds must be positive",
        ));
    }
    Ok(Some(value))
}

fn interval_ms_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = u64_schema(g);
    schema.insert("pattern".into(), r"^[1-9][0-9]*(?![\s\S])".into());
    schema
}

fn read_block_count<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let count = u32::deserialize(d)?;
    if !(1..=MAX_PRODUCE_BLOCKS).contains(&count) {
        return Err(serde::de::Error::custom(format!(
            "count must be between 1 and {MAX_PRODUCE_BLOCKS}"
        )));
    }
    Ok(count)
}

fn block_count_schema(g: &mut SchemaGenerator) -> Schema {
    let mut schema = u32_schema(g);
    schema.insert("minimum".into(), 1.into());
    schema.insert("maximum".into(), MAX_PRODUCE_BLOCKS.into());
    schema
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn produce_block_accepts_an_omitted_or_decimal_timepoint() {
        let empty: ProduceBlockRequest = serde_json::from_value(json!({})).unwrap();
        assert!(empty.block_timepoint.is_none());

        let pinned: ProduceBlockRequest =
            serde_json::from_value(json!({"blockTimepoint": "1760000000000000000"})).unwrap();
        assert_eq!(
            pinned.block_timepoint.as_deref(),
            Some(&1_760_000_000_000_000_000)
        );
        assert_eq!(
            serde_json::to_value(&pinned).unwrap(),
            json!({"blockTimepoint": "1760000000000000000"})
        );

        for invalid in [
            json!({"blockTimepoint": 1}),
            json!({"blockTimepoint": null}),
            json!({"blockTimepoint": "-1"}),
            json!({"other": "1"}),
        ] {
            assert!(serde_json::from_value::<ProduceBlockRequest>(invalid).is_err());
        }
    }

    #[test]
    fn produce_blocks_bounds_the_count() {
        let request: ProduceBlocksRequest =
            serde_json::from_value(json!({"count": 3, "intervalNanoseconds": "12"})).unwrap();
        assert_eq!(request.count, 3);
        assert_eq!(request.interval_nanoseconds, Some(12));

        for count in [0, MAX_PRODUCE_BLOCKS + 1] {
            assert!(
                serde_json::from_value::<ProduceBlocksRequest>(json!({"count": count})).is_err()
            );
        }
        assert!(serde_json::from_value::<ProduceBlocksRequest>(json!({"count": "1"})).is_err());
    }

    #[test]
    fn time_requests_use_decimal_strings() {
        let next: SetNextBlockTimepointRequest =
            serde_json::from_value(json!({"blockTimepoint": "5"})).unwrap();
        assert_eq!(*next.block_timepoint, 5);
        let increase: IncreaseTimeRequest =
            serde_json::from_value(json!({"nanoseconds": "259200000000000"})).unwrap();
        assert_eq!(increase.nanoseconds, 259_200_000_000_000);

        assert!(serde_json::from_value::<SetNextBlockTimepointRequest>(json!({})).is_err());
        assert!(serde_json::from_value::<IncreaseTimeRequest>(json!({"nanoseconds": 1})).is_err());
    }

    #[test]
    fn set_block_driver_names_the_mode() {
        let request: SetBlockDriverRequest =
            serde_json::from_value(json!({"mode": "interval", "intervalMilliseconds": "250"}))
                .unwrap();
        assert_eq!(request.mode, BlockDriverMode::Interval);
        assert_eq!(request.interval_milliseconds, Some(250));

        for invalid in [
            json!({"mode": "automine"}),
            json!({"mode": "interval", "intervalMilliseconds": "0"}),
            json!({}),
        ] {
            assert!(serde_json::from_value::<SetBlockDriverRequest>(invalid).is_err());
        }
    }
}
