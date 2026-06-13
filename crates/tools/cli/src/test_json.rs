use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::ResolvedProfile;

/// Schema version for the `groundhog test --json` payload.
///
/// Bump this when the shape or semantics of `TestReport` change in a
/// way that could break existing machine consumers.
pub const TEST_JSON_SCHEMA_VERSION: u32 = 1;

/// Structured machine-readable report for `groundhog test --json`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub schema_version: u32,
    #[serde(default)]
    pub mode: TestMode,
    pub project_root: PathBuf,
    pub profile: ResolvedProfile,
    pub wasm: PathBuf,
    pub success: bool,
    pub summary: TestSummary,
    pub runs: Vec<TestRun>,
    pub rerun: TestRerunInfo,
    pub watch: TestWatchInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration: Option<IntegrationTestInfo>,
}

/// Execution mode used for this `groundhog test` report.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TestMode {
    #[default]
    Wasm,
    Integration,
}

/// Integration-node metadata for `groundhog test --integration --json`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationTestInfo {
    pub rpc_url: String,
    pub contract_address: String,
    pub runtime_manifest: Option<PathBuf>,
    pub default_key: String,
}

/// Summary counts for one test invocation or aggregated test report.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TestSummary {
    pub source: TestSummarySource,
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub measured: u64,
    /// Number of tests libtest reported as filtered out.
    ///
    /// For aggregated multi-run reports this is set to `0`, because
    /// libtest reports the count per invocation and those values do not
    /// compose meaningfully across rerun filters.
    pub filtered_out: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
}

/// Indicates whether a summary came from libtest output or CLI derivation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TestSummarySource {
    Parsed,
    Derived,
    Empty,
}

/// One cargo/libtest invocation within `groundhog test`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestRun {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    pub exact_filter: bool,
    pub success: bool,
    pub exit_code: i32,
    pub summary: TestSummary,
    pub tests: Vec<TestCase>,
    /// Raw captured stdout for this invocation.
    ///
    /// This is retained for debugging and compatibility, but typed
    /// fields in `summary` and `tests` are the primary contract.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    /// Raw captured stderr for this invocation.
    ///
    /// This is retained for debugging and compatibility, but typed
    /// fields in `summary` and `tests` are the primary contract.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

/// One parsed libtest case result line.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestCase {
    pub name: String,
    pub status: TestCaseStatus,
}

/// Canonicalized libtest per-case outcome.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TestCaseStatus {
    Passed,
    Failed,
    Ignored,
    Measured,
}

/// Rerun-cache metadata for the overall command.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestRerunInfo {
    pub enabled: bool,
    pub cache_path: PathBuf,
    pub cached_tests: Vec<String>,
    pub cached_tests_after: Vec<String>,
    pub executed_tests: Vec<String>,
    pub failed_tests: Vec<String>,
}

/// Watch-mode metadata for the overall command.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestWatchInfo {
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_mode_deserializes_as_wasm_for_v1_compatibility() {
        let json = serde_json::json!({
            "schemaVersion": TEST_JSON_SCHEMA_VERSION,
            "projectRoot": "/project",
            "profile": {
                "name": "default",
                "rpcUrl": null,
                "defaultKey": null,
                "runtimeManifest": null,
                "contracts": {},
                "source": null
            },
            "wasm": "/project/target/demo.wasm",
            "success": true,
            "summary": {
                "source": "empty",
                "total": 0,
                "passed": 0,
                "failed": 0,
                "ignored": 0,
                "measured": 0,
                "filteredOut": 0
            },
            "runs": [],
            "rerun": {
                "enabled": false,
                "cachePath": "/project/target/groundhog/test-failures/demo.json",
                "cachedTests": [],
                "cachedTestsAfter": [],
                "executedTests": [],
                "failedTests": []
            },
            "watch": {
                "enabled": false
            }
        });

        let report: TestReport = serde_json::from_value(json).expect("v1 report should parse");

        assert_eq!(report.mode, TestMode::Wasm);
        assert!(report.integration.is_none());
    }
}
