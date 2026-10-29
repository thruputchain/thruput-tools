//! Markdown reference generated from the `OpenRPC` document.
//!
//! The JSON document is the contract. The reference shows patterns only on named
//! types and omits mutual-exclusion constraints.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

/// Render a Markdown method, error, and type reference from an `OpenRPC` document.
///
/// Error data is embedded per method in the document. Each data `kind` and each
/// error-data definition that is not a component schema is listed once, and must
/// render identically wherever it appears.
pub fn render_markdown(spec: &Value) -> String {
    let schemas = object(&spec["components"]["schemas"]);
    let methods = spec["methods"].as_array().map_or(&[][..], Vec::as_slice);
    let data_types = error_data_types(methods, schemas);

    let mut out = vec![
        format!(
            "# {}\n",
            spec["info"]["title"].as_str().unwrap_or("JSON-RPC")
        ),
        "<!-- Generated from thruput-spec.json by tests/spec_snapshot.rs. Do not edit. -->\n"
            .to_owned(),
        format!(
            "Wire format `{}`. Requests use JSON-RPC 2.0 with named parameters. \
             Fields marked `?` are optional.\n\n\
             This file lists all native methods. A server serves only the methods that \
             its backends support; `getVersionInfo` lists them. For exact schemas, see \
             [thruput-spec.json](thruput-spec.json).\n",
            spec["info"]["version"].as_str().unwrap_or_default()
        ),
        "## Methods\n".to_owned(),
        "| Method | Summary |\n| --- | --- |".to_owned(),
    ];
    for method in methods {
        let name = method["name"].as_str().unwrap_or_default();
        out.push(format!(
            "| [`{name}`](#{}) | {} |",
            anchor(name),
            cell(method["summary"].as_str().unwrap_or_default())
        ));
    }
    out.push(String::new());

    for method in methods {
        out.push(render_method(method, spec));
    }

    out.push("## Errors\n".to_owned());
    out.push("The `kind` field identifies the shape of `data`.\n".to_owned());
    out.push(
        "| Code | Error | Message | Data `kind` | Data fields |\n| --- | --- | --- | --- | --- |"
            .to_owned(),
    );
    let mut errors: Vec<_> = object(&spec["components"]["errors"]).iter().collect();
    errors.sort_by_key(|(_, error)| std::cmp::Reverse(error["code"].as_i64()));
    let data_kinds = error_data_kinds(methods);
    for (name, error) in errors {
        let message = cell(error["message"].as_str().unwrap_or_default());
        let kinds: Vec<_> = data_kinds
            .iter()
            .filter(|((error_name, _), _)| error_name == name)
            .collect();
        if kinds.is_empty() {
            out.push(format!("| {} | `{name}` | {message} | | |", error["code"]));
        }
        for ((_, kind), fields) in kinds {
            out.push(format!(
                "| {} | `{name}` | {message} | {} | {} |",
                error["code"],
                cell(kind),
                cell(fields)
            ));
        }
    }
    out.push(String::new());

    out.push("## Types\n".to_owned());
    for (name, schema) in schemas {
        out.push(render_type(name, schema));
    }

    if !data_types.is_empty() {
        out.push("## Error data types\n".to_owned());
        out.push("These types occur only in error `data`.\n".to_owned());
        out.extend(data_types.into_values());
    }

    let mut text = out.join("\n");
    text.truncate(text.trim_end().len());
    text.push('\n');
    text
}

/// Collect each error's data variants as summarized fields, keyed by error name and `kind`.
fn error_data_kinds(methods: &[Value]) -> BTreeMap<(String, String), String> {
    let mut kinds = BTreeMap::new();
    for method in methods {
        for (error_name, data) in object(&method["x-data-schema"]) {
            for (kind, fields) in variants(data) {
                if let Some(previous) =
                    kinds.insert((error_name.clone(), kind.clone()), fields.clone())
                {
                    assert_eq!(
                        previous, fields,
                        "bug: {error_name} data {kind} renders differently across methods"
                    );
                }
            }
        }
    }
    kinds
}

