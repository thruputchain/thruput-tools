use std::path::Path;

use serde::Serialize;

use crate::error::SimError;
use crate::result::{MetricAvailability, SIM_RESULT_SCHEMA_VERSION, SimResult};

/// Load a `result.json` from either a run directory or direct file path.
pub async fn load_result(path: &Path) -> Result<SimResult, SimError> {
    let path = if path.is_dir() {
        path.join("result.json")
    } else {
        path.to_path_buf()
    };
    let bytes = tokio::fs::read(&path).await?;
    let result: SimResult = serde_json::from_slice(&bytes)?;
    if result.schema_version != SIM_RESULT_SCHEMA_VERSION {
        return Err(SimError::UnsupportedResultSchema {
            actual: result.schema_version,
            path: path.display().to_string(),
        });
    }
    Ok(result)
}

/// Identity and availability diff for two run results.
#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
#[serde(rename_all = "camelCase")]
pub struct ResultDiff {
    pub same_config: bool,
    pub simulation_manifest_match: bool,
    pub runtime_manifest_match: bool,
    pub runtime_identity_match: bool,
    pub producer_identity_match: bool,
    pub storage_tokens_a: String,
    pub storage_tokens_b: String,
    pub nnint_compute_a: String,
    pub nnint_compute_b: String,
}

impl ResultDiff {
    /// Compare two run results.
    pub fn from_results(a: &SimResult, b: &SimResult) -> Self {
        let simulation_manifest_match = a.manifest_hashes.simulation_manifest_sha256
            == b.manifest_hashes.simulation_manifest_sha256;
        let runtime_manifest_match =
            a.manifest_hashes.runtime_manifest_sha256 == b.manifest_hashes.runtime_manifest_sha256;
        let runtime_identity_match = a.runtime_identity.network_id == b.runtime_identity.network_id
            && a.runtime_identity.protocol == b.runtime_identity.protocol
            && a.runtime_identity.genesis_hash == b.runtime_identity.genesis_hash;
        let producer_identity_match = a.runtime_identity.producer == b.runtime_identity.producer;

        Self {
            same_config: simulation_manifest_match
                && runtime_manifest_match
                && runtime_identity_match
                && producer_identity_match,
            simulation_manifest_match,
            runtime_manifest_match,
            runtime_identity_match,
            producer_identity_match,
            storage_tokens_a: availability_status(&a.metric_availability.storage_tokens_consumed),
            storage_tokens_b: availability_status(&b.metric_availability.storage_tokens_consumed),
            nnint_compute_a: availability_status(&a.metric_availability.nnint_compute_used),
            nnint_compute_b: availability_status(&b.metric_availability.nnint_compute_used),
        }
    }

    /// Render a human-readable summary for terminal output.
    pub fn render_text(&self) -> String {
        format!(
            concat!(
                "config match: {}\n",
                "runtime identity match: {}\n",
                "producer identity match: {}\n",
                "storage-token availability: {} -> {}\n",
                "NNInt compute availability: {} -> {}"
            ),
            self.same_config,
            self.runtime_identity_match,
            self.producer_identity_match,
            self.storage_tokens_a,
            self.storage_tokens_b,
            self.nnint_compute_a,
            self.nnint_compute_b
        )
    }
}

fn availability_status<T>(availability: &MetricAvailability<T>) -> String {
    match availability {
        MetricAvailability::Observed { .. } => "observed",
        MetricAvailability::Unsupported { .. } => "unsupported",
        MetricAvailability::Unavailable { .. } => "unavailable",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    #[tokio::test]
    async fn load_result_rejects_future_schema_version() {
        let file = NamedTempFile::new().expect("temp file should be created");
        tokio::fs::write(
            file.path(),
            serde_json::to_vec(&minimal_result_json(99)).unwrap(),
        )
        .await
        .expect("result should write");

        let err = load_result(file.path())
            .await
            .expect_err("future schema should reject");

        assert!(matches!(
            err,
            SimError::UnsupportedResultSchema { actual: 99, .. }
        ));
    }

    fn minimal_result_json(schema_version: u32) -> serde_json::Value {
        json!({
            "schemaVersion": schema_version,
            "name": "result-test",
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
                    "reason": "existing target"
                }
            },
            "metricAvailability": {
                "storageTokensConsumed": {
                    "status": "unsupported",
                    "reason": "not surfaced"
                },
                "nnintComputeUsed": {
                    "status": "unsupported",
                    "reason": "not surfaced"
                }
            }
        })
    }
}
