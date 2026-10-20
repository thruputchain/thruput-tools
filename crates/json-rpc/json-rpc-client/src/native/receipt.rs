//! Read-only native receipt waiting. Commitment is sticky within the caller's
//! verified chain instance. A reset invalidates that context even if its network
//! hash is unchanged; the caller must cancel the wait and discard old evidence.

use std::{collections::hash_map::RandomState, future::Future, hash::BuildHasher, time::Duration};

use jsonrpsee::core::client::{ClientT, Error as TransportError};
use thruput_json_rpc_types::{
    BlockHeight, NetworkHash,
    error::native::{DecodedError, ErrorData},
    requests::TransactionQuery,
    responses::{DecodedResponse, ReceiptResult, TransactionReceipt, TransactionStatus},
    transaction::TxHash,
};
use tokio::time::Instant;

use super::{NativeClient, NativeClientError};

/// Only the two read operations needed by the waiter. No submission operation
/// is available, so timeout, cancellation and retries cannot repeat a write.
pub trait ReceiptSource {
    /// Read the committed receipt, including failed execution, or current absence.
    fn transaction_receipt(
        &self,
        hash: TxHash,
    ) -> impl Future<Output = Result<DecodedResponse<ReceiptResult>, NativeClientError>> + Send;

    /// Read an advisory status. Errors do not erase prior observations.
    fn transaction_status(
        &self,
        hash: TxHash,
    ) -> impl Future<Output = Result<DecodedResponse<TransactionStatus>, NativeClientError>> + Send;
}

impl<C: ClientT + Sync> ReceiptSource for NativeClient<C> {
    async fn transaction_receipt(
        &self,
        hash: TxHash,
    ) -> Result<DecodedResponse<ReceiptResult>, NativeClientError> {
        self.get_transaction_receipt(TransactionQuery {
            transaction_hash: hash,
        })
        .await
    }

    async fn transaction_status(
        &self,
        hash: TxHash,
    ) -> Result<DecodedResponse<TransactionStatus>, NativeClientError> {
        self.get_transaction_status(TransactionQuery {
            transaction_hash: hash,
        })
        .await
    }
}

/// Commitment supplied or observed in one caller-verified chain instance.
/// A network hash identifies the network, not its reset generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownCommitment {
    pub network_hash: NetworkHash,
    pub transaction_hash: TxHash,
    pub block_height: BlockHeight,
}

/// Receipt wait timing. The timeout includes requests, backoff and sleeps.
#[derive(Debug, Clone, Copy)]
pub struct ReceiptPollConfig {
    pub timeout: Duration,
    pub interval: Duration,
}

impl Default for ReceiptPollConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            interval: Duration::from_millis(250),
        }
    }
}

/// Strongest evidence retained when waiting stops without a supported receipt.
#[derive(Debug)]
pub struct ReceiptEvidence {
    pub network_hash: NetworkHash,
    pub transaction_hash: TxHash,
    pub commitment: Option<KnownCommitment>,
    /// Last meaningful status. Unknown and failed reads do not erase it;
    /// a commitment supersedes all scheduler diagnostics.
    pub last_status: Option<TransactionStatus>,
    /// Original most recent failed read, also retained on timeout/cancellation.
    pub last_lookup_error: Option<NativeClientError>,
}

/// Waiting stopped; the evidence never grants permission to submit again.
#[derive(Debug, thiserror::Error)]
#[error("{cause}")]
pub struct ReceiptPollError {
    pub evidence: Box<ReceiptEvidence>,
    #[source]
    pub cause: ReceiptPollCause,
}

/// Reason a native receipt wait stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReceiptPollCause {
    #[error("timed out waiting for transaction receipt")]
    Timeout,
    #[error("transaction receipt wait cancelled")]
    Cancelled,
    #[error("receipt lookup failed: {0}")]
    Lookup(#[source] NativeClientError),
    #[error("unsupported {method} result")]
    Unsupported {
        method: &'static str,
        raw: serde_json::Value,
    },
    #[error("invalid receipt wait data: {message}")]
    Invalid {
        message: &'static str,
        /// Preserve a conflicting/invalid result for diagnosis when available.
        raw: Option<serde_json::Value>,
    },
}

impl ReceiptEvidence {
    fn new(
        network_hash: NetworkHash,
        transaction_hash: TxHash,
        known_commitment: Option<KnownCommitment>,
    ) -> Result<Self, ReceiptPollError> {
        let mut evidence = Self {
            network_hash,
            transaction_hash,
            commitment: None,
            last_status: None,
            last_lookup_error: None,
        };
        if let Some(known) = known_commitment {
            let checked = if known.network_hash == network_hash {
                evidence.commit(known.transaction_hash, &known.block_height)
            } else {
                Err("commitment belongs to a different network")
            };
            if let Err(message) = checked {
                return Err(evidence.fail(ReceiptPollCause::Invalid { message, raw: None }));
            }
        }
        Ok(evidence)
    }

