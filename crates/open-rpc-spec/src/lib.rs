use groundhog_open_rpc::{Project, ProjectInfo};
use groundhog_rpc_api::{GroundhogApiOpenRpc, GroundhogDevApiOpenRpc};

/// Build the canonical `OpenRPC` artifact for the current RPC surface.
///
/// The returned JSON is recursively key-sorted so the checked-in
/// snapshot diff is deterministic across runs.
pub fn build_spec() -> serde_json::Value {
    let mut spec = Project::new(ProjectInfo {
        title: "Groundhog JSON-RPC".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    });
    spec.module(GroundhogApiOpenRpc::module_doc);
    spec.module(GroundhogDevApiOpenRpc::module_doc);
    canonicalize(spec.to_json())
}

fn canonicalize(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
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