/// Collect error-data definitions that are not component schemas, rendered once by name.
fn error_data_types(methods: &[Value], schemas: &Map<String, Value>) -> BTreeMap<String, String> {
    let mut rendered = BTreeMap::new();
    for method in methods {
        for data in object(&method["x-data-schema"]).values() {
            for (name, schema) in object(&data["definitions"]) {
                if schemas.contains_key(name) {
                    continue;
                }
                let section = render_type(name, schema);
                if let Some(previous) = rendered.insert(name.clone(), section.clone()) {
                    assert_eq!(
                        previous, section,
                        "bug: error-data definition {name} renders differently across methods"
                    );
                }
            }
        }
    }
    rendered
}

fn render_method(method: &Value, spec: &Value) -> String {
    let name = method["name"].as_str().unwrap_or_default();
    let mut out = vec![
        format!("### `{name}`\n"),
        format!(
            "{}\n",
            prose(method["summary"].as_str().unwrap_or_default())
        ),
    ];

    let params = method["params"].as_array().map_or(&[][..], Vec::as_slice);
    if params.is_empty() {
        out.push("No parameters.\n".to_owned());
    } else {
        out.push(
            "| Parameter | Type | Required | Description |\n| --- | --- | --- | --- |".to_owned(),
        );
        for param in params {
            let description = param["description"]
                .as_str()
                .or_else(|| param["schema"]["description"].as_str())
                .unwrap_or_default();
            out.push(format!(
                "| `{}` | {} | {} | {} |",
                param["name"].as_str().unwrap_or_default(),
                cell(&type_expr(&param["schema"])),
                if param["required"] == true {
                    "yes"
                } else {
                    "no"
                },
                cell(description)
            ));
        }
        out.push(String::new());
    }

    let result = &method["result"];
    let result_line = match result["description"].as_str() {
        Some(description) => format!(
            "**Result:** {}. {}\n",
            type_expr(&result["schema"]),
            prose(description)
        ),
        None => format!("**Result:** {}\n", type_expr(&result["schema"])),
    };
    out.push(result_line);

    let errors = method["errors"].as_array().map_or(&[][..], Vec::as_slice);
    if !errors.is_empty() {
        out.push("| Code | [Error](#errors) | Data `kind` |\n| --- | --- | --- |".to_owned());
        for reference in errors {
            let error_name = reference["$ref"].as_str().map(ref_name).unwrap_or_default();
            let code = &spec["components"]["errors"][error_name]["code"];
            let kinds: Vec<_> = variants(&method["x-data-schema"][error_name])
                .into_iter()
                .map(|(kind, _)| kind)
                .collect();
            out.push(format!(
                "| {code} | `{error_name}` | {} |",
                cell(&kinds.join(", "))
            ));
        }
        out.push(String::new());
    }
    out.join("\n")
}

fn render_type(name: &str, schema: &Value) -> String {
    let mut out = vec![format!("### `{name}`\n")];
    if let Some(description) = schema["description"].as_str() {
        out.push(format!("{}\n", prose(description)));
    }
    let fields = object(schema);
    if fields.contains_key("properties") {
        out.push(field_table(schema));
        if schema["additionalProperties"] == false {
            out.push("The server rejects unknown fields.\n".to_owned());
        }
    } else if is_object_union(schema) {
        let (discriminator, variants) = discriminated_variants(schema);
        out.push(match discriminator {
            Some(key) => format!("| `{key}` | Other fields |\n| --- | --- |"),
            None => "| Variant | Fields |\n| --- | --- |".to_owned(),
        });
        for (label, fields) in variants {
            out.push(format!("| {} | {} |", cell(&label), cell(&fields)));
        }
        out.push(String::new());
    } else {
        out.push(format!(
            "Type: {}\n",
            type_expr(&without_description(schema))
        ));
        let constraints = constraints(fields);
        if !constraints.is_empty() {
            out.push(format!("Constraints: {}\n", constraints.join(", ")));
        }
    }
    out.join("\n")
}

