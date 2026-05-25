use std::path::Path;

use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::manifest::WorkloadKind;
use crate::result::{AssertionResult, SimRunOutcome};
use crate::target::TargetProviderKind;

pub struct EventWriter {
    file: tokio::fs::File,
}

impl EventWriter {
    pub async fn create(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadEvent {
    pub actor_id: u32,
    pub workload: WorkloadKind,
    pub scheduled_time_ms: u64,
    pub started_at_unix_ms: u128,
    pub completed_at_unix_ms: u128,
    pub latency_ms: u64,
    pub rpc_method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_height: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_block_hash: Option<String>,
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

    use super::*;

    #[test]
    fn workload_event_has_tagged_shape() {
        let event = ResultEvent::WorkloadCompleted(WorkloadEvent {
            actor_id: 7,
            workload: WorkloadKind::ChainTip,
            scheduled_time_ms: 1_000,
            started_at_unix_ms: 10,
            completed_at_unix_ms: 12,
            latency_ms: 2,
            rpc_method: "groundhog_chainTip",
            chain_height: Some(42),
            chain_block_hash: Some("0xabc".into()),
            error_class: None,
            error: None,
        });

        let value = serde_json::to_value(event).expect("event should serialize");
        assert_eq!(
            value,
            json!({
                "kind": "workload-completed",
                "actorId": 7,
                "workload": "chain-tip",
                "scheduledTimeMs": 1000,
                "startedAtUnixMs": 10,
                "completedAtUnixMs": 12,
                "latencyMs": 2,
                "rpcMethod": "groundhog_chainTip",
                "chainHeight": 42,
                "chainBlockHash": "0xabc"
            })
        );
    }
}
