use std::path::PathBuf;

use serde_json::json;

use groundhog_sim::manifest::*;
use groundhog_sim::target::TargetProviderKind;

#[test]
fn parses_manifest_with_baseline_and_closed_workload_enum() {
    let manifest: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": SIMULATION_SCHEMA_VERSION,
        "kind": "groundhog-simulation",
        "name": "local-wallet-soak",
        "seed": "seed",
        "target": {"provider": "existing"},
        "client": {"maxConcurrentRequests": 256, "poolSize": 8},
        "loadShape": [{"duration": "10s", "users": 2, "ratePerUserHz": 1.5}],
        "workloads": [
            {"name": "chain-tip", "weight": 10},
            {"name": "wallet-submit-smoke", "weight": 20}
        ],
        "baseline": "baselines/local-wallet-soak.json"
    }))
    .expect("manifest should parse");

    assert_eq!(manifest.target.provider, TargetProviderKind::Existing);
    assert_eq!(
        manifest
            .client
            .as_ref()
            .and_then(|client| client.max_concurrent_requests),
        Some(256)
    );
    assert_eq!(
        manifest.client.as_ref().and_then(|client| client.pool_size),
        Some(8)
    );
    assert_eq!(manifest.workloads[1].name, WorkloadKind::WalletSubmitSmoke);
    assert_eq!(
        manifest.load_shape[0].duration.parse_millis().unwrap(),
        10_000
    );
    assert_eq!(
        manifest.baseline.unwrap(),
        PathBuf::from("baselines/local-wallet-soak.json")
    );
}

#[test]
fn parses_optional_observability_without_serializing_empty_object() {
    let manifest: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": SIMULATION_SCHEMA_VERSION,
        "kind": "groundhog-simulation",
        "name": "observed",
        "seed": "seed",
        "target": {"provider": "existing"},
        "loadShape": [{"duration": "10s", "users": 1, "ratePerUserHz": 1.0}],
        "workloads": [{"name": "chain-tip", "weight": 1}],
        "observability": {"resourceSampleInterval": "1s"}
    }))
    .expect("manifest should parse");

    assert_eq!(
        manifest
            .observability
            .as_ref()
            .and_then(|observability| observability.resource_sample_interval.as_ref())
            .expect("interval should parse")
            .parse_millis()
            .unwrap(),
        1_000
    );

    let without_observability: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": 3,
        "kind": "groundhog-simulation",
        "name": "unobserved",
        "seed": "seed",
        "target": {"provider": "existing"},
        "loadShape": [{"duration": "10s", "users": 1, "ratePerUserHz": 1.0}],
        "workloads": [{"name": "chain-tip", "weight": 1}]
    }))
    .expect("manifest should parse");
    let value = serde_json::to_value(&without_observability).expect("manifest should serialize");
    assert!(value.get("observability").is_none());
}

#[test]
fn rejects_unknown_workload_name() {
    let err = serde_json::from_value::<SimulationManifest>(json!({
        "schemaVersion": 3,
        "kind": "groundhog-simulation",
        "name": "bad",
        "seed": "seed",
        "target": {"provider": "existing"},
        "loadShape": [{"duration": "10s", "users": 1, "ratePerUserHz": 1.0}],
        "workloads": [{"name": "surprise", "weight": 1}]
    }))
    .expect_err("closed enum should reject unknown workloads");

    assert!(err.to_string().contains("unknown variant"));
}

#[test]
fn rejects_wasm_contract_artifact_kind() {
    let err = serde_json::from_value::<SimulationManifest>(json!({
        "schemaVersion": 3,
        "kind": "groundhog-simulation",
        "name": "legacy-artifact",
        "seed": "seed",
        "target": {"provider": "existing"},
        "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
        "workloads": [{"name": "view-call", "weight": 1}],
        "contractArtifacts": [{"name": "fibonacci", "artifact": {"kind": "wasm", "path": "fibonacci.wasm"}}]
    }))
    .expect_err("legacy WASM artifact kind should be rejected");

    assert!(err.to_string().contains("unknown variant `wasm`"));
}

#[test]
fn event_query_defaults_match_first_page_at_end() {
    let manifest: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": 3,
        "kind": "groundhog-simulation",
        "name": "events",
        "seed": "seed",
        "target": {"provider": "existing"},
        "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
        "workloads": [{"name": "events-query", "weight": 1}],
        "contractArtifacts": [{"name": "fibonacci", "artifact": {"kind": "rv32", "path": "fibonacci.rv32a"}}],
        "eventQueries": [{"name": "fib-events", "contract": "fibonacci"}]
    }))
    .expect("minimal event query should parse");

    let query = &manifest.event_queries[0];
    assert_eq!(query.weight, 1);
    assert_eq!(query.order, EventQueryOrderSpec::Descending);
    assert_eq!(query.anchor, EventQueryAnchorSpec::AtEnd);
    assert_eq!(query.cursor, "0x");
    assert_eq!(query.limit, 50);
    assert_eq!(query.expect_min_events, None);
}

