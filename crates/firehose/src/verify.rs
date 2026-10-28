//! Commitment encoding mirrors hogd's Merkle trie node encoding and
//! `hogd/xdr/indexer_payload_validation.cc`. These are Patricia roots, not hashes
//! of the serialized batches. Prefix tries always have branch roots.
//! Leaves commit the full key and raw value per hogd#4603/#4615.
use sha2::{Digest, Sha256};
use thruput_indexer::EventIndexerPulledBlock;

use crate::{Error, encode, invalid};

type Leaf = (Vec<u8>, Vec<u8>);

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn path(bytes: &mut Vec<u8>, nibbles: &[u8]) {
    bytes.extend_from_slice(&(nibbles.len() as u64).to_le_bytes());
    for pair in nibbles.chunks(2) {
        bytes.push(pair[0] << 4 | pair.get(1).copied().unwrap_or(0));
    }
}

fn branch(leaves: &[Leaf], depth: usize) -> [u8; 32] {
    let mut bytes = vec![0, 0]; // branch, no value
    for digit in 0..16 {
        let start = leaves.partition_point(|(key, _)| key[depth] < digit);
        let end = leaves.partition_point(|(key, _)| key[depth] <= digit);
        let hash = if start == end {
            [0; 32]
        } else {
            node(&leaves[start..end], depth + 1)
        };
        bytes.extend_from_slice(&hash);
    }
    digest(&bytes)
}

fn node(leaves: &[Leaf], depth: usize) -> [u8; 32] {
    let key = &leaves[0].0;
    if leaves.len() == 1 {
        let mut bytes = vec![2]; // leaf, full key and raw value
        path(&mut bytes, key);
        bytes.extend_from_slice(&(leaves[0].1.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&leaves[0].1);
        return digest(&bytes);
    }
    let last = &leaves[leaves.len() - 1].0;
    let common = (depth..64).take_while(|&i| key[i] == last[i]).count();
    if common == 0 {
        return branch(leaves, depth);
    }
    let mut bytes = vec![1]; // extension
    path(&mut bytes, &key[depth..depth + common]);
    bytes.extend_from_slice(&branch(leaves, depth + common));
    digest(&bytes)
}

fn root(mut entries: Vec<Leaf>) -> Result<[u8; 32], Error> {
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    if entries.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(invalid("duplicate transaction hash"));
    }
    let mut bytes = vec![0, 0];
    for digit in 0..16 {
        let start = entries.partition_point(|(key, _)| key[0] < digit);
        let end = entries.partition_point(|(key, _)| key[0] <= digit);
        // Unlike a deeper empty child, which is the zero hash, every top-level
        // child is itself a branch node, empty or not.
        bytes.extend_from_slice(&branch(&entries[start..end], 1));
    }
    Ok(digest(&bytes))
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
    let mut events = Vec::with_capacity(payload.event_logs.len());
    let mut txs = Vec::with_capacity(payload.tx_bodies.len());
    for (event, body) in payload.event_logs.iter().zip(payload.tx_bodies.iter()) {
        let hash = digest(&encode(&body.signed_tx.tx)?);
        if encode(&event.tx_hash)? != hash {
            return Err(invalid("transaction body hash mismatch"));
        }
        let key: Vec<u8> = hash.iter().flat_map(|b| [b >> 4, b & 15]).collect();
        let mut event_bytes = encode(event)?;
        event_bytes.extend_from_slice(&encode(&body.nondeterministic_proposal_data)?);
        // The raw event value omits the leading 32-byte tx hash (the trie key).
        // The transaction trie commits presence only, so its value is empty.
        events.push((key.clone(), event_bytes[32..].to_vec()));
        txs.push((key, Vec::new()));
    }
    if encode(&value.proposal_header.events_hash)? != root(events)? {
        return Err(invalid("events_hash mismatch"));
    }
    if encode(&value.proposal_header.txs_hash)? != root(txs)? {
        return Err(invalid("txs_hash mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hogd_go_golden_roots_verify_and_reject_event_mutation() {
        use thruput_indexer::{
            IndexerCommittedBlock, IndexerCommittedBlockIdentity, IndexerCommittedBlockPayload,
            IndexerOutboxOrigin,
        };

        let hex: String = include_str!("../tests/fixtures/golden-committed-payload.hex")
            .split_whitespace()
            .collect();
        let bytes = hex::decode(hex).unwrap();
        assert_eq!(bytes.len(), 588);
        assert_eq!(
            hex::encode(digest(&bytes)),
            "87b210f3526d9271844760e180b96fbb17a391b60149c3883b817bbe2d866daa"
        );
        let payload: IndexerCommittedBlockPayload = xdrs::txs::decode(&bytes).unwrap();
        let header = &payload.consensus_value.proposal_header;
        assert_eq!(
            hex::encode(encode(&header.txs_hash).unwrap()),
            "cc7425c1b3d77f0421ad1bf20ddc8f57d30d9ba697a22f703b64a95aac67d12e"
        );
        assert_eq!(
            hex::encode(encode(&header.events_hash).unwrap()),
            "326795f5450bb6dbcbde01a7aa49752420428a9369ffae49bbdb472e94d83ff7"
        );
        assert_eq!(payload.tx_bodies.len(), 1);
        assert_eq!(payload.event_logs.len(), 1);
        let identity = IndexerCommittedBlockIdentity {
            consensus_hash: xdrs::txs::decode(&digest(&encode(&payload.consensus_value).unwrap()))
                .unwrap(),
        };
        let mut block = EventIndexerPulledBlock {
            origin: IndexerOutboxOrigin {
                protocol: Default::default(),
                epoch_sequence: 0,
                first_height: 0.into(),
                first_prev_hash: xdrs::txs::decode(&[0; 32]).unwrap(),
            },
            block: IndexerCommittedBlock { payload, identity },
        };
        assert!(verify(&block).is_ok());

        block.block.payload.event_logs.0[0].gas_consumed ^= 1;
        block.block.identity.consensus_hash = xdrs::txs::decode(&digest(
            &encode(&block.block.payload.consensus_value).unwrap(),
        ))
        .unwrap();
        assert!(
            verify(&block)
                .unwrap_err()
                .to_string()
                .contains("events_hash mismatch")
        );
    }

    #[test]
    fn live_hundred_transaction_roots_are_order_independent_and_reject_duplicates() {
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
