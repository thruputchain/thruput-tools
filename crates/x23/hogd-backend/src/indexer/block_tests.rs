use super::*;
use thruput_json_rpc_types::ConsensusValue;
use thruput_xdr::{header::ProposalHeader, types::ExtensionPoint};

fn value() -> ConsensusValue {
    let hash = thruput_xdr::types::SHA256Hash::from([7; 32]);
    ConsensusValue {
        protocol: 1.into(),
        ext: ExtensionPoint::_0,
        prev_hash: hash.into(),
        proposal_txs_hash: hash.into(),
        upgrades: xdr_common::VecM(vec![]),
        proposal_header: ProposalHeader {
            ext: ExtensionPoint::_0,
            network_hash: hash.into(),
            config_hash: hash.into(),
            proposer: [2; 32].into(),
            block_timepoint: 123.into(),
            block_height: 99.into(),
            txs_hash: hash.into(),
            storage_hash: hash.into(),
            events_hash: hash.into(),
        },
    }
}

#[test]
fn canonical_projection_preserves_block_identity_and_height() {
    let value = value();
    let expected = thruput_json_rpc_types::block::consensus_value_hash(&value).unwrap();
    let block = block_lookup(
        "indexer",
        &99.into(),
        BlockLookupResult::BlockFound { value },
    )
    .unwrap()
    .unwrap();
    assert_eq!(block.block_hash, expected);
    assert_eq!(*block.block_height, 99);
    assert_eq!(*block.block_timepoint, 123);
}

#[test]
fn only_a_recognized_miss_is_absence() {
    assert!(
        block_lookup(
            "indexer",
            &98.into(),
            BlockLookupResult::BlockFound { value: value() }
        )
        .is_err()
    );
    assert!(
        block_lookup(
            "indexer",
            &99.into(),
            BlockLookupResult::Default { outcome: 999 }
        )
        .is_err()
    );
    assert!(
        block_lookup(
            "indexer",
            &99.into(),
            BlockLookupResult::Default {
                outcome: BlockLookupOutcome::BlockNotFound.0
            }
        )
        .unwrap()
        .is_none()
    );
}