fn field_table(schema: &Value) -> String {
    let required = required_set(schema);
    let mut out =
        vec!["| Field | Type | Required | Description |\n| --- | --- | --- | --- |".to_owned()];
    for (field, field_schema) in object(&schema["properties"]) {
        out.push(format!(
            "| `{field}` | {} | {} | {} |",
            cell(&type_expr(field_schema)),
            if required.contains(field.as_str()) {
                "yes"
            } else {
                "no"
            },
            cell(field_schema["description"].as_str().unwrap_or_default())
        ));
    }
    out.push(String::new());
    out.join("\n")
}

/// Label and summarize the object variants of a union by their discriminator value.
fn variants(schema: &Value) -> Vec<(String, String)> {
    discriminated_variants(schema).1
}

/// The discriminating field, if any, and each variant's label and summarized fields.
fn discriminated_variants(schema: &Value) -> (Option<&str>, Vec<(String, String)>) {
    let options = union_options(schema).unwrap_or_else(|| vec![schema]);
    let discriminator = discriminator(&options);
    let variants = options
        .into_iter()
        .map(|option| {
            let label = match discriminator {
                Some(key) => format!("`{}`", option["properties"][key]["const"]),
                None if object(option).contains_key("properties") => "object".to_owned(),
                None => type_expr(option),
            };
            (label, variant_fields(option, discriminator))
        })
        .collect();
    (discriminator, variants)
}

fn variant_fields(option: &Value, discriminator: Option<&str>) -> String {
    let required = required_set(option);
    let parts = option["allOf"].as_array().map_or(&[][..], Vec::as_slice);
    let mut fields: Vec<String> = object(&option["properties"])
        .iter()
        .filter(|(field, _)| Some(field.as_str()) != discriminator)
        .map(|(field, schema)| {
            let marker = if required.contains(field.as_str()) {
                ""
            } else {
                "?"
            };
            // An `allOf` part can narrow a field to specific values for this variant.
            let schema = parts
                .iter()
                .find_map(|part| part["properties"].get(field))
                .unwrap_or(schema);
            format!("`{field}{marker}`: {}", type_expr(schema))
        })
        .collect();
    for part in parts {
        if let Some(reference) = part["$ref"].as_str() {
            fields.push(format!("fields of {}", type_link(reference)));
        }
    }
    if fields.is_empty() {
        "none".to_owned()
    } else {
        fields.join(", ")
    }
}

/// The first field that every variant requires with a constant value.
fn discriminator<'a>(options: &[&'a Value]) -> Option<&'a str> {
    let (first, rest) = options.split_first()?;
    object(&first["properties"])
        .keys()
        .map(String::as_str)
        .find(|key| {
            std::iter::once(first).chain(rest).all(|option| {
                option["properties"][*key].get("const").is_some()
                    && required_set(option).contains(key)
            })
        })
}

