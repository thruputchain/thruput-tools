use groundhog_rpc_api_types::runtime_manifest::RuntimeManifest;

use crate::manifest::{
    SIMULATION_KIND, SIMULATION_SCHEMA_VERSION, SimulationManifest, WorkloadKind,
};
use crate::target::{TargetCapabilities, TargetProviderKind};

pub fn validate_manifest(manifest: &SimulationManifest) -> Result<(), ValidationError> {
    if manifest.schema_version != SIMULATION_SCHEMA_VERSION {
        return Err(ValidationError::UnsupportedSimulationSchema {
            actual: manifest.schema_version,
        });
    }
    if manifest.kind != SIMULATION_KIND {
        return Err(ValidationError::InvalidSimulationKind {
            actual: manifest.kind.clone(),
        });
    }
    if manifest.name.trim().is_empty() {
        return Err(ValidationError::EmptyName);
    }
    if manifest.seed.trim().is_empty() {
        return Err(ValidationError::EmptySeed);
    }
    if manifest.load_shape.is_empty() {
        return Err(ValidationError::MissingLoadShape);
    }
    if manifest.workloads.is_empty() {
        return Err(ValidationError::MissingWorkloads);
    }

    for (index, phase) in manifest.load_shape.iter().enumerate() {
        phase
            .duration
            .parse_millis()
            .map_err(|source| ValidationError::InvalidDuration { index, source })?;
        if phase.users == 0 {
            return Err(ValidationError::InvalidUsers { index });
        }
        if !phase.rate_per_user_hz.is_finite() || phase.rate_per_user_hz <= 0.0 {
            return Err(ValidationError::InvalidRate { index });
        }
    }

    for workload in &manifest.workloads {
        if workload.weight == 0 {
            return Err(ValidationError::ZeroWeight {
                workload: workload.name,
            });
        }
    }

    let capabilities = TargetCapabilities::for_provider(manifest.target.provider);
    for event in &manifest.chaos {
        if !capabilities.supports_chaos(event.kind) {
            return Err(ValidationError::UnsupportedChaos {
                provider: manifest.target.provider,
                kind: format!("{:?}", event.kind),
            });
        }
        event
            .at
            .parse_millis()
            .map_err(|source| ValidationError::InvalidChaosTime { source })?;
    }

    Ok(())
}

pub fn validate_against_runtime(
    manifest: &SimulationManifest,
    runtime: &RuntimeManifest,
) -> Result<(), ValidationError> {
    let account_count = u32::try_from(runtime.bootstrap_accounts.len()).unwrap_or(u32::MAX);
    let max_users = manifest
        .load_shape
        .iter()
        .map(|phase| phase.users)
        .max()
        .unwrap_or(0);
    if max_users > account_count {
        return Err(ValidationError::TooManyActors {
            requested: max_users,
            available: account_count,
        });
    }

    for workload in manifest.workloads.iter().map(|workload| workload.name) {
        if workload.requires_indexer() && runtime.endpoints.indexer.is_none() {
            return Err(ValidationError::MissingReadPlane {
                workload,
                endpoint: "indexer",
            });
        }
        if workload.requires_db_gateway() && runtime.endpoints.db_gateway.is_none() {
            return Err(ValidationError::MissingReadPlane {
                workload,
                endpoint: "dbGateway",
            });
        }
        if workload.requires_view_gateway() && runtime.endpoints.view_gateway.is_none() {
            return Err(ValidationError::MissingReadPlane {
                workload,
                endpoint: "viewGateway",
            });
        }
    }

    Ok(())
}

