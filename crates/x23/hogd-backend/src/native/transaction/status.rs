use thruput_json_rpc_server::native::transaction::NativeReadError;
use thruput_json_rpc_types::responses;

/// Indexed commitment wins. Only scheduler pending/committed can override an
/// indexer failure; weaker scheduler evidence cannot hide unavailable history.
pub async fn sequence_status(
    indexed: Result<Option<responses::TransactionStatus>, NativeReadError>,
    scheduler: impl std::future::Future<
        Output = Result<Option<responses::TransactionStatus>, NativeReadError>,
    >,
) -> Result<responses::TransactionStatus, NativeReadError> {
    use responses::TransactionStatus;
    if let Ok(Some(status @ TransactionStatus::Committed { .. })) = indexed {
        return Ok(status);
    }
    match scheduler.await {
        Ok(Some(status @ TransactionStatus::Committed { .. })) => Ok(status),
        Ok(Some(status @ TransactionStatus::Pending { .. })) => {
            Ok(with_recent_failure(status, indexed.ok().flatten()))
        }
        Ok(Some(status)) => Ok(with_recent_failure(status, indexed?)),
        Ok(None) => Ok(indexed?.unwrap_or(TransactionStatus::Unknown {})),
        Err(error) => {
            indexed?;
            Err(error)
        }
    }
}

// These are independent observations: the indexed failure need not be the
// scheduler outcome's attempt. Never attach it to authoritative commitment.
fn with_recent_failure(
    mut status: responses::TransactionStatus,
    indexed: Option<responses::TransactionStatus>,
) -> responses::TransactionStatus {
    use responses::{RecentAttemptFailure, TransactionStatus};
    if let Some(TransactionStatus::RecentFailure {
        attempt_block_height,
        cause,
    }) = indexed
    {
        match &mut status {
            TransactionStatus::Pending { recent_failure }
            | TransactionStatus::Retry { recent_failure, .. }
            | TransactionStatus::Dropped { recent_failure, .. } => {
                *recent_failure = Some(RecentAttemptFailure {
                    attempt_block_height,
                    cause,
                });
            }
            _ => {}
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use responses::{FailureCause, SchedulerError, SchedulerErrorSource, TransactionStatus};
    use serde_json::json;
    use thruput_json_rpc_server::RpcServerError;

    fn unavailable(service: &str) -> NativeReadError {
        RpcServerError::BackendUnavailable(service.into()).into()
    }

    fn diagnostic() -> SchedulerError {
        SchedulerError {
            source: SchedulerErrorSource::Executor,
            code: 17,
        }
    }

    fn recent_failure() -> TransactionStatus {
        TransactionStatus::RecentFailure {
            attempt_block_height: 6.into(),
            cause: FailureCause::ContractRejected {
                contract_index: 0,
                code: responses::ContractRejectCode::TooMany,
            },
        }
    }

    #[tokio::test]
    async fn only_positive_scheduler_evidence_overrides_indexer_failure() {
        for (scheduler, expected) in [
            (
                Some(TransactionStatus::Pending {
                    recent_failure: None,
                }),
                Some(json!({"status":"pending"})),
            ),
            (
                Some(TransactionStatus::Committed {
                    block_height: 7.into(),
                }),
                Some(json!({"status":"committed", "blockHeight":"7"})),
            ),
            (
                Some(TransactionStatus::Retry {
                    error: diagnostic(),
                    recent_failure: None,
                }),
                None,
            ),
            (
                Some(TransactionStatus::Dropped {
                    error: diagnostic(),
                    recent_failure: None,
                }),
                None,
            ),
            (Some(recent_failure()), None),
            (None, None),
        ] {
            let result =
                sequence_status(Err(unavailable("indexer")), async { Ok(scheduler) }).await;
            if let Some(expected) = expected {
                assert_eq!(serde_json::to_value(result.unwrap()).unwrap(), expected);
            } else {
                assert!(matches!(
                    result.unwrap_err().error,
                    RpcServerError::BackendUnavailable(service) if service == "indexer"
                ));
            }
        }
        let error = sequence_status(Err(unavailable("indexer")), async {
            Err(unavailable("scheduler"))
        })
        .await
        .unwrap_err();
        assert!(matches!(
            error.error,
            RpcServerError::BackendUnavailable(service) if service == "indexer"
        ));
    }

    #[tokio::test]
    async fn indexed_commitment_does_not_poll_scheduler() {
        let status = sequence_status(
            Ok(Some(TransactionStatus::Committed {
                block_height: 7.into(),
            })),
            async { panic!("scheduler must not be polled after indexed commitment") },
        )
        .await
        .unwrap();
        assert!(matches!(
            status,
            TransactionStatus::Committed { block_height } if *block_height == 7
        ));
    }

    #[tokio::test]
    async fn recent_failure_augments_scheduler_observations_and_survives_scheduler_miss() {
        let failure = json!({"attemptBlockHeight":"6", "cause":{
            "kind":"contractRejected", "contractIndex":0, "code":"tooMany"
        }});
        for (scheduler, expected) in [
            (
                TransactionStatus::Pending {
                    recent_failure: None,
                },
                json!({"status":"pending", "recentFailure":failure}),
            ),
            (
                TransactionStatus::Committed {
                    block_height: 7.into(),
                },
                json!({"status":"committed", "blockHeight":"7"}),
            ),
            (
                TransactionStatus::Retry {
                    error: diagnostic(),
                    recent_failure: None,
                },
                json!({"status":"retry", "error":{"source":"executor", "code":17}, "recentFailure":failure}),
            ),
            (
                TransactionStatus::Dropped {
                    error: diagnostic(),
                    recent_failure: None,
                },
                json!({"status":"dropped", "error":{"source":"executor", "code":17}, "recentFailure":failure}),
            ),
        ] {
            let result = sequence_status(Ok(Some(recent_failure())), async { Ok(Some(scheduler)) })
                .await
                .unwrap();
            assert_eq!(serde_json::to_value(result).unwrap(), expected);
        }
        let status = sequence_status(Ok(Some(recent_failure())), async { Ok(None) })
            .await
            .unwrap();
        assert!(matches!(
            status,
            TransactionStatus::RecentFailure {
                attempt_block_height,
                ..
            } if *attempt_block_height == 6
        ));
        let error = sequence_status(Ok(Some(recent_failure())), async {
            Err(unavailable("scheduler"))
        })
        .await
        .unwrap_err();
        assert!(matches!(
            error.error,
            RpcServerError::BackendUnavailable(service) if service == "scheduler"
        ));
    }

    #[tokio::test]
    async fn unknown_requires_two_successful_misses() {
        assert!(matches!(
            sequence_status(Ok(None), async { Ok(None) }).await.unwrap(),
            TransactionStatus::Unknown {}
        ));
        assert!(
            sequence_status(Ok(None), async { Err(unavailable("scheduler")) })
                .await
                .is_err()
        );
    }
}
