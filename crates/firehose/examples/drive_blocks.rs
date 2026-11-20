//! Local-only measurement driver: scheduler proposal/commit cycles, mirroring
//! node-supervisor's manual block driver. Never run against a production node.
use std::time::{Duration, Instant};
use thruput_node_rpc::{
    SchedulerCommitOutcome, SchedulerGetTipResult, SchedulerProposalResult,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use thruput_xdr::{
    header::ProposalContext,
    types::{Protocol, TimePoint},
    validator::ValidatorID,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .ok_or("usage: drive_blocks SCHEDULER_COORD STORE COUNT INTERVAL_MS")?;
    let store = std::path::Path::new(args.get(2).ok_or("missing store")?);
    let count: usize = args.get(3).ok_or("missing count")?.parse()?;
    let interval: u64 = args.get(4).ok_or("missing interval")?.parse()?;
    let scheduler = thruput_x23_transport::connect(
        "local_measurement_driver",
        endpoint,
        1,
        SchedulerCoord_SchedulerCoord1_Client::new,
    )
    .await?;
    for _ in 0..count {
        let SchedulerGetTipResult::SchedulerGetTipOK { db_tip } =
            scheduler.schedcoord_get_tip().await?
        else {
            return Err("no tip".into());
        };
        let context = ProposalContext {
            protocol: Protocol::new(0),
            proposer: ValidatorID::default(),
            block_timepoint: TimePoint::new(0),
        };
        let SchedulerProposalResult::SchedulerProposalOK { proposed_block } = scheduler
            .schedulercoord_propose_block(&db_tip.id, &context)
            .await?
        else {
            return Err("proposal failed".into());
        };
        let height = *proposed_block.proposal.header.block_height;
        let value = thruput_node_supervisor::proposal_consensus_value(
            &db_tip.value,
            proposed_block.proposal,
        )?;
        let start = Instant::now();
        if scheduler
            .schedulercoord_commit_block(&proposed_block.block_id, &value)
            .await?
            != SchedulerCommitOutcome::SchedulerCommitOK
        {
            return Err("commit failed".into());
        }
        let commit_seconds = start.elapsed().as_secs_f64();
        let deadline = Instant::now() + Duration::from_secs(5);
        let path = store.join(format!("{height:020}.xdr"));
        while !path.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        println!(
            "{}",
            serde_json::json!({"height": height, "commitSeconds": commit_seconds, "commitRequestToArchiveSeconds": start.elapsed().as_secs_f64(), "archived": path.exists()})
        );
        tokio::time::sleep(Duration::from_millis(interval)).await;
    }
    Ok(())
}
