//! Native contract and method-specific error schemas.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use thruput_json_rpc_api::{
    WIRE_FORMAT_VERSION, native::NativeApiOpenRpc, types::error::native as errors,
};
use thruput_open_rpc::{Project, ProjectInfo};

/// Build the complete native contract.
///
/// This artifact describes typed transport bindings, not available backend
/// capabilities. Production discovery lists only methods registered by configured adapters.
pub fn build_spec() -> Value {
    let mut project = Project::new(ProjectInfo {
        title: "Thruput JSON-RPC".to_owned(),
        version: WIRE_FORMAT_VERSION.to_owned(),
    });
    project.module(NativeApiOpenRpc::module_doc);
    let mut document = project.to_json();
    // Error-data references contain method indices, so attach them only after
    // the generated declaration has established the final method order.
    add_error_docs(&mut document).expect("bug: native declarations must match the error catalogue");
    crate::canonicalize(document)
}

/// A document cannot safely receive native application-error documentation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ErrorDocsError {
    /// A container has an unexpected JSON shape.
    #[error("invalid OpenRPC shape at {0}")]
    InvalidShape(&'static str),
    /// A method is not part of the native contract, including legacy methods.
    #[error("unknown native OpenRPC method: {0}")]
    UnknownMethod(String),
    /// Native methods must be unique within a document.
    #[error("duplicate native OpenRPC method: {0}")]
    DuplicateMethod(String),
}

/// Attach the native error catalogue to an explicitly supplied native document.
///
/// The document may contain any subset of native methods. All method names and
/// affected containers are checked before mutation. This does not validate the
/// full `OpenRPC` document, register methods, or change [`crate::build_spec`].
///
/// `errors` contains ordinary `OpenRPC` references. Each method's `x-data-schema`
/// maps component names to generated, method-specific error-data schemas. `OpenRPC`
/// 1.2 permits extensions on methods, but not on error or reference objects.
/// Local schema references are rebased to their document paths; call this after
/// ordering methods, and again after changing their order.
pub fn add_error_docs(document: &mut Value) -> Result<(), ErrorDocsError> {
    validate(document)?;

    let methods = document["methods"]
        .as_array_mut()
        .expect("bug: methods validated as array");
    for (index, method) in methods.iter_mut().enumerate() {
        let name = method["name"]
            .as_str()
            .expect("bug: method name validated as string");
        let codes = errors::method_error_codes(name)
            .expect("bug: native method validated against catalogue");
        let mut references = Vec::with_capacity(codes.len());
        let mut schemas = Map::new();
        for code in codes {
            let info = errors::error_info_by_code(*code)
                .expect("bug: native method code has catalogue entry");
            references.push(json!({"$ref": format!("#/components/errors/{}", info.name)}));
            let mut schema = errors::method_data_schema(name, *code)
                .expect("bug: native method code has data schema");
            rebase_refs(
                &mut schema,
                &format!("#/methods/{index}/x-data-schema/{}", info.name),
            );
            schemas.insert(info.name.to_owned(), schema);
        }
        method["errors"] = references.into();
        method["x-data-schema"] = schemas.into();
    }

    let root = document
        .as_object_mut()
        .expect("bug: document validated as object");
    let components = root
        .entry("components")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("bug: components validated as object");
    components.insert(
        "errors".into(),
        errors::ERROR_INFOS
            .iter()
            .map(|info| {
                (
                    info.name.to_owned(),
                    json!({"code": info.code, "message": info.message}),
                )
            })
            .collect::<Map<_, _>>()
            .into(),
    );
    Ok(())
}

fn validate(document: &Value) -> Result<(), ErrorDocsError> {
    use ErrorDocsError::InvalidShape;
    let root = document.as_object().ok_or(InvalidShape("/"))?;
    if let Some(components) = root.get("components") {
        let components = components.as_object().ok_or(InvalidShape("/components"))?;
        if components.get("errors").is_some_and(|v| !v.is_object()) {
            return Err(InvalidShape("/components/errors"));
        }
    }
    let methods = root
        .get("methods")
        .and_then(Value::as_array)
        .ok_or(InvalidShape("/methods"))?;
    let mut seen = BTreeSet::new();
    for method in methods {
        let method = method.as_object().ok_or(InvalidShape("/methods/*"))?;
        let name = method
            .get("name")
            .and_then(Value::as_str)
            .ok_or(InvalidShape("/methods/*/name"))?;
        if errors::method_error_codes(name).is_none() {
            return Err(ErrorDocsError::UnknownMethod(name.into()));
        }
        if !seen.insert(name) {
            return Err(ErrorDocsError::DuplicateMethod(name.into()));
        }
        if !method.get("params").is_some_and(Value::is_array) {
            return Err(InvalidShape("/methods/*/params"));
        }
        if method.get("result").is_some_and(|v| !v.is_object()) {
            return Err(InvalidShape("/methods/*/result"));
        }
        if method.get("errors").is_some_and(|v| !v.is_array()) {
            return Err(InvalidShape("/methods/*/errors"));
        }
        if method.get("x-data-schema").is_some_and(|v| !v.is_object()) {
            return Err(InvalidShape("/methods/*/x-data-schema"));
        }
    }
    Ok(())
}

fn rebase_refs(schema: &mut Value, base: &str) {
    match schema {
        Value::Object(fields) => {
            if let Some(Value::String(reference)) = fields.get_mut("$ref")
                && let Some(local) = reference.strip_prefix('#')
            {
                *reference = format!("{base}{local}");
            }
            for value in fields.values_mut() {
                rebase_refs(value, base);
            }
        }
        Value::Array(values) => {
            for value in values {
                rebase_refs(value, base);
            }
        }
        _ => {}
    }
}
