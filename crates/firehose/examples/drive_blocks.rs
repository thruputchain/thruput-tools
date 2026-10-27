//! Local-only measurement driver: scheduler proposal/commit cycles, mirroring
//! node-supervisor's manual block driver. Never run against a production node.
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use thruput_node_rpc::{
    ConsensusValue, ParallelBatch, ProposalTxs, ProposalTxsHash, SchedulerCommitOutcome,
    SchedulerGetTipResult, SchedulerProposalResult,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use thruput_xdr::{
    header::ProposalContext,
    types::{ConsensusValueHash, ExtensionPoint, Protocol, SHA256Hash, TimePoint},
    validator::ValidatorID,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn hash<T: xdrs::WriteXDR>(value: &T) -> Result<[u8; 32]> {
    Ok(Sha256::digest(xdrs::txs::encode(value)?).into())
}

fn batch_hash(batch: &ParallelBatch) -> Result<[u8; 32]> {
    batch.validate().map_err(|e| e.to_string())?;
    let mut base = batch.clone();
    base.xdr.0.clear();
    let mut hash = hash(&base)?;
    let mut offset = 0;
    for &size in &batch.segment_bytes.0 {
        let end = offset + size as usize;
        let part = batch.xdr.0.get(offset..end).ok_or("bad segment range")?;
        for (dst, src) in hash.iter_mut().zip(Sha256::digest(part)) {
            *dst ^= src;
        }
        offset = end;
    }
    if offset != batch.xdr.len() {
        return Err("bad segment length".into());
    }
    Ok(hash)
}

fn proposal_hash(txs: &ProposalTxs) -> Result<ProposalTxsHash> {
    let mut base = txs.clone();
    let batches: [&mut ParallelBatch; 2] = [&mut base.signed_hash, &mut base.nondeterministic_data];
    for batch in batches {
        batch.segment_tokens.0.clear();
        batch.segment_bytes.0.clear();
        batch.xdr.0.clear();
    }
    let mut result = hash(&base)?;
    let batches: [&ParallelBatch; 2] = [&txs.signed_hash, &txs.nondeterministic_data];
    for batch in batches {
        for (dst, src) in result.iter_mut().zip(batch_hash(batch)?) {
            *dst ^= src;
        }
    }
    Ok(ProposalTxsHash::new(SHA256Hash::from(result)))
}

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
        let value = ConsensusValue {
            protocol: Protocol::new(0),
            ext: ExtensionPoint::_0,
            prev_hash: ConsensusValueHash::new(hash(&db_tip.value)?.into()),
            proposal_header: proposed_block.proposal.header,
            proposal_txs_hash: proposal_hash(&proposed_block.proposal.transactions)?,
            upgrades: xdrs::VecM::default(),
        };
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
