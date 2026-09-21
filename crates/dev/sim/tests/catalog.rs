use std::path::PathBuf;

use serde_json::json;
use tempfile::tempdir;

use groundhog_sim::catalog::*;

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

    validate_catalog(&catalog, &dir.path().join("catalog.json")).expect("catalog should validate");

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
    entry["contracts"][0]["artifact"] = json!(" fibonacci.rv32a ");
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
    let catalog: ScenarioCatalog = serde_json::from_slice(&bytes).expect("catalog should parse");

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
            json!([{"name": "fibonacci", "source": "groundhog-examples/fibonacci", "artifact": "fibonacci.rv32a"}])
        } else {
            json!([])
        },
        "runner": "hogsim run"
    })
}
