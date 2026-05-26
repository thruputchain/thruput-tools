use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::error::SimError;
use crate::manifest::{ChaosEventKind, ChaosEventSpec, SimulationManifest};
use crate::result::ChaosEventState;
use crate::validation::{ValidationError, validate_existing_target_flags};

/// Boxed future returned by target-provider lifecycle hooks.
pub type TargetProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Target provider configuration stored in a simulation manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct TargetProviderSpec {
    /// Provider implementation to use for this scenario.
    pub provider: TargetProviderKind,
    /// Public JSON-RPC URL for the `existing` provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    /// Runtime manifest path for the `existing` provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_manifest: Option<PathBuf>,
    /// `hogd-dev` binary path reserved for the Phase 2 `local` provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hogd_dev_binary: Option<PathBuf>,
}

/// Target provider selected by a simulation manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "kebab-case")]
pub enum TargetProviderKind {
    Local,
    Existing,
    RemoteSsh,
}

/// Capabilities advertised by a target provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct TargetCapabilities {
    /// Chaos event kinds this provider can execute.
    pub chaos: Vec<ChaosEventKind>,
}

impl TargetCapabilities {
    /// Return the static capabilities currently known for a provider kind.
    pub fn for_provider(provider: TargetProviderKind) -> Self {
        let chaos = match provider {
            TargetProviderKind::Local => vec![ChaosEventKind::RestartTarget],
            TargetProviderKind::Existing | TargetProviderKind::RemoteSsh => Vec::new(),
        };
        Self { chaos }
    }

    /// Return whether this capability set includes a chaos event kind.
    pub fn supports_chaos(&self, kind: ChaosEventKind) -> bool {
        self.chaos.contains(&kind)
    }
}

/// Ownership and cleanup policy for a resolved target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct TargetOwnership {
    /// Provider kind that owns or resolved the target.
    pub provider: TargetProviderKind,
    /// Whether this simulation process started the target.
    pub started_by_sim: bool,
    /// Whether this simulation process may clean up the target.
    pub cleanup_allowed: bool,
}

impl TargetOwnership {
    /// Return ownership metadata for an externally managed target.
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
#[non_exhaustive]
pub struct TargetOverrides {
    /// Public JSON-RPC URL to use for the run.
    pub rpc_url: Option<String>,
    /// Runtime manifest path to use for the run.
    pub runtime_manifest: Option<PathBuf>,
}

/// Concrete target endpoint and metadata acquired for one simulation run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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

/// Result of executing one chaos event against a provider-owned target.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChaosEventOutcome {
    /// Final state the provider observed for the event.
    pub state: ChaosEventState,
    /// Optional human-readable detail for failures or provider-specific notes.
    pub detail: Option<String>,
}

/// Target-provider lifecycle boundary used by the simulator runner.
pub trait TargetProvider {
    /// Return this provider's kind.
    fn kind(&self) -> TargetProviderKind;

    /// Return this provider's runtime capabilities.
    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::for_provider(self.kind())
    }

    /// Acquire or resolve the target endpoint for one simulation run.
    fn acquire<'a>(
        &'a self,
        manifest: &'a SimulationManifest,
        overrides: TargetOverrides,
    ) -> TargetProviderFuture<'a, Result<ResolvedTarget, SimError>>;

    /// Return the chaos controller for providers that can execute chaos events.
    fn chaos_controller(&self) -> Option<&dyn ChaosController> {
        None
    }

    /// Clean up target resources acquired for a run.
    ///
    /// Implementations must be idempotent because future managed providers may
    /// call cleanup from both normal and error paths.
    fn cleanup<'a>(
        &'a self,
        _target: &'a ResolvedTarget,
    ) -> TargetProviderFuture<'a, Result<(), SimError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Optional target fault controller for providers that support chaos events.
pub trait ChaosController {
    /// Execute one manifest chaos event against the provider-owned target.
    fn execute<'a>(
        &'a self,
        event: &'a ChaosEventSpec,
    ) -> TargetProviderFuture<'a, Result<ChaosEventOutcome, SimError>>;
}

/// Provider for an already-running target supplied by URL and runtime manifest.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExistingTargetProvider;

impl TargetProvider for ExistingTargetProvider {
    fn kind(&self) -> TargetProviderKind {
        TargetProviderKind::Existing
    }

    fn acquire<'a>(
        &'a self,
        manifest: &'a SimulationManifest,
        overrides: TargetOverrides,
    ) -> TargetProviderFuture<'a, Result<ResolvedTarget, SimError>> {
        Box::pin(async move {
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
        })
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
    async fn existing_provider_is_dyn_compatible() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545",
            "runtimeManifest": "/tmp/runtime-manifest.json"
        }));
        let provider: &dyn TargetProvider = &ExistingTargetProvider;

        let target = provider
            .acquire(&manifest, TargetOverrides::default())
            .await
            .expect("existing target should resolve through a trait object");

        assert_eq!(target.provider, TargetProviderKind::Existing);
        assert!(provider.chaos_controller().is_none());
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
    async fn existing_provider_rejects_non_existing_manifest_provider() {
        let manifest = manifest(&json!({
            "provider": "local",
            "hogdDevBinary": "/tmp/hogd-dev"
        }));

        let error = ExistingTargetProvider
            .acquire(&manifest, TargetOverrides::default())
            .await
            .expect_err("existing provider should reject local manifests");

        assert!(matches!(
            error,
            SimError::Validation(ValidationError::OnlyExistingProviderImplemented {
                provider: TargetProviderKind::Local
            })
        ));
    }

    #[tokio::test]
    async fn existing_provider_rejects_missing_runtime_manifest() {
        let manifest = manifest(&json!({
            "provider": "existing",
            "rpcUrl": "http://127.0.0.1:9545"
        }));

        let error = ExistingTargetProvider
            .acquire(&manifest, TargetOverrides::default())
            .await
            .expect_err("existing provider should require a runtime manifest");

        assert!(matches!(
            error,
            SimError::Validation(ValidationError::MissingRuntimeManifest)
        ));
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
