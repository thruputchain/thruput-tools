//! Native transaction observations, decoded before legacy projections lose data.

use thruput_indexer::indexer::{TxInfoOutcome, TxInfoResult};
use thruput_json_rpc_server::native::transaction::NativeReadError;
use thruput_json_rpc_types::{
    BlockHeight,
    error::native::{BackendDataInvalidReason as Reason, ReceiptObservation},
    hex::HexBytes,
    responses as out,
    transaction::TxHash,
};
use thruput_node_rpc::{
    SchedulerTransactionErrorSource as Source, SchedulerTransactionOutcomeBatch,
    SchedulerTransactionStatus as Status,
};
use thruput_xdr::{event::TxEventLog, exec_result::ExecResult, transaction::InvocationFrameOutput};

use super::invalid;
use crate::receipt::tx_hash_to_wire;

mod diagnostic;
mod input;
mod status;
pub use input::input_lookup;
pub use status::sequence_status;

fn valid_height(height: &BlockHeight) -> Result<(), NativeReadError> {
    // Unlike a tip or state snapshot, a retained transaction must have a real
    // block identity. The pre-genesis -1 sentinel cannot describe commitment.
    if **height < 0 {
        return Err(invalid(Reason::InconsistentResult, "negative transaction height").into());
    }
    Ok(())
}

fn identity(
    tx_hash: TxHash,
    height: &BlockHeight,
    log: &TxEventLog,
) -> Result<(), NativeReadError> {
    if log.tx_hash != tx_hash_to_wire(tx_hash) {
        return Err(invalid(
            Reason::InconsistentResult,
            "receipt transaction hash mismatch",
        )
        .into());
    }
    valid_height(height)
}

fn execution(
    result: ExecResult,
    output: InvocationFrameOutput,
) -> Result<out::ReceiptExecution, NativeReadError> {
    use out::ReceiptExecution as Execution;
    let value = match (result, output) {
        (ExecResult::ExecResultOK, InvocationFrameOutput::Return { return_buffer }) => {
            Execution::Return {
                return_data: HexBytes(return_buffer.0),
            }
        }
        (
            ExecResult::ExecResultTxRootFrameReverted,
            InvocationFrameOutput::Abort { abort_buffer },
        ) => Execution::Abort {
            abort_data: HexBytes(abort_buffer.0),
        },
        (ExecResult::ExecResultTxDeterministicError, InvocationFrameOutput::Fault) => {
            Execution::Fault {}
        }
        (
            ExecResult::ExecResultOK
            | ExecResult::ExecResultTxRootFrameReverted
            | ExecResult::ExecResultTxDeterministicError,
            _,
        ) => {
            return Err(invalid(
                Reason::InconsistentResult,
                "execution result contradicts root output",
            )
            .into());
        }
        // Invocation output precedes commitment. A return/abort buffer does
        // not turn a later commit failure into successful execution.
        (
            result,
            InvocationFrameOutput::Return { .. }
            | InvocationFrameOutput::Abort { .. }
            | InvocationFrameOutput::Fault,
        ) => {
            if result == ExecResult::ExecResultTxNonDeterministicError {
                Execution::NonDeterministicError {}
            } else if result == ExecResult::ExecResultFailedGasSponsorship {
                Execution::FailedGasSponsorship {}
            } else {
                // The receipt contract deliberately preserves unrepresented
                // execution results as Other, including future ExecResult values.
                Execution::Other {}
            }
        }
        _ => {
            return Err(invalid(
                Reason::UnsupportedValue,
                "unsupported root invocation output",
            )
            .into());
        }
    };
    Ok(value)
}

