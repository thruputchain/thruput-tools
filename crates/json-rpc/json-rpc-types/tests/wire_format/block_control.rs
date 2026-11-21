use serde_json::json;
use thruput_json_rpc_types::requests::{
    BlockDriverMode, IncreaseTimeRequest, MAX_PRODUCE_BLOCKS, ProduceBlockRequest,
    ProduceBlocksRequest, SetBlockDriverRequest, SetBlockTimeStepRequest,
    SetNextBlockTimepointRequest,
};
use thruput_json_rpc_types::responses::{
    BlockClock, BlockDriverStatus, BlockTimeStep, ProducedBlock, ProducedBlocks, ResponseData,
};

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
fn block_time_step_is_a_decimal_or_omitted() {
    let frozen: SetBlockTimeStepRequest =
        serde_json::from_value(json!({"stepNanoseconds": "0"})).unwrap();
    assert_eq!(frozen.step_nanoseconds, Some(0));
    assert_eq!(
        serde_json::to_value(&frozen).unwrap(),
        json!({"stepNanoseconds": "0"})
    );

    let wall: SetBlockTimeStepRequest = serde_json::from_value(json!({})).unwrap();
    assert_eq!(wall.step_nanoseconds, None);
    assert_eq!(serde_json::to_value(&wall).unwrap(), json!({}));

    for invalid in [
        json!({"stepNanoseconds": 0}),
        json!({"stepNanoseconds": null}),
        json!({"stepNanoseconds": "-1"}),
        json!({"stepNanoseconds": "18446744073709551616"}),
        json!({"step": "0"}),
    ] {
        assert!(
            serde_json::from_value::<SetBlockTimeStepRequest>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn produce_blocks_bounds_the_count() {
    let request: ProduceBlocksRequest =
        serde_json::from_value(json!({"count": 3, "intervalNanoseconds": "12"})).unwrap();
    assert_eq!(request.count, 3);
    assert_eq!(request.interval_nanoseconds, Some(12));

    for count in [0, MAX_PRODUCE_BLOCKS + 1] {
        assert!(serde_json::from_value::<ProduceBlocksRequest>(json!({"count": count})).is_err());
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
        serde_json::from_value(json!({"mode": "interval", "intervalMilliseconds": "250"})).unwrap();
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

#[test]
fn block_time_step_omits_a_wall_clock_step() {
    let frozen = json!({"stepNanoseconds": "0", "nextBlockTimepoint": "17"});
    let decoded: BlockTimeStep = serde_json::from_value(frozen.clone()).unwrap();
    assert_eq!(decoded.step_nanoseconds, Some(0));
    assert_eq!(serde_json::to_value(&decoded).unwrap(), frozen);

    let wall = json!({"nextBlockTimepoint": "17"});
    let decoded: BlockTimeStep = serde_json::from_value(wall.clone()).unwrap();
    assert_eq!(decoded.step_nanoseconds, None);
    assert_eq!(serde_json::to_value(&decoded).unwrap(), wall);
}

#[test]
fn produced_blocks_use_decimal_strings() {
    let value = json!({"blocks": [{"blockHeight": "4", "blockTimepoint": "17"}]});
    let decoded: ProducedBlocks = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(*decoded.blocks[0].block_height, 4);
    assert_eq!(*decoded.blocks[0].block_timepoint, 17);
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    assert!(
        serde_json::from_value::<ProducedBlock>(json!({"blockHeight": 4, "blockTimepoint": "17"}))
            .is_err()
    );
}

#[test]
fn an_unfamiliar_mode_is_retained_as_unsupported() {
    let decoded: BlockDriverStatus =
        serde_json::from_value(json!({"mode": "future", "intervalMilliseconds": "500"})).unwrap();
    assert!(!decoded.is_supported());

    let known: BlockDriverStatus =
        serde_json::from_value(json!({"mode": "manual", "intervalMilliseconds": "500"})).unwrap();
    assert!(known.is_supported());
    assert_eq!(known.interval_milliseconds, 500);
}

#[test]
fn block_clock_reports_the_next_timepoint() {
    let clock: BlockClock = serde_json::from_value(json!({"nextBlockTimepoint": "99"})).unwrap();
    assert_eq!(*clock.next_block_timepoint, 99);
}
