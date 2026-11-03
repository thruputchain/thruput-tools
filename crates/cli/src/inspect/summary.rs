//! Human presentation of the shared spec tool's JSON representation.

use serde_json::Value;

use crate::Print;

fn entries(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("?")
}

fn name(value: &Value) -> &str {
    text(&value["declared"])
}

fn fields(values: &Value, indexed: bool) -> Vec<String> {
    entries(values)
        .iter()
        .map(|field| {
            format!(
                "{}: {}{}",
                text(&field["name"]),
                type_label(&field["type"]),
                if indexed { " [indexed]" } else { "" }
            )
        })
        .collect()
}

fn type_label(value: &Value) -> String {
    match text(&value["kind"]) {
        "named" => name(&value["name"]).into(),
        "option" => format!("Option<{}>", type_label(&value["inner"])),
        "vec" => format!(
            "Vec<{}; max {}>",
            type_label(&value["element"]),
            value["max_len"]
        ),
        "array" => format!("[{}; {}]", type_label(&value["element"]), value["len"]),
        "tuple" => format!(
            "({})",
            entries(&value["items"])
                .iter()
                .map(type_label)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "struct" => format!("{{ {} }}", fields(&value["fields"], false).join(", ")),
        "signed_token" if value["preimage"].is_null() => "SignedToken".into(),
        "signed_token" => format!("SignedToken<{}>", type_label(&value["preimage"])),
        kind => kind.into(),
    }
}

fn method_signature(method: &Value) -> String {
    let args = fields(&method["args"], false).join(", ");
    let result = type_label(&method["return_type"]);
    let result = if result == "void" {
        String::new()
    } else {
        format!(" → {result}")
    };
    format!("{}({args}){result}", text(&method["name"]))
}

type Rows = Vec<(String, String)>;

fn line(rows: &mut Rows, value: impl Into<String>) {
    rows.push((String::new(), value.into()));
}

fn section(rows: &mut Rows, headings: &mut Vec<usize>, title: &str) {
    line(rows, "");
    headings.push(rows.len());
    line(rows, title);
}

fn definition_rows(definition: &Value, rows: &mut Rows) {
    line(rows, format!("  {}", name(&definition["name"])));
    match text(&definition["kind"]) {
        "enum" => {
            for variant in entries(&definition["variants"]) {
                line(
                    rows,
                    format!("    {} = {}", text(&variant["name"]), variant["value"]),
                );
            }
        }
        "struct" => {
            for field in fields(&definition["fields"], false) {
                line(rows, format!("    {field}"));
            }
        }
        "union" => {
            for case in entries(&definition["cases"]) {
                line(
                    rows,
                    format!(
                        "    {} = {}: {}",
                        text(&case["name"]),
                        case["discriminant"],
                        type_label(&case["case_type"])
                    ),
                );
            }
        }
        _ => {}
    }
}

fn effect_label(effect: &Value) -> String {
    match effect.as_str() {
        Some("readOnly") => " [declared read-only]".into(),
        Some("stateChanging") => " [declared state-changing]".into(),
        Some(effect) => format!(" [declared {effect}]"),
        None if effect.is_null() => String::new(),
        None => format!(" [declared effect {effect}]"),
    }
}

fn exception_types(interfaces: &[Value]) -> Vec<&Value> {
    let mut seen = std::collections::HashSet::new();
    interfaces
        .iter()
        .flat_map(|interface| entries(&interface["throws"]))
        .map(|error| &error["type"])
        .filter(|ty| seen.insert(*ty))
        .collect()
}

pub(super) fn render(data: &Value, print: &Print) {
    let mut headings = Vec::new();
    let spec = &data["spec"];
    let interfaces = entries(&spec["interfaces"]);
    let title = if let [interface] = interfaces {
        format!(
            "{} {}",
            text(&interface["name"]),
            text(&interface["version"])
        )
    } else {
        "Contract".into()
    };
    let mut rows = vec![
        ("Address".into(), text(&data["address"]).into()),
        (
            "Hash".into(),
            data["deployment"]["hash"].as_str().unwrap_or("—").into(),
        ),
        ("Block".into(), text(&data["stateBlockHeight"]).into()),
    ];
    if data["specStatus"] != "available" {
        rows.push(("Spec status".into(), text(&data["specStatus"]).into()));
    }

    for interface in interfaces {
        let heading = if interfaces.len() == 1 {
            "Methods".into()
        } else {
            format!(
                "{} {} — Methods",
                text(&interface["name"]),
                text(&interface["version"])
            )
        };
        section(&mut rows, &mut headings, &heading);
        for method in entries(&interface["methods"]) {
            let effect = effect_label(&method["effect"]);
            line(&mut rows, format!("  {}{effect}", method_signature(method)));
        }
        if interfaces.len() > 1 && !entries(&interface["throws"]).is_empty() {
            let errors = entries(&interface["throws"])
                .iter()
                .map(|error| type_label(&error["type"]))
                .collect::<Vec<_>>();
            line(&mut rows, format!("  Throws: {}", errors.join(", ")));
        }
    }

    if !entries(&spec["events"]).is_empty() {
        section(&mut rows, &mut headings, "Events");
        for event in entries(&spec["events"]) {
            let mut args = fields(&event["topics"], true);
            args.extend(fields(&event["fields"], false));
            line(
                &mut rows,
                format!("  {}({})", name(&event["name"]), args.join(", ")),
            );
        }
    }

    let errors = exception_types(interfaces);
    let definitions = entries(&spec["type_defs"]);
    let definitions_by_name: std::collections::HashMap<_, _> = definitions
        .iter()
        .map(|definition| (&definition["name"], definition))
        .collect();
    let error_names: std::collections::HashSet<_> = errors
        .iter()
        .filter(|error| error["kind"] == "named")
        .map(|error| &error["name"])
        .collect();
    if !errors.is_empty() {
        section(&mut rows, &mut headings, "Errors");
        for error in &errors {
            if let Some(definition) = (error["kind"] == "named")
                .then(|| definitions_by_name.get(&error["name"]))
                .flatten()
            {
                definition_rows(definition, &mut rows);
            } else {
                line(&mut rows, format!("  {}", type_label(error)));
            }
        }
    }
    let types = definitions
        .iter()
        .filter(|definition| !error_names.contains(&definition["name"]))
        .collect::<Vec<_>>();
    if !types.is_empty() {
        section(&mut rows, &mut headings, "Types");
        for definition in types {
            definition_rows(definition, &mut rows);
        }
    }
    print.sectioned_card(&title, &rows, &headings);
}
