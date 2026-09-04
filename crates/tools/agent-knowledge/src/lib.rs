//! Deterministic assembly and validation for Groundhog agent knowledge.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const SCHEMA_VERSION: u32 = 1;
const FRONTMATTER_OPEN: &str = "+++\n";
const FRONTMATTER_CLOSE: &str = "\n+++\n";
const CLI_VERSION_COMPONENT: &str = "groundhog-cli";
const CLI_VERSION_PLACEHOLDER: &str = "{{GROUNDHOG_CLI_VERSION}}";

/// Fully rendered artifacts for one knowledge bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleArtifacts {
    /// Normalized records, one compact JSON object per line.
    pub records_jsonl: String,
    /// Deterministic manifest describing compatibility and artifact hashes.
    pub manifest_json: String,
    /// Bootstrap skill template consumed by the released Groundhog CLI.
    pub skill_markdown: String,
}

/// A knowledge-source or snapshot validation failure.
#[derive(Debug, Error)]
pub enum KnowledgeError {
    /// A bundle file could not be read.
    #[error("failed to read {path}: {source}")]
    Read {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// A generated artifact could not be written.
    #[error("failed to write {path}: {source}")]
    Write {
        /// Path that could not be written.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// A TOML source file is invalid.
    #[error("invalid TOML in {path}: {source}")]
    Toml {
        /// Path containing invalid TOML.
        path: PathBuf,
        /// TOML parse failure.
        source: toml::de::Error,
    },
    /// A source violates the versioned knowledge contract.
    #[error("invalid agent knowledge: {0}")]
    Invalid(String),
    /// A generated snapshot no longer matches its sources.
    #[error("generated agent knowledge is stale: {0}")]
    Stale(PathBuf),
    /// A generated manifest could not be serialized.
    #[error("failed to serialize the agent knowledge manifest: {0}")]
    Json(serde_json::Error),
    /// An imported normalized record is invalid JSON.
    #[error("invalid JSON record at {path}:{line}: {source}")]
    JsonRecord {
        /// Import file containing invalid JSON.
        path: PathBuf,
        /// One-based JSONL line number.
        line: usize,
        /// JSON parse failure.
        source: serde_json::Error,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleSource {
    schema_version: u32,
    id: String,
    owners: Vec<String>,
    compatibility: BTreeMap<String, String>,
    skill: SkillSource,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillSource {
    name: String,
    description_lines: Vec<String>,
    version_component: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordHeader {
    schema_version: u32,
    id: String,
    owner: String,
    kind: String,
    audience: Vec<String>,
    components: Vec<String>,
    bootstrap_order: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NormalizedRecord {
    schema_version: u32,
    id: String,
    owner: String,
    kind: String,
    audience: Vec<String>,
    components: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bootstrap_order: Option<u32>,
    source: String,
    content: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedManifest<'a> {
    schema_version: u32,
    id: &'a str,
    owners: &'a [String],
    compatibility: &'a BTreeMap<String, String>,
    records: Vec<&'a str>,
    artifacts: BTreeMap<&'static str, GeneratedArtifact>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedArtifact {
    path: &'static str,
    sha256: String,
}

/// Assemble all source records below `bundle_root` into deterministic artifacts.
pub fn build_bundle(bundle_root: &Path) -> Result<BundleArtifacts, KnowledgeError> {
    let manifest_path = bundle_root.join("bundle.toml");
    let manifest_source = read_to_string(&manifest_path)?;
    let mut manifest: BundleSource =
        toml::from_str(&manifest_source).map_err(|source| KnowledgeError::Toml {
            path: manifest_path,
            source,
        })?;
    manifest.owners = sorted_unique(manifest.owners);

    let records_root = bundle_root.join("records");
    let mut records = Vec::new();
    for path in sorted_files(&records_root, "md", false)? {
        let source = read_to_string(&path)?;
        let relative = path.strip_prefix(bundle_root).map_err(|_| {
            KnowledgeError::Invalid(format!(
                "record path is outside bundle root: {}",
                path.display()
            ))
        })?;
        records.push(parse_record(relative, &path, &source)?);
    }
    let imports_root = bundle_root.join("imports");
    for path in sorted_files(&imports_root, "jsonl", true)? {
        records.extend(parse_import(&path)?);
    }

    assemble(&manifest, records)
}

/// Write generated snapshots below `bundle_root`.
pub fn write_bundle(bundle_root: &Path) -> Result<(), KnowledgeError> {
    let artifacts = build_bundle(bundle_root)?;
    let generated_root = bundle_root.join("generated");
    fs::create_dir_all(&generated_root).map_err(|source| KnowledgeError::Write {
        path: generated_root.clone(),
        source,
    })?;
    write_file(
        &generated_root.join("records.jsonl"),
        &artifacts.records_jsonl,
    )?;
    write_file(
        &generated_root.join("manifest.json"),
        &artifacts.manifest_json,
    )?;
    write_file(&generated_root.join("SKILL.md"), &artifacts.skill_markdown)
}

/// Check that all committed generated artifacts match their canonical sources.
pub fn check_bundle(bundle_root: &Path) -> Result<(), KnowledgeError> {
    let artifacts = build_bundle(bundle_root)?;
    let generated_root = bundle_root.join("generated");
    check_file(
        &generated_root.join("records.jsonl"),
        &artifacts.records_jsonl,
    )?;
    check_file(
        &generated_root.join("manifest.json"),
        &artifacts.manifest_json,
    )?;
    check_file(&generated_root.join("SKILL.md"), &artifacts.skill_markdown)
}

fn assemble(
    manifest: &BundleSource,
    mut records: Vec<NormalizedRecord>,
) -> Result<BundleArtifacts, KnowledgeError> {
    validate_manifest(manifest)?;
    validate_records(manifest, &records)?;

    records.sort_by(|left, right| left.id.cmp(&right.id));
    let mut records_jsonl = String::new();
    for record in &records {
        records_jsonl.push_str(&serde_json::to_string(record).map_err(KnowledgeError::Json)?);
        records_jsonl.push('\n');
    }

    let skill_markdown = render_skill(manifest, &records)?;
    let record_ids = records.iter().map(|record| record.id.as_str()).collect();
    let artifacts = BTreeMap::from([
        (
            "records",
            GeneratedArtifact {
                path: "generated/records.jsonl",
                sha256: sha256(&records_jsonl),
            },
        ),
        (
            "skillTemplate",
            GeneratedArtifact {
                path: "generated/SKILL.md",
                sha256: sha256(&skill_markdown),
            },
        ),
    ]);
    let compatibility = generated_compatibility(manifest);
    let generated_manifest = GeneratedManifest {
        schema_version: SCHEMA_VERSION,
        id: &manifest.id,
        owners: &manifest.owners,
        compatibility: &compatibility,
        records: record_ids,
        artifacts,
    };
    let mut manifest_json =
        serde_json::to_string_pretty(&generated_manifest).map_err(KnowledgeError::Json)?;
    manifest_json.push('\n');

    Ok(BundleArtifacts {
        records_jsonl,
        manifest_json,
        skill_markdown,
    })
}

fn generated_compatibility(manifest: &BundleSource) -> BTreeMap<String, String> {
    manifest
        .compatibility
        .iter()
        .map(|(component, version)| {
            let version =
                if component == CLI_VERSION_COMPONENT && version == CLI_VERSION_PLACEHOLDER {
                    env!("CARGO_PKG_VERSION").to_owned()
                } else {
                    version.clone()
                };
            (component.clone(), version)
        })
        .collect()
}

fn parse_record(
    relative_path: &Path,
    full_path: &Path,
    source: &str,
) -> Result<NormalizedRecord, KnowledgeError> {
    if source.contains('\r') {
        return Err(KnowledgeError::Invalid(format!(
            "{} must use LF line endings",
            relative_path.display()
        )));
    }
    let source = source.strip_prefix(FRONTMATTER_OPEN).ok_or_else(|| {
        KnowledgeError::Invalid(format!(
            "{} must start with TOML frontmatter",
            relative_path.display()
        ))
    })?;
    let (header, content) = source.split_once(FRONTMATTER_CLOSE).ok_or_else(|| {
        KnowledgeError::Invalid(format!(
            "{} has unterminated TOML frontmatter",
            relative_path.display()
        ))
    })?;
    let header: RecordHeader = toml::from_str(header).map_err(|source| KnowledgeError::Toml {
        path: full_path.to_path_buf(),
        source,
    })?;
    if content.is_empty() || !content.ends_with('\n') || content.ends_with("\n\n") {
        return Err(KnowledgeError::Invalid(format!(
            "{} content must be non-empty and end with exactly one newline",
            relative_path.display()
        )));
    }

    Ok(NormalizedRecord {
        schema_version: header.schema_version,
        id: header.id,
        owner: header.owner,
        kind: header.kind,
        audience: sorted_unique(header.audience),
        components: sorted_unique(header.components),
        bootstrap_order: header.bootstrap_order,
        source: relative_path.to_string_lossy().replace('\\', "/"),
        content: content.to_owned(),
    })
}

fn parse_import(path: &Path) -> Result<Vec<NormalizedRecord>, KnowledgeError> {
    let source = read_to_string(path)?;
    if source.contains('\r') || !source.ends_with('\n') {
        return Err(KnowledgeError::Invalid(format!(
            "{} must use LF line endings and end with one newline",
            path.display()
        )));
    }
    let mut records = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if line.is_empty() {
            return Err(KnowledgeError::Invalid(format!(
                "{}:{} is an empty JSONL record",
                path.display(),
                index + 1
            )));
        }
        let mut record: NormalizedRecord =
            serde_json::from_str(line).map_err(|source| KnowledgeError::JsonRecord {
                path: path.to_path_buf(),
                line: index + 1,
                source,
            })?;
        record.audience = sorted_unique(record.audience);
        record.components = sorted_unique(record.components);
        validate_import_source(path, index + 1, &record)?;
        records.push(record);
    }
    Ok(records)
}

fn validate_import_source(
    path: &Path,
    line: usize,
    record: &NormalizedRecord,
) -> Result<(), KnowledgeError> {
    let owner_prefix = format!("{}:", record.owner);
    let source_path = record.source.strip_prefix(&owner_prefix).ok_or_else(|| {
        KnowledgeError::Invalid(format!(
            "{}:{line} record {} source must start with {owner_prefix}",
            path.display(),
            record.id
        ))
    })?;
    let canonical = !source_path.is_empty()
        && !source_path.starts_with('/')
        && !source_path.ends_with('/')
        && !source_path.contains(['\\', '\r', '\n'])
        && source_path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if !canonical {
        return Err(KnowledgeError::Invalid(format!(
            "{}:{line} record {} source must be an owner-qualified canonical relative path",
            path.display(),
            record.id
        )));
    }
    Ok(())
}

fn validate_manifest(manifest: &BundleSource) -> Result<(), KnowledgeError> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(KnowledgeError::Invalid(format!(
            "bundle schema version {} is unsupported",
            manifest.schema_version
        )));
    }
    validate_id("bundle id", &manifest.id)?;
    validate_id("skill name", &manifest.skill.name)?;
    if manifest.owners.is_empty() || manifest.compatibility.is_empty() {
        return Err(KnowledgeError::Invalid(
            "bundle owners and compatibility must be non-empty".to_owned(),
        ));
    }
    if manifest.skill.description_lines.is_empty()
        || manifest
            .skill
            .description_lines
            .iter()
            .any(|line| line.is_empty() || line.starts_with(' ') || line.ends_with(' '))
    {
        return Err(KnowledgeError::Invalid(
            "skill description lines must be non-empty and unpadded".to_owned(),
        ));
    }
    if !manifest
        .compatibility
        .contains_key(&manifest.skill.version_component)
    {
        return Err(KnowledgeError::Invalid(format!(
            "skill version component {} is not in compatibility",
            manifest.skill.version_component
        )));
    }
    for owner in &manifest.owners {
        validate_id("owner", owner)?;
    }
    for (component, version) in &manifest.compatibility {
        validate_id("component", component)?;
        if version.is_empty() {
            return Err(KnowledgeError::Invalid(format!(
                "component {component} has an empty version"
            )));
        }
    }
    Ok(())
}

fn validate_records(
    manifest: &BundleSource,
    records: &[NormalizedRecord],
) -> Result<(), KnowledgeError> {
    if records.is_empty() {
        return Err(KnowledgeError::Invalid(
            "bundle must contain at least one record".to_owned(),
        ));
    }
    let owners = manifest
        .owners
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut ids = BTreeSet::new();
    let mut bootstrap_orders = BTreeSet::new();
    for record in records {
        if record.schema_version != SCHEMA_VERSION {
            return Err(KnowledgeError::Invalid(format!(
                "record {} has unsupported schema version {}",
                record.id, record.schema_version
            )));
        }
        validate_id("record id", &record.id)?;
        validate_id("record kind", &record.kind)?;
        if !ids.insert(record.id.as_str()) {
            return Err(KnowledgeError::Invalid(format!(
                "duplicate record id {}",
                record.id
            )));
        }
        if !owners.contains(record.owner.as_str()) {
            return Err(KnowledgeError::Invalid(format!(
                "record {} has unknown owner {}",
                record.id, record.owner
            )));
        }
        if record.audience.is_empty() || record.components.is_empty() {
            return Err(KnowledgeError::Invalid(format!(
                "record {} must name an audience and component",
                record.id
            )));
        }
        if record.source.is_empty()
            || record.content.is_empty()
            || record.content.contains('\r')
            || !record.content.ends_with('\n')
            || record.content.ends_with("\n\n")
        {
            return Err(KnowledgeError::Invalid(format!(
                "record {} must name its source and have content ending with one newline",
                record.id
            )));
        }
        for audience in &record.audience {
            validate_id("audience", audience)?;
        }
        for component in &record.components {
            if !manifest.compatibility.contains_key(component) {
                return Err(KnowledgeError::Invalid(format!(
                    "record {} references unknown component {}",
                    record.id, component
                )));
            }
        }
        if let Some(order) = record.bootstrap_order
            && !bootstrap_orders.insert(order)
        {
            return Err(KnowledgeError::Invalid(format!(
                "duplicate bootstrap order {order}"
            )));
        }
    }
    Ok(())
}

fn validate_id(label: &str, value: &str) -> Result<(), KnowledgeError> {
    let valid = !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
        && !value.starts_with(['.', '-', '_'])
        && !value.ends_with(['.', '-', '_']);
    if !valid {
        return Err(KnowledgeError::Invalid(format!(
            "{label} {value:?} must be a stable lowercase identifier"
        )));
    }
    Ok(())
}

fn render_skill(
    manifest: &BundleSource,
    records: &[NormalizedRecord],
) -> Result<String, KnowledgeError> {
    let version = &manifest.compatibility[&manifest.skill.version_component];
    let mut bootstrap = records
        .iter()
        .filter_map(|record| record.bootstrap_order.map(|order| (order, record)))
        .collect::<Vec<_>>();
    bootstrap.sort_by(|(left_order, left), (right_order, right)| {
        left_order
            .cmp(right_order)
            .then_with(|| left.id.cmp(&right.id))
    });
    if bootstrap.is_empty() {
        return Err(KnowledgeError::Invalid(
            "bundle must select at least one bootstrap record".to_owned(),
        ));
    }

    let mut skill = format!("---\nname: {}\ndescription: >\n", manifest.skill.name);
    for line in &manifest.skill.description_lines {
        skill.push_str("  ");
        skill.push_str(line);
        skill.push('\n');
    }
    skill.push_str("---\n\n<!-- ");
    skill.push_str(&manifest.skill.version_component);
    skill.push_str("-version: ");
    skill.push_str(version);
    skill.push_str(" -->\n\n");
    for (index, (_, record)) in bootstrap.iter().enumerate() {
        if index > 0 {
            skill.push('\n');
        }
        skill.push_str(&record.content);
    }
    Ok(skill)
}

fn sorted_unique(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn sha256(contents: &str) -> String {
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(contents.as_bytes()))
    )
}

fn read_to_string(path: &Path) -> Result<String, KnowledgeError> {
    fs::read_to_string(path).map_err(|source| KnowledgeError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn sorted_files(
    root: &Path,
    extension: &str,
    optional: bool,
) -> Result<Vec<PathBuf>, KnowledgeError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(source) => {
            return Err(KnowledgeError::Read {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| KnowledgeError::Read {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|candidate| candidate == extension)
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn write_file(path: &Path, contents: &str) -> Result<(), KnowledgeError> {
    fs::write(path, contents).map_err(|source| KnowledgeError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn check_file(path: &Path, expected: &str) -> Result<(), KnowledgeError> {
    let actual = read_to_string(path)?;
    if actual == expected {
        Ok(())
    } else {
        Err(KnowledgeError::Stale(path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cli/assets/groundhog/knowledge")
    }

    #[test]
    fn checked_in_bundle_is_current() {
        check_bundle(&bundle_root()).unwrap();
    }

    #[test]
    fn assembly_is_deterministic() {
        assert_eq!(
            build_bundle(&bundle_root()).unwrap(),
            build_bundle(&bundle_root()).unwrap()
        );
    }

    #[test]
    fn generated_manifest_has_concrete_cli_version() {
        let artifacts = build_bundle(&bundle_root()).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&artifacts.manifest_json).unwrap();

        assert_eq!(
            manifest["compatibility"][CLI_VERSION_COMPONENT],
            env!("CARGO_PKG_VERSION")
        );
        assert!(!artifacts.manifest_json.contains(CLI_VERSION_PLACEHOLDER));
        assert!(artifacts.skill_markdown.contains(CLI_VERSION_PLACEHOLDER));
    }

    #[test]
    fn duplicate_record_ids_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let records_root = root.path().join("records");
        fs::create_dir(&records_root).unwrap();
        write_test_manifest(root.path(), r#"["groundhog-rpc"]"#);
        let record = test_record("duplicate", "groundhog-rpc", 10);
        fs::write(records_root.join("a.md"), &record).unwrap();
        fs::write(records_root.join("b.md"), record).unwrap();

        let error = build_bundle(root.path()).unwrap_err().to_string();
        assert!(error.contains("duplicate record id duplicate"));
    }

    #[test]
    fn duplicate_bootstrap_orders_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let records_root = root.path().join("records");
        fs::create_dir(&records_root).unwrap();
        write_test_manifest(root.path(), r#"["groundhog-rpc"]"#);
        fs::write(
            records_root.join("first.md"),
            test_record("first", "groundhog-rpc", 10),
        )
        .unwrap();
        fs::write(
            records_root.join("second.md"),
            test_record("second", "groundhog-rpc", 10),
        )
        .unwrap();

        let error = build_bundle(root.path()).unwrap_err().to_string();
        assert!(error.contains("duplicate bootstrap order 10"));
    }

    #[test]
    fn imported_records_are_merged_and_normalized() {
        let root = tempfile::tempdir().unwrap();
        let records_root = root.path().join("records");
        let imports_root = root.path().join("imports");
        fs::create_dir(&records_root).unwrap();
        fs::create_dir(&imports_root).unwrap();
        write_test_manifest(root.path(), r#"["groundhog-rpc", "sdk_rs"]"#);
        fs::write(
            records_root.join("local.md"),
            test_record("z-local", "groundhog-rpc", 20),
        )
        .unwrap();
        fs::write(
            imports_root.join("sdk.jsonl"),
            concat!(
                r##"{"schemaVersion":1,"id":"a-imported","owner":"sdk_rs","kind":"api","audience":["developer","coding-agent"],"components":["groundhog-cli"],"bootstrapOrder":10,"source":"sdk_rs:crates/sdk/src/lib.rs","content":"# Imported\n"}"##,
                "\n"
            ),
        )
        .unwrap();

        let artifacts = build_bundle(root.path()).unwrap();
        let lines = artifacts.records_jsonl.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(r#""id":"a-imported""#));
        assert!(lines[0].contains(r#""audience":["coding-agent","developer"]"#));
        assert!(
            artifacts.skill_markdown.find("# Imported").unwrap()
                < artifacts.skill_markdown.find("# z-local").unwrap()
        );
    }

    #[test]
    fn unknown_record_owners_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let records_root = root.path().join("records");
        fs::create_dir(&records_root).unwrap();
        write_test_manifest(root.path(), r#"["groundhog-rpc"]"#);
        fs::write(
            records_root.join("unknown.md"),
            test_record("unknown", "sdk_rs", 10),
        )
        .unwrap();

        let error = build_bundle(root.path()).unwrap_err().to_string();
        assert!(error.contains("record unknown has unknown owner sdk_rs"));
    }

    #[test]
    fn unknown_record_components_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let records_root = root.path().join("records");
        fs::create_dir(&records_root).unwrap();
        write_test_manifest(root.path(), r#"["groundhog-rpc"]"#);
        let record = test_record("unknown-component", "groundhog-rpc", 10).replace(
            r#"components = ["groundhog-cli"]"#,
            r#"components = ["sdk"]"#,
        );
        fs::write(records_root.join("unknown-component.md"), record).unwrap();

        let error = build_bundle(root.path()).unwrap_err().to_string();
        assert!(error.contains("record unknown-component references unknown component sdk"));
    }

    #[test]
    fn bundle_requires_a_bootstrap_record() {
        let root = tempfile::tempdir().unwrap();
        let records_root = root.path().join("records");
        fs::create_dir(&records_root).unwrap();
        write_test_manifest(root.path(), r#"["groundhog-rpc"]"#);
        let record =
            test_record("not-bootstrap", "groundhog-rpc", 10).replace("bootstrap_order = 10\n", "");
        fs::write(records_root.join("not-bootstrap.md"), record).unwrap();

        let error = build_bundle(root.path()).unwrap_err().to_string();
        assert!(error.contains("bundle must select at least one bootstrap record"));
    }

    #[test]
    fn imported_records_require_canonical_source_and_content() {
        for (source, content) in [
            (r"sdk_rs:crates\sdk\src\lib.rs", "# Imported\n"),
            ("sdk_rs:crates/sdk/src/lib.rs", "# Imported\r\n"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let records_root = root.path().join("records");
            let imports_root = root.path().join("imports");
            fs::create_dir(&records_root).unwrap();
            fs::create_dir(&imports_root).unwrap();
            write_test_manifest(root.path(), r#"["groundhog-rpc", "sdk_rs"]"#);
            fs::write(
                records_root.join("local.md"),
                test_record("local", "groundhog-rpc", 20),
            )
            .unwrap();
            let imported = NormalizedRecord {
                schema_version: 1,
                id: "imported".to_owned(),
                owner: "sdk_rs".to_owned(),
                kind: "api".to_owned(),
                audience: vec!["coding-agent".to_owned()],
                components: vec!["groundhog-cli".to_owned()],
                bootstrap_order: Some(10),
                source: source.to_owned(),
                content: content.to_owned(),
            };
            fs::write(
                imports_root.join("sdk.jsonl"),
                format!("{}\n", serde_json::to_string(&imported).unwrap()),
            )
            .unwrap();

            assert!(build_bundle(root.path()).is_err());
        }
    }

    #[test]
    fn changed_sources_make_generated_artifacts_stale() {
        let root = tempfile::tempdir().unwrap();
        let records_root = root.path().join("records");
        fs::create_dir(&records_root).unwrap();
        write_test_manifest(root.path(), r#"["groundhog-rpc"]"#);
        fs::write(
            records_root.join("record.md"),
            test_record("record", "groundhog-rpc", 10),
        )
        .unwrap();
        write_bundle(root.path()).unwrap();
        assert!(root.path().join("generated/SKILL.md").is_file());

        fs::write(
            records_root.join("record.md"),
            test_record("changed", "groundhog-rpc", 10),
        )
        .unwrap();

        assert!(matches!(
            check_bundle(root.path()),
            Err(KnowledgeError::Stale(_))
        ));
    }

    fn write_test_manifest(root: &Path, owners: &str) {
        let manifest = format!(
            r#"schema_version = 1
id = "groundhog"
owners = {owners}

[compatibility]
groundhog-cli = "1.0.0"

[skill]
name = "groundhog"
description_lines = ["Groundhog guidance."]
version_component = "groundhog-cli"
"#
        );
        fs::write(root.join("bundle.toml"), manifest).unwrap();
    }

    fn test_record(id: &str, owner: &str, order: u32) -> String {
        format!(
            r#"+++
schema_version = 1
id = "{id}"
owner = "{owner}"
kind = "guide"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = {order}
+++
# {id}
"#
        )
    }
}
