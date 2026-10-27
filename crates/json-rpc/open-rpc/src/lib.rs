//! `OpenRPC` document generation for the Thruput JSON-RPC API.
//!
//! Provides types and helpers that the `thruput-open-rpc-macros` proc
//! macro expands into. Application code aggregates methods from
//! annotated API traits into a [`Project`], then serializes to
//! the `OpenRPC` 1.2.6 JSON format.

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One parameter or return-value descriptor in an `OpenRPC` method.
#[derive(Debug, Clone, Serialize)]
pub struct ContentDescriptor {
    /// Parameter / result name.
    pub name: String,
    /// Whether the parameter is required.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// JSON Schema for the value.
    pub schema: serde_json::Value,
}

/// One RPC method in the `OpenRPC` document.
#[derive(Debug, Clone, Serialize)]
pub struct MethodDoc {
    /// Wire method name (e.g. `faucet_requestAirdrop`).
    pub name: String,
    /// One-line description extracted from doc comments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Field descriptors of the request object, from [`named_params`].
    pub params: Vec<ContentDescriptor>,
    /// Whole-object parameter contract, from [`named_params`].
    #[serde(flatten)]
    pub named_params: NamedParamsDoc,
    /// Return-value descriptor.
    pub result: ContentDescriptor,
}

/// Whole-object request schema and its `OpenRPC` parameter convention.
#[derive(Debug, Clone, Serialize)]
pub struct NamedParamsDoc {
    #[serde(rename = "paramStructure")]
    structure: ParamStructure,
    #[serde(rename = "x-params-schema")]
    schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
enum ParamStructure {
    #[serde(rename = "by-name")]
    ByName,
}

/// Generate field descriptors and the complete request schema from one DTO.
///
/// The DTO must have an object schema with explicit top-level properties.
/// Cross-field constraints remain in `x-params-schema`; descriptors alone cannot
/// express them. Non-object request types are a definition error.
/// Required descriptors precede optional ones, with names sorted in each group.
///
/// # Panics
///
/// Panics when `T`'s schema is not an object schema.
pub fn named_params<T: JsonSchema>(
    generator: &mut SchemaGenerator,
) -> (Vec<ContentDescriptor>, NamedParamsDoc) {
    let schema = schema_to_value(&T::json_schema(generator));
    assert_eq!(
        schema["type"], "object",
        "bug: named request must have an object schema"
    );
    let required = schema["required"].as_array();
    let mut params: Vec<_> = schema["properties"]
        .as_object()
        .into_iter()
        .flat_map(|fields| fields.iter())
        .map(|(name, value)| ContentDescriptor {
            name: name.clone(),
            required: required.is_some_and(|names| names.iter().any(|n| n == name)),
            schema: value.clone(),
        })
        .collect();
    params.sort_by(|a, b| {
        b.required
            .cmp(&a.required)
            .then_with(|| a.name.cmp(&b.name))
    });
    (
        params,
        NamedParamsDoc {
            structure: ParamStructure::ByName,
            schema,
        },
    )
}

/// Builder passed to generated `module_doc(...)` functions.
#[derive(Debug)]
pub struct ModuleBuilder<'a> {
    methods: Vec<MethodDoc>,
    generator: &'a mut SchemaGenerator,
}

impl ModuleBuilder<'_> {
    /// Append a method to the module being built.
    pub fn add_method(&mut self, method: MethodDoc) {
        self.methods.push(method);
    }

    /// Borrow the shared schema generator for descriptor construction.
    pub fn generator(&mut self) -> &mut SchemaGenerator {
        self.generator
    }
}

/// Title and version for the top-level `OpenRPC` `info` object.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    /// Human-readable API title.
    pub title: String,
    /// Semantic version string.
    pub version: String,
}

/// Full `OpenRPC` document aggregating one or more modules.
#[derive(Debug, Clone)]
pub struct Project {
    info: ProjectInfo,
    methods: Vec<MethodDoc>,
    generator: SchemaGenerator,
}

impl Project {
    /// Start building a new `OpenRPC` document.
    pub fn new(info: ProjectInfo) -> Self {
        Self {
            info,
            methods: Vec::new(),
            generator: openrpc_schema_generator(),
        }
    }

