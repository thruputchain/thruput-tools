use super::*;
use thruput_json_rpc_types::{ConsensusValue, hex::HexBytes};
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
fn canonical_metadata_retains_all_committed_fields() {
    let mut value = value();
    value.upgrades = xdr_common::VecM(vec![xdr_common::BytesM(vec![0x12, 0xab])]);
    let expected = block::BlockInfo::try_from(value.clone()).unwrap();
    let response = block_lookup(
        &99.into(),
        BlockLookupResult::BlockFound {
            value: value.clone(),
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(response.block_hash, expected.block_hash);
    assert_eq!(response.network_hash, expected.network_hash);
    assert_eq!(response.config_hash, expected.config_hash);
    assert_eq!(response.proposer, expected.proposer);
    assert_eq!(response.protocol, expected.protocol);
    assert_eq!(response.block_timepoint, expected.block_timepoint);
    assert_eq!(response.block_height, expected.block_height);
    assert_eq!(response.txs_hash, expected.txs_hash);
    assert_eq!(response.storage_hash, expected.storage_hash);
    assert_eq!(response.events_hash, expected.events_hash);
    assert_eq!(response.prev_block_hash, expected.prev_block_hash);
    assert_eq!(response.proposal_txs_hash, expected.proposal_txs_hash);
    assert_eq!(response.upgrades, vec![HexBytes(vec![0x12, 0xab])]);
    let json = serde_json::to_value(response).unwrap();
    assert_eq!(json["blockHeight"], "99");
    assert_eq!(json["blockTimepoint"], "123");
    assert_eq!(json["upgrades"], serde_json::json!(["0x12ab"]));
    value.proposal_txs_hash = thruput_xdr::types::SHA256Hash::from([9; 32]).into();
    assert_ne!(
        block::consensus_value_hash(&value).unwrap(),
        expected.block_hash
    );
}

#[test]
fn only_authoritative_misses_are_null() {
    for height in [0, 100] {
        assert!(
            block_lookup(
                &height.into(),
                BlockLookupResult::Default {
                    outcome: BlockLookupOutcome::BlockNotFound.0
                }
            )
            .unwrap()
            .is_none()
        );
    }
    assert!(matches!(
        block_lookup(&99.into(), BlockLookupResult::Default { outcome: 999 }),
        Err(RpcServerError::BackendDataInvalid {
            reason: BackendDataInvalidReason::UnsupportedValue,
            ..
        })
    ));
    for height in [-1, 0, 98] {
        assert!(matches!(
            block_lookup(
                &height.into(),
                BlockLookupResult::BlockFound { value: value() }
            ),
            Err(RpcServerError::BackendDataInvalid {
                reason: BackendDataInvalidReason::InconsistentResult,
                ..
            })
        ));
    }
}

#[test]
fn matched_genesis_metadata_is_invalid_even_when_the_height_agrees() {
    let mut value = value();
    value.proposal_header.block_height = 0.into();
    assert!(matches!(
        block_lookup(&0.into(), BlockLookupResult::BlockFound { value }),
        Err(RpcServerError::BackendDataInvalid {
            reason: BackendDataInvalidReason::InconsistentResult,
            ..
        })
    ));
}

#[test]
fn unsupported_consensus_extension_cannot_decode_as_a_valid_base_value() {
    let mut bytes = xdrs::txs::encode(&value()).unwrap();
    // Protocol is the first u32; the next u32 is ConsensusValue.ext.
    bytes[4..8].copy_from_slice(&1u32.to_be_bytes());
    assert!(xdrs::txs::decode::<ConsensusValue>(&bytes).is_err());
}

#[test]
fn unencodable_committed_metadata_is_an_encoding_error() {
    let mut value = value();
    // An upgrade above the 128-byte XDR limit cannot be re-encoded for hashing.
    value.upgrades = xdr_common::VecM(vec![xdr_common::BytesM(vec![0; 129])]);
    assert!(matches!(
        block_lookup(&99.into(), BlockLookupResult::BlockFound { value }),
        Err(RpcServerError::BackendDataInvalid {
            reason: BackendDataInvalidReason::InvalidEncoding,
            ..
        })
    ));
}
