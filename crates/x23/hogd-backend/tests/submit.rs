use std::time::Duration;
use std::time::Instant;

use groundhog_hogd_backend::submit::*;

#[test]
fn each_transaction_becomes_its_own_single_token_segment() {
    let first = vec![1u8; 3];
    let second = vec![2u8; 5];
    let batch = pack([first.as_slice(), second.as_slice()]).expect("packs");

    assert_eq!(batch.segment_tokens.0, vec![1, 1]);
    assert_eq!(batch.segment_bytes.0, vec![3, 5]);
    assert_eq!(batch.xdr.0, [first, second].concat());
}

#[test]
fn an_empty_batch_packs_to_no_segments() {
    let batch = pack(Vec::<&[u8]>::new()).expect("packs");

    assert!(batch.segment_tokens.0.is_empty());
    assert!(batch.xdr.0.is_empty());
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
