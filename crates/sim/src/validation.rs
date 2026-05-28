use std::collections::HashSet;

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
    validate_contract_artifacts(manifest)?;

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

fn validate_contract_artifacts(manifest: &SimulationManifest) -> Result<(), ValidationError> {
    let mut artifact_names = HashSet::new();
    for artifact in &manifest.contract_artifacts {
        let name = artifact.name.trim();
        if name.is_empty() {
            return Err(ValidationError::EmptyContractArtifactName);
        }
        if name != artifact.name {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "contractArtifacts[].name",
                value: artifact.name.clone(),
            });
        }
        if !artifact_names.insert(name.to_owned()) {
            return Err(ValidationError::DuplicateContractArtifactName {
                name: name.to_owned(),
            });
        }
        if artifact.wasm.as_os_str().is_empty() {
            return Err(ValidationError::EmptyContractArtifactPath {
                name: name.to_owned(),
            });
        }
    }

    let mut view_names = HashSet::new();
    for view in &manifest.view_calls {
        let name = view.name.trim();
        if name.is_empty() {
            return Err(ValidationError::EmptyViewCallName);
        }
        if name != view.name {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "viewCalls[].name",
                value: view.name.clone(),
            });
        }
        if view.contract.trim() != view.contract {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "viewCalls[].contract",
                value: view.contract.clone(),
            });
        }
        if !view_names.insert(name.to_owned()) {
            return Err(ValidationError::DuplicateViewCallName {
                name: name.to_owned(),
            });
        }
        if !artifact_names.contains(view.contract.trim()) {
            return Err(ValidationError::UnknownViewCallContract {
                view: name.to_owned(),
                contract: view.contract.clone(),
            });
        }
        validate_hex_len(&view.entrypoint, Some(8), "entrypoint", name)?;
        validate_hex_len(&view.calldata, None, "calldata", name)?;
        if let Some(expected) = &view.expect_return_data {
            validate_hex_len(expected, None, "expectReturnData", name)?;
        }
    }

    if manifest
        .workloads
        .iter()
        .any(|workload| workload.name == WorkloadKind::ViewCall)
    {
        if manifest.contract_artifacts.is_empty() {
            return Err(ValidationError::MissingContractArtifactsForViewCall);
        }
        if manifest.view_calls.is_empty() {
            return Err(ValidationError::MissingViewCallsForViewCall);
        }
    }

    Ok(())
}