    /// Add all methods produced by a generated module companion.
    pub fn module(&mut self, f: impl FnOnce(&mut ModuleBuilder<'_>)) {
        let mut builder = ModuleBuilder {
            methods: Vec::new(),
            generator: &mut self.generator,
        };
        f(&mut builder);
        self.methods.extend(builder.methods);
    }

    /// Serialize to the `OpenRPC` 1.2.6 JSON format.
    pub fn to_json(&self) -> serde_json::Value {
        let mut json = serde_json::json!({
            "openrpc": "1.2.6",
            "info": {
                "title": self.info.title,
                "version": self.info.version,
            },
            "methods": serde_json::to_value(&self.methods)
                .expect("bug: MethodDoc serialization cannot fail"),
        });

        let definitions = self.generator.definitions().clone();
        if !definitions.is_empty() {
            json.as_object_mut()
                .expect("bug: OpenRPC document must be a JSON object")
                .insert(
                    "components".into(),
                    serde_json::json!({
                        "schemas": serde_json::Value::Object(definitions),
                    }),
                );
        }

        json
    }
}

// ---------------------------------------------------------------------------
// Builder helpers (called by generated code)
// ---------------------------------------------------------------------------

/// Build a [`ContentDescriptor`] for a method return value whose type
/// implements [`JsonSchema`].
pub fn result_descriptor<T: JsonSchema>(
    generator: &mut SchemaGenerator,
    name: &str,
) -> ContentDescriptor {
    let schema = generator.subschema_for::<T>();
    ContentDescriptor {
        name: name.to_owned(),
        required: false,
        schema: schema_to_value(&schema),
    }
}

fn openrpc_schema_generator() -> SchemaGenerator {
    SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.definitions_path = "#/components/schemas/".into();
            settings.meta_schema = None;
        })
        .into_generator()
}

