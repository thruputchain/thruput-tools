//! End-to-end coverage for each attribute form `ErrorMeta` accepts.

use std::borrow::Cow;

use groundhog_cli_macros::ErrorMeta;
use serde_json::json;

const SHARED_HINT: &str = "shared next step";

#[derive(Debug, ErrorMeta)]
enum TestError {
    Plain,
    #[code("renamed_code")]
    #[hint("a static next step")]
    Renamed,
    #[hint("check `{0}` and `{1}`")]
    Positional(String, u8),
    #[hint("check `{field}`{}", suffix(other))]
    #[details]
    Formatted {
        field: String,
        other: String,
    },
    #[hint(text = SHARED_HINT)]
    #[details(kept)]
    Subset {
        kept: String,
        dropped: String,
    },
    #[hint(expr = branching_hint(*flag))]
    #[details(expr = Some(json!({ "renamed": value })))]
    Computed {
        flag: bool,
        value: u32,
    },
    #[details]
    SnakeCasedKeys {
        wallet_name: String,
    },
}

fn suffix(other: &str) -> String {
    format!("{other}!")
}

fn branching_hint(flag: bool) -> Option<Cow<'static, str>> {
    flag.then_some(Cow::Borrowed("only when set"))
}

#[test]
fn codes_default_to_the_variant_name_in_snake_case() {
    assert_eq!(TestError::Plain.code(), "plain");
    assert_eq!(
        TestError::SnakeCasedKeys {
            wallet_name: String::new()
        }
        .code(),
        "snake_cased_keys"
    );
    assert_eq!(TestError::Renamed.code(), "renamed_code");
}

#[test]
fn the_catalog_lists_every_variant_in_declaration_order() {
    assert_eq!(
        TestError::CODES.first(),
        Some(&("Plain", "plain")),
        "the catalog should start at the first declared variant"
    );
    assert_eq!(TestError::CODES.len(), 7);
    assert!(TestError::CODES.contains(&("Renamed", "renamed_code")));
}

#[test]
fn hints_format_over_the_variants_fields() {
    assert_eq!(TestError::Plain.hint(), None);
    assert_eq!(
        TestError::Renamed.hint().as_deref(),
        Some("a static next step")
    );
    assert_eq!(
        TestError::Positional("left".into(), 7).hint().as_deref(),
        Some("check `left` and `7`")
    );
    assert_eq!(
        TestError::Formatted {
            field: "one".into(),
            other: "two".into(),
        }
        .hint()
        .as_deref(),
        Some("check `one`two!")
    );
    assert_eq!(
        TestError::Subset {
            kept: String::new(),
            dropped: String::new(),
        }
        .hint()
        .as_deref(),
        Some(SHARED_HINT)
    );
}

#[test]
fn an_expression_hint_may_depend_on_a_field() {
    let set = TestError::Computed {
        flag: true,
        value: 1,
    };
    let unset = TestError::Computed {
        flag: false,
        value: 1,
    };
    assert_eq!(set.hint().as_deref(), Some("only when set"));
    assert_eq!(unset.hint(), None);
}

#[test]
fn details_carry_the_selected_fields_under_camel_case_keys() {
    assert_eq!(TestError::Plain.details_json(), None);
    assert_eq!(
        TestError::Formatted {
            field: "one".into(),
            other: "two".into(),
        }
        .details_json(),
        Some(json!({ "field": "one", "other": "two" }))
    );
    assert_eq!(
        TestError::Subset {
            kept: "yes".into(),
            dropped: "no".into(),
        }
        .details_json(),
        Some(json!({ "kept": "yes" }))
    );
    assert_eq!(
        TestError::Computed {
            flag: false,
            value: 9,
        }
        .details_json(),
        Some(json!({ "renamed": 9 }))
    );
    assert_eq!(
        TestError::SnakeCasedKeys {
            wallet_name: "alice".into(),
        }
        .details_json(),
        Some(json!({ "walletName": "alice" }))
    );
}
