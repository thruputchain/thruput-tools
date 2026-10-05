//! Pins the `--json` error contract: every variant's code, its hint, and the
//! keys its structured details carry.
//!
//! All three are derived from what the variant declares, so the snapshot is
//! what turns an edit to any of them — including a variant rename, which
//! renames its derived code — into a failing test instead of a silently
//! changed contract. Run with `UPDATE_SNAPSHOTS=1` to record an intended
//! change.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use groundhog_cli::error::CliError;
use groundhog_json_rpc_types::error::TxInvalidDetail;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const SNAPSHOT_PATH: &str = "tests/snapshots/error-metadata.txt";

// Variants that intentionally report the code of a broader one, because the
// distinction is an implementation detail callers should not branch on.
const SHARED_CODES: &[(&str, &str)] = &[
    ("missing_rpc_url", "LocalNodeUnavailable"),
    ("cargo_failed", "CargoFailedWithOutput"),
    ("invalid_event_limit", "InvalidTokenTransferLimit"),
    ("invalid_event_cursor", "InvalidTokenTransferCursor"),
    ("invalid_event_options", "InvalidTokenTransferOptions"),
];

#[test]
fn metadata_matches_snapshot() {
    let actual = render_metadata();
    let path = snapshot_path();

    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        fs::write(&path, &actual).expect("snapshot should be writable");
        return;
    }

    let expected = fs::read_to_string(&path).expect("snapshot file should exist");
    assert_eq!(actual, expected);
}

#[test]
fn every_code_is_lower_snake_case() {
    for (variant, code, ..) in CliError::METADATA {
        assert!(
            !code.is_empty()
                && code.chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
                }),
            "`{variant}` has a non-snake_case code `{code}`"
        );
    }
}

#[test]
fn codes_are_unique_apart_from_the_documented_aliases() {
    let mut owner: HashMap<&str, &str> = HashMap::new();
    for (variant, code, ..) in CliError::METADATA {
        if let Some(first) = owner.insert(code, variant) {
            assert!(
                SHARED_CODES.contains(&(code, variant)),
                "`{variant}` reuses `{first}`'s code `{code}`; give it its own code or \
                 record the alias in SHARED_CODES"
            );
            owner.insert(code, first);
        }
    }
    for (code, variant) in SHARED_CODES {
        assert!(
            CliError::METADATA
                .iter()
                .any(|(name, reported, ..)| name == variant && reported == code),
            "SHARED_CODES lists `{variant}` = `{code}`, which no variant reports"
        );
    }
}

#[test]
fn details_keys_follow_the_camel_case_wire_convention() {
    for (variant, _, _, _, details_keys) in CliError::METADATA {
        if details_keys.is_empty() || *details_keys == "computed" {
            continue;
        }
        for key in details_keys.split(", ") {
            assert!(
                !key.is_empty() && !key.contains('_'),
                "`{variant}` carries a details key `{key}` that is not camelCase"
            );
        }
    }
}

// The catalog reports `computed` for a payload an expression builds, because
// its keys depend on the error's field values. These are those payloads.
#[test]
fn computed_details_payloads_carry_their_keys() {
    assert_eq!(
        details_of(&CliError::Rv32LowerFailed(
            groundhog_rv32_lower::LowerError::Invalid("bad .text".to_owned())
        )),
        json!({ "message": "bad .text" }),
    );
    assert_eq!(
        details_of(&CliError::ConfirmationRequired {
            action: "remove key `alice`",
        }),
        json!({ "action": "remove key `alice`", "requiredFlag": "--force" }),
    );
    assert_eq!(
        details_of(&CliError::FaucetService {
            url: "https://faucet.example/airdrop".to_owned(),
            http_status: 429,
            service_code: "rate_limited".to_owned(),
            message: "slow down".to_owned(),
            retry_after_seconds: Some(30),
        }),
        json!({
            "url": "https://faucet.example/airdrop",
            "httpStatus": 429,
            "code": "rate_limited",
            "retryAfterSeconds": "30",
        }),
        "the faucet payload renames service_code and omits the message the display carries"
    );
    assert_eq!(
        details_of(&CliError::TransactionRecentFailure {
            tx_hash: "0xab".to_owned(),
            attempt_height: 17,
            cause:
                groundhog_json_rpc_types::transaction::TransactionFailureCause::ContractRejected {
                    contract_index: 2,
                    code: groundhog_json_rpc_types::transaction::ContractRejectCode::InvalidProgram,
                },
        }),
        json!({
            "txHash": "0xab",
            "attemptHeight": 17,
            "cause": { "kind": "contractRejected", "contractIndex": 2, "code": "invalidProgram" },
        }),
    );

    // This one delegates to the detail's own serialization, so its keys vary
    // by rejection reason and `json-rpc-types` owns their shape.
    let rejected = details_of(&CliError::TransactionRejectedBeforeSubmission {
        detail: TxInvalidDetail::TooManyContracts {
            attached: 5,
            max: 4,
        },
    });
    assert_eq!(rejected["reason"], "tooManyContracts");
    assert_eq!(rejected["attached"], 5);
}

fn details_of(error: &CliError) -> Value {
    error
        .details_json()
        .expect("this variant declares structured details")
}

fn render_metadata() -> String {
    let mut out = String::new();
    for (variant, code, hint_kind, hint_text, details_keys) in CliError::METADATA {
        writeln!(out, "{variant}").expect("bug: writing to a String cannot fail");
        writeln!(out, "  code: {code}").expect("bug: writing to a String cannot fail");
        match *hint_kind {
            "" => {}
            "computed" => {
                writeln!(out, "  hint: computed").expect("bug: writing to a String cannot fail");
            }
            kind => writeln!(out, "  hint {kind}: {hint_text}")
                .expect("bug: writing to a String cannot fail"),
        }
        if !details_keys.is_empty() {
            writeln!(out, "  details: {details_keys}")
                .expect("bug: writing to a String cannot fail");
        }
        out.push('\n');
    }
    out
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_PATH)
}
