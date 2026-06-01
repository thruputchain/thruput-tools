use std::path::Path;

use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::manifest::{ChaosEventKind, WorkloadKind};
use crate::result::{
    AssertionResult, ChaosEventState, ContractDeploymentRecord, SimMetricAvailability,
    SimRunOutcome,
};
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
    ChaosTransition {
        #[serde(flatten)]
        event: ChaosTransitionEvent,
    },
    ContractDeployStarted {
        #[serde(flatten)]
        event: ContractDeploymentRecord,
    },
    ContractDeployCompleted {
        #[serde(flatten)]
        event: ContractDeploymentRecord,
    },
    ContractDeployFailed {
        #[serde(flatten)]
        event: ContractDeploymentRecord,
    },
    AssertionChecked {
        #[serde(flatten)]
        assertion: AssertionResult,
    },
    MetricAvailabilityRecorded {
        #[serde(flatten)]
        metric_availability: SimMetricAvailability,
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
    /// Chain height observed by the workload: tip height for `chain-tip`,
    /// receipt commit height for `receipt-poll`, or storage snapshot height
    /// for `storage-read`.
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
    pub view_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_return_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_abort_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_event_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_call_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_call_contract: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_read_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_contract: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_item_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_item_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_query_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_contract: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_latest_block: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_oldest_block: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_after_write_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_after_write_contract: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_after_write_expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_after_write_observed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_after_write_matched: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_after_write_rebased: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChaosTransitionEvent {
    pub chaos_index: usize,
    pub chaos_kind: ChaosEventKind,
    pub state: ChaosEventState,
    pub simulation_time_ms: u64,
    pub started_at_unix_ms: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at_unix_ms: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
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
    use crate::result::{ContractDeploymentStatus, DeployAddressSource};
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
                view_result: None,
                view_return_data: None,
                view_abort_data: None,
                view_event_count: None,
                contract_call_name: None,
                contract_call_contract: None,
                storage_read_name: None,
                storage_contract: None,
                storage_key: None,
                storage_item_present: None,
                storage_item_kind: None,
                event_query_name: None,
                event_contract: None,
                event_count: None,
                event_cursor: None,
                event_latest_block: None,
                event_oldest_block: None,
                error_class: None,
                error: None,
                read_after_write_name: None,
                read_after_write_contract: None,
                read_after_write_expected: None,
                read_after_write_observed: None,
                read_after_write_matched: None,
                read_after_write_rebased: None,
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
    fn events_query_workload_event_shape_matches_stream_contract() {
        let event = ResultEvent::WorkloadCompleted {
            event: WorkloadEvent {
                actor_id: 3,
                workload: WorkloadKind::EventsQuery,
                scheduled_time_ms: 2_000,
                started_at_unix_ms: "20".into(),
                completed_at_unix_ms: "25".into(),
                latency_ms: 5,
                rpc_method: "groundhog_getEvents",
                chain_height: None,
                chain_block_hash: None,
                tx_hash: None,
                tx_nonce: None,
                receipt_present: None,
                receipt_status: None,
                receipt_status_detail: None,
                submit_to_receipt_latency_ms: None,
                view_result: None,
                view_return_data: None,
                view_abort_data: None,
                view_event_count: None,
                contract_call_name: None,
                contract_call_contract: None,
                storage_read_name: None,
                storage_contract: None,
                storage_key: None,
                storage_item_present: None,
                storage_item_kind: None,
                event_query_name: Some("fib-events".into()),
                event_contract: Some("0x88".into()),
                event_count: Some(0),
                event_cursor: Some("0x".into()),
                event_latest_block: Some(42),
                event_oldest_block: Some(0),
                error_class: None,
                error: None,
                read_after_write_name: None,
                read_after_write_contract: None,
                read_after_write_expected: None,
                read_after_write_observed: None,
                read_after_write_matched: None,
                read_after_write_rebased: None,
            },
        };

        let value =
            serde_json::to_value(VersionedEvent::new(&event)).expect("event should serialize");
        assert_eq!(
            value,
            json!({
                "kind": "workload-completed",
                "schemaVersion": 1,
                "actorId": 3,
                "workload": "events-query",
                "scheduledTimeMs": 2000,
                "startedAtUnixMs": "20",
                "completedAtUnixMs": "25",
                "latencyMs": 5,
                "rpcMethod": "groundhog_getEvents",
                "eventQueryName": "fib-events",
                "eventContract": "0x88",
                "eventCount": 0,
                "eventCursor": "0x",
                "eventLatestBlock": 42,
                "eventOldestBlock": 0
            })
        );
    }

    #[test]
    fn contract_call_workload_event_shape_matches_stream_contract() {
        let event = ResultEvent::WorkloadCompleted {
            event: WorkloadEvent {
                actor_id: 3,
                workload: WorkloadKind::ContractCall,
                scheduled_time_ms: 2_000,
                started_at_unix_ms: "20".into(),
                completed_at_unix_ms: "30".into(),
                latency_ms: 10,
                rpc_method: "groundhog_sendRawTransaction",
                chain_height: Some(77),
                chain_block_hash: None,
                tx_hash: Some(format!("0x{}", "6c".repeat(32))),
                tx_nonce: Some("4294967296".into()),
                receipt_present: Some(true),
                receipt_status: Some("ok".into()),
                receipt_status_detail: None,
                submit_to_receipt_latency_ms: Some(8),
                view_result: None,
                view_return_data: None,
                view_abort_data: None,
                view_event_count: None,
                contract_call_name: Some("events-store-value".into()),
                contract_call_contract: Some(format!("0x{}", "88".repeat(32))),
                storage_read_name: None,
                storage_contract: None,
                storage_key: None,
                storage_item_present: None,
                storage_item_kind: None,
                event_query_name: None,
                event_contract: None,
                event_count: None,
                event_cursor: None,
                event_latest_block: None,
                event_oldest_block: None,
                error_class: None,
                error: None,
                read_after_write_name: None,
                read_after_write_contract: None,
                read_after_write_expected: None,
                read_after_write_observed: None,
                read_after_write_matched: None,
                read_after_write_rebased: None,
            },
        };

        let value =
            serde_json::to_value(VersionedEvent::new(&event)).expect("event should serialize");
        assert_eq!(
            value,
            json!({
                "kind": "workload-completed",
                "schemaVersion": 1,
                "actorId": 3,
                "workload": "contract-call",
                "scheduledTimeMs": 2000,
                "startedAtUnixMs": "20",
                "completedAtUnixMs": "30",
                "latencyMs": 10,
                "rpcMethod": "groundhog_sendRawTransaction",
                "chainHeight": 77,
                "txHash": format!("0x{}", "6c".repeat(32)),
                "txNonce": "4294967296",
                "receiptPresent": true,
                "receiptStatus": "ok",
                "submitToReceiptLatencyMs": 8,
                "contractCallName": "events-store-value",
                "contractCallContract": format!("0x{}", "88".repeat(32))
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

    #[test]
    fn metric_availability_event_shape_matches_stream_contract() {
        let event = ResultEvent::MetricAvailabilityRecorded {
            metric_availability: SimMetricAvailability::phase_one_defaults(),
        };

        let value =
            serde_json::to_value(VersionedEvent::new(&event)).expect("event should serialize");

        assert_eq!(
            value,
            json!({
                "kind": "metric-availability-recorded",
                "schemaVersion": 1,
                "storageTokensConsumed": {
                    "status": "unsupported",
                    "reason": "public receipt path does not surface storage-token deltas"
                },
                "nnintComputeUsed": {
                    "status": "unsupported",
                    "reason": "public receipt path does not surface NNInt compute usage"
                }
            })
        );
    }

    #[test]
    fn chaos_transition_event_shape_matches_stream_contract() {
        let event = ResultEvent::ChaosTransition {
            event: ChaosTransitionEvent {
                chaos_index: 0,
                chaos_kind: crate::manifest::ChaosEventKind::RestartTarget,
                state: ChaosEventState::Recovered,
                simulation_time_ms: 1_000,
                started_at_unix_ms: "10".into(),
                completed_at_unix_ms: Some("15".into()),
                latency_ms: Some(5),
                detail: Some("restarted".into()),
            },
        };

        let value =
            serde_json::to_value(VersionedEvent::new(&event)).expect("event should serialize");

        assert_eq!(
            value,
            json!({
                "kind": "chaos-transition",
                "schemaVersion": 1,
                "chaosIndex": 0,
                "chaosKind": "restart-target",
                "state": "recovered",
                "simulationTimeMs": 1000,
                "startedAtUnixMs": "10",
                "completedAtUnixMs": "15",
                "latencyMs": 5,
                "detail": "restarted"
            })
        );
    }

    #[test]
    fn scheduled_chaos_transition_omits_completion_fields() {
        let event = ResultEvent::ChaosTransition {
            event: ChaosTransitionEvent {
                chaos_index: 0,
                chaos_kind: crate::manifest::ChaosEventKind::RestartTarget,
                state: ChaosEventState::Scheduled,
                simulation_time_ms: 1_000,
                started_at_unix_ms: "10".into(),
                completed_at_unix_ms: None,
                latency_ms: None,
                detail: None,
            },
        };

        let value =
            serde_json::to_value(VersionedEvent::new(&event)).expect("event should serialize");

        assert_eq!(value["state"], "scheduled");
        assert!(value.get("completedAtUnixMs").is_none());
        assert!(value.get("latencyMs").is_none());
        assert!(value.get("detail").is_none());
    }

    #[test]
    fn contract_deploy_completed_event_shape_matches_stream_contract() {
        let event = ResultEvent::ContractDeployCompleted {
            event: ContractDeploymentRecord {
                name: "fibonacci".into(),
                status: ContractDeploymentStatus::Completed,
                wasm_path: Some("/tmp/fibonacci.wasm".into()),
                wasm_sha256: Some(format!("0x{}", "11".repeat(32))),
                contract_hash: Some(format!("0x{}", "22".repeat(32))),
                nonce: Some("123".into()),
                deploy_address_nonce: Some(format!("0x{}", "33".repeat(32))),
                tx_hash: Some(format!("0x{}", "44".repeat(32))),
                address: Some(format!("0x{}", "55".repeat(32))),
                address_source: Some(DeployAddressSource::Derived),
                height: Some(42),
                error: None,
            },
        };

        let value =
            serde_json::to_value(VersionedEvent::new(&event)).expect("event should serialize");

        assert_eq!(
            value,
            json!({
                "kind": "contract-deploy-completed",
                "schemaVersion": 1,
                "name": "fibonacci",
                "status": "completed",
                "wasmPath": "/tmp/fibonacci.wasm",
                "wasmSha256": format!("0x{}", "11".repeat(32)),
                "contractHash": format!("0x{}", "22".repeat(32)),
                "nonce": "123",
                "deployAddressNonce": format!("0x{}", "33".repeat(32)),
                "txHash": format!("0x{}", "44".repeat(32)),
                "address": format!("0x{}", "55".repeat(32)),
                "addressSource": "derived",
                "height": 42
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
