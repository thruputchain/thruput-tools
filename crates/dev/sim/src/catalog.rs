use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SimError;

pub const SCENARIO_CATALOG_SCHEMA_VERSION: u32 = 1;
pub const SCENARIO_CATALOG_KIND: &str = "groundhog-sim-scenario-catalog";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioCatalog {
    pub schema_version: u32,
    pub kind: String,
    pub scenarios: Vec<ScenarioCatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioCatalogEntry {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub summary: String,
    pub provider: String,
    pub tier: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub requires_contracts: bool,
    #[serde(default)]
    pub contracts: Vec<ScenarioContractRef>,
    pub runner: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioContractRef {
    pub name: String,
    pub source: String,
    pub artifact: String,
}

impl ScenarioCatalog {
    /// Return catalog entries matching `tag`, or all entries when no tag is supplied.
    pub fn filter_by_tag<'a>(&'a self, tag: Option<&str>) -> Vec<&'a ScenarioCatalogEntry> {
        self.scenarios
            .iter()
            .filter(|entry| {
                tag.is_none_or(|tag| entry.tags.iter().any(|entry_tag| entry_tag == tag))
            })
            .collect()
    }
}

pub async fn load_catalog(path: &Path) -> Result<ScenarioCatalog, SimError> {
    let bytes = tokio::fs::read(path).await?;
    let catalog: ScenarioCatalog = serde_json::from_slice(&bytes)?;
    validate_catalog(&catalog, path)?;
    Ok(catalog)
}

/// Return the checked-in simulator scenario catalog path.
///
/// Prefer the repository-relative path when the command is run from the repo
/// root. Otherwise fall back to a path derived from the compiled sim crate's
/// manifest directory.
pub fn default_catalog_path() -> PathBuf {
    let repo_relative = PathBuf::from("examples/sim/catalog.json");
    if repo_relative.is_file() {
        return repo_relative;
    }
    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("examples/sim/catalog.json");
    fallback.canonicalize().unwrap_or(fallback)
}

/// Resolve a scenario path relative to the directory containing `catalog_path`.
///
/// Absolute `scenario_path` values are returned unchanged. Relative paths are
/// joined to the catalog file's parent directory so scenario manifests remain
/// portable regardless of the current working directory.
pub fn resolve_scenario_path(catalog_path: &Path, scenario_path: &Path) -> PathBuf {
    if scenario_path.is_absolute() {
        scenario_path.to_path_buf()
    } else {
        catalog_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(scenario_path)
    }
}

