use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::SimError;
use crate::manifest::{SimulationManifest, WorkloadKind};
use crate::target::TargetProviderKind;

/// A simulation manifest loaded from disk with its encoded bytes and hash.
#[derive(Debug, Clone)]
pub struct LoadedSimulationManifest {
    pub manifest: SimulationManifest,
    pub sha256: String,
    pub config_sha256: String,
    pub bytes: Vec<u8>,
}

impl LoadedSimulationManifest {
    /// Re-encode the manifest after runtime overrides and refresh both hashes.
    pub fn refresh_hashes(&mut self) -> Result<(), SimError> {
        let bytes = serde_json::to_vec_pretty(&self.manifest)?;
        let sha256 = sha256_hex(&bytes);
        let config_sha256 = simulation_config_sha256(&self.manifest)?;
        self.bytes = bytes;
        self.sha256 = sha256;
        self.config_sha256 = config_sha256;
        Ok(())
    }
}

/// Load a simulation manifest from a JSON file.
pub async fn load_simulation_manifest(path: &Path) -> Result<LoadedSimulationManifest, SimError> {
    let bytes = tokio::fs::read(path).await?;
    let manifest = serde_json::from_slice(&bytes)?;
    let sha256 = sha256_hex(&bytes);
    let config_sha256 = simulation_config_sha256(&manifest)?;
    Ok(LoadedSimulationManifest {
        manifest,
        sha256,
        config_sha256,
        bytes,
    })
}

/// Hash effective simulation configuration without run-resolved identity.
///
/// The projection omits `resolved`, the diagnostic-only baseline reference, the
/// runtime-manifest filesystem path, and the local provider's resolved RPC URL.
/// An existing provider's RPC URL remains part of configuration because it
/// identifies the measured endpoint.
pub fn simulation_config_sha256(manifest: &SimulationManifest) -> Result<String, SimError> {
    let mut config = manifest.clone();
    config.resolved = None;
    config.baseline = None;
    config.target.runtime_manifest = None;
    if matches!(config.target.provider, TargetProviderKind::Local) {
        config.target.rpc_url = None;
    }
    Ok(sha256_hex(&serde_json::to_vec(&config)?))
}

/// Return a 0x-prefixed SHA-256 digest for bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(Sha256::digest(bytes)))
}

/// Write a value as pretty JSON.
pub async fn write_json_pretty(path: PathBuf, value: &impl Serialize) -> Result<(), SimError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    tokio::fs::write(path, bytes).await?;
    Ok(())
}

/// Atomically replace a JSON file with pretty-encoded contents.
///
/// On rename failure the temporary file is removed before the error is
/// returned so callers never see an orphaned `.json.tmp` sibling.
pub async fn write_json_pretty_atomic(
    path: PathBuf,
    value: &impl Serialize,
) -> Result<(), SimError> {
    let tmp = path.with_extension("json.tmp");
    write_json_pretty(tmp.clone(), value).await?;
    if let Err(error) = tokio::fs::rename(&tmp, &path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(error.into());
    }
    Ok(())
}

/// Create a sortable run identifier from the current wall clock.
pub fn run_id() -> Result<String, SimError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SimError::ClockBeforeEpoch)?;
    Ok(format!("{}-{:09}", now.as_secs(), now.subsec_nanos()))
}

/// Default output directory for a run.
pub fn default_run_dir(run_id: &str, name: &str) -> PathBuf {
    PathBuf::from("hogsim-runs").join(format!("{run_id}-{name}"))
}

/// Current Unix time in milliseconds.
pub fn unix_ms() -> Result<u128, SimError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SimError::ClockBeforeEpoch)?
        .as_millis())
}

/// Convert a duration into milliseconds, saturating at `u64::MAX`.
pub fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Return whether a command name resolves to a regular file on `PATH`.
pub fn command_on_path(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
}

/// Convert a serializable API value into a JSON string field.
pub fn json_string<T: Serialize>(value: &T, field: &'static str) -> Result<String, SimError> {
    serde_json::to_value(value)
        .map_err(SimError::Json)?
        .as_str()
        .map(str::to_owned)
        .ok_or(SimError::JsonStringField { field })
}

/// Return the JSON name for a workload kind.
pub fn workload_name(workload: WorkloadKind) -> String {
    serde_json::to_value(workload)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{workload:?}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

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
            config_sha256: String::new(),
            manifest: manifest.clone(),
            sha256: String::new(),
            bytes: Vec::new(),
        };

        loaded.refresh_hashes().expect("hashes should refresh");

        assert_eq!(loaded.sha256, sha256_hex(&loaded.bytes));
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
            loaded.config_sha256,
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
            loaded.config_sha256,
            simulation_config_sha256(&different_manifest_path).expect("config should hash")
        );

        let mut different_existing_rpc = manifest.clone();
        different_existing_rpc.target.rpc_url = Some("http://127.0.0.1:9546".into());
        assert_ne!(
            loaded.config_sha256,
            simulation_config_sha256(&different_existing_rpc).expect("config should hash")
        );

        let local_a =
            simulation_manifest("local", "http://127.0.0.1:49152", "/tmp/runtime-a.json", 1);
        let local_b =
            simulation_manifest("local", "http://127.0.0.1:49153", "/tmp/runtime-b.json", 2);
        assert_eq!(
            simulation_config_sha256(&local_a).expect("config should hash"),
            simulation_config_sha256(&local_b).expect("config should hash")
        );

        let mut different_seed = manifest;
        different_seed.baseline = Some("baselines/previous-result.json".into());
        assert_eq!(
            loaded.config_sha256,
            simulation_config_sha256(&different_seed).expect("config should hash")
        );

        different_seed.seed = "different-seed".into();
        assert_ne!(
            loaded.config_sha256,
            simulation_config_sha256(&different_seed).expect("config should hash")
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
}
