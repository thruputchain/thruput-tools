//! `OpenRPC` document generation for the Groundhog JSON-RPC API.
//!
//! Provides types and helpers that the `thruput-open-rpc-macros` proc
//! macro expands into. Application code aggregates methods from
//! annotated jsonrpsee traits into a [`Project`], then serializes to
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
    /// Whether the parameter is required (`true` for all positional params).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// JSON Schema for the value.
    pub schema: serde_json::Value,
}

/// Grouping label attached to methods.
#[derive(Debug, Clone, Serialize)]
pub struct Tag {
    /// Display name of the tag.
    pub name: String,
}

/// One RPC method in the `OpenRPC` document.
#[derive(Debug, Clone, Serialize)]
pub struct MethodDoc {
    /// Fully qualified method name (e.g. `groundhog_chainTip`).
    pub name: String,
    /// One-line description extracted from doc comments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Tags for grouping in documentation UIs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
    /// Ordered list of parameter descriptors.
    pub params: Vec<ContentDescriptor>,
    /// Return-value descriptor.
    pub result: ContentDescriptor,
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

/// Build a [`ContentDescriptor`] for a method parameter whose type
/// implements [`JsonSchema`].
pub fn param_descriptor<T: JsonSchema>(
    generator: &mut SchemaGenerator,
    name: &str,
) -> ContentDescriptor {
    let schema = generator.subschema_for::<T>();
    ContentDescriptor {
        name: name.to_owned(),
        required: true,
        schema: schema_to_value(&schema),
    }
}

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
    fn project_serializes_to_openrpc_126() {
        let mut project = Project::new(ProjectInfo {
            title: "Test API".to_owned(),
            version: "0.1.0".to_owned(),
        });
        project.module(|module| {
            let params = vec![param_descriptor::<String>(module.generator(), "input")];
            let result = result_descriptor::<u64>(module.generator(), "output");
            module.add_method(MethodDoc {
                name: "test_echo".to_owned(),
                summary: Some("Echo a string back.".to_owned()),
                tags: vec![Tag {
                    name: "Demo".to_owned(),
                }],
                params,
                result,
            });
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
        assert_eq!(methods[0]["tags"][0]["name"], "Demo");
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
        project.module(|module| {
            let result = result_descriptor::<bool>(module.generator(), "ok");
            module.add_method(MethodDoc {
                name: "a".to_owned(),
                summary: None,
                tags: vec![],
                params: vec![],
                result,
            });
        });
        project.module(|module| {
            let result = result_descriptor::<bool>(module.generator(), "ok");
            module.add_method(MethodDoc {
                name: "b".to_owned(),
                summary: None,
                tags: vec![],
                params: vec![],
                result,
            });
        });

        let json = project.to_json();
        let methods = json["methods"]
            .as_array()
            .expect("methods should be an array");
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0]["name"], "a");
        assert_eq!(methods[1]["name"], "b");
    }

    #[test]
    fn param_descriptor_is_required() {
        let mut generator = openrpc_schema_generator();
        let desc = param_descriptor::<u64>(&mut generator, "count");
        assert!(desc.required);
    }

    #[test]
    fn shared_named_types_use_refs_and_one_component_entry() {
        let mut project = Project::new(ProjectInfo {
            title: "Shared".to_owned(),
            version: "1.0.0".to_owned(),
        });
        project.module(|module| {
            let params = vec![param_descriptor::<SharedType>(module.generator(), "input")];
            let result = result_descriptor::<SharedType>(module.generator(), "output");
            module.add_method(MethodDoc {
                name: "a".to_owned(),
                summary: None,
                tags: vec![],
                params,
                result,
            });
        });
        project.module(|module| {
            let params = vec![param_descriptor::<SharedType>(module.generator(), "input")];
            let result = result_descriptor::<bool>(module.generator(), "ok");
            module.add_method(MethodDoc {
                name: "b".to_owned(),
                summary: None,
                tags: vec![],
                params,
                result,
            });
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
            methods[0]["params"][0]["schema"]["$ref"],
            "#/components/schemas/SharedType"
        );
        assert_eq!(
            methods[0]["result"]["schema"]["$ref"],
            "#/components/schemas/SharedType"
        );
        assert_eq!(
            methods[1]["params"][0]["schema"]["$ref"],
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
            let result = result_descriptor::<u64>(module.generator(), "count");
            module.add_method(MethodDoc {
                name: "count".to_owned(),
                summary: None,
                tags: vec![],
                params: vec![],
                result,
            });
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
            let first_result = result_descriptor::<first::Foo>(module.generator(), "first");
            module.add_method(MethodDoc {
                name: "first".to_owned(),
                summary: None,
                tags: vec![],
                params: vec![],
                result: first_result,
            });
            let second_result = result_descriptor::<second::Foo>(module.generator(), "second");
            module.add_method(MethodDoc {
                name: "second".to_owned(),
                summary: None,
                tags: vec![],
                params: vec![],
                result: second_result,
            });
        });

        let schemas = project.to_json()["components"]["schemas"]
            .as_object()
            .expect("components.schemas should be an object")
            .clone();
        assert!(schemas.contains_key("Foo"));
        assert!(schemas.contains_key("SecondFoo"));
    }
}
