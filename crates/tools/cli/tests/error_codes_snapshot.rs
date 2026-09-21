//! Pins the stable `--json` error codes and the invariants they hold to.
//!
//! The codes are derived from the variant names, so the snapshot is what
//! turns a rename into a failing test instead of a silently changed contract.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use groundhog_cli::error::CliError;
use pretty_assertions::assert_eq;

const SNAPSHOT_PATH: &str = "tests/snapshots/error-codes.txt";

// Variants that intentionally report the code of a broader one, because the
// distinction is an implementation detail callers should not branch on.
const SHARED_CODES: &[(&str, &str)] = &[
    ("cargo_failed", "CargoFailedWithOutput"),
    ("invalid_event_limit", "InvalidTokenTransferLimit"),
    ("invalid_event_cursor", "InvalidTokenTransferCursor"),
    ("invalid_event_options", "InvalidTokenTransferOptions"),
];

#[test]
fn error_codes_match_snapshot() {
    let mut actual = String::new();
    for (variant, code) in CliError::CODES {
        writeln!(actual, "{variant} = {code}").expect("bug: writing to a String cannot fail");
    }
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
    for (variant, code) in CliError::CODES {
        assert!(
            !code.is_empty()
                && code.chars().all(|character| character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || character == '_'),
            "`{variant}` has a non-snake_case code `{code}`"
        );
    }
}

#[test]
fn codes_are_unique_apart_from_the_documented_aliases() {
    let mut owner: HashMap<&str, &str> = HashMap::new();
    for (variant, code) in CliError::CODES {
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
            CliError::CODES.contains(&(variant, code)),
            "SHARED_CODES lists `{variant}` = `{code}`, which no variant reports"
        );
    }
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_PATH)
}
