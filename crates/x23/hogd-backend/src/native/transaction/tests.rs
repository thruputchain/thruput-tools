use super::*;
use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::base58::Base58;
use thruput_scheduler::SchedulerTransactionOutcome;
use thruput_trace::{
    ContractRejectCode, ContractRejectDiagnostic, DbConflictCode, DbConflictDiagnostic,
    TxFailureCause, TxFailureDiagnostic,
};
use thruput_xdr::{
    event::TxEmittedEvent,
    types::{AddressAndKey, StorageKey},
};
use xdr_common::{BytesM, VecM};

fn hash() -> TxHash {
    TxHash(Base58([7; 32].into()))
}
fn log(result: ExecResult, output: InvocationFrameOutput) -> TxEventLog {
    TxEventLog {
        tx_hash: tx_hash_to_wire(hash()),
        exec_result: result,
        root_output: output,
        gas_consumed: u64::MAX,
        events: VecM(vec![]),
    }
}
fn outputs() -> [InvocationFrameOutput; 3] {
    [
        InvocationFrameOutput::Return {
            return_buffer: BytesM(vec![1, 2]),
        },
        InvocationFrameOutput::Abort {
            abort_buffer: BytesM(vec![3]),
        },
        InvocationFrameOutput::Fault,
    ]
}
fn reason(error: &NativeReadError, expected: Reason) {
    assert!(
        matches!(error.error, RpcServerError::BackendDataInvalid { reason, .. } if reason == expected)
    );
}

#[test]
fn execution_matrix_distinguishes_invocation_from_commitment() {
    let results = [
        ExecResult::ExecResultOK,
        ExecResult::ExecResultTxRootFrameReverted,
        ExecResult::ExecResultTxDeterministicError,
        ExecResult::ExecResultTxNonDeterministicError,
        ExecResult::ExecResultFailedGasSponsorship,
        ExecResult(9999),
    ];
    for (i, result) in results.into_iter().enumerate() {
        for (j, output) in outputs().into_iter().enumerate() {
            let decoded = receipt(hash(), 42.into(), log(result, output));
            if i < 3 && i != j {
                let error = decoded.unwrap_err();
                reason(&error, Reason::InconsistentResult);
                assert_eq!(error.observation.unwrap().block_height, 42.into());
            } else {
                let value = decoded.unwrap();
                assert_eq!(value.gas_used, u64::MAX);
                let expected = [
                    "return",
                    "abort",
                    "fault",
                    "nonDeterministicError",
                    "failedGasSponsorship",
                    "other",
                ][i];
                assert_eq!(
                    serde_json::to_value(value.execution).unwrap()["result"],
                    expected
                );
            }
        }
    }
}

#[test]
fn return_and_abort_preserve_required_empty_and_nonempty_bytes() {
    for data in [vec![], vec![1, 2, 3]] {
        let value = receipt(
            hash(),
            0.into(),
            log(
                ExecResult::ExecResultOK,
                InvocationFrameOutput::Return {
                    return_buffer: BytesM(data.clone()),
                },
            ),
        )
        .unwrap();
        assert!(
            matches!(value.execution, out::ReceiptExecution::Return { return_data } if return_data.0 == data)
        );
        let value = receipt(
            hash(),
            0.into(),
            log(
                ExecResult::ExecResultTxRootFrameReverted,
                InvocationFrameOutput::Abort {
                    abort_buffer: BytesM(data.clone()),
                },
            ),
        )
        .unwrap();
        assert!(
            matches!(value.execution, out::ReceiptExecution::Abort { abort_data } if abort_data.0 == data)
        );
    }
}

fn malformed_log() -> TxEventLog {
    let mut value = log(ExecResult::ExecResultOK, outputs()[0].clone());
    value.events = VecM(vec![TxEmittedEvent {
        contract_address: [8; 32].into(),
        num_topics: 1,
        serialized_event: BytesM(vec![0; 31]),
    }]);
    value
}

