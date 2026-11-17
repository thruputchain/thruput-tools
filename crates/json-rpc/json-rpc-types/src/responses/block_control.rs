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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn produced_blocks_use_decimal_strings() {
        let value = json!({"blocks": [{"blockHeight": "4", "blockTimepoint": "17"}]});
        let decoded: ProducedBlocks = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(*decoded.blocks[0].block_height, 4);
        assert_eq!(*decoded.blocks[0].block_timepoint, 17);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
        assert!(
            serde_json::from_value::<ProducedBlock>(
                json!({"blockHeight": 4, "blockTimepoint": "17"})
            )
            .is_err()
        );
    }

    #[test]
    fn an_unfamiliar_mode_is_retained_as_unsupported() {
        let decoded: BlockDriverStatus =
            serde_json::from_value(json!({"mode": "future", "intervalMilliseconds": "500"}))
                .unwrap();
        assert!(!decoded.is_supported());

        let known: BlockDriverStatus =
            serde_json::from_value(json!({"mode": "manual", "intervalMilliseconds": "500"}))
                .unwrap();
        assert!(known.is_supported());
        assert_eq!(known.interval_milliseconds, 500);
    }

    #[test]
    fn block_clock_reports_the_next_timepoint() {
        let clock: BlockClock =
            serde_json::from_value(json!({"nextBlockTimepoint": "99"})).unwrap();
        assert_eq!(*clock.next_block_timepoint, 99);
    }
}
