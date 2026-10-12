use thruput_json_rpc_server::RpcServerError;
use thruput_scheduler::ExpectedDBTip;
use thruput_scheduler::RPCError;
use thruput_scheduler::SchedGatewaySubmitOutcome;

use thruput_json_rpc_types::transaction::{SchedulerTransactionErrorSource, TransactionStatus};
use thruput_scheduler::{
    SchedulerTransactionErrorSource as Source, SchedulerTransactionOutcome,
    SchedulerTransactionStatus as Status,
};
use xdr_common::VecM;

use thruput_hogd_backend::scheduler::*;

#[test]
fn admission_outcomes_separate_client_faults_from_backend_faults() {
    assert!(submit_result(SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted).is_ok());
    assert!(matches!(
        submit_result(SchedGatewaySubmitOutcome::SchedGatewaySubmitAuthenticationRejected),
        Err(RpcServerError::TxRejected(_))
    ));
    assert!(matches!(
        submit_result(SchedGatewaySubmitOutcome::SchedGatewaySubmitRetryable),
        Err(RpcServerError::Overloaded(_))
    ));
    assert!(matches!(
        submit_result(SchedGatewaySubmitOutcome::SchedGatewaySubmitInternalError),
        Err(RpcServerError::BackendUnavailable(_))
    ));
    assert!(matches!(
        submit_result(SchedGatewaySubmitOutcome(4242)),
        Err(RpcServerError::BackendUnavailable(_))
    ));
}

fn outcome(status: Status, source: Source, code: i32) -> SchedulerTransactionOutcome {
    SchedulerTransactionOutcome {
        status,
        error_source: source,
        error_code: code,
        height: 42.into(),
    }
}

#[test]
fn scheduler_outcomes_preserve_processing_results() {
    let decode = |status| {
        transaction_outcome("scheduler", &VecM(vec![outcome(status, Source(999), 999)])).unwrap()
    };
    assert!(decode(Status::SchedulerTransactionNotFound).is_none());
    assert!(matches!(
        decode(Status::SchedulerTransactionPending),
        Some(TransactionStatus::Pending)
    ));
    assert!(matches!(
        decode(Status::SchedulerTransactionSucceeded),
        Some(TransactionStatus::Committed { height }) if height == 42.into()
    ));
    for (source, expected) in [
        (
            Source::SchedulerTransactionErrorAuth,
            SchedulerTransactionErrorSource::Auth,
        ),
        (
            Source::SchedulerTransactionErrorScheduler,
            SchedulerTransactionErrorSource::Scheduler,
        ),
        (
            Source::SchedulerTransactionErrorExecutor,
            SchedulerTransactionErrorSource::Executor,
        ),
    ] {
        for status in [
            Status::SchedulerTransactionRetry,
            Status::SchedulerTransactionDropped,
        ] {
            let decoded =
                transaction_outcome("scheduler", &VecM(vec![outcome(status, source, -7)]))
                    .unwrap()
                    .unwrap();
            let error = match decoded {
                TransactionStatus::Retry { error }
                    if status == Status::SchedulerTransactionRetry =>
                {
                    error
                }
                TransactionStatus::Dropped { error }
                    if status == Status::SchedulerTransactionDropped =>
                {
                    error
                }
                other => panic!("unexpected status: {other:?}"),
            };
            assert_eq!(error.source, expected);
            assert_eq!(error.code, -7);
        }
    }
}

#[test]
fn scheduler_commitment_heights_preserve_zero_and_wide_values() {
    for height in [0, 42, (1_i64 << 53) + 1, i64::MAX] {
        let mut result = outcome(
            Status::SchedulerTransactionSucceeded,
            Source::SchedulerTransactionErrorNone,
            0,
        );
        result.height = height.into();
        let status = transaction_outcome("scheduler", &VecM(vec![result]))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({"status": "committed", "height": height.to_string()})
        );
    }
}

#[test]
fn malformed_scheduler_outcomes_are_not_absence() {
    let absent = outcome(
        Status::SchedulerTransactionNotFound,
        Source::SchedulerTransactionErrorNone,
        0,
    );
    for batch in [
        VecM(vec![]),
        VecM(vec![absent.clone(), absent]),
        VecM(vec![outcome(Status(4242), Source(0), 0)]),
        VecM(vec![outcome(
            Status::SchedulerTransactionRetry,
            Source(4242),
            0,
        )]),
        VecM(vec![outcome(
            Status::SchedulerTransactionDropped,
            Source::SchedulerTransactionErrorNone,
            0,
        )]),
    ] {
        assert!(matches!(
            transaction_outcome("scheduler", &batch),
            Err(RpcServerError::BackendUnavailable(_))
        ));
    }
}

#[test]
fn a_missing_tip_is_decoded_rather_than_named() {
    assert!(matches!(
        tip_answer(
            "scheduler",
            ExpectedDBTip::Default {
                error: RPCError::kRPCNotFound.0,
            },
        ),
        Ok(TipAnswer::NoCommittedBlock)
    ));

    let error = tip_answer("scheduler", ExpectedDBTip::Default { error: 4242 }).unwrap_err();
    assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
}
