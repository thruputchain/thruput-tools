use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::manifest::ChaosEventKind;

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