fn validate_hex_len(
    value: &str,
    expected_len: Option<usize>,
    field: &'static str,
    view: &str,
) -> Result<(), ValidationError> {
    let Some(stripped) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    else {
        return Err(ValidationError::InvalidViewCallHex {
            view: view.to_owned(),
            field,
            reason: "hex value must be 0x-prefixed".into(),
        });
    };
    if stripped.len() % 2 != 0 {
        return Err(ValidationError::InvalidViewCallHex {
            view: view.to_owned(),
            field,
            reason: "hex value must contain an even number of digits".into(),
        });
    }
    if let Err(error) = hex::decode(stripped) {
        return Err(ValidationError::InvalidViewCallHex {
            view: view.to_owned(),
            field,
            reason: error.to_string(),
        });
    }
    if let Some(expected_len) = expected_len
        && stripped.len() / 2 != expected_len
    {
        return Err(ValidationError::InvalidViewCallHex {
            view: view.to_owned(),
            field,
            reason: format!("expected {expected_len} bytes"),
        });
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

pub(crate) fn validate_existing_target_flags(
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

pub(crate) fn validate_local_target_flags(
    manifest: &SimulationManifest,
    rpc_url: Option<&str>,
    runtime_manifest_supplied: bool,
) -> Result<(), ValidationError> {
    if manifest.target.provider != TargetProviderKind::Local {
        return Err(ValidationError::ProviderMismatch {
            expected: TargetProviderKind::Local,
            actual: manifest.target.provider,
        });
    }
    if rpc_url
        .or(manifest.target.rpc_url.as_deref())
        .is_some_and(|value| !value.is_empty())
    {
        return Err(ValidationError::LocalRejectsRpcUrl);
    }
    if runtime_manifest_supplied || manifest.target.runtime_manifest.is_some() {
        return Err(ValidationError::LocalRejectsRuntimeManifest);
    }
    Ok(())
}

pub(crate) fn required_bootstrap_accounts(manifest: &SimulationManifest) -> u32 {
    manifest
        .load_shape
        .iter()
        .map(|phase| phase.users)
        .max()
        .unwrap_or(0)
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
    #[error("contract artifact name must not be empty")]
    EmptyContractArtifactName,
    #[error("contract artifact {name} wasm path must not be empty")]
    EmptyContractArtifactPath { name: String },
    #[error("field {field} must not contain leading or trailing whitespace: `{value}`")]
    InvalidContractNameWhitespace { field: &'static str, value: String },
    #[error("duplicate contract artifact name `{name}`")]
    DuplicateContractArtifactName { name: String },
    #[error("view call name must not be empty")]
    EmptyViewCallName,
    #[error("duplicate view call name `{name}`")]
    DuplicateViewCallName { name: String },
    #[error("view call `{view}` references unknown contract artifact `{contract}`")]
    UnknownViewCallContract { view: String, contract: String },
    #[error("view call `{view}` field `{field}` is invalid: {reason}")]
    InvalidViewCallHex {
        view: String,
        field: &'static str,
        reason: String,
    },
    #[error("view-call workload requires at least one contract artifact")]
    MissingContractArtifactsForViewCall,
    #[error("view-call workload requires at least one viewCalls entry")]
    MissingViewCallsForViewCall,
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
    #[error("target provider {provider:?} is not implemented")]
    ProviderNotImplemented { provider: TargetProviderKind },
    #[error("target provider mismatch: expected {expected:?}, got {actual:?}")]
    ProviderMismatch {
        expected: TargetProviderKind,
        actual: TargetProviderKind,
    },
    #[error("existing target requires an RPC URL")]
    MissingRpcUrl,
    #[error("existing target requires a runtime manifest path")]
    MissingRuntimeManifest,
    #[error("existing target does not accept local launch option {flag}")]
    ExistingRejectsLocalLaunchOverride { flag: &'static str },
    #[error("local target produces its RPC URL; do not supply target.rpcUrl or --rpc-url")]
    LocalRejectsRpcUrl,
    #[error(
        "local target produces its runtime manifest; do not supply target.runtimeManifest or --runtime-manifest"
    )]
    LocalRejectsRuntimeManifest,
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
    fn rejects_chaos_for_remote_ssh_provider() {
        let mut manifest = basic_manifest();
        manifest.target.provider = TargetProviderKind::RemoteSsh;
        manifest.chaos = vec![crate::manifest::ChaosEventSpec {
            at: DurationSpec("1s".into()),
            kind: crate::manifest::ChaosEventKind::RestartTarget,
        }];

        let err = validate_manifest(&manifest).expect_err("remote-ssh should not support chaos");

        assert!(matches!(
            err,
            ValidationError::UnsupportedChaos {
                provider: TargetProviderKind::RemoteSsh,
                ..
            }
        ));
    }

    #[test]
    fn validates_view_call_contract_references_and_hex() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![crate::manifest::ContractArtifactSpec {
            name: "fibonacci".into(),
            wasm: "fibonacci.wasm".into(),
        }];
        manifest.view_calls = vec![crate::manifest::ViewCallSpec {
            name: "fib-10".into(),
            contract: "fibonacci".into(),
            entrypoint: "0x1111111111111111".into(),
            calldata: "0x0a".into(),
            expect_return_data: Some("0x37".into()),
        }];
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::ViewCall,
            weight: 1,
        }];

        validate_manifest(&manifest).expect("view-call manifest should validate");
    }

    #[test]
    fn rejects_view_call_without_matching_contract_artifact() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![crate::manifest::ContractArtifactSpec {
            name: "fibonacci".into(),
            wasm: "fibonacci.wasm".into(),
        }];
        manifest.view_calls = vec![crate::manifest::ViewCallSpec {
            name: "fib-10".into(),
            contract: "missing".into(),
            entrypoint: "0x1111111111111111".into(),
            calldata: "0x0a".into(),
            expect_return_data: None,
        }];

        let err = validate_manifest(&manifest).expect_err("unknown contract should reject");

        assert!(matches!(
            err,
            ValidationError::UnknownViewCallContract { .. }
        ));
    }

    #[test]
    fn rejects_view_call_entrypoint_that_is_not_eight_bytes() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![crate::manifest::ContractArtifactSpec {
            name: "fibonacci".into(),
            wasm: "fibonacci.wasm".into(),
        }];
        manifest.view_calls = vec![crate::manifest::ViewCallSpec {
            name: "fib-10".into(),
            contract: "fibonacci".into(),
            entrypoint: "0x1111".into(),
            calldata: "0x0a".into(),
            expect_return_data: None,
        }];

        let err = validate_manifest(&manifest).expect_err("short entrypoint should reject");

        assert!(matches!(err, ValidationError::InvalidViewCallHex { .. }));
    }

    #[test]
    fn rejects_view_call_contract_reference_whitespace() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![crate::manifest::ContractArtifactSpec {
            name: "fibonacci".into(),
            wasm: "fibonacci.wasm".into(),
        }];
        manifest.view_calls = vec![crate::manifest::ViewCallSpec {
            name: "fib-10".into(),
            contract: " fibonacci ".into(),
            entrypoint: "0x1111111111111111".into(),
            calldata: "0x0a".into(),
            expect_return_data: None,
        }];

        let err = validate_manifest(&manifest).expect_err("padded reference should reject");

        assert!(matches!(
            err,
            ValidationError::InvalidContractNameWhitespace { .. }
        ));
    }

    #[test]
    fn validates_local_restart_chaos() {
        let mut manifest = basic_manifest();
        manifest.target.provider = TargetProviderKind::Local;
        manifest.target.rpc_url = None;
        manifest.target.runtime_manifest = None;
        manifest.chaos = vec![crate::manifest::ChaosEventSpec {
            at: DurationSpec("1s".into()),
            kind: crate::manifest::ChaosEventKind::RestartTarget,
        }];

        validate_manifest(&manifest).expect("local restart chaos should validate");
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

    #[test]
    fn validates_local_target_flags_reject_output_overrides() {
        let mut manifest = basic_manifest();
        manifest.target.provider = TargetProviderKind::Local;
        manifest.target.runtime_manifest = None;

        let err = validate_local_target_flags(&manifest, Some("http://127.0.0.1:9545"), false)
            .expect_err("local should reject rpc url overrides");
        assert!(matches!(err, ValidationError::LocalRejectsRpcUrl));

        manifest.target.rpc_url = None;
        let err = validate_local_target_flags(&manifest, None, true)
            .expect_err("local should reject runtime manifest overrides");
        assert!(matches!(err, ValidationError::LocalRejectsRuntimeManifest));
    }

    #[test]
    fn required_bootstrap_accounts_uses_max_phase_users() {
        let mut manifest = basic_manifest();
        manifest.load_shape = vec![
            LoadPhase {
                duration: DurationSpec("1s".into()),
                users: 2,
                rate_per_user_hz: 1.0,
            },
            LoadPhase {
                duration: DurationSpec("1s".into()),
                users: 5,
                rate_per_user_hz: 1.0,
            },
            LoadPhase {
                duration: DurationSpec("1s".into()),
                users: 3,
                rate_per_user_hz: 1.0,
            },
        ];

        assert_eq!(required_bootstrap_accounts(&manifest), 5);
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
            contract_artifacts: Vec::new(),
            view_calls: Vec::new(),
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