/// Validate catalog metadata and referenced scenario files.
pub fn validate_catalog(catalog: &ScenarioCatalog, path: &Path) -> Result<(), CatalogError> {
    if catalog.schema_version != SCENARIO_CATALOG_SCHEMA_VERSION {
        return Err(CatalogError::UnsupportedSchema {
            actual: catalog.schema_version,
        });
    }
    if catalog.kind != SCENARIO_CATALOG_KIND {
        return Err(CatalogError::InvalidKind {
            actual: catalog.kind.clone(),
        });
    }
    let mut ids = HashSet::new();
    for entry in &catalog.scenarios {
        let id = entry.id.trim();
        if id.is_empty() {
            return Err(CatalogError::EmptyId);
        }
        if id != entry.id {
            return Err(CatalogError::PaddedId {
                id: entry.id.clone(),
            });
        }
        if !ids.insert(entry.id.clone()) {
            return Err(CatalogError::DuplicateId {
                id: entry.id.clone(),
            });
        }
        if entry.path.as_os_str().is_empty() {
            return Err(CatalogError::EmptyPath {
                id: entry.id.clone(),
            });
        }
        let scenario_path = resolve_scenario_path(path, &entry.path);
        if !scenario_path.is_file() {
            return Err(CatalogError::MissingScenario {
                id: entry.id.clone(),
                path: scenario_path.display().to_string(),
            });
        }
        if entry.title.trim().is_empty() {
            return Err(CatalogError::EmptyField {
                id: entry.id.clone(),
                field: "title",
            });
        }
        if entry.summary.trim().is_empty() {
            return Err(CatalogError::EmptyField {
                id: entry.id.clone(),
                field: "summary",
            });
        }
        if entry.provider.trim().is_empty() {
            return Err(CatalogError::EmptyField {
                id: entry.id.clone(),
                field: "provider",
            });
        }
        if entry.tier.trim().is_empty() {
            return Err(CatalogError::EmptyField {
                id: entry.id.clone(),
                field: "tier",
            });
        }
        if entry.runner.trim().is_empty() {
            return Err(CatalogError::EmptyField {
                id: entry.id.clone(),
                field: "runner",
            });
        }
        let mut tags = HashSet::new();
        for tag in &entry.tags {
            if tag.trim().is_empty() || tag.trim() != tag {
                return Err(CatalogError::InvalidTag {
                    id: entry.id.clone(),
                    tag: tag.clone(),
                });
            }
            if !tags.insert(tag.clone()) {
                return Err(CatalogError::DuplicateTag {
                    id: entry.id.clone(),
                    tag: tag.clone(),
                });
            }
        }
        if entry.requires_contracts && entry.contracts.is_empty() {
            return Err(CatalogError::MissingContracts {
                id: entry.id.clone(),
            });
        }
        for (index, contract) in entry.contracts.iter().enumerate() {
            validate_contract_field(&entry.id, index, "name", &contract.name)?;
            validate_contract_field(&entry.id, index, "source", &contract.source)?;
            validate_contract_field(&entry.id, index, "artifact", &contract.artifact)?;
        }
    }
    Ok(())
}

