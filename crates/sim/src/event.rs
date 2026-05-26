use std::path::Path;

use serde::{Serialize, Serializer, ser::Error as SerdeError};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::manifest::WorkloadKind;
use crate::result::{AssertionResult, SimRunOutcome};
use crate::target::TargetProviderKind;

pub const RESULT_EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub struct EventWriter {
    file: tokio::fs::File,
}

impl EventWriter {
    pub async fn create(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        Ok(Self { file })
    }

    pub async fn write<T: Serialize + ?Sized>(&mut self, event: &T) -> Result<(), EventWriteError> {
        let bytes = serde_json::to_vec(event)?;
        self.file.write_all(&bytes).await?;
        self.file.write_all(b"\n").await?;
        self.file.flush().await?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultEvent {
    RunStarted {
        run_id: String,
        target_provider: TargetProviderKind,
        rpc_url: String,
        actor_accounts: usize,
        nonce_namespace: u32,
    },
    TargetResolved {
        target_provider: TargetProviderKind,
        rpc_url: String,
        runtime_manifest_sha256: String,
    },
    PhaseStarted {
        phase_index: usize,
        simulation_time_ms: u64,
        duration_ms: u64,
        users: u32,
        rate_per_user_millihz: u64,
    },
    WorkloadCompleted(WorkloadEvent),
    WorkloadFailed(WorkloadEvent),
    WorkloadSkipped(WorkloadEvent),
    AssertionChecked(AssertionResult),
    RunCompleted {
        outcome: SimRunOutcome,
        scheduled_workloads: u64,
        completed_workloads: u64,
        failed_workloads: u64,
        skipped_workloads: u64,
    },
}

impl Serialize for ResultEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = match self {
            Self::RunStarted {
                run_id,
                target_provider,
                rpc_url,
                actor_accounts,
                nonce_namespace,
            } => json!({
                "kind": "run-started",
                "runId": run_id,
                "targetProvider": target_provider,
                "rpcUrl": rpc_url,
                "actorAccounts": actor_accounts,
                "nonceNamespace": nonce_namespace,
            }),
            Self::TargetResolved {
                target_provider,
                rpc_url,
                runtime_manifest_sha256,
            } => json!({
                "kind": "target-resolved",
                "targetProvider": target_provider,
                "rpcUrl": rpc_url,
                "runtimeManifestSha256": runtime_manifest_sha256,
            }),
            Self::PhaseStarted {
                phase_index,
                simulation_time_ms,
                duration_ms,
                users,
                rate_per_user_millihz,
            } => json!({
                "kind": "phase-started",
                "phaseIndex": phase_index,
                "simulationTimeMs": simulation_time_ms,
                "durationMs": duration_ms,
                "users": users,
                "ratePerUserMillihz": rate_per_user_millihz,
            }),
            Self::WorkloadCompleted(event) => {
                tagged_event("workload-completed", event).map_err(S::Error::custom)?
            }
            Self::WorkloadFailed(event) => {
                tagged_event("workload-failed", event).map_err(S::Error::custom)?
            }
            Self::WorkloadSkipped(event) => {
                tagged_event("workload-skipped", event).map_err(S::Error::custom)?
            }
            Self::AssertionChecked(assertion) => {
                tagged_event("assertion-checked", assertion).map_err(S::Error::custom)?
            }
            Self::RunCompleted {
                outcome,
                scheduled_workloads,
                completed_workloads,
                failed_workloads,
                skipped_workloads,
            } => json!({
                "kind": "run-completed",
                "outcome": outcome,
                "scheduledWorkloads": scheduled_workloads,
                "completedWorkloads": completed_workloads,
                "failedWorkloads": failed_workloads,
                "skippedWorkloads": skipped_workloads,
            }),
        };

        let object = value
            .as_object_mut()
            .ok_or_else(|| S::Error::custom("result event did not serialize as a JSON object"))?;
        object.insert("schemaVersion".into(), json!(RESULT_EVENT_SCHEMA_VERSION));
        value.serialize(serializer)
    }
}

fn tagged_event<T: Serialize>(kind: &'static str, event: &T) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(event)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("kind".into(), json!(kind));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadEvent {
    pub actor_id: u32,
    pub workload: WorkloadKind,
    pub scheduled_time_ms: u64,
    pub started_at_unix_ms: String,
    pub completed_at_unix_ms: String,
    pub latency_ms: u64,
    pub rpc_method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_height: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_block_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EventWriteError {
    #[error("failed filesystem operation: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to encode JSONL event: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn workload_event_has_tagged_shape() {
        let event = ResultEvent::WorkloadCompleted(WorkloadEvent {
            actor_id: 7,
            workload: WorkloadKind::ChainTip,
            scheduled_time_ms: 1_000,
            started_at_unix_ms: "10".into(),
            completed_at_unix_ms: "12".into(),
            latency_ms: 2,
            rpc_method: "groundhog_chainTip",
            chain_height: Some(42),
            chain_block_hash: Some("0xabc".into()),
            tx_hash: None,
            tx_nonce: None,
            error_class: None,
            error: None,
        });

        let value = serde_json::to_value(event).expect("event should serialize");
        assert_eq!(
            value,
            json!({
                "kind": "workload-completed",
                "schemaVersion": 1,
                "actorId": 7,
                "workload": "chain-tip",
                "scheduledTimeMs": 1000,
                "startedAtUnixMs": "10",
                "completedAtUnixMs": "12",
                "latencyMs": 2,
                "rpcMethod": "groundhog_chainTip",
                "chainHeight": 42,
                "chainBlockHash": "0xabc"
            })
        );
    }

    #[tokio::test]
    async fn event_writer_appends_to_existing_stream() {
        let file = NamedTempFile::new().expect("temp file should be created");
        tokio::fs::write(file.path(), b"{\"kind\":\"existing\"}\n")
            .await
            .expect("seed event should write");

        let mut writer = EventWriter::create(file.path())
            .await
            .expect("event writer should open");
        writer
            .write(&json!({"kind": "new"}))
            .await
            .expect("new event should append");

        let contents = tokio::fs::read_to_string(file.path())
            .await
            .expect("event stream should read");
        assert_eq!(contents, "{\"kind\":\"existing\"}\n{\"kind\":\"new\"}\n");
    }
}
