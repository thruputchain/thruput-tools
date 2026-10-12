use schemars::generate::SchemaSettings;
use thruput_json_rpc_api::types::airdrop::LocalFundingErrorData;
use thruput_json_rpc_api::{
    AirdropApiOpenRpc, GroundhogApiOpenRpc, LocalFundingApiOpenRpc, WIRE_FORMAT_VERSION,
    types::error::{ERROR_INFOS, RpcErrorInfo, TxInvalidDetail, codes, error_info_by_code},
};
use thruput_open_rpc::{Project, ProjectInfo};

/// Build the canonical `OpenRPC` artifact for the current RPC surface.
///
/// The returned JSON is recursively key-sorted so the checked-in
/// snapshot diff is deterministic across runs.
pub fn build_spec() -> serde_json::Value {
    let mut spec = Project::new(ProjectInfo {
        title: "Groundhog JSON-RPC".to_owned(),
        version: WIRE_FORMAT_VERSION.to_owned(),
    });
    spec.module(GroundhogApiOpenRpc::module_doc);
    spec.module(AirdropApiOpenRpc::module_doc);
    spec.module(LocalFundingApiOpenRpc::module_doc);
    let mut json = spec.to_json();
    add_error_docs(&mut json);
    canonicalize(json)
}

fn add_error_docs(spec: &mut serde_json::Value) {
    let Some(root) = spec.as_object_mut() else {
        return;
    };
    let components = root
        .entry("components")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .expect("bug: components should be an object");

    let errors = ERROR_INFOS
        .iter()
        .map(|info| (info.name.to_owned(), error_object(info)))
        .collect();
    components.insert("errors".to_owned(), serde_json::Value::Object(errors));

    if let Some(methods) = root
        .get_mut("methods")
        .and_then(serde_json::Value::as_array_mut)
    {
        for method in methods {
            let Some(name) = method
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
            else {
                continue;
            };

            let refs: Vec<serde_json::Value> = method_error_codes(&name)
                .iter()
                .map(|code| {
                    error_info_by_code(*code)
                        .unwrap_or_else(|| panic!("bug: undocumented JSON-RPC error code {code}"))
                })
                .map(|info| {
                    serde_json::json!({
                        "$ref": format!("#/components/errors/{}", info.name)
                    })
                })
                .collect();
            method
                .as_object_mut()
                .expect("bug: methods should contain objects")
                .insert("errors".to_owned(), serde_json::Value::Array(refs));
        }
    }
}

fn error_object(info: &RpcErrorInfo) -> serde_json::Value {
    let mut object = serde_json::json!({
        "code": info.code,
        "message": info.message,
        "x-description": info.description,
        "x-name": info.name,
    });
    // OpenRPC's `data` on an error object is an example value, not a schema,
    // so the typed payload shape rides on an extension key. `data` itself
    // stays reserved.
    if let Some(schema) = error_data_schema(info.code) {
        object["x-data-schema"] = schema;
    }
    object
}

/// JSON Schema of the structured `data` a documented error may carry.
fn error_data_schema(code: i32) -> Option<serde_json::Value> {
    let schema = match code {
        codes::VIEW_HEIGHT_UNAVAILABLE => SchemaSettings::draft2020_12()
            .with(|settings| settings.meta_schema = None)
            .into_generator()
            .into_root_schema_for::<thruput_json_rpc_api::types::view::ViewHeightUnavailable>(),
        codes::TX_INVALID => SchemaSettings::draft2020_12()
            .with(|settings| settings.meta_schema = None)
            .into_generator()
            .into_root_schema_for::<TxInvalidDetail>(),
        codes::LOCAL_FUNDING_UNRESOLVED => SchemaSettings::draft2020_12()
            .with(|settings| settings.meta_schema = None)
            .into_generator()
            .into_root_schema_for::<LocalFundingErrorData>(),
        _ => return None,
    };
    Some(serde_json::to_value(schema).expect("bug: schemars Schema serialization cannot fail"))
}

fn method_error_codes(method: &str) -> &'static [i32] {
    match method {
        "groundhog_view" | "groundhog_viewBatch" => &[
            codes::INVALID_PARAMS,
            codes::VIEW_HEIGHT_UNAVAILABLE,
            codes::BACKEND_UNAVAILABLE,
            codes::BACKEND_TIMEOUT,
            codes::OVERLOADED,
            codes::NOT_IMPLEMENTED,
        ],
        "groundhog_sendRawTransaction" => &[
            codes::INVALID_PARAMS,
            codes::TX_INVALID,
            codes::TX_REJECTED,
            codes::BACKEND_UNAVAILABLE,
            codes::BACKEND_TIMEOUT,
            codes::OVERLOADED,
            codes::NOT_IMPLEMENTED,
        ],
        "groundhog_getTransactionReceipt" | "groundhog_getTransactionStatus" => &[
            codes::INVALID_PARAMS,
            codes::TX_NOT_FOUND,
            codes::BACKEND_UNAVAILABLE,
            codes::BACKEND_TIMEOUT,
            codes::OVERLOADED,
            codes::NOT_IMPLEMENTED,
        ],
        "groundhog_getBlockByNumber" => &[
            codes::INVALID_PARAMS,
            codes::BLOCK_NOT_FOUND,
            codes::BACKEND_UNAVAILABLE,
            codes::BACKEND_TIMEOUT,
            codes::OVERLOADED,
            codes::NOT_IMPLEMENTED,
        ],
        "groundhog_getBalance"
        | "groundhog_getAddressAuthData"
        | "groundhog_getEvents"
        | "groundhog_getTransactionsByAddress"
        | "groundhog_getStorageAt" => &[
            codes::INVALID_PARAMS,
            codes::BACKEND_UNAVAILABLE,
            codes::BACKEND_TIMEOUT,
            codes::OVERLOADED,
            codes::NOT_IMPLEMENTED,
        ],
        "groundhog_chainTip" | "groundhog_chainConfig" | "groundhog_pendingTransactionCount" => &[
            codes::BACKEND_UNAVAILABLE,
            codes::BACKEND_TIMEOUT,
            codes::OVERLOADED,
            codes::NOT_IMPLEMENTED,
        ],
        "groundhog_networkInfo" | "groundhog_serverLimits" | "groundhog_version" => &[],
        "groundhog_fundAccount" => &[
            codes::LOCAL_FUNDING_UNRESOLVED,
            codes::INVALID_PARAMS,
            codes::TX_REJECTED,
            codes::BACKEND_UNAVAILABLE,
            codes::BACKEND_TIMEOUT,
            codes::OVERLOADED,
        ],
        "groundhog_requestAirdrop" => &[
            codes::INVALID_PARAMS,
            codes::TX_REJECTED,
            codes::BACKEND_UNAVAILABLE,
            codes::BACKEND_TIMEOUT,
            codes::OVERLOADED,
        ],
        _ => panic!("bug: undocumented OpenRPC method `{method}`"),
    }
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
