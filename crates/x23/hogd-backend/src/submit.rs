//! Pack client submissions into one scheduler batch, and group them by
//! deadline first so one batch's timeout is honest for every caller in it.

use std::time::{Duration, Instant};

use groundhog_json_rpc_server::RpcServerError;
use groundhog_scheduler::parallel::ParallelSignedTransactionBatch;
use xdr_common::VecM;
use xdrs::{ChunkedBytes, EncodesAs, WriteXDR};

/// Transaction blobs laid out as one scheduler submission batch.
///
/// It encodes as a [`ParallelSignedTransactionBatch`], but writes each
/// transaction from its own buffer instead of first copying them all into
/// one payload, so it goes straight to `schedgateway_submit_tx_batch`.
#[derive(Clone, Debug, PartialEq, Eq, WriteXDR)]
pub struct SubmitBatch<B: AsRef<[u8]>> {
    /// Tokens per segment; each transaction is a one-token segment.
    pub segment_tokens: VecM<u32>,
    /// Each segment's length in bytes.
    pub segment_bytes: VecM<u32>,
    /// The transactions, in segment order.
    pub xdr: ChunkedBytes<Vec<B>>,
}

// The fields are `ParallelBatch`'s, in its order, with `ChunkedBytes`
// standing in for the `BytesM` payload.
impl<B: AsRef<[u8]>> EncodesAs<ParallelSignedTransactionBatch> for SubmitBatch<B> {}

/// Lay out transaction blobs as one scheduler submission batch.
///
/// Each transaction becomes its own single-token segment, so the scheduler
/// admits or rejects them one by one and the outcome batch lines up with the
/// input order. A transaction wider than `u32` cannot be described by the
/// segment table, so it is rejected as invalid rather than truncated.
pub fn pack<B: AsRef<[u8]>>(transactions: Vec<B>) -> Result<SubmitBatch<B>, RpcServerError> {
    let segment_bytes = transactions
        .iter()
        .map(|tx_bytes| {
            u32::try_from(tx_bytes.as_ref().len())
                .map_err(|_| RpcServerError::TxInvalid("transaction too large".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SubmitBatch {
        segment_tokens: VecM(vec![1; transactions.len()]),
        segment_bytes: VecM(segment_bytes),
        xdr: ChunkedBytes(transactions),
    })
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
