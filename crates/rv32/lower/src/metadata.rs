//! The optional JSON metadata sidecar emitted next to the artifact.
//!
//! Keys are sorted at every level, two-space indentation, a trailing newline,
//! and the build-info fields are always present (`null` when not supplied).

use std::collections::BTreeMap;

use serde_json::Value;
use thruput_rv32_artifact::{FORMAT_FINGERPRINT, declared_syscall_api, sha256_hex};

use crate::lower::Lowered;

/// Inputs for [`render_metadata`].
#[derive(Debug)]
pub struct MetadataInput<'a> {
    /// The input ELF file bytes (hashed into `elf_sha256`).
    pub elf: &'a [u8],
    /// The full artifact bytes.
    pub artifact: &'a [u8],
    /// The code section.
    pub code: &'a [u8],
    /// The stored initial-memory span.
    pub initial_memory: &'a [u8],
    /// Guest data address where the stored span is staged.
    pub initial_memory_start: usize,
    /// Total guest data address space, in bytes.
    pub memory_bytes: usize,
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
    // BTreeMap guarantees sorted keys independent of serde_json feature flags.
    let mut doc: BTreeMap<&str, Value> = BTreeMap::new();
    doc.insert("elf_sha256", Value::String(sha256_hex(input.elf)));
    doc.insert("artifact_sha256", Value::String(sha256_hex(input.artifact)));
    doc.insert(
        "artifact_format_fingerprint",
        Value::String(format!("{FORMAT_FINGERPRINT:#018x}")),
    );
    doc.insert("code_sha256", Value::String(sha256_hex(input.code)));
    doc.insert(
        "initial_memory_sha256",
        Value::String(sha256_hex(input.initial_memory)),
    );
    doc.insert("code_bytes", Value::from(input.code.len()));
    doc.insert("memory_bytes", Value::from(input.memory_bytes));
    doc.insert(
        "initial_memory_bytes",
        Value::from(input.initial_memory.len()),
    );
    doc.insert(
        "initial_memory_start",
        Value::from(input.initial_memory_start),
    );
    doc.insert("artifact_bytes", Value::from(input.artifact.len()));
    // What the artifact declares, read back from its header.
    doc.insert(
        "syscall_api_version",
        declared_syscall_api(input.artifact).map_or(Value::Null, |v| Value::String(v.to_string())),
    );
    doc.insert("rustc_version", optional(input.rustc_version.as_ref()));
    doc.insert("target_triple", optional(input.target_triple.as_ref()));
    doc.insert("build_flags", optional(input.build_flags.as_ref()));

    let mut out = serde_json::to_string_pretty(&doc).expect("bug: metadata serializes");
    out.push('\n');
    out
}

/// Render a compact, one-line JSON summary of a successful lowering, for
/// `--json` / scripting: the output path, byte sizes, and digests. Distinct
/// from [`render_metadata`] (the on-disk sidecar) — this is the tool's
/// machine-readable stdout envelope.
#[must_use]
pub fn summary_json(out: &str, lowered: &Lowered) -> String {
    let doc = serde_json::json!({
        "status": "ok",
        "out": out,
        "artifact_bytes": lowered.artifact.len(),
        "artifact_format_fingerprint": format!("{FORMAT_FINGERPRINT:#018x}"),
        "artifact_sha256": sha256_hex(&lowered.artifact),
        "code_bytes": lowered.code.len(),
        "code_sha256": sha256_hex(&lowered.code),
        "memory_bytes": lowered.memory_bytes,
        "initial_memory_bytes": lowered.initial_memory.len(),
        "initial_memory_start": lowered.initial_memory_start,
        "syscall_api_version": lowered.syscall_api.to_string(),
        "warnings": lowered.warnings,
    });
    doc.to_string()
}
