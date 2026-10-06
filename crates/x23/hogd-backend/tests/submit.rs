use std::time::Duration;
use std::time::Instant;

use groundhog_hogd_backend::submit::*;
use groundhog_scheduler::{ParallelBatch, parallel::ParallelSignedTransactionBatch};
use xdr_common::{BytesM, VecM};
use xdrs::{EncodesAs, cxs, txs};

#[test]
fn each_transaction_becomes_its_own_single_token_segment() {
    let first = vec![1u8; 3];
    let second = vec![2u8; 5];
    let batch = pack(vec![first.as_slice(), second.as_slice()]).expect("packs");

    assert_eq!(batch.segment_tokens.0, vec![1, 1]);
    assert_eq!(batch.segment_bytes.0, vec![3, 5]);
    assert_eq!(batch.xdr.0, vec![first.as_slice(), second.as_slice()]);
}

#[test]
fn an_empty_batch_packs_to_no_segments() {
    let batch = pack(Vec::<&[u8]>::new()).expect("packs");

    assert!(batch.segment_tokens.0.is_empty());
    assert!(batch.xdr.0.is_empty());
}

// The scheduler client takes its argument as `EncodesAs` the wire batch,
// so the batch must encode exactly as the owned `ParallelSignedTransactionBatch`
// would. Lengths that are not multiples of 4 check that the payload is
// padded once, after the last transaction.
#[test]
fn a_submit_batch_encodes_as_the_wire_batch() {
    fn encode_arg(batch: &(impl EncodesAs<ParallelSignedTransactionBatch> + ?Sized)) -> Vec<u8> {
        txs::encode(batch).expect("encodes")
    }

    let transactions = vec![vec![1u8; 3], vec![], vec![2; 5], vec![3; 8]];
    let batch = pack(transactions.clone()).expect("packs");
    let wire = ParallelSignedTransactionBatch::new(ParallelBatch {
        segment_tokens: VecM(vec![1; 4]),
        segment_bytes: VecM(vec![3, 0, 5, 8]),
        xdr: BytesM(transactions.concat()),
    });

    let encoded = encode_arg(&batch);
    assert_eq!(encoded, txs::encode(&wire).expect("encodes"));
    assert_eq!(
        cxs::encode(&batch).expect("encodes"),
        cxs::encode(&wire).expect("encodes")
    );
    assert_eq!(
        txs::decode::<ParallelSignedTransactionBatch>(&encoded).expect("decodes"),
        wire
    );
}

#[test]
fn cohorts_are_ordered_and_bounded_by_their_earliest_deadline() {
    let start = Instant::now();
    let skew = Duration::from_secs(1);
    let deadlines = [
        start + Duration::from_secs(10),
        start + Duration::from_millis(500),
        start + Duration::from_millis(900),
        start,
    ];

    let cohorts = deadline_cohorts(deadlines.to_vec(), skew, |at| *at);

    assert_eq!(cohorts.len(), 2);
    assert_eq!(cohorts[0], vec![deadlines[3], deadlines[1], deadlines[2]]);
    assert_eq!(cohorts[1], vec![deadlines[0]]);
    for cohort in &cohorts {
        let earliest = cohort[0];
        assert!(
            cohort
                .iter()
                .all(|at| at.saturating_duration_since(earliest) <= skew)
        );
    }
}
