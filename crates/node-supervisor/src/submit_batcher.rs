//! Bounded internal batching for local-node transaction submission.

use std::{
    fmt,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use groundhog_hogd_backend::scheduler as shared_scheduler;
use groundhog_hogd_backend::submit::{self as shared_submit, SubmitBatch};
use groundhog_json_rpc_server::{RpcRequestContext, RpcServerError};
use groundhog_scheduler::{
    SchedGatewaySubmitOutcomeBatch, sched_gateway::SchedGateway_SchedGateway1_Client,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::AbortHandle,
};

use crate::{
    block_driver::BlockDriverTrigger, request_deadline::within_request_deadline,
    service::SCHEDULER_GATEWAY,
};
use groundhog_x23_transport::HOGD_LARGE_MESSAGE_SIZE;

const MAX_BATCH_SIZE: usize = 1_000;
const INGRESS_QUEUE_DEPTH: usize = 10_000;
const XDR_ENVELOPE_HEADROOM: usize = 64 * 1024;
const MAX_BATCH_BYTES: usize = HOGD_LARGE_MESSAGE_SIZE as usize - XDR_ENVELOPE_HEADROOM;
const INGRESS_QUEUE_BYTES: usize = HOGD_LARGE_MESSAGE_SIZE as usize;
const BACKEND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DEADLINE_SKEW: Duration = Duration::from_secs(1);

#[async_trait]
trait SubmitSink: Send + Sync + 'static {
    async fn submit(
        &self,
        batch: SubmitBatch<Vec<u8>>,
    ) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError>;
}

struct SchedulerSink {
    scheduler: SchedGateway_SchedGateway1_Client,
}

#[async_trait]
impl SubmitSink for SchedulerSink {
    async fn submit(
        &self,
        batch: SubmitBatch<Vec<u8>>,
    ) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError> {
        self.scheduler
            .schedgateway_submit_tx_batch(&batch)
            .await
            .map_err(|error| {
                RpcServerError::BackendUnavailable(format!(
                    "{SCHEDULER_GATEWAY} submit outcome is indeterminate: {error}"
                ))
            })
    }
}

struct Item {
    tx_bytes: Vec<u8>,
    backend_deadline: Instant,
    reply: oneshot::Sender<Result<(), RpcServerError>>,
    _ingress_bytes: OwnedSemaphorePermit,
}

struct Inner {
    ingress: mpsc::Sender<Item>,
    ingress_bytes: Arc<Semaphore>,
    block_driver: Arc<RwLock<Option<BlockDriverTrigger>>>,
    worker: AbortHandle,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

/// Cloneable handle that combines concurrent public submissions into scheduler batches.
#[derive(Clone)]
pub(crate) struct SubmitBatcher {
    inner: Arc<Inner>,
}

impl fmt::Debug for SubmitBatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubmitBatcher").finish_non_exhaustive()
    }
}

impl SubmitBatcher {
    /// Start a bounded submission worker on the current Tokio runtime.
    ///
    /// Clones share one queue and worker, with at most one scheduler batch in flight.
    /// Dropping the last handle aborts the worker rather than draining queued work.
    pub(crate) fn spawn(scheduler: SchedGateway_SchedGateway1_Client) -> Self {
        Self::spawn_with(
            Arc::new(SchedulerSink { scheduler }),
            MAX_BATCH_SIZE,
            MAX_BATCH_BYTES,
            INGRESS_QUEUE_DEPTH,
            INGRESS_QUEUE_BYTES,
        )
    }

    fn spawn_with(
        sink: Arc<dyn SubmitSink>,
        max_batch_size: usize,
        max_batch_bytes: usize,
        ingress_queue_depth: usize,
        ingress_queue_bytes: usize,
    ) -> Self {
        let (ingress_tx, ingress_rx) = mpsc::channel(ingress_queue_depth);
        let ingress_bytes = Arc::new(Semaphore::new(ingress_queue_bytes));
        let block_driver = Arc::new(RwLock::new(None));
        let worker = tokio::spawn(run(
            ingress_rx,
            sink,
            max_batch_size,
            max_batch_bytes,
            block_driver.clone(),
        ))
        .abort_handle();
        Self {
            inner: Arc::new(Inner {
                ingress: ingress_tx,
                ingress_bytes,
                block_driver,
                worker,
            }),
        }
    }

