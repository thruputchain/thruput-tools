//! Verify payload format, consensus and transaction body hashes, cardinality,
//! and duplicate transactions. Deliberately do not recompute `txs_hash` or
//! `events_hash`: hogd owns that hashing and recomputes on load; the firehose
//! must not track its trie encoding.
use sha2::{Digest, Sha256};
use thruput_indexer::EventIndexerPulledBlock;

use crate::{Error, encode, invalid};

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn verify(pulled: &EventIndexerPulledBlock) -> Result<(), Error> {
    let payload = &pulled.block.payload;
    let value = &payload.consensus_value;
    if payload.format_version != 2 || *value.proposal_header.block_height < 0 {
        return Err(invalid("unsupported payload format or negative height"));
    }
    if encode(&pulled.block.identity.consensus_hash)? != digest(&encode(value)?) {
        return Err(invalid("consensus hash mismatch"));
    }
    if payload.event_logs.len() != payload.tx_bodies.len() {
        return Err(invalid("event/body cardinality mismatch"));
    }
    let mut txs = Vec::with_capacity(payload.tx_bodies.len());
    for (event, body) in payload.event_logs.iter().zip(payload.tx_bodies.iter()) {
        let hash = digest(&encode(&body.signed_tx.tx)?);
        if encode(&event.tx_hash)? != hash {
            return Err(invalid("transaction body hash mismatch"));
        }
        txs.push(hash);
    }
    txs.sort_unstable();
    if txs.windows(2).any(|w| w[0] == w[1]) {
        return Err(invalid("duplicate transaction hash"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_fixture_verifies_regardless_of_body_order_and_rejects_duplicates() {
        let hex: String = include_str!("../tests/fixtures/hundred-transactions.hex")
            .split_whitespace()
            .collect();
        let mut block: EventIndexerPulledBlock =
            xdrs::txs::decode(&hex::decode(hex).unwrap()).unwrap();
        assert_eq!(block.block.payload.tx_bodies.len(), 100);
        verify(&block).unwrap();
        block.block.payload.tx_bodies.0.reverse();
        block.block.payload.event_logs.0.reverse();
        verify(&block).unwrap();
        block
            .block
            .payload
            .tx_bodies
            .0
            .push(block.block.payload.tx_bodies[0].clone());
        block
            .block
            .payload
            .event_logs
            .0
            .push(block.block.payload.event_logs[0].clone());
        assert!(
            verify(&block)
                .unwrap_err()
                .to_string()
                .contains("duplicate transaction hash")
        );
    }
}