/// Decode an entire committed receipt. Only validated hash/height evidence
/// survives malformed events or inconsistent execution; no partial receipt does.
pub fn receipt(
    tx_hash: TxHash,
    height: BlockHeight,
    log: TxEventLog,
) -> Result<out::TransactionReceipt, NativeReadError> {
    identity(tx_hash, &height, &log)?;
    let observation = ReceiptObservation {
        transaction_hash: tx_hash,
        block_height: height.clone(),
    };
    let decode = || {
        if log.invoked_addresses.0.len()
            > thruput_xdr::event::kMaxInvokedAddressesPerTransaction as usize
            || log
                .invoked_addresses
                .0
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(invalid(Reason::InvalidEncoding, "noncanonical invoked addresses").into());
        }
        let execution = execution(log.exec_result, log.root_output)?;
        let parsed = crate::event::parse_emitted_events(log.events, Some(tx_hash));
        if parsed.dropped != 0 {
            return Err(invalid(Reason::InvalidEncoding, "malformed receipt event").into());
        }
        let events = parsed
            .events
            .into_iter()
            .map(|event| out::EmittedEvent {
                address: event.contract_address,
                topics: event.topics,
                data: event.data,
            })
            .collect();
        Ok(out::TransactionReceipt {
            transaction_hash: tx_hash,
            block_height: height,
            execution,
            invoked_addresses: log
                .invoked_addresses
                .0
                .into_iter()
                .map(Into::into)
                .collect(),
            events,
            gas_used: log.gas_consumed,
        })
    };
    decode().map_err(|mut error: NativeReadError| {
        error.observation = Some(Box::new(observation));
        error
    })
}

/// Return commitment from validated identity without parsing receipt payloads.
/// Unknown outcomes and malformed advisory diagnostics never become misses.
pub fn indexed_status(
    tx_hash: TxHash,
    result: TxInfoResult,
) -> Result<Option<out::TransactionStatus>, NativeReadError> {
    match result {
        TxInfoResult::TxInfoFound { info } => {
            identity(tx_hash, &info.height, &info.events)?;
            Ok(Some(out::TransactionStatus::Committed {
                block_height: info.height,
            }))
        }
        TxInfoResult::TxInfoRecentFailure { recent_failure } => {
            valid_height(&recent_failure.attempt_height)?;
            Ok(Some(out::TransactionStatus::RecentFailure {
                attempt_block_height: recent_failure.attempt_height,
                cause: diagnostic::cause(recent_failure.cause)?,
            }))
        }
        TxInfoResult::Default { outcome } if outcome == TxInfoOutcome::TxInfoNotFound.0 => Ok(None),
        _ => Err(invalid(
            Reason::UnsupportedValue,
            "unsupported indexer transaction outcome",
        )
        .into()),
    }
}

/// Only a recognized successful lookup without a committed record becomes null.
pub fn receipt_lookup(
    tx_hash: TxHash,
    result: TxInfoResult,
) -> Result<Option<out::TransactionReceipt>, NativeReadError> {
    match result {
        TxInfoResult::TxInfoFound { info } => receipt(tx_hash, info.height, info.events).map(Some),
        other => {
            indexed_status(tx_hash, other)?;
            Ok(None)
        }
    }
}

/// Validate the one requested hash's scheduler answer. Error codes remain raw
/// numeric subsystem diagnostics; unknown status/source tags are not guessed.
pub fn scheduler_status(
    results: &SchedulerTransactionOutcomeBatch,
) -> Result<Option<out::TransactionStatus>, NativeReadError> {
    let [value] = results.0.as_slice() else {
        return Err(invalid(
            Reason::InvalidCardinality,
            "scheduler lookup must return exactly one result",
        )
        .into());
    };
    Ok(Some(match value.status {
        Status::SchedulerTransactionNotFound => return Ok(None),
        Status::SchedulerTransactionPending => out::TransactionStatus::Pending {
            recent_failure: None,
        },
        Status::SchedulerTransactionSucceeded => {
            valid_height(&value.height)?;
            out::TransactionStatus::Committed {
                block_height: value.height.clone(),
            }
        }
        Status::SchedulerTransactionRetry | Status::SchedulerTransactionDropped => {
            let source = match value.error_source {
                Source::SchedulerTransactionErrorAuth => out::SchedulerErrorSource::Auth,
                Source::SchedulerTransactionErrorScheduler => out::SchedulerErrorSource::Scheduler,
                Source::SchedulerTransactionErrorExecutor => out::SchedulerErrorSource::Executor,
                _ => {
                    return Err(invalid(
                        Reason::UnsupportedValue,
                        "unsupported scheduler diagnostic source",
                    )
                    .into());
                }
            };
            let error = out::SchedulerError {
                source,
                code: value.error_code,
            };
            if value.status == Status::SchedulerTransactionRetry {
                out::TransactionStatus::Retry {
                    error,
                    recent_failure: None,
                }
            } else {
                out::TransactionStatus::Dropped {
                    error,
                    recent_failure: None,
                }
            }
        }
        _ => {
            return Err(invalid(
                Reason::UnsupportedValue,
                "unsupported scheduler transaction status",
            )
            .into());
        }
    }))
}

#[cfg(test)]
mod tests;