    fn fail(self, cause: ReceiptPollCause) -> ReceiptPollError {
        ReceiptPollError {
            evidence: Box::new(self),
            cause,
        }
    }

    fn commit(&mut self, hash: TxHash, height: &BlockHeight) -> Result<(), &'static str> {
        if hash != self.transaction_hash {
            return Err("transaction hash does not match the requested hash");
        }
        if **height < 0 {
            return Err("commitment height must be nonnegative");
        }
        if self
            .commitment
            .as_ref()
            .is_some_and(|known| known.block_height != *height)
        {
            return Err("commitment height conflicts with prior evidence");
        }
        self.commitment = Some(KnownCommitment {
            network_hash: self.network_hash,
            transaction_hash: hash,
            block_height: height.clone(),
        });
        self.last_status = Some(TransactionStatus::Committed {
            block_height: height.clone(),
        });
        Ok(())
    }
}

/// Wait for a supported committed receipt, including failed execution. This
/// helper never signs or submits. Pass `std::future::pending()` when cancellation
/// is unnecessary. Dropping the future also cancels reads but cannot return its
/// evidence; use `cancelled` when the caller needs an evidence-bearing error.
///
/// `network_hash` and the source must belong to the same verified chain instance.
/// Supplied commitment must match that context and the original transaction hash.
/// Retry/Dropped statuses never end a wait; even without a prior commitment they
/// do not prove nonexecution. Transient receipt failures use bounded jittered
/// backoff. Transient status failures retain evidence and continue to the next
/// receipt read. Unsupported or malformed status representations (including an
/// unfamiliar optional attempt diagnostic) stop with the original response
/// evidence; they never permit transaction resubmission.
#[tracing::instrument(skip_all)]
pub async fn poll_receipt<S, F>(
    source: &S,
    network_hash: NetworkHash,
    transaction_hash: TxHash,
    known_commitment: Option<KnownCommitment>,
    config: ReceiptPollConfig,
    cancelled: F,
) -> Result<TransactionReceipt, ReceiptPollError>
where
    S: ReceiptSource + Sync + ?Sized,
    F: Future<Output = ()>,
{
    let mut evidence = ReceiptEvidence::new(network_hash, transaction_hash, known_commitment)?;
    let Some(deadline) = Instant::now().checked_add(config.timeout) else {
        return Err(evidence.fail(ReceiptPollCause::Invalid {
            message: "timeout exceeds the clock range",
            raw: None,
        }));
    };
    if config.interval.is_zero() {
        return Err(evidence.fail(ReceiptPollCause::Invalid {
            message: "poll interval must be positive",
            raw: None,
        }));
    }
    tokio::pin!(cancelled);
    let mut failures = 0u32;
    loop {
        let receipt = tokio::select! {
            biased;
            () = &mut cancelled => return Err(evidence.fail(ReceiptPollCause::Cancelled)),
            () = tokio::time::sleep_until(deadline) => return Err(evidence.fail(ReceiptPollCause::Timeout)),
            result = source.transaction_receipt(transaction_hash) => result,
        };
        match receipt {
            Ok(
                result @ (DecodedResponse::Known(Some(_)) | DecodedResponse::Unsupported { .. }),
            ) => {
                return finish_receipt(&mut evidence, result).map_err(|cause| evidence.fail(cause));
            }
            Ok(DecodedResponse::Known(None)) => {
                let status = tokio::select! {
                    biased;
                    () = &mut cancelled => return Err(evidence.fail(ReceiptPollCause::Cancelled)),
                    () = tokio::time::sleep_until(deadline) => return Err(evidence.fail(ReceiptPollCause::Timeout)),
                    result = source.transaction_status(transaction_hash) => result,
                };
                match status {
                    Ok(status) => {
                        if let Err(cause) = observe_status(&mut evidence, status) {
                            return Err(evidence.fail(cause));
                        }
                        failures = 0;
                    }
                    Err(error) => {
                        evidence.last_lookup_error = Some(error);
                        failures = failures.saturating_add(1);
                    }
                }
            }
            Ok(_) => {
                return Err(evidence.fail(ReceiptPollCause::Invalid {
                    message: "unsupported response classification",
                    raw: None,
                }));
            }
            Err(error) => {
                if let Some((hash, height)) = receipt_observation(&error)
                    && let Err(message) = evidence.commit(hash, height)
                {
                    evidence.last_lookup_error = Some(error);
                    return Err(evidence.fail(ReceiptPollCause::Invalid { message, raw: None }));
                }
                if !transient(&error) {
                    return Err(evidence.fail(ReceiptPollCause::Lookup(error)));
                }
                evidence.last_lookup_error = Some(error);
                failures = failures.saturating_add(1);
            }
        }
        let pause = retry_pause(config.interval, failures)
            .min(deadline.saturating_duration_since(Instant::now()));
        tokio::select! {
            biased;
            () = &mut cancelled => return Err(evidence.fail(ReceiptPollCause::Cancelled)),
            () = tokio::time::sleep_until(deadline) => return Err(evidence.fail(ReceiptPollCause::Timeout)),
            () = tokio::time::sleep(pause) => {}
        }
    }
}

