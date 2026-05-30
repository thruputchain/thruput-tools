//! The optional JSON metadata sidecar emitted next to the artifact.
//!
//! Byte-compatible with the `scripts/rv32_lower.py` sidecar: keys sorted at
//! every level, two-space indentation, a trailing newline, and the build-info
//! fields always present (`null` when not supplied).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::artifact::{DispatchEntry, sha256_hex};

/// Inputs for [`render_metadata`].
#[derive(Debug)]
pub struct MetadataInput<'a> {
    /// The input ELF file bytes (hashed into `elf_sha256`).
    pub elf: &'a [u8],
    /// The full artifact bytes.
    pub artifact: &'a [u8],
    /// The code section.
    pub code: &'a [u8],
    /// The initial-memory image.
    pub initial_memory: &'a [u8],
    /// Discovered dispatch entries (sorted by method id).
    pub entries: &'a [DispatchEntry],
    /// Recorded `rustc` version (emitted as `null` when absent).
    pub rustc_version: Option<String>,
    /// Recorded target triple (emitted as `null` when absent).
    pub target_triple: Option<String>,
    /// Recorded build flags (emitted as `null` when absent).
    pub build_flags: Option<String>,
}

fn optional(value: Option<&String>) -> Value {
    value.map_or(Value::Null, |s| Value::String(s.clone()))
}

/// Render the metadata JSON document (with a trailing newline).
#[must_use]
pub fn render_metadata(input: &MetadataInput) -> String {
    // BTreeMap guarantees sorted keys independent of serde_json feature flags,
    // matching Python's `json.dumps(sort_keys=True)`.
    let entries: Vec<Value> = input
        .entries
        .iter()
        .map(|entry| {
            let mut map: BTreeMap<&str, Value> = BTreeMap::new();
            map.insert(
                "method_id",
                Value::String(format!("0x{:016X}", entry.method_id)),
            );
            map.insert("pc", Value::from(entry.pc));
            // to_value of a BTreeMap yields a sorted object.
            serde_json::to_value(map).expect("bug: dispatch entry serializes")
        })
        .collect();

    let mut doc: BTreeMap<&str, Value> = BTreeMap::new();
    doc.insert("elf_sha256", Value::String(sha256_hex(input.elf)));
    doc.insert("artifact_sha256", Value::String(sha256_hex(input.artifact)));
    doc.insert("code_sha256", Value::String(sha256_hex(input.code)));
    doc.insert(
        "initial_memory_sha256",
        Value::String(sha256_hex(input.initial_memory)),
    );
    doc.insert("code_bytes", Value::from(input.code.len()));
    doc.insert("memory_bytes", Value::from(input.initial_memory.len()));
    doc.insert("artifact_bytes", Value::from(input.artifact.len()));
    doc.insert("dispatch_entries", Value::Array(entries));
    doc.insert("rustc_version", optional(input.rustc_version.as_ref()));
    doc.insert("target_triple", optional(input.target_triple.as_ref()));
    doc.insert("build_flags", optional(input.build_flags.as_ref()));

    let mut out = serde_json::to_string_pretty(&doc).expect("bug: metadata serializes");
    out.push('\n');
    out
}