#[test]
fn identity_errors_never_attach_observation_even_with_bad_payload() {
    let mut value = malformed_log();
    value.tx_hash = [8; 32].into();
    let error = receipt(hash(), 42.into(), value).unwrap_err();
    reason(&error, Reason::InconsistentResult);
    assert!(error.observation.is_none());
    for height in [-1, -2, i64::MIN] {
        let error = receipt(hash(), height.into(), malformed_log()).unwrap_err();
        reason(&error, Reason::InconsistentResult);
        assert!(error.observation.is_none());
    }
}

#[test]
fn malformed_events_fail_the_whole_receipt_with_only_validated_identity() {
    for num_topics in [1, 4] {
        let mut value = malformed_log();
        value.events.0[0].num_topics = num_topics;
        let error = receipt(hash(), 42.into(), value).unwrap_err();
        reason(&error, Reason::InvalidEncoding);
        let observed = error.observation.unwrap();
        assert_eq!(observed.transaction_hash, hash());
        assert_eq!(observed.block_height, 42.into());
    }
}

#[test]
fn valid_events_preserve_order_address_topics_and_data() {
    let mut value = log(ExecResult::ExecResultOK, outputs()[0].clone());
    value.events = VecM(vec![
        TxEmittedEvent {
            contract_address: [8; 32].into(),
            num_topics: 1,
            serialized_event: BytesM([vec![4; 32], vec![5, 6]].concat()),
        },
        TxEmittedEvent {
            contract_address: [9; 32].into(),
            num_topics: 0,
            serialized_event: BytesM(vec![]),
        },
    ]);
    let value = receipt(hash(), 42.into(), value).unwrap();
    assert_eq!(value.events.len(), 2);
    assert_eq!(value.events[0].address, Base58([8; 32].into()));
    assert_eq!(value.events[0].topics[0].0, [4; 32]);
    assert_eq!(value.events[0].data.0, vec![5, 6]);
    assert_eq!(value.events[1].address, Base58([9; 32].into()));
    assert!(value.events[1].topics.is_empty());
}

fn scheduler(status: Status) -> SchedulerTransactionOutcome {
    SchedulerTransactionOutcome {
        status,
        error_source: Source::SchedulerTransactionErrorExecutor,
        error_code: i32::MIN,
        height: 42.into(),
    }
}
#[test]
fn scheduler_checks_cardinality_tags_and_committed_height() {
    for values in [
        vec![],
        vec![scheduler(Status::SchedulerTransactionPending); 2],
    ] {
        let error = scheduler_status(&VecM(values)).unwrap_err();
        reason(&error, Reason::InvalidCardinality);
        assert!(error.observation.is_none());
    }
    for value in [
        scheduler(Status(9999)),
        SchedulerTransactionOutcome {
            error_source: Source(9999),
            ..scheduler(Status::SchedulerTransactionRetry)
        },
    ] {
        reason(
            &scheduler_status(&VecM(vec![value])).unwrap_err(),
            Reason::UnsupportedValue,
        );
    }
    for height in [-1, -2] {
        let value = SchedulerTransactionOutcome {
            height: height.into(),
            ..scheduler(Status::SchedulerTransactionSucceeded)
        };
        reason(
            &scheduler_status(&VecM(vec![value])).unwrap_err(),
            Reason::InconsistentResult,
        );
    }
    assert!(
        scheduler_status(&VecM(vec![scheduler(Status::SchedulerTransactionNotFound)]))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        scheduler_status(&VecM(vec![scheduler(Status::SchedulerTransactionPending)])).unwrap(),
        Some(out::TransactionStatus::Pending {
            recent_failure: None
        })
    ));
    // Genesis is a real block, so a commitment at height 0 is valid.
    for height in [42, 0] {
        let value = SchedulerTransactionOutcome {
            height: height.into(),
            ..scheduler(Status::SchedulerTransactionSucceeded)
        };
        assert_eq!(
            serde_json::to_value(scheduler_status(&VecM(vec![value])).unwrap()).unwrap(),
            serde_json::json!({"status": "committed", "blockHeight": height.to_string()})
        );
    }
    for (status, tag) in [
        (Status::SchedulerTransactionRetry, "retry"),
        (Status::SchedulerTransactionDropped, "dropped"),
    ] {
        for (source, name) in [
            (Source::SchedulerTransactionErrorAuth, "auth"),
            (Source::SchedulerTransactionErrorScheduler, "scheduler"),
            (Source::SchedulerTransactionErrorExecutor, "executor"),
        ] {
            let result = scheduler_status(&VecM(vec![SchedulerTransactionOutcome {
                error_source: source,
                ..scheduler(status)
            }]))
            .unwrap()
            .unwrap();
            assert_eq!(
                serde_json::to_value(result).unwrap(),
                serde_json::json!({"status": tag, "error": {"source": name, "code": i32::MIN}})
            );
        }
    }
}

