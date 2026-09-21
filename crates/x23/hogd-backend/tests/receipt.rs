use groundhog_json_rpc_types::receipt::DETERMINISTIC_ERROR_KIND_FAULT;
use groundhog_json_rpc_types::receipt::ExecStatus;
use groundhog_xdr::exec_result::ExecResult;
use groundhog_xdr::transaction::InvocationFrameOutput;

use xdr_common::BytesM;

use groundhog_hogd_backend::receipt::*;

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
