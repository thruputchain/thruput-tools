use serde_json::json;

use thruput_sim::result::*;

#[test]
fn metric_availability_is_explicit_not_zero_filled() {
    let availability = SimMetricAvailability::phase_one_defaults();
    let value = serde_json::to_value(availability).expect("availability serializes");

    assert_eq!(
        value["storageTokensConsumed"],
        json!({
            "status": "unsupported",
            "reason": "public receipt path does not surface storage-token deltas"
        })
    );
    assert_eq!(
        value["nnintComputeUsed"],
        json!({
            "status": "unsupported",
            "reason": "public receipt path does not surface NNInt compute usage"
        })
    );
}

#[test]
fn chaos_event_states_match_design_shape() {
    let states = [
        ChaosEventState::Scheduled,
        ChaosEventState::Started,
        ChaosEventState::Completed,
        ChaosEventState::Failed,
        ChaosEventState::Recovered,
    ];

    let names: Vec<_> = states
        .into_iter()
        .map(|state| serde_json::to_value(state).expect("state serializes"))
        .collect();
    assert_eq!(
        names,
        vec![
            json!("scheduled"),
            json!("started"),
            json!("completed"),
            json!("failed"),
            json!("recovered"),
        ]
    );
}

#[test]
fn chaos_event_record_serializes_aggregate_shape() {
    let record = ChaosEventRecord {
        kind: "restart-target".into(),
        state: ChaosEventState::Recovered,
        simulation_time_ms: 1_000,
        chaos_index: Some(0),
        latency_ms: Some(12),
        detail: Some("restarted local target oldPid=1 newPid=2".into()),
    };

    let value = serde_json::to_value(record).expect("record serializes");

    assert_eq!(
        value,
        json!({
            "kind": "restart-target",
            "state": "recovered",
            "simulationTimeMs": 1000,
            "chaosIndex": 0,
            "latencyMs": 12,
            "detail": "restarted local target oldPid=1 newPid=2"
        })
    );
}

#[test]
fn result_defaults_missing_contracts_for_old_artifacts() {
    let value = json!({
        "schemaVersion": 1,
        "runId": "run-1",
        "name": "old-result",
        "outcome": "passed",
        "targetProvider": "existing",
        "targetOwnership": {
            "provider": "existing",
            "startedBySim": false,
            "cleanupAllowed": false
        },
        "nonceNamespace": 1,
        "manifestHashes": {
            "simulationManifestSha256": "sim",
            "runtimeManifestSha256": "runtime",
            "bootstrapManifest": {
                "status": "unsupported",
                "reason": "old"
            }
        },
        "metricAvailability": {
            "storageTokensConsumed": {
                "status": "unsupported",
                "reason": "old"
            },
            "nnintComputeUsed": {
                "status": "unsupported",
                "reason": "old"
            }
        }
    });

    let result: SimResult = serde_json::from_value(value).expect("old result should parse");

    assert!(result.contracts.is_empty());
    assert!(result.manifest_hashes.simulation_config_sha256.is_none());
    assert!(result.manifest_hashes.runtime_config_sha256.is_none());
}