fn recent(cause: TxFailureCause, height: i64) -> TxInfoResult {
    TxInfoResult::TxInfoRecentFailure {
        recent_failure: TxFailureDiagnostic {
            attempt_height: height.into(),
            cause,
        },
    }
}
fn conflict(code: DbConflictCode) -> TxFailureCause {
    TxFailureCause::TxFailureDbConflict {
        conflict: DbConflictDiagnostic {
            addrkey: AddressAndKey {
                address: [8; 32].into(),
                key: StorageKey::FreeNativeTokenBalance,
            },
            code,
        },
    }
}
// The wire names are the public diagnostic vocabulary; each hogd code must
// keep mapping to its own name.
#[test]
fn every_known_failure_code_maps_to_its_wire_name() {
    for (code, wire) in [
        (DbConflictCode::ExclusiveLockDenied, "exclusiveLock"),
        (DbConflictCode::TypeMismatch, "typeMismatch"),
        (DbConflictCode::ValueMismatch, "valueMismatch"),
        (DbConflictCode::BoundsExceeded, "boundsExceeded"),
        (DbConflictCode::Overflow, "overflow"),
        (DbConflictCode::ReplayProtection, "replayProtection"),
        (DbConflictCode::QueueConsumer, "queueConsumer"),
        (DbConflictCode::CapacityExceeded, "capacityExceeded"),
        (DbConflictCode::InsufficientFunds, "insufficientFunds"),
    ] {
        let result = indexed_status(hash(), recent(conflict(code), 43))
            .unwrap()
            .unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["cause"]["kind"], "dbConflict", "{wire}");
        assert_eq!(value["cause"]["conflict"]["code"], wire);
    }
    for (code, wire) in [
        (ContractRejectCode::ContractRejectTooMany, "tooMany"),
        (
            ContractRejectCode::ContractRejectBlobTooLarge,
            "blobTooLarge",
        ),
        (
            ContractRejectCode::ContractRejectNotDeployable,
            "notDeployable",
        ),
        (ContractRejectCode::ContractRejectDuplicate, "duplicate"),
        (
            ContractRejectCode::ContractRejectMalformedArtifact,
            "malformedArtifact",
        ),
        (
            ContractRejectCode::ContractRejectUnsupportedFormat,
            "unsupportedFormat",
        ),
        (
            ContractRejectCode::ContractRejectUnsupportedAbi,
            "unsupportedAbi",
        ),
        (
            ContractRejectCode::ContractRejectGasScheduleMismatch,
            "gasScheduleMismatch",
        ),
        (
            ContractRejectCode::ContractRejectMemoryTooLarge,
            "memoryTooLarge",
        ),
        (
            ContractRejectCode::ContractRejectCodeSizeInvalid,
            "codeSizeInvalid",
        ),
        (
            ContractRejectCode::ContractRejectInitialMemoryInvalid,
            "initialMemoryInvalid",
        ),
        (
            ContractRejectCode::ContractRejectDigestMismatch,
            "digestMismatch",
        ),
        (
            ContractRejectCode::ContractRejectInvalidProgram,
            "invalidProgram",
        ),
    ] {
        let cause = TxFailureCause::TxFailureContractRejected {
            contract: ContractRejectDiagnostic {
                contract_index: 3,
                code,
            },
        };
        let result = indexed_status(hash(), recent(cause, 43)).unwrap().unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["cause"]["kind"], "contractRejected", "{wire}");
        assert_eq!(value["cause"]["contractIndex"], 3);
        assert_eq!(value["cause"]["code"], wire);
    }
}

