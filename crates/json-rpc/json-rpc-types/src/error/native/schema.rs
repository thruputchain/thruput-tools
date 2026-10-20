use super::{
    ErrorData,
    catalogue::{EVIDENCE_FIELDS, allowed_evidence, kind_allowed, kind_code},
};
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

/// Standalone producer schema for all data refinements of a numeric code.
/// Future reasons accepted by readers are intentionally not producer values.
pub fn data_schema(code: i32) -> Option<Value> {
    schema(code, None)
}

/// Standalone producer schema narrowed to an invoked method's evidence. These
/// schemas describe field presence/domains, not cryptographic or receipt-hash
/// equality checks. `DecodedError` additionally checks retained hash equality.
pub fn method_data_schema(method: &str, code: i32) -> Option<Value> {
    schema(code, Some(method))
}

fn schema(code: i32, method: Option<&str>) -> Option<Value> {
    let generated = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<ErrorData>();
    let mut root = serde_json::to_value(generated).expect("bug: schema serialization cannot fail");
    let variants = root.get_mut("oneOf")?.as_array_mut()?;
    variants.retain(|variant| {
        let Some(kind) = variant["properties"]["kind"]["const"].as_str() else {
            return false;
        };
        kind_code(kind) == Some(code) && method.is_none_or(|m| kind_allowed(m, kind))
    });
    if variants.is_empty() {
        return None;
    }
    refine(&mut root, method);
    Some(root)
}

fn refine(variant: &mut Value, method: Option<&str>) {
    // Retained funding causes obey the same evidence rules as top-level stops.
    match variant {
        Value::Object(map) => {
            for child in map.values_mut() {
                refine(child, method);
            }
        }
        Value::Array(items) => {
            for child in items {
                refine(child, method);
            }
        }
        _ => return,
    }
    if variant["type"] != "object" {
        return;
    }
    let Some(tag) = variant["properties"]["kind"]["const"].as_str() else {
        return;
    };
    if kind_code(tag).is_none() {
        return;
    }
    let kind = variant["properties"]["kind"]["const"]
        .as_str()
        .expect("bug: tagged schema has kind")
        .to_owned();
    let mut conditions = Vec::new();
    if let Some(method) = method {
        for field in EVIDENCE_FIELDS {
            if !allowed_evidence(method, &kind, field) {
                conditions.push(json!({"not": {"required": [field]}}));
            }
        }
        if method == "sendTransaction" && allowed_evidence(method, &kind, "admission") {
            conditions.push(json!({"required": ["admission"]}));
        }
    }
    if kind == "transactionInvalid" {
        conditions.push(json!({"properties": {"admission": {"const": "notAttempted"}}}));
    }
    if kind == "transactionRejected" {
        conditions
            .push(json!({"properties": {"admission": {"enum": ["notAttempted", "rejected"]}}}));
    }
    if kind == "fundingAttemptStopped" {
        conditions.push(json!({"dependencies": {"signedTransaction": ["transactionHash"], "receipt": ["transactionHash"]}}));
    }
    if !conditions.is_empty() {
        let object = variant
            .as_object_mut()
            .expect("bug: tagged schema is object");
        object
            .entry("allOf")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("bug: generated allOf is an array")
            .extend(conditions);
    }
}
