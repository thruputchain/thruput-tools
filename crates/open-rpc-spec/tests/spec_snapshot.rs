use std::fs;
use std::path::PathBuf;

use groundhog_open_rpc_spec::build_spec;
use pretty_assertions::assert_eq;

const SNAPSHOT_PATH: &str = "snapshots/groundhog-spec.json";

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
    assert_eq!(
        tx_hash_refs, 3,
        "TxHash should be reused across the real RPC surface"
    );

    let send_raw_transaction = methods
        .iter()
        .find(|method| method["name"] == "groundhog_sendRawTransaction")
        .expect("sendRawTransaction should exist");
    assert_eq!(
        send_raw_transaction["params"][0]["schema"]["type"], "string",
        "primitive parameter schemas should stay inline",
    );
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_PATH)
}