fn validate_contract_field(
    id: &str,
    index: usize,
    field: &'static str,
    value: &str,
) -> Result<(), CatalogError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(CatalogError::InvalidContractField {
            id: id.to_owned(),
            index,
            field,
        });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("unsupported scenario catalog schemaVersion {actual}")]
    UnsupportedSchema { actual: u32 },
    #[error("invalid scenario catalog kind `{actual}`")]
    InvalidKind { actual: String },
    #[error("scenario catalog entry id must not be empty")]
    EmptyId,
    #[error("scenario catalog entry id must not contain leading or trailing whitespace: `{id}`")]
    PaddedId { id: String },
    #[error("duplicate scenario catalog id `{id}`")]
    DuplicateId { id: String },
    #[error("scenario catalog entry `{id}` path must not be empty")]
    EmptyPath { id: String },
    #[error("scenario catalog entry `{id}` references missing scenario path `{path}`")]
    MissingScenario { id: String, path: String },
    #[error("scenario catalog entry `{id}` field `{field}` must not be empty")]
    EmptyField { id: String, field: &'static str },
    #[error("scenario catalog entry `{id}` has invalid tag `{tag}`")]
    InvalidTag { id: String, tag: String },
    #[error("scenario catalog entry `{id}` has duplicate tag `{tag}`")]
    DuplicateTag { id: String, tag: String },
    #[error("scenario catalog entry `{id}` requires contracts but declares none")]
    MissingContracts { id: String },
    #[error(
        "scenario catalog entry `{id}` contract {index} field `{field}` must not be empty or padded"
    )]
    InvalidContractField {
        id: String,
        index: usize,
        field: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parses_and_filters_catalog_entries_by_tag() {
        let dir = tempdir().expect("temp dir should create");
        std::fs::write(dir.path().join("one.json"), "{}").expect("scenario should write");
        std::fs::write(dir.path().join("two.json"), "{}").expect("scenario should write");
        let catalog: ScenarioCatalog = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-sim-scenario-catalog",
            "scenarios": [
                entry("one", "one.json", ["local", "smoke"]),
                entry("two", "two.json", ["contract", "smoke"])
            ]
        }))
        .expect("catalog should parse");

        validate_catalog(&catalog, &dir.path().join("catalog.json"))
            .expect("catalog should validate");

        let contract = catalog.filter_by_tag(Some("contract"));
        assert_eq!(contract.len(), 1);
        assert_eq!(contract[0].id, "two");
        assert_eq!(catalog.filter_by_tag(Some("missing")).len(), 0);
        assert_eq!(catalog.filter_by_tag(None).len(), 2);
    }

    #[test]
    fn rejects_duplicate_ids() {
        let dir = tempdir().expect("temp dir should create");
        std::fs::write(dir.path().join("one.json"), "{}").expect("scenario should write");
        let catalog: ScenarioCatalog = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-sim-scenario-catalog",
            "scenarios": [
                entry("one", "one.json", ["local"]),
                entry("one", "one.json", ["smoke"])
            ]
        }))
        .expect("catalog should parse");

        let err = validate_catalog(&catalog, &dir.path().join("catalog.json"))
            .expect_err("duplicate ids should reject");

        assert!(matches!(err, CatalogError::DuplicateId { .. }));
    }

    #[test]
    fn rejects_missing_scenario_paths() {
        let dir = tempdir().expect("temp dir should create");
        let catalog: ScenarioCatalog = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-sim-scenario-catalog",
            "scenarios": [
                entry("missing", "missing.json", ["local"])
            ]
        }))
        .expect("catalog should parse");

        let err = validate_catalog(&catalog, &dir.path().join("catalog.json"))
            .expect_err("missing path should reject");

        assert!(matches!(err, CatalogError::MissingScenario { .. }));
    }

    #[test]
    fn rejects_duplicate_tags() {
        let dir = tempdir().expect("temp dir should create");
        std::fs::write(dir.path().join("one.json"), "{}").expect("scenario should write");
        let catalog: ScenarioCatalog = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-sim-scenario-catalog",
            "scenarios": [
                entry("one", "one.json", ["local", "local"])
            ]
        }))
        .expect("catalog should parse");

        let err = validate_catalog(&catalog, &dir.path().join("catalog.json"))
            .expect_err("duplicate tags should reject");

        assert!(matches!(err, CatalogError::DuplicateTag { .. }));
    }

    #[test]
    fn rejects_empty_or_padded_contract_fields() {
        let dir = tempdir().expect("temp dir should create");
        std::fs::write(dir.path().join("one.json"), "{}").expect("scenario should write");
        let mut entry = entry("one", "one.json", ["contract"]);
        entry["contracts"][0]["artifact"] = json!(" fibonacci.wasm ");
        let catalog: ScenarioCatalog = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-sim-scenario-catalog",
            "scenarios": [entry]
        }))
        .expect("catalog should parse");

        let err = validate_catalog(&catalog, &dir.path().join("catalog.json"))
            .expect_err("invalid contract artifact should reject");

        assert!(matches!(
            err,
            CatalogError::InvalidContractField {
                field: "artifact",
                ..
            }
        ));
    }

    #[test]
    fn checked_in_catalog_validates() {
        let catalog_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("examples/sim/catalog.json");
        let bytes = std::fs::read(&catalog_path).expect("catalog should read");
        let catalog: ScenarioCatalog =
            serde_json::from_slice(&bytes).expect("catalog should parse");

        validate_catalog(&catalog, &catalog_path).expect("catalog should validate");
    }

    fn entry<const N: usize>(id: &str, path: &str, tags: [&str; N]) -> serde_json::Value {
        json!({
            "id": id,
            "path": path,
            "title": id,
            "summary": "summary",
            "provider": "local",
            "tier": "smoke",
            "tags": tags.as_slice(),
            "requiresContracts": tags.contains(&"contract"),
            "contracts": if tags.contains(&"contract") {
                json!([{"name": "fibonacci", "source": "groundhog-examples/fibonacci", "artifact": "fibonacci.wasm"}])
            } else {
                json!([])
            },
            "runner": "hogsim run"
        })
    }
}
