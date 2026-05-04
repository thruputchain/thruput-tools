use groundhog_open_rpc::{Project, ProjectInfo};
use groundhog_rpc_api::{GroundhogApiOpenRpc, GroundhogDevApiOpenRpc};

/// Recursively sort all object keys so the snapshot is deterministic
/// regardless of whether `serde_json`'s `preserve_order` feature is
/// active (it gets unified across the workspace).
fn canonicalize(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .map(|(k, v)| (k, canonicalize(v)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect();
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(canonicalize).collect())
        }
        other => other,
    }
}

#[test]
fn openrpc_spec_snapshot() {
    let mut spec = Project::new(ProjectInfo {
        title: "Groundhog JSON-RPC".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    });
    spec.add_module(GroundhogApiOpenRpc::module_doc());
    spec.add_module(GroundhogDevApiOpenRpc::module_doc());

    let json = serde_json::to_string_pretty(&canonicalize(spec.to_json())).unwrap();

    insta::assert_snapshot!("openrpc", json);
}
