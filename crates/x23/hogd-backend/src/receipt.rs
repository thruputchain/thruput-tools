//! Map hogd's execution result and root-frame output onto receipt fields.

use groundhog_json_rpc_types::{
    hex::HexBytes,
    receipt::{DETERMINISTIC_ERROR_KIND_FAULT, ExecStatus},
    transaction::TxHash,
};
use groundhog_xdr::{
    exec_result::ExecResult,
    transaction::{InvocationFrameOutput, TransactionHash},
};

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

#[cfg(test)]
mod tests {
    use xdr_common::BytesM;

    use super::*;

    #[test]
    fn deterministic_exec_result_maps_to_the_shared_fault_kind() {
        match exec_status(ExecResult::ExecResultTxDeterministicError) {
            ExecStatus::DeterministicError { kind } => {
                assert_eq!(kind, DETERMINISTIC_ERROR_KIND_FAULT);
            }
            other => panic!("unexpected status: {other:?}"),
        }
    }

    #[test]
    fn known_exec_results_map_and_unknown_ones_fall_back_to_other() {
        assert!(matches!(
            exec_status(ExecResult::ExecResultOK),
            ExecStatus::Ok
        ));
        assert!(matches!(
            exec_status(ExecResult::ExecResultTxRootFrameReverted),
            ExecStatus::Reverted { message: None }
        ));
        assert!(matches!(
            exec_status(ExecResult::ExecResultTxNonDeterministicError),
            ExecStatus::NonDeterministicError
        ));
        assert!(matches!(
            exec_status(ExecResult::ExecResultFailedGasSponsorship),
            ExecStatus::FailedGasSponsorship
        ));
        assert!(matches!(exec_status(ExecResult(9999)), ExecStatus::Other));
    }

    #[test]
    fn root_output_yields_at_most_one_buffer() {
        let (ret, abort) = root_output_buffers(InvocationFrameOutput::Return {
            return_buffer: BytesM(vec![1, 2, 3]),
        });
        assert_eq!(ret.map(|data| data.0), Some(vec![1, 2, 3]));
        assert!(abort.is_none());

        let (ret, abort) = root_output_buffers(InvocationFrameOutput::Abort {
            abort_buffer: BytesM(vec![4, 5]),
        });
        assert!(ret.is_none());
        assert_eq!(abort.map(|data| data.0), Some(vec![4, 5]));

        let (ret, abort) = root_output_buffers(InvocationFrameOutput::Fault);
        assert!(ret.is_none() && abort.is_none());
    }
}
