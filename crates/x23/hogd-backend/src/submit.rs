//! Pack client submissions into one scheduler batch, and group them by
//! deadline first so one batch's timeout is honest for every caller in it.

use std::time::{Duration, Instant};

use groundhog_json_rpc_server::RpcServerError;
use groundhog_scheduler::{ParallelBatch, parallel::ParallelSignedTransactionBatch};
use xdr_common::{BytesM, VecM};

/// Pack transaction blobs into one `ParallelSignedTransactionBatch`.
///
/// Each transaction becomes its own single-token segment, so the scheduler
/// admits or rejects them one by one and the outcome batch lines up with the
/// input order. A transaction wider than `u32` cannot be described by the
/// segment table, so it is rejected as invalid rather than truncated.
pub fn pack<'a, I>(transactions: I) -> Result<ParallelSignedTransactionBatch, RpcServerError>
where
    I: IntoIterator<Item = &'a [u8]>,
    I::IntoIter: ExactSizeIterator + Clone,
{
    let transactions = transactions.into_iter();
    let count = transactions.len();
    let mut segment_tokens = Vec::with_capacity(count);
    let mut segment_bytes = Vec::with_capacity(count);
    // One pass to size the payload buffer; submission is hot enough that
    // growing it mid-copy is worth the extra walk over the lengths.
    let mut xdr = Vec::with_capacity(transactions.clone().map(<[u8]>::len).sum());
    for tx_bytes in transactions {
        let tx_len = u32::try_from(tx_bytes.len())
            .map_err(|_| RpcServerError::TxInvalid("transaction too large".into()))?;
        segment_tokens.push(1);
        segment_bytes.push(tx_len);
        xdr.extend_from_slice(tx_bytes);
    }
    Ok(ParallelSignedTransactionBatch::new(ParallelBatch {
        segment_tokens: VecM(segment_tokens),
        segment_bytes: VecM(segment_bytes),
        xdr: BytesM(xdr),
    }))
}

/// Group items into cohorts whose deadlines are within `max_skew` of the
/// cohort's earliest.
///
/// One backend call can carry only one deadline, so a batch has to run to
/// its most impatient member's. Sorting first, then cutting a new cohort
/// whenever an item is more than `max_skew` past its cohort's earliest,
/// keeps a long-deadline caller from being cut short by a short-deadline
/// one while still batching everything that can share a deadline. Cohorts
/// come back in ascending deadline order, so `cohort[0]`'s deadline bounds
/// the whole cohort.
pub fn deadline_cohorts<I>(
    mut items: Vec<I>,
    max_skew: Duration,
    deadline: impl Fn(&I) -> Instant,
) -> Vec<Vec<I>> {
    items.sort_unstable_by_key(&deadline);
    let mut cohorts: Vec<Vec<I>> = Vec::new();
    for item in items {
        let item_deadline = deadline(&item);
        let joins_current = cohorts.last().is_some_and(|cohort| {
            item_deadline.saturating_duration_since(deadline(&cohort[0])) <= max_skew
        });
        if joins_current {
            cohorts
                .last_mut()
                .expect("bug: current cohort should exist")
                .push(item);
        } else {
            cohorts.push(vec![item]);
        }
    }
    cohorts
}
