use thruput_json_rpc_types::hex::Hex0x;
use thruput_json_rpc_types::receipt::DETERMINISTIC_ERROR_KIND_FAULT;
use thruput_json_rpc_types::receipt::ExecStatus;
use thruput_json_rpc_types::transaction::TxHash;
use thruput_xdr::event::{TxEmittedEvent, TxEventLog};
use thruput_xdr::exec_result::ExecResult;
use thruput_xdr::transaction::InvocationFrameOutput;

use xdr_common::{BytesM, VecM};

use thruput_hogd_backend::receipt::*;

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

#[test]
fn a_committed_record_becomes_a_receipt_with_its_malformed_events_counted() {
    let tx_hash = TxHash(Hex0x([0x22; 32].into()));
    let good = TxEmittedEvent {
        contract_address: [7; 32].into(),
        num_topics: 1,
        serialized_event: BytesM([[0xaa; 32].as_slice(), &[1, 2]].concat()),
    };
    let malformed = TxEmittedEvent {
        contract_address: [7; 32].into(),
        num_topics: 2,
        serialized_event: BytesM(vec![0; 40]),
    };
    let decoded = transaction_receipt(
        tx_hash,
        9.into(),
        TxEventLog {
            tx_hash: [0x22; 32].into(),
            exec_result: ExecResult::ExecResultTxRootFrameReverted,
            root_output: InvocationFrameOutput::Abort {
                abort_buffer: BytesM(vec![4, 5]),
            },
            gas_consumed: 77,
            events: VecM(vec![good, malformed]),
        },
    );

    assert_eq!(decoded.dropped_events, 1);
    let receipt = decoded.receipt;
    assert_eq!(receipt.tx_hash, tx_hash);
    assert_eq!(receipt.height, 9.into());
    assert!(matches!(
        receipt.status,
        ExecStatus::Reverted { message: None }
    ));
    assert_eq!(receipt.events.len(), 1);
    assert_eq!(receipt.events[0].data.0, vec![1, 2]);
    assert!(receipt.return_data.is_none());
    assert_eq!(receipt.abort_data.map(|data| data.0), Some(vec![4, 5]));
    assert_eq!(receipt.gas_used, 77);
}
