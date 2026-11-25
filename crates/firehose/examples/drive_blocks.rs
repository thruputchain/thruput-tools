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
    preimage::DomainSeparator,
    types::{ConsensusValueHash, ExtensionPoint, Protocol, SHA256Hash, TimePoint},
    validator::ValidatorID,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn hash<T: xdrs::WriteXDR>(value: &T) -> Result<[u8; 32]> {
    Ok(Sha256::digest(xdrs::txs::encode(value)?).into())
}

/// `cxs(DomainSeparator(name))`, as hogd builds it.
fn domain_separator(name: &[u8]) -> Result<Vec<u8>> {
    Ok(xdrs::cxs::encode(&DomainSeparator {
        ext: ExtensionPoint::_0,
        domain_separator: xdrs::BytesM(name.to_vec()),
    })?)
}

// Mirrors node-supervisor's proposal_txs_compatibility_hash, which matches
// hogd/xdr/proposal_hash.h as of hog-devnode 0.1.37:
//   SHA-256(cxs(DomainSeparator("ParallelBatch")) || cxs(BatchCommit))
// A BatchCommit is an ext of 0, the batch's segment_tokens and segment_bytes,
// and the array of the SHA-256 hashes of its segments.
fn batch_hash(batch: &ParallelBatch) -> Result<[u8; 32]> {
    batch.validate().map_err(|e| e.to_string())?;
    let mut segment_hashes = Vec::with_capacity(batch.segment_bytes.0.len());
    let mut offset = 0;
    for &size in &batch.segment_bytes.0 {
        let end = offset + size as usize;
        let part = batch.xdr.0.get(offset..end).ok_or("bad segment range")?;
        segment_hashes.push(SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(part))));
        offset = end;
    }
    if offset != batch.xdr.len() {
        return Err("bad segment length".into());
    }
    let mut hasher = Sha256::new();
    hasher.update(domain_separator(b"ParallelBatch")?);
    hasher.update(xdrs::cxs::encode(&ExtensionPoint::_0)?);
    hasher.update(xdrs::cxs::encode(&batch.segment_tokens)?);
    hasher.update(xdrs::cxs::encode(&batch.segment_bytes)?);
    hasher.update(xdrs::cxs::encode(&xdrs::VecM::<SHA256Hash>(
        segment_hashes,
    ))?);
    Ok(hasher.finalize().into())
}

//   SHA-256(cxs(DomainSeparator("ProposalTxs")) || cxs(ProposalTxsCommit))
// A ProposalTxsCommit is the ext of the ProposalTxs, then the hashes of its
// two batches.
fn proposal_hash(txs: &ProposalTxs) -> Result<ProposalTxsHash> {
    let mut hasher = Sha256::new();
    hasher.update(domain_separator(b"ProposalTxs")?);
    hasher.update(xdrs::cxs::encode(&txs.ext)?);
    hasher.update(xdrs::cxs::encode(&SHA256Hash::from(batch_hash(
        &txs.signed_hash,
    )?))?);
    hasher.update(xdrs::cxs::encode(&SHA256Hash::from(batch_hash(
        &txs.nondeterministic_data,
    )?))?);
    Ok(ProposalTxsHash::new(SHA256Hash::from(<[u8; 32]>::from(
        hasher.finalize(),
    ))))
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

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_node_rpc::{
        ParallelNondeterministicProposalDataBatch, ParallelSignedTransactionHashBatch,
    };
    use xdrs::{BytesM, VecM};

    #[test]
    fn proposal_hash_matches_hogd_golden_vector() {
        let txs = ProposalTxs {
            ext: ExtensionPoint::_0,
            signed_hash: ParallelSignedTransactionHashBatch::new(ParallelBatch {
                segment_tokens: VecM(vec![1, 2]),
                segment_bytes: VecM(vec![4, 8]),
                xdr: BytesM(vec![0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3]),
            }),
            nondeterministic_data: ParallelNondeterministicProposalDataBatch::new(ParallelBatch {
                segment_tokens: VecM(vec![1]),
                segment_bytes: VecM(vec![4]),
                xdr: BytesM(vec![0xde, 0xad, 0xbe, 0xef]),
            }),
        };
        assert_eq!(
            proposal_hash(&txs).unwrap().into_inner(),
            // hogd's Mempool.ProposalTxsCompatibilityHashGoldenVector.
            SHA256Hash::from([
                0x55, 0x6a, 0xf4, 0x47, 0x8a, 0x82, 0xe4, 0x99, 0x1f, 0x0e, 0x59, 0x42, 0xaf, 0xce,
                0x8b, 0x66, 0xe6, 0x83, 0x4c, 0xb4, 0xec, 0xf8, 0x82, 0x65, 0xd8, 0xac, 0xa7, 0x48,
                0x5d, 0x2d, 0x3e, 0xa3,
            ])
        );
    }
}
