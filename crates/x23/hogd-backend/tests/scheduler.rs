use groundhog_json_rpc_server::RpcServerError;
use groundhog_scheduler::ExpectedDBTip;
use groundhog_scheduler::RPCError;
use groundhog_scheduler::SchedGatewaySubmitOutcome;

use groundhog_xdr::transaction::OptSignedTransaction;
use xdr_common::VecM;

use groundhog_hogd_backend::scheduler::*;

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

#[test]
fn a_lookup_slot_count_other_than_one_is_not_an_absence() {
    assert_eq!(
        mempool_holds_tx(&VecM(vec![OptSignedTransaction::None])),
        Some(false)
    );
    assert_eq!(mempool_holds_tx(&VecM(vec![])), None);
    assert_eq!(
        mempool_holds_tx(&VecM(vec![
            OptSignedTransaction::None,
            OptSignedTransaction::None,
        ])),
        None
    );
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
