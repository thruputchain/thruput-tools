//! Chain-building helpers shared by the archive and subscription suites.
use sha2::{Digest, Sha256};
use thruput_indexer::EventIndexerPulledBlock;

/// Height 1 of a live node, captured with its origin and both commitments.
pub fn fixture() -> EventIndexerPulledBlock {
    let hex: String = include_str!("../fixtures/bootstrap.hex")
        .split_whitespace()
        .collect();
    xdrs::txs::decode(&hex::decode(hex).unwrap()).unwrap()
}

/// Restore the identity commitment after editing the consensus value.
pub fn rehash(block: &mut EventIndexerPulledBlock) {
    let bytes = xdrs::txs::encode(&block.block.payload.consensus_value).unwrap();
    block.block.identity.consensus_hash =
        thruput_xdr::types::ConsensusValueHash::new(<[u8; 32]>::from(Sha256::digest(bytes)).into());
}

/// The next block in the chain: one height on, parented by this one.
pub fn next(block: &EventIndexerPulledBlock) -> EventIndexerPulledBlock {
    let mut next = block.clone();
    next.block.payload.consensus_value.prev_hash = block.block.identity.consensus_hash.clone();
    next.block
        .payload
        .consensus_value
        .proposal_header
        .block_height += 1;
    rehash(&mut next);
    next
}