fn finish_receipt(
    evidence: &mut ReceiptEvidence,
    result: DecodedResponse<ReceiptResult>,
) -> Result<TransactionReceipt, ReceiptPollCause> {
    match result {
        DecodedResponse::Known(Some(receipt)) => {
            evidence
                .commit(receipt.transaction_hash, &receipt.block_height)
                .map_err(|message| ReceiptPollCause::Invalid {
                    message,
                    raw: serde_json::to_value(&receipt).ok(),
                })?;
            Ok(receipt)
        }
        DecodedResponse::Unsupported { raw } => {
            // The transport validated known fields before declaring support.
            // Re-read those fields to retain commitment with an unknown outcome.
            let checked = match serde_json::from_value::<TransactionReceipt>(raw.clone()) {
                Ok(receipt) => evidence.commit(receipt.transaction_hash, &receipt.block_height),
                Err(_) => Err("unsupported receipt has invalid identity or shape"),
            };
            if let Err(message) = checked {
                return Err(ReceiptPollCause::Invalid {
                    message,
                    raw: Some(raw),
                });
            }
            Err(ReceiptPollCause::Unsupported {
                method: "getTransactionReceipt",
                raw,
            })
        }
        _ => Err(ReceiptPollCause::Invalid {
            message: "expected a present receipt",
            raw: None,
        }),
    }
}

fn observe_status(
    evidence: &mut ReceiptEvidence,
    result: DecodedResponse<TransactionStatus>,
) -> Result<(), ReceiptPollCause> {
    match result {
        DecodedResponse::Known(status) => {
            if let TransactionStatus::Committed { block_height } = &status {
                evidence
                    .commit(evidence.transaction_hash, block_height)
                    .map_err(|message| ReceiptPollCause::Invalid {
                        message,
                        raw: serde_json::to_value(&status).ok(),
                    })?;
            } else if evidence.commitment.is_none()
                && !matches!(status, TransactionStatus::Unknown { .. })
            {
                evidence.last_status = Some(status);
            }
            Ok(())
        }
        DecodedResponse::Unsupported { raw } => Err(ReceiptPollCause::Unsupported {
            method: "getTransactionStatus",
            raw,
        }),
        _ => Err(ReceiptPollCause::Invalid {
            message: "unsupported response classification",
            raw: None,
        }),
    }
}

fn receipt_observation(error: &NativeClientError) -> Option<(TxHash, &BlockHeight)> {
    if let NativeClientError::Application { data, .. } = error
        && let DecodedError::Known { data, .. } = data.as_ref()
        && let ErrorData::BackendDataInvalid {
            observation: Some(observation),
            ..
        } = data.as_ref()
    {
        Some((observation.transaction_hash, &observation.block_height))
    } else {
        None
    }
}

fn transient(error: &NativeClientError) -> bool {
    match error {
        NativeClientError::Application { data, .. } => matches!(data.as_ref(),
            DecodedError::Known { data, .. } if matches!(data.as_ref(),
                ErrorData::BackendUnavailable { .. } | ErrorData::BackendTimeout { .. } | ErrorData::Overloaded { .. })),
        NativeClientError::Transport(
            TransportError::Transport(_) | TransportError::RequestTimeout,
        ) => true,
        _ => false,
    }
}

fn retry_pause(interval: Duration, failures: u32) -> Duration {
    if failures == 0 {
        return interval;
    }
    let cap = Duration::from_secs(2).max(interval);
    let base = interval.saturating_mul(1 << failures.min(8)).min(cap);
    // Standard-library randomized hashing supplies jitter without a new RNG
    // dependency. Timing is bounded to 50..100% of the capped backoff.
    let percent = u32::try_from(RandomState::new().hash_one(failures) % 51).unwrap_or(0) + 50;
    (base / 100).saturating_mul(percent).max(interval)
}

#[cfg(test)]
mod tests;