#[test]
fn indexed_diagnostics_are_typed_and_unknown_values_are_errors() {
    let input = recent(conflict(DbConflictCode::ReplayProtection), 43);
    let result = indexed_status(hash(), input.clone()).unwrap().unwrap();
    let value = serde_json::to_value(result).unwrap();
    assert_eq!(value["attemptBlockHeight"], "43");
    assert_eq!(value["cause"]["conflict"]["code"], "replayProtection");
    assert!(receipt_lookup(hash(), input).unwrap().is_none());
    for input in [
        recent(conflict(DbConflictCode(9999)), 43),
        recent(
            TxFailureCause::TxFailureContractRejected {
                contract: ContractRejectDiagnostic {
                    contract_index: 3,
                    code: ContractRejectCode(9999),
                },
            },
            43,
        ),
        TxInfoResult::Default { outcome: 9999 },
    ] {
        reason(
            &indexed_status(hash(), input.clone()).unwrap_err(),
            Reason::UnsupportedValue,
        );
        reason(
            &receipt_lookup(hash(), input).unwrap_err(),
            Reason::UnsupportedValue,
        );
    }
    reason(
        &indexed_status(
            hash(),
            recent(conflict(DbConflictCode::ReplayProtection), -1),
        )
        .unwrap_err(),
        Reason::InconsistentResult,
    );
    assert!(
        indexed_status(
            hash(),
            TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0
            }
        )
        .unwrap()
        .is_none()
    );
    assert!(
        receipt_lookup(
            hash(),
            TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0
            }
        )
        .unwrap()
        .is_none()
    );
}

fn found(events: TxEventLog, height: i64) -> TxInfoResult {
    use thruput_xdr::{
        auth_claim::{AuthClaim, AuthClaim_source_address_claim},
        transaction::{
            Invocation, NondeterministicProposalData, ProposedTransaction, SignedTransaction,
            Transaction, Uniquifier,
        },
    };
    TxInfoResult::TxInfoFound {
        info: thruput_indexer::TxInfo {
            height: height.into(),
            events,
            tx: ProposedTransaction {
                signed_tx: SignedTransaction {
                    tx: Transaction {
                        invocation: Invocation {
                            invoked_address: [0; 32].into(),
                            entrypoint: [0; 8].into(),
                            gas_limit: 0,
                            calldata: BytesM(vec![]),
                        },
                        uniquifier: Uniquifier {
                            block_expiry_height: 0.into(),
                            nonce: 0,
                        },
                        contracts_to_deploy: VecM(vec![]),
                        auth_claim: AuthClaim::kSourceAddressAuthData {
                            source_address_claim: AuthClaim_source_address_claim {
                                allowed_keys: 0,
                                required_weight: 0,
                            },
                        },
                    },
                    signatures: VecM(vec![]),
                    pubkey_claim_key: None,
                },
                nondeterministic_proposal_data: NondeterministicProposalData {
                    nondeterministic_bits: BytesM(vec![]),
                    nondeterministic_bit_count: 0,
                    observed_queue_entry_sequence: VecM(vec![]),
                },
            },
        },
    }
}

#[test]
fn indexed_commitment_needs_identity_but_not_receipt_payload() {
    let input = found(malformed_log(), 42);
    assert!(
        matches!(indexed_status(hash(),input.clone()).unwrap(),Some(out::TransactionStatus::Committed {block_height}) if block_height==42.into())
    );
    let error = receipt_lookup(hash(), input).unwrap_err();
    reason(&error, Reason::InvalidEncoding);
    assert!(error.observation.is_some());
    let mut value = malformed_log();
    value.tx_hash = [9; 32].into();
    for input in [found(value, 42), found(malformed_log(), -1)] {
        let error = indexed_status(hash(), input).unwrap_err();
        reason(&error, Reason::InconsistentResult);
        assert!(error.observation.is_none());
    }
}