fn schema_to_value(schema: &Schema) -> serde_json::Value {
    serde_json::to_value(schema).expect("bug: schemars Schema serialization cannot fail")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    struct SharedType {
        value: u64,
    }

    mod first {
        use schemars::JsonSchema;

        #[allow(dead_code)]
        #[derive(JsonSchema)]
        pub struct Foo {
            pub value: u64,
        }
    }

    mod second {
        use schemars::JsonSchema;

        #[allow(dead_code)]
        #[derive(JsonSchema)]
        #[schemars(rename = "SecondFoo")]
        pub struct Foo {
            pub flag: bool,
        }
    }

    #[test]
    fn named_params_order_required_before_optional() {
        #[allow(dead_code)]
        #[derive(JsonSchema)]
        #[schemars(rename_all = "camelCase", deny_unknown_fields)]
        struct MixedRequest {
            transaction_hash: String,
            limit: Option<u32>,
            address: String,
            cursor: Option<String>,
        }

        let mut generator = openrpc_schema_generator();
        let (params, named) = named_params::<MixedRequest>(&mut generator);
        let ordering: Vec<_> = params
            .iter()
            .map(|param| (param.name.as_str(), param.required))
            .collect();
        assert_eq!(
            ordering,
            [
                ("address", true),
                ("transactionHash", true),
                ("cursor", false),
                ("limit", false),
            ]
        );
        let expected = schema_to_value(&MixedRequest::json_schema(&mut generator));
        assert_eq!(named.schema, expected);
        for param in params {
            assert_eq!(param.schema, expected["properties"][&param.name]);
        }
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    struct EmptyRequest {}

    /// Add a method with request object `Request` returning `Response`.
    fn add_method<Request: JsonSchema, Response: JsonSchema>(
        module: &mut ModuleBuilder<'_>,
        name: &str,
        summary: Option<&str>,
        result_name: &str,
    ) {
        let (params, named_params) = named_params::<Request>(module.generator());
        let result = result_descriptor::<Response>(module.generator(), result_name);
        module.add_method(MethodDoc {
            name: name.to_owned(),
            summary: summary.map(str::to_owned),
            params,
            named_params,
            result,
        });
    }

    #[test]
    fn project_serializes_to_openrpc_126() {
        #[allow(dead_code)]
        #[derive(JsonSchema)]
        struct EchoRequest {
            input: String,
        }

        let mut project = Project::new(ProjectInfo {
            title: "Test API".to_owned(),
            version: "0.1.0".to_owned(),
        });
        project.module(|module| {
            add_method::<EchoRequest, u64>(
                module,
                "test_echo",
                Some("Echo a string back."),
                "output",
            );
        });

        let json = project.to_json();

        assert_eq!(json["openrpc"], "1.2.6");
        assert_eq!(json["info"]["title"], "Test API");
        assert_eq!(json["info"]["version"], "0.1.0");

        let methods = json["methods"]
            .as_array()
            .expect("methods should be an array");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["name"], "test_echo");
        assert_eq!(methods[0]["summary"], "Echo a string back.");
        assert!(methods[0].get("tags").is_none());
        assert_eq!(methods[0]["paramStructure"], "by-name");
        assert_eq!(methods[0]["x-params-schema"]["type"], "object");
        assert_eq!(methods[0]["params"][0]["name"], "input");
        assert!(
            methods[0]["params"][0]["required"]
                .as_bool()
                .unwrap_or(false)
        );
        assert_eq!(methods[0]["result"]["name"], "output");
        assert_eq!(methods[0]["params"][0]["schema"]["type"], "string");
        assert_eq!(methods[0]["result"]["schema"]["type"], "integer");
    }

    #[test]
    fn empty_project_has_no_methods_or_components() {
        let project = Project::new(ProjectInfo {
            title: "Empty".to_owned(),
            version: "0.0.0".to_owned(),
        });
        let json = project.to_json();
        assert_eq!(
            json["methods"]
                .as_array()
                .expect("methods should be an array")
                .len(),
            0
        );
        assert!(json.get("components").is_none());
    }

    #[test]
    fn multiple_modules_are_merged() {
        let mut project = Project::new(ProjectInfo {
            title: "Multi".to_owned(),
            version: "1.0.0".to_owned(),
        });
        project.module(|module| add_method::<EmptyRequest, bool>(module, "a", None, "ok"));
        project.module(|module| add_method::<EmptyRequest, bool>(module, "b", None, "ok"));

        let json = project.to_json();
        let methods = json["methods"]
            .as_array()
            .expect("methods should be an array");
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0]["name"], "a");
        assert_eq!(methods[1]["name"], "b");
    }

    #[test]
    fn shared_named_types_use_refs_and_one_component_entry() {
        #[allow(dead_code)]
        #[derive(JsonSchema)]
        struct SharedRequest {
            input: SharedType,
        }

        let mut project = Project::new(ProjectInfo {
            title: "Shared".to_owned(),
            version: "1.0.0".to_owned(),
        });
        project.module(|module| {
            add_method::<EmptyRequest, SharedType>(module, "a", None, "output");
        });
        project.module(|module| {
            add_method::<SharedRequest, bool>(module, "b", None, "ok");
        });

        let json = project.to_json();
        let schemas = json["components"]["schemas"]
            .as_object()
            .expect("components.schemas should be an object");
        assert_eq!(schemas.len(), 1);
        assert!(schemas.contains_key("SharedType"));

        let methods = json["methods"]
            .as_array()
            .expect("methods should be an array");
        assert_eq!(
            methods[0]["result"]["schema"]["$ref"],
            "#/components/schemas/SharedType"
        );
        assert_eq!(
            methods[1]["params"][0]["schema"]["$ref"],
            "#/components/schemas/SharedType"
        );
        assert_eq!(
            methods[1]["x-params-schema"]["properties"]["input"]["$ref"],
            "#/components/schemas/SharedType"
        );
    }

    #[test]
    fn primitive_schemas_stay_inline() {
        let mut project = Project::new(ProjectInfo {
            title: "Primitive".to_owned(),
            version: "1.0.0".to_owned(),
        });
        project.module(|module| {
            add_method::<EmptyRequest, u64>(module, "count", None, "count");
        });

        let json = project.to_json();
        assert!(json.get("components").is_none());
        assert_eq!(json["methods"][0]["result"]["schema"]["type"], "integer");
    }

    #[test]
    fn renamed_colliding_types_keep_distinct_component_keys() {
        let mut project = Project::new(ProjectInfo {
            title: "Rename".to_owned(),
            version: "1.0.0".to_owned(),
        });
        project.module(|module| {
            add_method::<EmptyRequest, first::Foo>(module, "first", None, "first");
            add_method::<EmptyRequest, second::Foo>(module, "second", None, "second");
        });

        let schemas = project.to_json()["components"]["schemas"]
            .as_object()
            .expect("components.schemas should be an object")
            .clone();
        assert!(schemas.contains_key("Foo"));
        assert!(schemas.contains_key("SecondFoo"));
    }
}
