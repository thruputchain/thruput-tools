//! Decode `SchedGateway` results: submission outcomes, mempool lookups, and
//! the committed chain tip.

use groundhog_json_rpc_server::{RpcServerError, TipReading};
use groundhog_json_rpc_types::block::{ChainTip, consensus_value_hash};
use groundhog_scheduler::{
    DbTip, ExpectedDBTip, RPCError, SchedGatewaySubmitOutcome, consensus::ConsensusValue,
};
use groundhog_xdr::transaction::OptSignedTransactionBatch;

/// What the scheduler gateway answered when asked for the committed tip.
#[derive(Debug)]
pub enum TipAnswer {
    /// The scheduler's current committed tip and the network config it
    /// carries.
    Tip(Box<DbTip>),
    /// The chain has no committed block yet. Callers differ on whether that
    /// is a normal pre-genesis state or a startup-bootstrap failure, so the
    /// message is theirs to choose.
    NoCommittedBlock,
}

/// Map one per-transaction admission outcome onto a submit result.
///
/// Success means the transaction was admitted to the mempool — not that it
/// was nominated or included in a committed block.
pub fn submit_result(outcome: SchedGatewaySubmitOutcome) -> Result<(), RpcServerError> {
    match outcome {
        SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted => Ok(()),
        SchedGatewaySubmitOutcome::SchedGatewaySubmitAuthenticationRejected => {
            Err(RpcServerError::TxRejected(
                "scheduler rejected authentication for the signed transaction".into(),
            ))
        }
        SchedGatewaySubmitOutcome::SchedGatewaySubmitRetryable => Err(RpcServerError::Overloaded(
            "scheduler could not admit the transaction; retry with backoff".into(),
        )),
        SchedGatewaySubmitOutcome::SchedGatewaySubmitInternalError => Err(
            RpcServerError::BackendUnavailable("scheduler admission dependency failed".into()),
        ),
        unknown => Err(RpcServerError::BackendUnavailable(format!(
            "scheduler returned unknown submission outcome {}",
            unknown.0
        ))),
    }
}

/// Read a single-hash `schedgateway_lookup_tx` answer.
///
/// `Some(true)` means the mempool holds the transaction and `Some(false)`
/// that it does not. `None` means the scheduler did not answer with exactly
/// one per-hash slot, which leaves the queried transaction unaccounted for:
/// never read it as absent, because a status of `Unknown` claims the
/// transaction is in neither the mempool nor the indexer.
pub fn mempool_holds_tx(results: &OptSignedTransactionBatch) -> Option<bool> {
    if results.0.len() != 1 {
        return None;
    }
    results.iter().next().map(Option::is_some)
}

/// Compute the public chain tip for a consensus value.
pub fn chain_tip(service: &str, value: &ConsensusValue) -> Result<ChainTip, RpcServerError> {
    let block_hash = consensus_value_hash(value).map_err(|error| {
        RpcServerError::BackendUnavailable(format!(
            "{service} returned an unencodable consensus value: {error}"
        ))
    })?;
    Ok(ChainTip {
        height: value.proposal_header.block_height.clone(),
        block_hash,
    })
}

/// Build a tip reading — the chain tip plus the network config committed
/// with it — from a scheduler db tip.
pub fn tip_reading(service: &str, db_tip: DbTip) -> Result<TipReading, RpcServerError> {
    let DbTip {
        value,
        network_config,
        ..
    } = db_tip;
    Ok(TipReading::new(chain_tip(service, &value)?, network_config))
}

/// Decode a `schedgateway_get_tip` result.
///
/// A missing tip is reported as [`TipAnswer::NoCommittedBlock`] rather than
/// an error so the caller can phrase it; every other non-tip result is a
/// protocol disagreement and fails here.
pub fn tip_answer(service: &str, result: ExpectedDBTip) -> Result<TipAnswer, RpcServerError> {
    match result {
        ExpectedDBTip::kRPCNone { db_tip } => Ok(TipAnswer::Tip(Box::new(db_tip))),
        ExpectedDBTip::Default { error } if RPCError(error) == RPCError::kRPCNotFound => {
            Ok(TipAnswer::NoCommittedBlock)
        }
        other => Err(RpcServerError::BackendUnavailable(format!(
            "unexpected {service} tip result: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use groundhog_xdr::transaction::OptSignedTransaction;
    use xdr_common::VecM;

    use super::*;

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
}
