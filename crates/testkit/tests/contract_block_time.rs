//! A contract view sees the block time a local node moves forward.
//!
//! Deploys the `block-clock` fixture from thruput-examples, which returns the
//! timepoint of the block it runs in. Requires a `hog-devnode` binary whose
//! views use the tip's timepoint when it is ahead of the wall clock, and a
//! built `block-clock` artifact, so it is `#[ignore]`d and driven by
//! `scripts/smoke-testkit-local.sh`.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use thruput_testkit::rpc_types::responses::InvocationOutput;
use thruput_testkit::xdr::types::{ContractAddress, TimePoint};
use thruput_testkit::{
    Agent, AgentContext, DeployOptions, LocalNetworkConfig, NetworkConfig, Result, Scenario,
    entrypoint_id,
};

/// `block-clock`'s interface identity, from which method selectors derive.
const INTERFACE: &str = "BlockClock";
const VERSION: &str = "0.1.0";

const DAY: u64 = 24 * 60 * 60 * 1_000_000_000;

/// The built fixture. A missing fixture is a setup error to report, not a
/// reason to pass.
fn block_clock_artifact() -> PathBuf {
    let root = std::env::var("THRUPUT_TESTKIT_ARTIFACT_ROOT").expect(
        "set THRUPUT_TESTKIT_ARTIFACT_ROOT to a directory containing block_clock.rv32a \
         (see scripts/smoke-testkit-local.sh)",
    );
    PathBuf::from(root).join("block_clock.rv32a")
}

fn wall_clock_nanos() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("bug: the wall clock is after the epoch")
            .as_nanos(),
    )
    .expect("bug: the wall clock fits u64 nanoseconds")
}

/// Call `now()` as a view and decode its CXS `u64`. Testkit floors the read
/// at the highest block it has seen, so it already runs at or after `height`.
async fn view_now(ctx: &AgentContext, contract: ContractAddress, height: i64) -> Result<u64> {
    let now = entrypoint_id(INTERFACE, VERSION, "now");
    let response = ctx.network().view_call(contract, now, &[], None).await?;
    assert!(
        *response.state_block_height >= height,
        "a read after block {height} ran on state {}",
        *response.state_block_height
    );
    let InvocationOutput::Return { return_data } = response.output else {
        panic!("now() did not return a value: {:?}", response.output);
    };
    Ok(thruput_testkit::xdrs::cxs::decode::<u64>(&return_data.0)
        .expect("now() return decodes as a u64"))
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY and a built block-clock fixture in THRUPUT_TESTKIT_ARTIFACT_ROOT"]
async fn a_view_reads_the_block_time_after_a_jump() {
    let mut config = LocalNetworkConfig::default();
    if let Ok(path) = std::env::var("THRUPUT_DEVNODE_BINARY") {
        config.hog_devnode_binary = Some(path.into());
    }
    let scenario = Scenario::builder("block-clock-smoke")
        .network(NetworkConfig::local(config))
        .artifact("block-clock", block_clock_artifact())
        .accounts(1)
        .build()
        .await
        .expect("local node starts and advertises a runtime manifest");

    scenario
        .run_agents([Agent::new("alice", |mut ctx| async move {
            ctx.fund(1_000_000_000).await?;
            let contract = ctx
                .deploy_artifact("block-clock", DeployOptions::funded(100_000_000))
                .await?
                .address;

            // Before any jump, a view runs at about the wall clock.
            let before = view_now(&ctx, contract, 0).await?;
            assert!(before.abs_diff(wall_clock_nanos()) < DAY);

            // An empty block three days ahead: a view reads exactly its time.
            let target = wall_clock_nanos() + 3 * DAY;
            let pinned = ctx
                .network()
                .produce_block_at(TimePoint::new(target))
                .await?;
            assert_eq!(*pinned.timepoint, target);
            assert_eq!(view_now(&ctx, contract, pinned.height).await?, target);

            // Thirty more days, made visible to views by an empty block.
            ctx.network()
                .increase_time(Duration::from_hours(30 * 24))
                .await?;
            let later = ctx.network().produce_block().await?;
            assert!(*later.timepoint >= target + 30 * DAY);
            assert_eq!(
                view_now(&ctx, contract, later.height).await?,
                *later.timepoint
            );
            Ok(())
        })])
        .await
        .expect("block-clock views follow the moved block time");
}