#[test]
fn default_target_weights_are_not_serialized() {
    let manifest: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": 3,
        "kind": "groundhog-simulation",
        "name": "weights",
        "seed": "seed",
        "target": {"provider": "existing"},
        "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
        "workloads": [{"name": "contract-call", "weight": 1}],
        "contractArtifacts": [{"name": "events", "artifact": {"kind": "rv32", "path": "events.rv32a"}}],
        "contractCalls": [{
            "name": "events-store-value",
            "contract": "events",
            "entrypoint": "0x0c012d3b21f5676d",
            "calldata": "0x2a64"
        }]
    }))
    .expect("manifest should parse");

    let json = serde_json::to_value(&manifest).expect("manifest should serialize");

    assert_eq!(manifest.contract_calls[0].weight, 1);
    assert!(json["contractCalls"][0].get("weight").is_none());
}

#[test]
fn entrypoint_spec_preserves_raw_and_symbolic_json_shapes() {
    let raw: EntrypointSpec =
        serde_json::from_value(json!("0x0123456789abcdef")).expect("raw entrypoint should parse");
    let symbolic: EntrypointSpec = serde_json::from_value(json!({
        "interface": "Fibonacci",
        "version": "0.1.0",
        "method": "compute"
    }))
    .expect("symbolic entrypoint should parse");

    assert_eq!(
        serde_json::to_value(&raw).expect("raw entrypoint should serialize"),
        json!("0x0123456789abcdef")
    );
    assert_eq!(
        serde_json::to_value(&symbolic).expect("symbolic entrypoint should serialize"),
        json!({
            "interface": "Fibonacci",
            "version": "0.1.0",
            "method": "compute"
        })
    );
}

#[test]
fn bundled_symbolic_entrypoints_use_known_canonical_selectors() {
    let cases = [
        (
            "Fibonacci",
            "compute",
            [0x14, 0x50, 0x68, 0x6b, 0xab, 0x1d, 0x6a, 0x5f],
        ),
        (
            "CallerCounter",
            "bump",
            [0xb3, 0x04, 0xd6, 0xb5, 0xe0, 0x57, 0xd0, 0xf6],
        ),
        (
            "CallerCounter",
            "get",
            [0x67, 0xfa, 0x35, 0x7d, 0x44, 0xea, 0x25, 0xc5],
        ),
        (
            "Calldata",
            "sum16",
            [0x5b, 0x8f, 0x68, 0xbd, 0x06, 0xe3, 0x17, 0x40],
        ),
        (
            "NnintStress",
            "churn",
            [0xbe, 0x88, 0xa9, 0x58, 0x0f, 0xab, 0x0c, 0x83],
        ),
    ];

    for (interface, method, expected) in cases {
        let entrypoint = EntrypointSpec::Symbolic(SymbolicEntrypointSpec {
            interface: interface.into(),
            version: "0.1.0".into(),
            method: method.into(),
        });

        assert_eq!(
            entrypoint.resolve().expect("entrypoint should resolve"),
            expected
        );
    }
}

#[test]
fn symbolic_entrypoint_rejects_invalid_identity_without_panicking() {
    let invalid_version = EntrypointSpec::Symbolic(SymbolicEntrypointSpec {
        interface: "Fibonacci".into(),
        version: "latest".into(),
        method: "compute".into(),
    });
    let padded_method = EntrypointSpec::Symbolic(SymbolicEntrypointSpec {
        interface: "Fibonacci".into(),
        version: "0.1.0".into(),
        method: " compute ".into(),
    });

    assert!(matches!(
        invalid_version.resolve(),
        Err(EntrypointSpecError::InvalidVersion { .. })
    ));
    assert!(matches!(
        padded_method.resolve(),
        Err(EntrypointSpecError::PaddedSymbolicField { field: "method" })
    ));
}

#[test]
fn parses_duration_suffixes_to_milliseconds() {
    assert_eq!(DurationSpec("1500ms".into()).parse_millis().unwrap(), 1_500);
    assert_eq!(DurationSpec("1.5s".into()).parse_millis().unwrap(), 1_500);
    assert_eq!(DurationSpec("2s".into()).parse_millis().unwrap(), 2_000);
    assert_eq!(DurationSpec("3m".into()).parse_millis().unwrap(), 180_000);
    assert_eq!(DurationSpec("1h".into()).parse_millis().unwrap(), 3_600_000);
}
