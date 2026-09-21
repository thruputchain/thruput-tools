use groundhog_sim::error::SimError;
use groundhog_sim::manifest::SimulationManifest;

use serde_json::json;
use tempfile::tempdir;

use groundhog_sim::validation::{ValidationError, validate_manifest};

use groundhog_sim::util::*;

#[test]
fn sha256_hex_returns_prefixed_digest() {
    assert_eq!(
        sha256_hex(b"abc"),
        "0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn simulation_config_hash_excludes_run_identity_but_preserves_effective_config() {
    let manifest = simulation_manifest(
        "existing",
        "http://127.0.0.1:9545",
        "/tmp/runtime-a.json",
        1,
    );
    let mut loaded = LoadedSimulationManifest {
        config_sha256: None,
        manifest: manifest.clone(),
        sha256: String::new(),
        bytes: Vec::new(),
    };

    loaded.refresh_hashes().expect("hashes should refresh");

    assert_eq!(loaded.sha256, sha256_hex(&loaded.bytes));
    let loaded_config_sha256 = loaded
        .config_sha256
        .as_deref()
        .expect("refreshed config hash should exist");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&loaded.bytes)
            .expect("normalized manifest should parse")["resolved"]["nonceNamespace"],
        json!(1)
    );

    let mut different_nonce = manifest.clone();
    different_nonce
        .resolved
        .as_mut()
        .expect("resolved metadata should exist")
        .nonce_namespace = 2;
    let different_nonce_bytes =
        serde_json::to_vec_pretty(&different_nonce).expect("manifest should encode");
    assert_ne!(loaded.sha256, sha256_hex(&different_nonce_bytes));
    assert_eq!(
        loaded_config_sha256,
        simulation_config_sha256(&different_nonce).expect("config should hash")
    );

    let mut different_manifest_path = manifest.clone();
    different_manifest_path.target.runtime_manifest = Some("/tmp/runtime-b.json".into());
    different_manifest_path
        .resolved
        .as_mut()
        .expect("resolved metadata should exist")
        .runtime_manifest = "/tmp/runtime-b.json".into();
    assert_eq!(
        loaded_config_sha256,
        simulation_config_sha256(&different_manifest_path).expect("config should hash")
    );

    let mut different_existing_rpc = manifest.clone();
    different_existing_rpc.target.rpc_url = Some("http://127.0.0.1:9546".into());
    assert_ne!(
        loaded_config_sha256,
        simulation_config_sha256(&different_existing_rpc).expect("config should hash")
    );

    let local_a = simulation_manifest("local", "http://127.0.0.1:49152", "/tmp/runtime-a.json", 1);
    let local_b = simulation_manifest("local", "http://127.0.0.1:49153", "/tmp/runtime-b.json", 2);
    assert_eq!(
        simulation_config_sha256(&local_a).expect("config should hash"),
        simulation_config_sha256(&local_b).expect("config should hash")
    );

    let mut different_seed = manifest;
    different_seed.baseline = Some("baselines/previous-result.json".into());
    assert_eq!(
        loaded_config_sha256,
        simulation_config_sha256(&different_seed).expect("config should hash")
    );

    different_seed.seed = "different-seed".into();
    assert_ne!(
        loaded_config_sha256,
        simulation_config_sha256(&different_seed).expect("config should hash")
    );
}

#[test]
fn simulation_config_hash_normalizes_entrypoint_representations() {
    let mut raw = simulation_manifest("existing", "http://127.0.0.1:9545", "/tmp/runtime.json", 1);
    raw.contract_calls = serde_json::from_value(json!([{
        "name": "fibonacci",
        "contract": "fibonacci",
        "entrypoint": "0x1450686bab1d6a5f",
        "calldata": "0x0a"
    }]))
    .expect("raw contract call should parse");
    let mut symbolic = raw.clone();
    symbolic.contract_calls[0].entrypoint = serde_json::from_value(json!({
        "interface": "Fibonacci",
        "version": "0.1.0",
        "method": "compute"
    }))
    .expect("symbolic entrypoint should parse");

    assert_eq!(
        simulation_config_sha256(&raw).expect("raw config should hash"),
        simulation_config_sha256(&symbolic).expect("symbolic config should hash")
    );
}

