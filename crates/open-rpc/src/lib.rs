//! `OpenRPC` document generation for the Groundhog JSON-RPC API.
//!
//! Provides types and helpers that the `groundhog-open-rpc-macros` proc
//! macro expands into.  Application code aggregates [`Module`]s from
//! annotated jsonrpsee traits into a [`Project`], then serializes to the
//! `OpenRPC` 1.2.6 JSON format.

use schemars::JsonSchema;
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

/// Collection of methods contributed by a single jsonrpsee trait.
#[derive(Debug, Clone, Default)]
pub struct Module {
    methods: Vec<MethodDoc>,
}

impl Module {
    /// Create an empty module.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a method to this module.
    pub fn add_method(&mut self, method: MethodDoc) {
        self.methods.push(method);
    }

    /// Iterate over the methods in declaration order.
    pub fn methods(&self) -> &[MethodDoc] {
        &self.methods
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
    modules: Vec<Module>,
}

impl Project {
    /// Start building a new `OpenRPC` document.
    pub fn new(info: ProjectInfo) -> Self {
        Self {
            info,
            modules: Vec::new(),
        }
    }

    /// Add all methods from a module to the document.
    pub fn add_module(&mut self, module: Module) {
        self.modules.push(module);
    }

    /// Serialize to the `OpenRPC` 1.2.6 JSON format.
    pub fn to_json(&self) -> serde_json::Value {
        let methods: Vec<&MethodDoc> = self.modules.iter().flat_map(Module::methods).collect();

        serde_json::json!({
            "openrpc": "1.2.6",
            "info": {
                "title": self.info.title,
                "version": self.info.version,
            },
            "methods": serde_json::to_value(&methods)
                .expect("bug: MethodDoc serialization cannot fail"),
        })
    }
}

// ---------------------------------------------------------------------------
// Builder helpers (called by generated code)
// ---------------------------------------------------------------------------

/// Build a [`ContentDescriptor`] for a method parameter whose type
/// implements [`JsonSchema`].
pub fn param_descriptor<T: JsonSchema>(name: &str) -> ContentDescriptor {
    let schema = schemars::schema_for!(T);
    ContentDescriptor {
        name: name.to_owned(),
        required: true,
        schema: serde_json::to_value(&schema)
            .expect("bug: schemars Schema serialization cannot fail"),
    }
}

/// Build a [`ContentDescriptor`] for a method return value whose type
/// implements [`JsonSchema`].
pub fn result_descriptor<T: JsonSchema>(name: &str) -> ContentDescriptor {
    let schema = schemars::schema_for!(T);
    ContentDescriptor {
        name: name.to_owned(),
        required: false,
        schema: serde_json::to_value(&schema)
            .expect("bug: schemars Schema serialization cannot fail"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_serializes_to_openrpc_126() {
        let mut module = Module::new();
        module.add_method(MethodDoc {
            name: "test_echo".to_owned(),
            summary: Some("Echo a string back.".to_owned()),
            tags: vec![Tag {
                name: "Demo".to_owned(),
            }],
            params: vec![param_descriptor::<String>("input")],
            result: result_descriptor::<u64>("output"),
        });

        let mut project = Project::new(ProjectInfo {
            title: "Test API".to_owned(),
            version: "0.1.0".to_owned(),
        });
        project.add_module(module);

        let json = project.to_json();

        assert_eq!(json["openrpc"], "1.2.6");
        assert_eq!(json["info"]["title"], "Test API");
        assert_eq!(json["info"]["version"], "0.1.0");

        let methods = json["methods"].as_array().unwrap();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["name"], "test_echo");
        assert_eq!(methods[0]["summary"], "Echo a string back.");
        assert_eq!(methods[0]["tags"][0]["name"], "Demo");
        assert_eq!(methods[0]["params"][0]["name"], "input");
        assert!(methods[0]["params"][0]["required"].as_bool().unwrap());
        assert_eq!(methods[0]["result"]["name"], "output");

        // Param schema should describe a string
        let param_schema = &methods[0]["params"][0]["schema"];
        assert_eq!(param_schema["type"], "string");

        // Result schema should describe an integer
        let result_schema = &methods[0]["result"]["schema"];
        assert!(result_schema["type"] == "integer" || result_schema["format"] == "uint64");
    }

    #[test]
    fn empty_project_has_no_methods() {
        let project = Project::new(ProjectInfo {
            title: "Empty".to_owned(),
            version: "0.0.0".to_owned(),
        });
        let json = project.to_json();
        assert_eq!(json["methods"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn multiple_modules_are_merged() {
        let mut m1 = Module::new();
        m1.add_method(MethodDoc {
            name: "a".to_owned(),
            summary: None,
            tags: vec![],
            params: vec![],
            result: result_descriptor::<bool>("ok"),
        });

        let mut m2 = Module::new();
        m2.add_method(MethodDoc {
            name: "b".to_owned(),
            summary: None,
            tags: vec![],
            params: vec![],
            result: result_descriptor::<bool>("ok"),
        });

        let mut project = Project::new(ProjectInfo {
            title: "Multi".to_owned(),
            version: "1.0.0".to_owned(),
        });
        project.add_module(m1);
        project.add_module(m2);

        let json = project.to_json();
        let methods = json["methods"].as_array().unwrap();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0]["name"], "a");
        assert_eq!(methods[1]["name"], "b");
    }

    #[test]
    fn param_descriptor_is_required() {
        let desc = param_descriptor::<u64>("count");
        assert!(desc.required);
    }
}