pub fn validate_existing_target_flags(
    manifest: &SimulationManifest,
    rpc_url: Option<&str>,
    runtime_manifest_supplied: bool,
) -> Result<(), ValidationError> {
    if manifest.target.provider != TargetProviderKind::Existing {
        return Err(ValidationError::OnlyExistingProviderImplemented {
            provider: manifest.target.provider,
        });
    }
    if rpc_url
        .or(manifest.target.rpc_url.as_deref())
        .is_none_or(str::is_empty)
    {
        return Err(ValidationError::MissingRpcUrl);
    }
    if !runtime_manifest_supplied && manifest.target.runtime_manifest.is_none() {
        return Err(ValidationError::MissingRuntimeManifest);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("unsupported simulation schemaVersion {actual}")]
    UnsupportedSimulationSchema { actual: u32 },
    #[error("invalid simulation kind `{actual}`")]
    InvalidSimulationKind { actual: String },
    #[error("simulation name must not be empty")]
    EmptyName,
    #[error("simulation seed must not be empty")]
    EmptySeed,
    #[error("simulation must include at least one load phase")]
    MissingLoadShape,
    #[error("simulation must include at least one workload")]
    MissingWorkloads,
    #[error("load phase {index} has invalid duration: {source}")]
    InvalidDuration {
        index: usize,
        #[source]
        source: crate::manifest::DurationParseError,
    },
    #[error("load phase {index} must have at least one user")]
    InvalidUsers { index: usize },
    #[error("load phase {index} ratePerUserHz must be positive")]
    InvalidRate { index: usize },
    #[error("workload {workload:?} must have a nonzero weight")]
    ZeroWeight { workload: WorkloadKind },
    #[error("target provider {provider:?} does not support chaos event {kind}")]
    UnsupportedChaos {
        provider: TargetProviderKind,
        kind: String,
    },
    #[error("chaos event has invalid schedule: {source}")]
    InvalidChaosTime {
        #[source]
        source: crate::manifest::DurationParseError,
    },
    #[error("only the existing target provider is implemented in Phase 1, got {provider:?}")]
    OnlyExistingProviderImplemented { provider: TargetProviderKind },
    #[error("existing target requires an RPC URL")]
    MissingRpcUrl,
    #[error("existing target requires a runtime manifest path")]
    MissingRuntimeManifest,
    #[error(
        "scenario requests {requested} actors but runtime manifest advertises {available} bootstrap accounts"
    )]
    TooManyActors { requested: u32, available: u32 },
    #[error("workload {workload:?} requires runtime manifest endpoint {endpoint}")]
    MissingReadPlane {
        workload: WorkloadKind,
        endpoint: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use groundhog_rpc_api_types::hex::Hex0x;
    use groundhog_rpc_api_types::runtime_manifest::RuntimeManifest;
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::Ed25519PublicKey;
    use serde_json::json;

    use super::*;
    use crate::manifest::{DurationSpec, LoadPhase, WorkloadSpec};
    use crate::target::TargetProviderSpec;

    #[test]
    fn rejects_chaos_for_existing_provider() {
        let mut manifest = basic_manifest();
        manifest.chaos = vec![crate::manifest::ChaosEventSpec {
            at: DurationSpec("1s".into()),
            kind: crate::manifest::ChaosEventKind::RestartTarget,
        }];

        let err = validate_manifest(&manifest).expect_err("existing should not support chaos");

        assert!(matches!(err, ValidationError::UnsupportedChaos { .. }));
    }

    #[test]
    fn rejects_receipt_workload_without_indexer() {
        let mut manifest = basic_manifest();
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::ReceiptPoll,
            weight: 1,
        }];
        let mut runtime = runtime_manifest();
        runtime.endpoints.indexer = None;

        let err = validate_against_runtime(&manifest, &runtime)
            .expect_err("receipt-poll should require indexer");

        assert!(matches!(
            err,
            ValidationError::MissingReadPlane {
                workload: WorkloadKind::ReceiptPoll,
                endpoint: "indexer"
            }
        ));
    }

    #[test]
    fn rejects_actor_count_above_bootstrap_accounts() {
        let mut manifest = basic_manifest();
        manifest.load_shape[0].users = 2;
        let runtime = runtime_manifest();

        let err = validate_against_runtime(&manifest, &runtime)
            .expect_err("actors should not exceed accounts");

        assert!(matches!(
            err,
            ValidationError::TooManyActors {
                requested: 2,
                available: 1
            }
        ));
    }

    fn basic_manifest() -> SimulationManifest {
        SimulationManifest {
            schema_version: SIMULATION_SCHEMA_VERSION,
            kind: SIMULATION_KIND.into(),
            name: "test".into(),
            seed: "seed".into(),
            target: TargetProviderSpec {
                provider: TargetProviderKind::Existing,
                rpc_url: Some("http://127.0.0.1:9545".into()),
                runtime_manifest: Some("runtime-manifest.json".into()),
                hogd_dev_binary: None,
            },
            bootstrap: None,
            load_shape: vec![LoadPhase {
                duration: DurationSpec("1s".into()),
                users: 1,
                rate_per_user_hz: 1.0,
            }],
            workloads: vec![WorkloadSpec {
                name: WorkloadKind::ChainTip,
                weight: 1,
            }],
            assertions: crate::manifest::AssertionSpec::default(),
            chaos: Vec::new(),
            baseline: None,
            resolved: None,
        }
    }

    fn runtime_manifest() -> RuntimeManifest {
        let signer = derive_dev_account_signer("seed", 0);
        let public_key = Hex0x(Ed25519PublicKey::from(signer.verifying_key_bytes()));
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x11),
            "producer": {
                "binary": "hogd-dev",
                "version": "0.1.0",
                "commitHash": "abcdef"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
                "indexer": "127.0.0.1:1001"
            },
            "bootstrapAccountDerivation": {
                "scheme": crate::runtime::SUPPORTED_DERIVATION_SCHEME,
                "seed": "seed"
            },
            "bootstrapAccounts": [{
                "index": 0,
                "role": "local_user",
                "publicKey": public_key,
                "walletAddress": prefixed_hex(0x22),
                "initialStorageTokens": "1000000000"
            }],
            "bootstrap": {
                "devWallet": {
                    "contentHash": prefixed_hex(0x33),
                    "deployedCodeHash": prefixed_hex(0x44),
                    "artifactHint": "dev-wallet.wasm",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "doActionEntrypoint": "0x646f416374696f6e"
                },
                "storageCostModel": {
                    "hash": prefixed_hex(0x55),
                    "baseDataItemCost": "1",
                    "bytestringCostPerByte": "1",
                    "baseContractDeploymentCost": "1",
                    "contractDeploymentCostPerByte": "1",
                    "authPerKeyCost": "1",
                    "rcqCostPerEntry": "1",
                    "replayCacheCostPerEntry": "1",
                    "balanceCost": "1"
                }
            }
        }))
        .expect("runtime manifest fixture should parse")
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