/// Summarize a schema as an inline type expression.
fn type_expr(schema: &Value) -> String {
    let fields = match schema {
        Value::Object(fields) => fields,
        Value::Bool(true) => return "any".to_owned(),
        other => return format!("`{other}`"),
    };
    if let Some(reference) = fields.get("$ref").and_then(Value::as_str) {
        return type_link(reference);
    }
    if let Some(value) = fields.get("const") {
        return format!("`{value}`");
    }
    if let Some(Value::Array(values)) = fields.get("enum") {
        return values
            .iter()
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(options) = union_options(schema) {
        let mut seen = BTreeSet::new();
        return options
            .into_iter()
            .map(type_expr)
            .filter(|expr| seen.insert(expr.clone()))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(Value::Array(parts)) = fields.get("allOf") {
        let typed: Vec<_> = parts.iter().filter(|part| !is_constraint(part)).collect();
        if let [single] = typed[..]
            && !fields.contains_key("properties")
        {
            return type_expr(single);
        }
    }
    match fields.get("type") {
        Some(Value::String(ty)) => scalar(ty, fields),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .map(|ty| scalar(ty, fields))
            .collect::<Vec<_>>()
            .join(" | "),
        _ if fields.contains_key("properties") => "object".to_owned(),
        _ => format!("`{}`", Value::Object(fields.clone())),
    }
}

fn scalar(ty: &str, fields: &Map<String, Value>) -> String {
    match ty {
        "string" if fields.contains_key("x-integer-minimum") => "decimal string".to_owned(),
        "string" if pattern(fields).is_some_and(|pattern| pattern.starts_with("^0x")) => {
            "hex string".to_owned()
        }
        "integer" | "string" => match fields.get("format").and_then(Value::as_str) {
            Some(format) => format!("{ty} ({format})"),
            None => ty.to_owned(),
        },
        "array" => {
            let mut item = type_expr(fields.get("items").unwrap_or(&Value::Bool(true)));
            if item.contains(" | ") {
                item = format!("({item})");
            }
            let bounds = match (
                fields.get("minItems").and_then(Value::as_u64),
                fields.get("maxItems").and_then(Value::as_u64),
            ) {
                (Some(min), Some(max)) => format!(" ({min} to {max} items)"),
                (Some(min), None) => format!(" (at least {min})"),
                (None, Some(max)) => format!(" (at most {max})"),
                (None, None) => String::new(),
            };
            format!("{item}[]{bounds}")
        }
        other => other.to_owned(),
    }
}

fn constraints(fields: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    match (
        fields.get("x-integer-minimum").and_then(Value::as_str),
        fields.get("x-integer-maximum").and_then(Value::as_str),
    ) {
        (Some(min), Some(max)) => out.push(format!("value {min} to {max}")),
        _ => match (fields.get("minimum"), fields.get("maximum")) {
            (Some(min), Some(max)) => out.push(format!("value {min} to {max}")),
            (Some(min), None) => out.push(format!("value at least {min}")),
            (None, Some(max)) => out.push(format!("value at most {max}")),
            (None, None) => {}
        },
    }
    match (
        fields.get("minLength").and_then(Value::as_u64),
        fields.get("maxLength").and_then(Value::as_u64),
    ) {
        (Some(min), Some(max)) if min == max => out.push(format!("length {min}")),
        (Some(min), Some(max)) => out.push(format!("length {min} to {max}")),
        (Some(min), None) => out.push(format!("length at least {min}")),
        (None, Some(max)) => out.push(format!("length at most {max}")),
        (None, None) => {}
    }
    if let Some(pattern) = pattern(fields) {
        out.push(format!("pattern `{pattern}`"));
    }
    out
}

fn union_options(schema: &Value) -> Option<Vec<&Value>> {
    ["oneOf", "anyOf"]
        .into_iter()
        .find_map(|key| schema[key].as_array())
        .map(|options| options.iter().collect())
}

fn is_object_union(schema: &Value) -> bool {
    union_options(schema).is_some_and(|options| {
        options
            .iter()
            .all(|option| object(option).contains_key("properties"))
    })
}

/// Parts of `allOf` that only constrain presence, rather than contribute a type.
fn is_constraint(part: &Value) -> bool {
    object(part)
        .keys()
        .all(|key| matches!(key.as_str(), "not" | "dependencies"))
}

fn required_set(schema: &Value) -> BTreeSet<&str> {
    schema["required"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

fn without_description(schema: &Value) -> Value {
    let mut schema = schema.clone();
    if let Some(fields) = schema.as_object_mut() {
        fields.remove("description");
    }
    schema
}

fn pattern(fields: &Map<String, Value>) -> Option<&str> {
    fields.get("pattern").and_then(Value::as_str)
}

fn type_link(reference: &str) -> String {
    let name = ref_name(reference);
    format!("[`{name}`](#{})", anchor(name))
}

fn ref_name(reference: &str) -> &str {
    reference.rsplit('/').next().unwrap_or(reference)
}

/// GitHub heading anchor for a heading consisting of one code span.
fn anchor(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn object(value: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
    value.as_object().unwrap_or(&EMPTY)
}

/// Join wrapped description lines, keeping paragraph breaks.
fn prose(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Fit text into one Markdown table cell.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}
