//! Block production and block time controls on a real local node.
//!
//! Requires a `hog-devnode` binary (via `THRUPUT_DEVNODE_BINARY` or `PATH`),
//! so it is `#[ignore]`d.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thruput_testkit::xdr::types::TimePoint;
use thruput_testkit::{
    BlockDriverConfig, BlockDriverMode, LocalNetworkConfig, NetworkConfig, Scenario, TestkitError,
};

const DAY: u64 = 24 * 60 * 60 * 1_000_000_000;

fn wall_clock_nanos() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("bug: the wall clock is after the epoch")
            .as_nanos(),
    )
    .expect("bug: the wall clock fits u64 nanoseconds")
}

#[tokio::test]
#[ignore = "requires a hog-devnode binary (set THRUPUT_DEVNODE_BINARY)"]
async fn block_time_controls_on_a_real_node() {
    let mut config = LocalNetworkConfig::default();
    config.block_driver = BlockDriverConfig::manual();
    if let Ok(path) = std::env::var("THRUPUT_DEVNODE_BINARY") {
        config.hog_devnode_binary = Some(path.into());
    }
    let scenario = Scenario::builder("block-time-live")
        .network(NetworkConfig::local(config))
        .build()
        .await
        .expect("local node starts and advertises a runtime manifest");
    let network = scenario.network();

    // An empty block, then one pinned three days ahead.
    let empty = network.produce_block().await.unwrap();
    let target = wall_clock_nanos() + 3 * DAY;
    let pinned = network
        .produce_block_at(TimePoint::new(target))
        .await
        .unwrap();
    assert_eq!(pinned.height, empty.height + 1);
    assert_eq!(*pinned.timepoint, target);

    // Later blocks continue from the pinned time.
    let next = network.produce_block().await.unwrap();
    assert!(*next.timepoint >= target && *next.timepoint < target + DAY);

    // Blocks never move backward.
    let error = network
        .produce_block_at(TimePoint::new(target))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            TestkitError::NativeCall {
                method: "dev_produceBlock",
                ..
            }
        ),
        "{error}"
    );

    // Increasing time and setting the next timepoint move later blocks.
    let preview = network
        .increase_time(Duration::from_hours(24))
        .await
        .unwrap();
    assert!(*preview >= *next.timepoint + DAY);
    let exact = *preview + DAY;
    let set = network
        .set_next_block_timepoint(TimePoint::new(exact))
        .await
        .unwrap();
    assert_eq!(*set, exact);
    let blocks = network
        .produce_blocks(3, Some(Duration::from_secs(12)))
        .await
        .unwrap();
    assert_eq!(*blocks[0].timepoint, exact);
    for pair in blocks.windows(2) {
        assert_eq!(pair[1].height, pair[0].height + 1);
        assert_eq!(*pair[1].timepoint - *pair[0].timepoint, 12_000_000_000);
    }

    // A zero step freezes the clock; a one-second step spaces blocks exactly.
    let tick = *blocks[2].timepoint + DAY;
    network
        .produce_block_at(TimePoint::new(tick))
        .await
        .unwrap();
    let frozen = network
        .set_block_time_step(Some(Duration::ZERO))
        .await
        .unwrap();
    assert_eq!(*frozen, tick);
    for block in network.produce_blocks(3, None).await.unwrap() {
        assert_eq!(*block.timepoint, tick);
    }
    network
        .set_block_time_step(Some(Duration::from_secs(1)))
        .await
        .unwrap();
    let stepped = network.produce_blocks(2, None).await.unwrap();
    assert_eq!(*stepped[0].timepoint, tick + 1_000_000_000);
    assert_eq!(*stepped[1].timepoint, tick + 2_000_000_000);
    let resumed = network.set_block_time_step(None).await.unwrap();
    assert!((tick + 2_000_000_000..tick + DAY).contains(&*resumed));

    // Switching modes reports the active settings.
    let status = network
        .set_block_driver(BlockDriverMode::Interval, Some(Duration::from_millis(250)))
        .await
        .unwrap();
    assert_eq!(status.interval_milliseconds, 250);
    network
        .set_block_driver(BlockDriverMode::Manual, None)
        .await
        .unwrap();
}
