use std::fs;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use thruput_json_rpc_api::{
    WIRE_FORMAT_VERSION,
    types::{
        airdrop::MAX_CLIENT_NONCE_BYTES,
        error::{ERROR_INFOS, codes, error_info_by_code},
        events::MAX_TOPIC_VALUES_PER_POSITION,
    },
};
use thruput_open_rpc_spec::build_spec;

const SNAPSHOT_PATH: &str = "snapshots/thruput-spec.json";

// Methods that are infallible by design and intentionally carry an empty
// `errors: []` array. Add a method here only when it has no backend
// dispatch and cannot fail. Both current methods are sourced from immutable
// process state.
const INFALLIBLE_METHODS: &[&str] = &[
    "groundhog_networkInfo",
    "groundhog_serverLimits",
    "groundhog_version",
];

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
    let snapshot_path = snapshot_path();

    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        if let Some(parent) = snapshot_path.parent() {
            fs::create_dir_all(parent).expect("snapshot directory should be creatable");
        }
        fs::write(&snapshot_path, format!("{actual}\n")).expect("snapshot should be writable");
        return;
    }

    let expected = fs::read_to_string(&snapshot_path).expect("snapshot file should exist");
    assert_eq!(actual, expected.trim_end());
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
fn openrpc_spec_has_expected_shared_schema_invariants() {
    let spec = build_spec();
    let schemas = spec["components"]["schemas"]
        .as_object()
        .expect("components.schemas should be an object");
    assert!(
        !schemas.is_empty(),
        "real spec should define shared schemas"
    );
    assert!(schemas.contains_key("TxHash"), "TxHash should be shared");
    assert!(
        schemas.keys().all(|name| !name.starts_with("VecM<")),
        "public schema names should not expose raw Rust VecM generic keys"
    );
    assert!(
        schemas.contains_key("ContractAddressList")
            && schemas.contains_key("EventFilterList")
            && schemas.contains_key("TopicList"),
        "event query list schemas should have stable public names"
    );
    assert_eq!(
        schemas["TopicList"]["maxItems"], MAX_TOPIC_VALUES_PER_POSITION,
        "TopicList should expose the explicit RPC topic-position bound"
    );

    let methods = spec["methods"]
        .as_array()
        .expect("methods should be an array");

    let tx_hash_refs = methods
        .iter()
        .flat_map(|method| {
            method["params"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(std::iter::once(&method["result"]))
        })
        .filter(|descriptor| descriptor["schema"]["$ref"] == "#/components/schemas/TxHash")
        .count();
    assert!(
        tx_hash_refs > 1,
        "TxHash should be referenced via $ref from more than one method \
         (found {tx_hash_refs}); the byte-exact snapshot covers the precise shape"
    );

    let send_raw_transaction = methods
        .iter()
        .find(|method| method["name"] == "groundhog_sendRawTransaction")
        .expect("sendRawTransaction should exist");
    assert_eq!(
        send_raw_transaction["params"][0]["schema"]["type"], "string",
        "primitive parameter schemas should stay inline",
    );

    let network_info = methods
        .iter()
        .find(|method| method["name"] == "groundhog_networkInfo")
        .expect("networkInfo should exist");
    assert_eq!(
        network_info["params"],
        serde_json::json!([]),
        "networkInfo should take no parameters"
    );
    assert_eq!(
        network_info["result"]["schema"]["$ref"], "#/components/schemas/NetworkInfo",
        "networkInfo should return the shared NetworkInfo schema"
    );
    assert_eq!(
        schemas["NetworkInfo"]["required"],
        serde_json::json!(["networkHash"]),
        "NetworkInfo should require only the signing-domain identity"
    );
    assert_eq!(
        schemas["Base58_SHA256Hash"]["pattern"], "^[1-9A-HJ-NP-Za-km-z]+$",
        "SHA-256 hashes should expose their exact canonical wire length"
    );
    assert_eq!(
        network_info["errors"],
        serde_json::json!([]),
        "networkInfo should be infallible after startup validation"
    );

    assert_airdrop_contract(schemas, methods);

    assert_error_docs_are_wired(&spec, methods, send_raw_transaction);
}

fn assert_airdrop_contract(
    schemas: &serde_json::Map<String, serde_json::Value>,
    methods: &[serde_json::Value],
) {
    let request_airdrop = methods
        .iter()
        .find(|method| method["name"] == "groundhog_requestAirdrop")
        .expect("requestAirdrop should exist in the canonical contract");
    assert_eq!(
        request_airdrop["result"]["schema"]["$ref"],
        "#/components/schemas/AirdropAttemptResponse"
    );
    assert_eq!(
        schemas["ClientNonce"]["pattern"],
        format!("^0x(?:[0-9a-f]{{2}}){{1,{MAX_CLIENT_NONCE_BYTES}}}$")
    );
    assert_eq!(
        schemas["AirdropAttemptResponse"]["oneOf"]
            .as_array()
            .map(Vec::len),
        Some(2),
        "airdrop responses should be a two-variant tagged union"
    );
    let airdrop_variants = schemas["AirdropAttemptResponse"]["oneOf"]
        .as_array()
        .expect("AirdropAttemptResponse should be a union");
    assert_eq!(
        airdrop_variants[0]["required"],
        serde_json::json!([
            "attemptStatus",
            "recipient",
            "rawAmount",
            "retryAfterSeconds"
        ])
    );
    assert!(airdrop_variants[0]["properties"].get("height").is_none());
    assert_eq!(
        airdrop_variants[1]["required"],
        serde_json::json!([
            "attemptStatus",
            "recipient",
            "txHash",
            "rawAmount",
            "height",
            "status"
        ])
    );
    assert!(
        airdrop_variants[1]["properties"]
            .get("retryAfterSeconds")
            .is_none()
    );
}

fn assert_error_docs_are_wired(
    spec: &serde_json::Value,
    methods: &[serde_json::Value],
    send_raw_transaction: &serde_json::Value,
) {
    let errors = spec["components"]["errors"]
        .as_object()
        .expect("components.errors should be an object");
    assert_eq!(
        errors.len(),
        ERROR_INFOS.len(),
        "every documented API error should be emitted once"
    );
    assert_eq!(
        errors["BackendUnavailable"]["code"],
        codes::BACKEND_UNAVAILABLE,
        "BackendUnavailable should expose the public JSON-RPC code"
    );
    for info in ERROR_INFOS {
        assert_eq!(
            error_info_by_code(info.code),
            Some(info),
            "catalog lookup should resolve every documented error code"
        );
        assert!(
            errors.contains_key(info.name),
            "components.errors missing {}",
            info.name
        );
        assert_eq!(
            errors[info.name]["x-name"], info.name,
            "error component should expose the catalog name as an extension"
        );
        assert_eq!(
            errors[info.name]["x-description"], info.description,
            "error component should expose the catalog description as an extension"
        );
        assert_eq!(
            errors[info.name]["name"],
            serde_json::Value::Null,
            "error component metadata should use x-name, not name"
        );
        assert_eq!(
            errors[info.name]["description"],
            serde_json::Value::Null,
            "error component metadata should use x-description, not description"
        );
        assert_eq!(
            errors[info.name]["data"],
            serde_json::Value::Null,
            "error.data is an example slot in OpenRPC; typed payload shapes use x-data-schema"
        );
    }

    assert_error_data_schemas_are_wired(errors);
    let send_errors = send_raw_transaction["errors"]
        .as_array()
        .expect("sendRawTransaction should document possible errors");
    assert!(
        send_errors
            .iter()
            .any(|error| error["$ref"] == "#/components/errors/TxRejected"),
        "sendRawTransaction should document scheduler rejection"
    );
    assert!(
        methods.iter().all(|method| {
            let name = method["name"].as_str().unwrap_or_default();
            INFALLIBLE_METHODS.contains(&name)
                || method["errors"]
                    .as_array()
                    .is_some_and(|errors| !errors.is_empty())
        }),
        "every fallible current method should document at least one JSON-RPC error"
    );
    for method in methods {
        for error in method["errors"]
            .as_array()
            .expect("method errors should be an array")
        {
            let reference = error["$ref"]
                .as_str()
                .expect("method error should be a $ref string");
            let key = reference
                .strip_prefix("#/components/errors/")
                .expect("method error $ref should target components.errors");
            assert!(
                errors.contains_key(key),
                "method {} references missing error component {}",
                method["name"],
                key
            );
        }
    }
}

fn assert_error_data_schemas_are_wired(errors: &serde_json::Map<String, serde_json::Value>) {
    let tx_invalid_data = &errors["TxInvalid"]["x-data-schema"];
    assert!(
        tx_invalid_data.is_object(),
        "TxInvalid should document its structured data payload"
    );
    let reasons: Vec<&str> = tx_invalid_data["oneOf"]
        .as_array()
        .expect("TxInvalid data schema should be a tagged union")
        .iter()
        .map(|variant| {
            variant["properties"]["reason"]["const"]
                .as_str()
                .expect("each TxInvalid data variant should pin its reason tag")
        })
        .collect();
    assert_eq!(
        reasons,
        [
            "tooManyContracts",
            "contractTooLarge",
            "duplicateContract",
            "rv32MemoryTooLarge",
            "rv32CodeTooLarge",
        ],
        "TxInvalid data variants should be discriminated by camelCase reason tags"
    );
    let funding = &errors["LocalFundingUnresolved"]["x-data-schema"];
    assert_eq!(funding["type"], "object");
    assert_eq!(
        funding["required"],
        serde_json::json!(["txHash", "signedTransaction"])
    );
    assert_eq!(funding["properties"]["txHash"]["$ref"], "#/$defs/TxHash");
    assert_eq!(
        funding["properties"]["signedTransaction"]["$ref"],
        "#/$defs/HexBytes"
    );
    assert!(
        errors
            .values()
            .filter(|error| !matches!(
                error["x-name"].as_str(),
                Some("TxInvalid" | "LocalFundingUnresolved" | "ViewHeightUnavailable")
            ))
            .all(|error| error["x-data-schema"].is_null()),
        "only transaction validation, unresolved funding, and view height errors document structured data"
    );
    let height_data = &errors["ViewHeightUnavailable"]["x-data-schema"];
    assert!(height_data["properties"]["requested"].is_object());
    assert!(height_data["properties"]["observed"].is_object());
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_PATH)
}
