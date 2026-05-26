use std::future::Future;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::SimError;
use crate::manifest::{ChaosEventKind, SimulationManifest};
use crate::validation::{ValidationError, validate_existing_target_flags};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetProviderSpec {
    pub provider: TargetProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_manifest: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hogd_dev_binary: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetProviderKind {
    Local,
    Existing,
    RemoteSsh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetCapabilities {
    pub chaos: Vec<ChaosEventKind>,
}

impl TargetCapabilities {
    pub fn for_provider(provider: TargetProviderKind) -> Self {
        let chaos = match provider {
            TargetProviderKind::Local => vec![ChaosEventKind::RestartTarget],
            TargetProviderKind::Existing | TargetProviderKind::RemoteSsh => Vec::new(),
        };
        Self { chaos }
    }

    pub fn supports_chaos(&self, kind: ChaosEventKind) -> bool {
        self.chaos.contains(&kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetOwnership {
    pub provider: TargetProviderKind,
    pub started_by_sim: bool,
    pub cleanup_allowed: bool,
}

impl TargetOwnership {
    pub fn existing() -> Self {
        Self {
            provider: TargetProviderKind::Existing,
            started_by_sim: false,
            cleanup_allowed: false,
        }
    }
}

/// Command-line target overrides supplied outside the simulation manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetOverrides {
    /// Public JSON-RPC URL to use for the run.
    pub rpc_url: Option<String>,
    /// Runtime manifest path to use for the run.
    pub runtime_manifest: Option<PathBuf>,
}

/// Concrete target endpoint and metadata acquired for one simulation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// Provider kind that produced this target.
    pub provider: TargetProviderKind,
    /// Runtime capabilities declared by the provider.
    pub capabilities: TargetCapabilities,
    /// Ownership and cleanup policy for the target.
    pub ownership: TargetOwnership,
    /// Public JSON-RPC URL used by workload execution.
    pub rpc_url: String,
    /// Runtime manifest used for validation and account derivation.
    pub runtime_manifest: PathBuf,
}

/// Target-provider lifecycle boundary used by the simulator runner.
pub trait TargetProvider {
    fn kind(&self) -> TargetProviderKind;

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::for_provider(self.kind())
    }

    fn acquire(
        &self,
        manifest: &SimulationManifest,
        overrides: TargetOverrides,
    ) -> impl Future<Output = Result<ResolvedTarget, SimError>> + Send;

    fn cleanup(
        &self,
        _target: &ResolvedTarget,
    ) -> impl Future<Output = Result<(), SimError>> + Send {
        async { Ok(()) }
    }
}

/// Optional target fault controller for providers that support chaos events.
pub trait ChaosController {
    fn restart_target(&self) -> impl Future<Output = Result<(), SimError>> + Send;
}

/// Provider for an already-running target supplied by URL and runtime manifest.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExistingTargetProvider;

impl TargetProvider for ExistingTargetProvider {
    fn kind(&self) -> TargetProviderKind {
        TargetProviderKind::Existing
    }

    #[allow(clippy::manual_async_fn)]
    fn acquire(
        &self,
        manifest: &SimulationManifest,
        overrides: TargetOverrides,
    ) -> impl Future<Output = Result<ResolvedTarget, SimError>> + Send {
        async move {
            validate_existing_target_flags(
                manifest,
                overrides.rpc_url.as_deref(),
                overrides.runtime_manifest.is_some(),
            )?;
            let rpc_url = overrides
                .rpc_url
                .or_else(|| manifest.target.rpc_url.clone())
                .ok_or(ValidationError::MissingRpcUrl)?;
            let runtime_manifest = overrides
                .runtime_manifest
                .or_else(|| manifest.target.runtime_manifest.clone())
                .ok_or(ValidationError::MissingRuntimeManifest)?;
            Ok(ResolvedTarget {
                provider: TargetProviderKind::Existing,
                capabilities: self.capabilities(),
                ownership: TargetOwnership::existing(),
                rpc_url,
                runtime_manifest,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn existing_provider_resolves_manifest_target() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545",
            "runtimeManifest": "/tmp/runtime-manifest.json"
        }));

        let target = ExistingTargetProvider
            .acquire(&manifest, TargetOverrides::default())
            .await
            .expect("existing target should resolve");

        assert_eq!(target.provider, TargetProviderKind::Existing);
        assert_eq!(target.rpc_url, "http://127.0.0.1:9545");
        assert_eq!(
            target.runtime_manifest,
            PathBuf::from("/tmp/runtime-manifest.json")
        );
        assert_eq!(target.ownership, TargetOwnership::existing());
        assert!(target.capabilities.chaos.is_empty());
    }

    #[tokio::test]
    async fn existing_provider_prefers_cli_overrides() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545",
            "runtimeManifest": "/tmp/manifest-a.json"
        }));

        let target = ExistingTargetProvider
            .acquire(
                &manifest,
                TargetOverrides {
                    rpc_url: Some("http://127.0.0.1:9546".into()),
                    runtime_manifest: Some(PathBuf::from("/tmp/manifest-b.json")),
                },
            )
            .await
            .expect("existing target should resolve");

        assert_eq!(target.rpc_url, "http://127.0.0.1:9546");
        assert_eq!(
            target.runtime_manifest,
            PathBuf::from("/tmp/manifest-b.json")
        );
    }

    #[tokio::test]
    async fn existing_provider_cleanup_is_noop() {
        let target = ResolvedTarget {
            provider: TargetProviderKind::Existing,
            capabilities: TargetCapabilities::for_provider(TargetProviderKind::Existing),
            ownership: TargetOwnership::existing(),
            rpc_url: "http://127.0.0.1:9545".into(),
            runtime_manifest: PathBuf::from("/tmp/runtime-manifest.json"),
        };

        ExistingTargetProvider
            .cleanup(&target)
            .await
            .expect("existing cleanup should be a no-op");
    }

    fn manifest(target: &serde_json::Value) -> SimulationManifest {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "target-test",
            "seed": "seed",
            "target": target,
            "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}]
        }))
        .expect("manifest should parse")
    }
}
