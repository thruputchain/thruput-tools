//! Map hogd's execution result and root-frame output onto receipt fields.

use groundhog_json_rpc_types::{
    BlockHeight,
    hex::HexBytes,
    receipt::{DETERMINISTIC_ERROR_KIND_FAULT, ExecStatus, TransactionReceipt},
    transaction::TxHash,
};
use groundhog_xdr::{
    event::TxEventLog,
    exec_result::ExecResult,
    transaction::{InvocationFrameOutput, TransactionHash},
};

use crate::event::parse_emitted_events;

/// A public receipt decoded from a committed record, with the count of
/// malformed events dropped so the caller can meter or log them.
#[derive(Debug)]
pub struct DecodedReceipt {
    /// The receipt to return to the client.
    pub receipt: TransactionReceipt,
    /// Events dropped as malformed; they are absent from `receipt.events`.
    pub dropped_events: usize,
}

/// Build the public receipt for a transaction committed at `height`.
///
/// A malformed event is dropped rather than failing the receipt, so one bad
/// record cannot hide a committed transaction's outcome.
pub fn transaction_receipt(
    tx_hash: TxHash,
    height: BlockHeight,
    log: TxEventLog,
) -> DecodedReceipt {
    let parsed = parse_emitted_events(log.events);
    let (return_data, abort_data) = root_output_buffers(log.root_output);
    DecodedReceipt {
        receipt: TransactionReceipt {
            tx_hash,
            height,
            status: exec_status(log.exec_result),
            events: parsed.events,
            return_data,
            abort_data,
            gas_used: log.gas_consumed,
        },
        dropped_events: parsed.dropped,
    }
}

/// Map hogd's `ExecResult` int enum onto the public `ExecStatus`.
///
/// `ExecResult` carries no sub-type for deterministic errors, so every one
/// reports the shared fault kind until the indexer exposes finer-grained
/// codes (for example `out_of_gas` or `stack_overflow`). Unknown
/// discriminants from a newer protocol map to `Other` rather than failing
/// the read.
pub fn exec_status(result: ExecResult) -> ExecStatus {
    if result == ExecResult::ExecResultOK {
        ExecStatus::Ok
    } else if result == ExecResult::ExecResultTxRootFrameReverted {
        ExecStatus::Reverted { message: None }
    } else if result == ExecResult::ExecResultTxDeterministicError {
        ExecStatus::DeterministicError {
            kind: DETERMINISTIC_ERROR_KIND_FAULT.into(),
        }
    } else if result == ExecResult::ExecResultTxNonDeterministicError {
        ExecStatus::NonDeterministicError
    } else if result == ExecResult::ExecResultFailedGasSponsorship {
        ExecStatus::FailedGasSponsorship
    } else {
        ExecStatus::Other
    }
}

/// Split a root invocation frame's output into a receipt's `(return, abort)`
/// data.
///
/// At most one is `Some`. A faulted or unrecognized frame yields neither,
/// which is how a receipt reports "the transaction produced no buffer".
pub fn root_output_buffers(output: InvocationFrameOutput) -> (Option<HexBytes>, Option<HexBytes>) {
    match output {
        InvocationFrameOutput::Return { return_buffer } => (Some(HexBytes(return_buffer.0)), None),
        InvocationFrameOutput::Abort { abort_buffer } => (None, Some(HexBytes(abort_buffer.0))),
        _ => (None, None),
    }
}

/// Convert the RPC-layer transaction hash to its wire form.
pub fn tx_hash_to_wire(tx_hash: TxHash) -> TransactionHash {
    TransactionHash::from(tx_hash.0.0.into_inner())
}
