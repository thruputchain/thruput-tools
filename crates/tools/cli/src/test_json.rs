use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::artifacts::ArtifactKind;
use crate::config::ResolvedProfile;

/// Schema version for the `groundhog test --json` payload.
///
/// Bump this when the shape or semantics of `TestReport` change in a
/// way that could break existing machine consumers.
pub const TEST_JSON_SCHEMA_VERSION: u32 = 7;

/// Schema version for the `groundhog test --workspace --json` payload.
///
/// Bump this when the shape or semantics of `WorkspaceTestReport` change
/// in a way that could break existing machine consumers.
pub const WORKSPACE_TEST_JSON_SCHEMA_VERSION: u32 = 1;

/// Structured machine-readable report for `groundhog test --json`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub schema_version: u32,
    #[serde(default)]
    pub mode: TestMode,
    /// Name of the tested package.
    #[serde(default)]
    pub package: String,
    pub project_root: PathBuf,
    pub profile: ResolvedProfile,
    pub artifact: TestArtifactInfo,
    pub success: bool,
    pub summary: TestSummary,
    pub runs: Vec<TestRun>,
    pub rerun: TestRerunInfo,
    pub watch: TestWatchInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration: Option<IntegrationTestInfo>,
}

/// Structured machine-readable report for `groundhog test --workspace --json`.
///
/// `packages` holds one [`TestReport`] per tested workspace member in the
/// order they ran; `summary` aggregates their runs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTestReport {
    pub schema_version: u32,
    pub workspace_root: PathBuf,
    /// True when every tested package succeeded.
    pub success: bool,
    pub summary: TestSummary,
    pub packages: Vec<TestReport>,
    /// Selected packages that did not run because the run ended earlier:
    /// under `--fail-fast` after a failing package, or when a package
    /// errored before its tests ran.
    pub skipped: Vec<String>,
    /// The package whose build or preflight errored before its tests ran,
    /// ending the run. The envelope's `error` describes the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errored: Option<String>,
}

/// Built contract artifact metadata for a `groundhog test --json` report.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestArtifactInfo {
    /// Artifact runtime kind.
    pub kind: ArtifactKind,
    /// Path to the built artifact used by this test run.
    pub path: PathBuf,
}

impl TestArtifactInfo {
    /// Build serialized test artifact metadata from the shared artifact kind.
    pub fn new(kind: ArtifactKind, path: &std::path::Path) -> Self {
        Self {
            kind,
            path: path.to_path_buf(),
        }
    }
}

/// Execution mode used for this `groundhog test` report.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum TestMode {
    #[default]
    Rv32,
    Integration,
}

/// Integration-node metadata for `groundhog test --integration --json`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationTestInfo {
    pub rpc_url: String,
    pub contract_address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
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
