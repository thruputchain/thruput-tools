use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::SimError;
use crate::manifest::SimulationManifest;

/// A simulation manifest loaded from disk with its encoded bytes and hash.
#[derive(Debug, Clone)]
pub struct LoadedSimulationManifest {
    pub manifest: SimulationManifest,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

impl LoadedSimulationManifest {
    /// Re-encode the manifest after runtime overrides and refresh its hash.
    pub fn refresh_sha256(&mut self) -> Result<(), SimError> {
        self.bytes = serde_json::to_vec_pretty(&self.manifest)?;
        self.sha256 = sha256_hex(&self.bytes);
        Ok(())
    }
}

/// Load a simulation manifest from a JSON file.
pub async fn load_simulation_manifest(path: &Path) -> Result<LoadedSimulationManifest, SimError> {
    let bytes = tokio::fs::read(path).await?;
    let manifest = serde_json::from_slice(&bytes)?;
    let sha256 = sha256_hex(&bytes);
    Ok(LoadedSimulationManifest {
        manifest,
        sha256,
        bytes,
    })
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
pub async fn write_json_pretty_atomic(
    path: PathBuf,
    value: &impl Serialize,
) -> Result<(), SimError> {
    let tmp = path.with_extension("json.tmp");
    write_json_pretty(tmp.clone(), value).await?;
    tokio::fs::rename(tmp, path).await?;
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
    PathBuf::from("hoglab-runs").join(format!("{run_id}-{name}"))
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

/// Convert a serializable API value into its JSON string representation.
pub fn json_string<T: Serialize + std::fmt::Debug>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{value:?}"))
}
