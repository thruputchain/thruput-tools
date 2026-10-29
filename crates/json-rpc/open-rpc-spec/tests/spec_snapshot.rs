use pretty_assertions::assert_eq;
use std::{fs, path::PathBuf};
use thruput_json_rpc_api::WIRE_FORMAT_VERSION;
use thruput_open_rpc_spec::{build_spec, reference::render_markdown};

const SNAPSHOT_PATH: &str = "snapshots/thruput-spec.json";
const REFERENCE_PATH: &str = "snapshots/thruput-spec.md";

fn find_wide_json_number(value: &serde_json::Value) -> Option<&serde_json::Value> {
    match value {
        serde_json::Value::Object(fields) => {
            let is_integer = match fields.get("type") {
                Some(serde_json::Value::String(ty)) => ty == "integer",
                Some(serde_json::Value::Array(types)) => types.iter().any(|ty| ty == "integer"),
                _ => false,
            };
            if is_integer
                && matches!(
                    fields.get("format").and_then(serde_json::Value::as_str),
                    Some("int64" | "uint64" | "int128" | "uint128")
                )
            {
                return Some(value);
            }
            fields.values().find_map(find_wide_json_number)
        }
        serde_json::Value::Array(values) => values.iter().find_map(find_wide_json_number),
        _ => None,
    }
}

#[test]
fn public_schemas_never_expose_wide_json_numbers() {
    if let Some(value) = find_wide_json_number(&build_spec()) {
        panic!("wide integer must be a decimal string: {value}");
    }
}

#[test]
fn wide_number_guard_detects_scalar_and_nullable_integers() {
    for format in ["int64", "uint64", "int128", "uint128"] {
        for ty in [
            serde_json::json!("integer"),
            serde_json::json!(["integer", "null"]),
        ] {
            let number = serde_json::json!({"type": ty, "format": format});
            let schema = serde_json::json!({"properties": {"value": {"anyOf": [number]}}});
            assert_eq!(find_wide_json_number(&schema), Some(&number));
        }
    }
}

#[test]
fn wide_number_guard_detects_generated_optional_u64_schema() {
    let schema = serde_json::to_value(schemars::schema_for!(Option<u64>)).unwrap();
    assert!(find_wide_json_number(&schema).is_some(), "{schema}");
}

#[test]
fn wide_number_guard_accepts_narrow_numbers_and_decimal_strings() {
    for schema in [
        serde_json::to_value(schemars::schema_for!(Option<u32>)).unwrap(),
        serde_json::json!({"type": ["string", "null"], "format": "uint64"}),
    ] {
        assert_eq!(find_wide_json_number(&schema), None);
    }
}

#[test]
fn openrpc_spec_matches_snapshot() {
    let spec = build_spec();
    let actual = serde_json::to_string_pretty(&spec).expect("spec JSON serialization should work");
    check_snapshot(SNAPSHOT_PATH, &actual);
}

#[test]
fn markdown_reference_matches_snapshot() {
    check_snapshot(REFERENCE_PATH, &render_markdown(&build_spec()));
}

#[test]
fn markdown_reference_covers_every_method_and_type() {
    let spec = build_spec();
    let reference = render_markdown(&spec);
    for method in spec["methods"].as_array().unwrap() {
        let name = method["name"].as_str().unwrap();
        assert!(reference.contains(&format!("### `{name}`")), "{name}");
    }
    for name in spec["components"]["schemas"].as_object().unwrap().keys() {
        assert!(reference.contains(&format!("### `{name}`")), "{name}");
    }
}

/// Compare generated text with a checked-in file, or rewrite it with `UPDATE_SNAPSHOTS=1`.
fn check_snapshot(relative_path: &str, actual: &str) {
    let snapshot_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);

    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        if let Some(parent) = snapshot_path.parent() {
            fs::create_dir_all(parent).expect("snapshot directory should be creatable");
        }
        fs::write(&snapshot_path, format!("{}\n", actual.trim_end()))
            .expect("snapshot should be writable");
        return;
    }

    let expected = fs::read_to_string(&snapshot_path).expect("snapshot file should exist");
    assert_eq!(actual.trim_end(), expected.trim_end());
}

#[test]
fn wire_format_version_is_valid_semver_and_matches_spec_info() {
    semver::Version::parse(WIRE_FORMAT_VERSION)
        .expect("bug: WIRE_FORMAT_VERSION should be valid SemVer");
    assert_eq!(
        build_spec()["info"]["version"]
            .as_str()
            .expect("bug: OpenRPC info.version should be a string"),
        WIRE_FORMAT_VERSION,
        "OpenRPC info.version should come from WIRE_FORMAT_VERSION"
    );
}

#[test]
fn canonical_contract_has_only_native_named_methods() {
    let spec = build_spec();
    let methods = spec["methods"].as_array().unwrap();
    let names: std::collections::BTreeSet<_> = methods
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        thruput_json_rpc_api::native::METHOD_NAMES
            .iter()
            .copied()
            .collect()
    );
    assert!(methods.iter().all(|m| m["paramStructure"] == "by-name"));
    assert!(!names.iter().any(|name| name.starts_with("groundhog_")));
    assert_eq!(spec, thruput_open_rpc_spec::native::build_spec());
}