#[tokio::test]
async fn loading_defers_entrypoint_resolution_until_contextual_validation() {
    let mut manifest =
        simulation_manifest("existing", "http://127.0.0.1:9545", "/tmp/runtime.json", 1);
    manifest.load_shape = serde_json::from_value(json!([{
        "duration": "1s",
        "users": 1,
        "ratePerUserHz": 1.0
    }]))
    .expect("load shape should parse");
    manifest.workloads = serde_json::from_value(json!([{
        "name": "contract-call",
        "weight": 1
    }]))
    .expect("workload should parse");
    manifest.contract_artifacts = serde_json::from_value(json!([{
        "name": "counter",
        "artifact": {"kind": "rv32", "path": "counter.rv32a"}
    }]))
    .expect("artifact should parse");
    manifest.contract_calls = serde_json::from_value(json!([{
        "name": "invalid-call",
        "contract": "counter",
        "entrypoint": "not-hex",
        "calldata": "0x"
    }]))
    .expect("contract call should parse");
    let dir = tempdir().expect("temp dir should create");
    let path = dir.path().join("scenario.json");
    tokio::fs::write(
        &path,
        serde_json::to_vec(&manifest).expect("scenario should encode"),
    )
    .await
    .expect("scenario should write");

    let loaded = load_simulation_manifest(&path)
        .await
        .expect("loading should only parse the scenario");
    assert!(loaded.config_sha256.is_none());
    let error = validate_manifest(&loaded.manifest)
        .expect_err("invalid selector should fail contextual validation");
    assert!(
        matches!(
            &error,
            ValidationError::InvalidContractCallEntrypoint { call, .. }
                if call == "invalid-call"
        ),
        "unexpected validation error: {error:?}"
    );
}

#[test]
fn json_string_rejects_non_string_values() {
    let err = json_string(&123_u64, "notAString").expect_err("number should reject");

    assert!(matches!(
        err,
        SimError::JsonStringField {
            field: "notAString"
        }
    ));
}

#[tokio::test]
async fn write_json_pretty_atomic_cleans_up_tmp_when_rename_fails() {
    let dir = tempdir().expect("temp dir should be created");
    // Parent directory does not exist, so the final rename target is
    // unreachable. The intermediate tmp write also targets the missing
    // parent, so this exercises the error path even when the failure
    // happens before rename.
    let path = dir.path().join("missing").join("value.json");

    let err = write_json_pretty_atomic(path.clone(), &json!({"new": true}))
        .await
        .expect_err("write should fail");
    assert!(matches!(err, SimError::Io(_)));
    assert!(
        !path
            .with_extension("json.tmp")
            .try_exists()
            .expect("tmp existence should be knowable")
    );
}

#[tokio::test]
async fn write_json_pretty_atomic_replaces_target() {
    let dir = tempdir().expect("temp dir should be created");
    let path = dir.path().join("value.json");
    tokio::fs::write(&path, b"{\"old\":true}")
        .await
        .expect("seed file should write");

    write_json_pretty_atomic(path.clone(), &json!({"new": true}))
        .await
        .expect("atomic write should succeed");

    let value: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(&path).await.expect("json should read"))
            .expect("json should parse");
    assert_eq!(value, json!({"new": true}));
    assert!(
        !path
            .with_extension("json.tmp")
            .try_exists()
            .expect("tmp existence should be knowable")
    );
}

fn simulation_manifest(
    provider: &str,
    rpc_url: &str,
    runtime_manifest: &str,
    nonce_namespace: u32,
) -> SimulationManifest {
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "kind": "groundhog-simulation",
        "name": "config-hash-test",
        "seed": "seed-v1",
        "target": {
            "provider": provider,
            "rpcUrl": rpc_url,
            "runtimeManifest": runtime_manifest
        },
        "resolved": {
            "rpcUrl": rpc_url,
            "runtimeManifest": runtime_manifest,
            "nonceNamespace": nonce_namespace
        }
    }))
    .expect("simulation manifest fixture should parse")
}
