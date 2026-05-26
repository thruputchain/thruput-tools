use std::path::Path;

use serde::Serialize;
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

    pub async fn write(&mut self, event: &ResultEvent) -> Result<(), EventWriteError> {
        let bytes = serde_json::to_vec(&VersionedEvent::new(event))?;
        self.file.write_all(&bytes).await?;
        self.file.write_all(b"\n").await?;
        self.file.flush().await?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
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
    WorkloadCompleted {
        #[serde(flatten)]
        event: WorkloadEvent,
    },
    WorkloadFailed {
        #[serde(flatten)]
        event: WorkloadEvent,
    },
    WorkloadSkipped {
        #[serde(flatten)]
        event: WorkloadEvent,
    },
    AssertionChecked {
        #[serde(flatten)]
        assertion: AssertionResult,
    },
    RunCompleted {
        outcome: SimRunOutcome,
        scheduled_workloads: u64,
        completed_workloads: u64,
        failed_workloads: u64,
        skipped_workloads: u64,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionedEvent<'a> {
    schema_version: u32,
    #[serde(flatten)]
    event: &'a ResultEvent,
}

impl<'a> VersionedEvent<'a> {
    fn new(event: &'a ResultEvent) -> Self {
        Self {
            schema_version: RESULT_EVENT_SCHEMA_VERSION,
            event,
        }
    }
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
    /// Chain height observed by the workload: tip height for `chain-tip`, or
    /// receipt commit height for `receipt-poll`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_height: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_block_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_status_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit_to_receipt_latency_ms: Option<u64>,
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
    fn versioned_workload_event_has_tagged_shape() {
        let event = ResultEvent::WorkloadCompleted {
            event: WorkloadEvent {
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
                receipt_present: None,
                receipt_status: None,
                receipt_status_detail: None,
                submit_to_receipt_latency_ms: None,
                error_class: None,
                error: None,
            },
        };

        let value =
            serde_json::to_value(VersionedEvent::new(&event)).expect("event should serialize");
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

    #[test]
    fn derived_run_event_shape_matches_stream_contract() {
        let event = ResultEvent::RunStarted {
            run_id: "run-1".into(),
            target_provider: crate::target::TargetProviderKind::Existing,
            rpc_url: "http://127.0.0.1:9545".into(),
            actor_accounts: 2,
            nonce_namespace: 9,
        };

        let value =
            serde_json::to_value(VersionedEvent::new(&event)).expect("event should serialize");

        assert_eq!(
            value,
            json!({
                "kind": "run-started",
                "schemaVersion": 1,
                "runId": "run-1",
                "targetProvider": "existing",
                "rpcUrl": "http://127.0.0.1:9545",
                "actorAccounts": 2,
                "nonceNamespace": 9
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
            .write(&ResultEvent::RunCompleted {
                outcome: SimRunOutcome::Passed,
                scheduled_workloads: 0,
                completed_workloads: 0,
                failed_workloads: 0,
                skipped_workloads: 0,
            })
            .await
            .expect("new event should append");

        let contents = tokio::fs::read_to_string(file.path())
            .await
            .expect("event stream should read");
        assert_eq!(
            contents,
            concat!(
                "{\"kind\":\"existing\"}\n",
                "{\"schemaVersion\":1,\"kind\":\"run-completed\",\"outcome\":\"passed\",",
                "\"scheduledWorkloads\":0,\"completedWorkloads\":0,",
                "\"failedWorkloads\":0,\"skippedWorkloads\":0}\n"
            )
        );
    }
}
