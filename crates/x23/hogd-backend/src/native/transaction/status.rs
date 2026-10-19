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
        Ok(Some(
            status @ (TransactionStatus::Pending {} | TransactionStatus::Committed { .. }),
        )) => Ok(status),
        Ok(Some(status)) => {
            indexed?;
            Ok(status)
        }
        Ok(None) => Ok(indexed?.unwrap_or(TransactionStatus::Unknown {})),
        Err(error) => {
            indexed?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use responses::{FailureCause, SchedulerError, SchedulerErrorSource, TransactionStatus};
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
            (Some(TransactionStatus::Pending {}), Some("pending")),
            (
                Some(TransactionStatus::Committed {
                    block_height: 7.into(),
                }),
                Some("committed"),
            ),
            (
                Some(TransactionStatus::Retry {
                    error: diagnostic(),
                }),
                None,
            ),
            (
                Some(TransactionStatus::Dropped {
                    error: diagnostic(),
                }),
                None,
            ),
            (Some(recent_failure()), None),
            (None, None),
        ] {
            let result =
                sequence_status(Err(unavailable("indexer")), async { Ok(scheduler) }).await;
            if let Some(expected) = expected {
                assert_eq!(
                    serde_json::to_value(result.unwrap()).unwrap()["status"],
                    expected
                );
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
    async fn recent_failure_answers_only_after_scheduler_miss() {
        for scheduler in [
            TransactionStatus::Pending {},
            TransactionStatus::Committed {
                block_height: 7.into(),
            },
            TransactionStatus::Retry {
                error: diagnostic(),
            },
            TransactionStatus::Dropped {
                error: diagnostic(),
            },
        ] {
            let expected = serde_json::to_value(&scheduler).unwrap();
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
