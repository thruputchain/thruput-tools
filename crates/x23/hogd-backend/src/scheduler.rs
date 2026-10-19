//! Decode `SchedGateway` results: admission and processing outcomes, and
//! the committed chain tip.

use thruput_json_rpc_server::{RpcServerError, TipReading};
use thruput_json_rpc_types::block::{ChainTip, consensus_value_hash};
use thruput_scheduler::{
    DbTip, ExpectedDBTip, RPCError, SchedGatewaySubmitOutcome, consensus::ConsensusValue,
};

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

/// Decode a single-hash scheduler outcome lookup. Malformed cardinality or
/// unknown protocol values are backend failures, never an absent transaction.
pub fn transaction_outcome(
    service: &str,
    results: &thruput_scheduler::SchedulerTransactionOutcomeBatch,
) -> Result<Option<thruput_json_rpc_types::transaction::TransactionStatus>, RpcServerError> {
    use thruput_json_rpc_types::transaction::{
        SchedulerTransactionError, SchedulerTransactionErrorSource, TransactionStatus,
    };
    use thruput_scheduler::{
        SchedulerTransactionErrorSource as Source, SchedulerTransactionStatus as Status,
    };
    let [outcome] = results.0.as_slice() else {
        return Err(RpcServerError::BackendUnavailable(format!(
            "{service} returned {} lookup results for one transaction",
            results.0.len()
        )));
    };
    let status = match outcome.status {
        Status::SchedulerTransactionNotFound => return Ok(None),
        Status::SchedulerTransactionPending => TransactionStatus::Pending,
        Status::SchedulerTransactionSucceeded => TransactionStatus::Committed {
            height: outcome.height.clone(),
        },
        Status::SchedulerTransactionRetry | Status::SchedulerTransactionDropped => {
            let source = match outcome.error_source {
                Source::SchedulerTransactionErrorAuth => SchedulerTransactionErrorSource::Auth,
                Source::SchedulerTransactionErrorScheduler => {
                    SchedulerTransactionErrorSource::Scheduler
                }
                Source::SchedulerTransactionErrorExecutor => {
                    SchedulerTransactionErrorSource::Executor
                }
                other => {
                    return Err(RpcServerError::BackendUnavailable(format!(
                        "{service} returned unknown scheduler error source {}",
                        other.0
                    )));
                }
            };
            let error = SchedulerTransactionError::new(source, outcome.error_code);
            if outcome.status == Status::SchedulerTransactionRetry {
                TransactionStatus::Retry { error }
            } else {
                TransactionStatus::Dropped { error }
            }
        }
        other => {
            return Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned unknown transaction outcome {}",
                other.0
            )));
        }
    };
    Ok(Some(status))
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

/// Preserve per-attempt refusal evidence before the legacy cause mapping loses it.
/// Unknown outcomes do not establish either admission or rejection.
pub fn native_submit_result(
    outcome: SchedGatewaySubmitOutcome,
    attempt: &thruput_json_rpc_server::native::transaction::SubmissionAttempt,
) -> Result<(), RpcServerError> {
    match outcome {
        SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted => Ok(()),
        SchedGatewaySubmitOutcome::SchedGatewaySubmitAuthenticationRejected
        | SchedGatewaySubmitOutcome::SchedGatewaySubmitRetryable
        | SchedGatewaySubmitOutcome::SchedGatewaySubmitInternalError => {
            attempt.rejected();
            submit_result(outcome)
        }
        other => Err(RpcServerError::invalid_data(
            thruput_json_rpc_types::error::native::BackendDataInvalidReason::UnsupportedValue,
            format!("scheduler returned unknown submission outcome {}", other.0),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_json_rpc_server::native::transaction::SubmissionAttempt;
    use thruput_json_rpc_types::error::native::{AttemptAdmission, BackendDataInvalidReason};

    #[test]
    fn native_submit_outcomes_preserve_only_authoritative_refusal() {
        for outcome in [
            SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted,
            SchedGatewaySubmitOutcome::SchedGatewaySubmitAuthenticationRejected,
            SchedGatewaySubmitOutcome::SchedGatewaySubmitRetryable,
            SchedGatewaySubmitOutcome::SchedGatewaySubmitInternalError,
            SchedGatewaySubmitOutcome(4242),
        ] {
            let attempt = SubmissionAttempt::default();
            assert!(attempt.begin_dispatch());
            let result = native_submit_result(outcome, &attempt);
            let evidence = attempt.cancel();
            match outcome {
                SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted => {
                    assert!(result.is_ok());
                    // A caller that loses the success reply must not infer refusal.
                    assert!(matches!(evidence, AttemptAdmission::Unknown));
                }
                SchedGatewaySubmitOutcome::SchedGatewaySubmitAuthenticationRejected
                | SchedGatewaySubmitOutcome::SchedGatewaySubmitRetryable
                | SchedGatewaySubmitOutcome::SchedGatewaySubmitInternalError => {
                    assert_eq!(result, submit_result(outcome));
                    assert!(matches!(evidence, AttemptAdmission::Rejected));
                }
                _ => {
                    assert!(matches!(
                        result,
                        Err(RpcServerError::BackendDataInvalid {
                            reason: BackendDataInvalidReason::UnsupportedValue,
                            ..
                        })
                    ));
                    assert!(matches!(evidence, AttemptAdmission::Unknown));
                }
            }
        }
    }
}