    /// Queue validated transaction bytes and await their individual admission result.
    ///
    /// Exhausted queue capacity or payload-byte budget returns `Overloaded` immediately.
    /// The backend deadline is capped at 30 seconds from request start, or the earlier
    /// request deadline. Canceled callers are skipped before packing where possible;
    /// canceling this future does not cancel a scheduler batch already in flight.
    /// Success confirms individual admission, not transaction commitment.
    #[tracing::instrument(skip_all, fields(tx_bytes_len = tx_bytes.len()))]
    pub(crate) async fn submit(
        &self,
        context: RpcRequestContext,
        tx_bytes: Vec<u8>,
    ) -> Result<(), RpcServerError> {
        if tx_bytes.len() > MAX_BATCH_BYTES {
            return Err(RpcServerError::TxInvalid("transaction too large".into()));
        }
        let backend_deadline = context
            .capped_from_start(BACKEND_TIMEOUT)
            .deadline()
            .ok_or_else(|| {
                RpcServerError::BackendUnavailable(
                    "dev-node submit deadline could not be calculated".into(),
                )
            })?;
        if backend_deadline <= Instant::now() {
            return Err(RpcServerError::BackendTimeout);
        }
        let byte_count = u32::try_from(tx_bytes.len())
            .map_err(|_| RpcServerError::TxInvalid("transaction too large".into()))?;
        let ingress_bytes = self
            .inner
            .ingress_bytes
            .clone()
            .try_acquire_many_owned(byte_count)
            .map_err(|_| RpcServerError::Overloaded("dev-node submit queue full".into()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.inner
            .ingress
            .try_send(Item {
                tx_bytes,
                backend_deadline,
                reply: reply_tx,
                _ingress_bytes: ingress_bytes,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    RpcServerError::Overloaded("dev-node submit queue full".into())
                }
                mpsc::error::TrySendError::Closed(_) => {
                    RpcServerError::BackendUnavailable("dev-node submit batcher stopped".into())
                }
            })?;

        within_request_deadline(context, async {
            reply_rx.await.map_err(|_| {
                RpcServerError::BackendUnavailable("dev-node submit batcher stopped".into())
            })?
        })
        .await
    }

    /// Install or replace the block-driver trigger shared by all batcher clones.
    ///
    /// The worker invokes the current trigger once after each scheduler batch with at least one admitted transaction.
    pub(crate) fn set_block_driver(&self, block_driver: BlockDriverTrigger) {
        *self
            .inner
            .block_driver
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(block_driver);
    }

    #[cfg(test)]
    fn abort(&self) {
        self.inner.worker.abort();
    }
}

async fn run(
    mut ingress: mpsc::Receiver<Item>,
    sink: Arc<dyn SubmitSink>,
    max_batch_size: usize,
    max_batch_bytes: usize,
    block_driver: Arc<RwLock<Option<BlockDriverTrigger>>>,
) {
    let mut deferred = None;
    loop {
        let first = if let Some(item) = deferred.take() {
            item
        } else {
            let Some(item) = ingress.recv().await else {
                return;
            };
            item
        };
        let mut items = Vec::with_capacity(max_batch_size.min(64));
        let mut batch_bytes = first.tx_bytes.len();
        items.push(first);
        while items.len() < max_batch_size {
            let Ok(item) = ingress.try_recv() else {
                break;
            };
            if item.reply.is_closed() {
                continue;
            }
            if item.backend_deadline <= Instant::now() {
                let _ = item.reply.send(Err(RpcServerError::BackendTimeout));
                continue;
            }
            let Some(next_bytes) = batch_bytes.checked_add(item.tx_bytes.len()) else {
                deferred = Some(item);
                break;
            };
            if next_bytes > max_batch_bytes {
                deferred = Some(item);
                break;
            }
            batch_bytes = next_bytes;
            items.push(item);
        }

        let live = live_items(items, Instant::now());
        if live.is_empty() {
            continue;
        }

        for cohort in
            shared_submit::deadline_cohorts(live, MAX_DEADLINE_SKEW, |item| item.backend_deadline)
        {
            let mut cohort = live_items(cohort, Instant::now());
            if cohort.is_empty() {
                continue;
            }
            // Cohorts come back in ascending deadline order, so the first
            // item's deadline is the tightest one this batch must honor.
            let deadline = cohort[0].backend_deadline;
            tracing::debug!(
                batch_size = cohort.len(),
                "submitting dev-node transaction batch"
            );
            // Move the bytes into the batch; the items keep their replies.
            let transactions = cohort
                .iter_mut()
                .map(|item| std::mem::take(&mut item.tx_bytes))
                .collect();
            let result = match shared_submit::pack(transactions) {
                Ok(batch) => submit_before(&sink, batch, deadline).await,
                Err(error) => Err(error),
            };
            let results = match result {
                Ok(outcomes) if outcomes.0.len() == cohort.len() => outcomes
                    .0
                    .into_iter()
                    .map(shared_scheduler::submit_result)
                    .collect::<Vec<_>>(),
                Ok(outcomes) => vec![
                    Err(RpcServerError::BackendUnavailable(format!(
                        "scheduler gateway returned {} admission outcomes for {} transactions",
                        outcomes.0.len(),
                        cohort.len()
                    )));
                    cohort.len()
                ],
                Err(error) => vec![Err(error); cohort.len()],
            };
            if results.iter().any(Result::is_ok)
                && let Some(trigger) = block_driver
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            {
                trigger.trigger_after_submit();
            }
            for (item, result) in cohort.into_iter().zip(results) {
                let _ = item.reply.send(result);
            }
        }
    }
}

async fn submit_before(
    sink: &Arc<dyn SubmitSink>,
    batch: SubmitBatch<Vec<u8>>,
    deadline: Instant,
) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError> {
    let submit = sink.submit(batch);
    tokio::time::timeout_at(deadline.into(), submit)
        .await
        .map_err(|_| RpcServerError::BackendTimeout)?
}

fn live_items(items: Vec<Item>, now: Instant) -> Vec<Item> {
    items
        .into_iter()
        .filter_map(|item| {
            if item.reply.is_closed() {
                return None;
            }
            if item.backend_deadline <= now {
                let _ = item.reply.send(Err(RpcServerError::BackendTimeout));
                return None;
            }
            Some(item)
        })
        .collect()
}

#[cfg(test)]
mod tests;
