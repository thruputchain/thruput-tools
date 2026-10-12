use thruput_cli::CliError;
use thruput_json_rpc_types::error::TxInvalidDetail;
use thruput_rv32_lower::LowerError;

fn hint(error: &CliError) -> String {
    error.hint().expect("bug: hint expected").into_owned()
}

#[test]
fn rv32_lower_data_too_large_hints_at_stack_size_and_memory_size() {
    let error = CliError::Rv32LowerFailed(LowerError::DataTooLarge {
        section: ".bss".to_string(),
        end: 70168,
        max: 65536,
    });
    let hint = hint(&error);
    assert!(hint.contains("--stack-size"));
    assert!(hint.contains("initial_memory_bytes"));
}

#[test]
fn pre_submission_rejections_hint_at_the_violated_bound() {
    let code = CliError::TransactionRejectedBeforeSubmission {
        detail: TxInvalidDetail::Rv32CodeTooLarge {
            index: 0,
            code_bytes: 70_000,
            max_bytes: 65_536,
        },
    };
    assert!(hint(&code).contains("reduce code size"));
    assert_eq!(code.code(), "transaction_rejected_before_submission");
    assert_eq!(
        code.details_json().expect("bug: details expected")["reason"],
        "rv32CodeTooLarge"
    );

    let memory = CliError::TransactionRejectedBeforeSubmission {
        detail: TxInvalidDetail::Rv32MemoryTooLarge {
            index: 0,
            memory_bytes: 131_072,
            max_bytes: 65_536,
        },
    };
    assert!(hint(&memory).contains("initial_memory_bytes"));

    let count = CliError::TransactionRejectedBeforeSubmission {
        detail: TxInvalidDetail::TooManyContracts {
            attached: 5,
            max: 4,
        },
    };
    assert!(hint(&count).contains("resubmit"));
    assert!(
        count
            .to_string()
            .contains("transaction attaches 5 contracts; chain permits 4")
    );
}

#[test]
fn package_metadata_invalid_names_the_key_and_package() {
    let error = CliError::PackageMetadataInvalid {
        package: "my-counter".to_string(),
        key: "initial_memory_bytes",
        message: "0 is not a positive byte count".to_string(),
    };
    assert_eq!(error.code(), "package_metadata_invalid");
    assert!(
        error
            .to_string()
            .contains("`[package.metadata.thruput] initial_memory_bytes` in package `my-counter`")
    );
    assert!(hint(&error).contains("128K"));
    assert_eq!(
        error.details_json().expect("bug: details expected")["key"],
        "initial_memory_bytes"
    );
}

#[test]
fn rv32_lower_invalid_keeps_the_malformed_elf_hint() {
    let error = CliError::Rv32LowerFailed(LowerError::Invalid(
        ".text must start at vaddr 0, got 4".to_string(),
    ));
    assert!(hint(&error).contains("malformed"));
}

#[test]
fn rv32_lower_verify_hints_at_reporting_a_bug() {
    let error = CliError::Rv32LowerFailed(LowerError::Verify("code digest mismatch".to_string()));
    assert!(hint(&error).contains("bug"));
}

#[test]
fn node_supervisor_errors_hint_at_the_contract_artifact_directory_override() {
    let error =
        CliError::NodeSupervisor("hog-devnode exited before emitting readiness".to_string());

    assert!(hint(&error).contains("--contract-artifacts-dir"));
}

#[test]
fn rv32_lower_failed_details_json_keeps_the_message_field() {
    let inner = LowerError::DataTooLarge {
        section: ".data".to_string(),
        end: 65540,
        max: 65536,
    };
    let message = inner.to_string();
    let details = CliError::Rv32LowerFailed(inner)
        .details_json()
        .expect("bug: details expected");
    assert_eq!(details["message"], message);
}

#[test]
fn generated_command_hints_quote_dynamic_arguments() {
    let error = CliError::NodeSupervisorProtocolIntroRejected {
        message: "rejected".into(),
        expected_protocol: Some("hogp/test;echo bad".into()),
    };
    assert_eq!(
        hint(&error),
        "pass `--hogd-protocol='hogp/test;echo bad'` to override, or rebuild thruput node against a matching thruput-xdr"
    );
}

#[test]
fn faucet_service_error_preserves_machine_readable_response_fields() {
    let error = CliError::FaucetService {
        url: "https://faucet.example/v1/airdrop".into(),
        http_status: 429,
        service_code: "airdrop_recipient_cooldown".into(),
        message: "recipient is cooling down".into(),
        retry_after_seconds: Some(42),
    };
    let details = error.details_json().expect("bug: details should exist");

    assert_eq!(error.code(), "faucet_service_error");
    assert_eq!(details["httpStatus"], 429);
    assert_eq!(details["code"], "airdrop_recipient_cooldown");
    assert_eq!(details["retryAfterSeconds"], "42");
}
