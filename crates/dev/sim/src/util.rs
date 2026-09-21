use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::SimError;
use crate::manifest::{EntrypointSpec, SimulationManifest, WorkloadKind};
use crate::target::TargetProviderKind;

/// A simulation manifest loaded from disk with its encoded bytes and hash.
#[derive(Debug, Clone)]
pub struct LoadedSimulationManifest {
    pub manifest: SimulationManifest,
    pub sha256: String,
    pub config_sha256: Option<String>,
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
        self.config_sha256 = Some(config_sha256);
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
        config_sha256: None,
        bytes,
    })
}

/// Hash effective simulation configuration without run-resolved identity.
///
/// The projection omits `resolved`, the diagnostic-only baseline reference, the
/// runtime-manifest filesystem path, and the local provider's resolved RPC URL.
/// An existing provider's RPC URL remains part of configuration because it
/// identifies the measured endpoint. Entrypoints are normalized to their
/// resolved selectors.
pub fn simulation_config_sha256(manifest: &SimulationManifest) -> Result<String, SimError> {
    let mut config = manifest.clone();
    config.resolved = None;
    config.baseline = None;
    config.target.runtime_manifest = None;
    if matches!(config.target.provider, TargetProviderKind::Local) {
        config.target.rpc_url = None;
    }
    canonicalize_entrypoints(&mut config)?;
    Ok(sha256_hex(&serde_json::to_vec(&config)?))
}

fn canonicalize_entrypoints(manifest: &mut SimulationManifest) -> Result<(), SimError> {
    for call in &mut manifest.view_calls {
        canonicalize_entrypoint(&mut call.entrypoint)?;
    }
    for call in &mut manifest.contract_calls {
        canonicalize_entrypoint(&mut call.entrypoint)?;
    }
    for round_trip in &mut manifest.read_after_write {
        canonicalize_entrypoint(&mut round_trip.write.entrypoint)?;
        canonicalize_entrypoint(&mut round_trip.read.entrypoint)?;
    }
    Ok(())
}

fn canonicalize_entrypoint(entrypoint: &mut EntrypointSpec) -> Result<(), SimError> {
    let bytes = entrypoint
        .resolve()
        .map_err(|error| SimError::Executor(format!("failed to resolve entrypoint: {error}")))?;
    *entrypoint = EntrypointSpec::Raw(format!("0x{}", hex::encode(bytes)));
    Ok(())
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
