use std::collections::HashSet;

use thruput_json_rpc_types::runtime_manifest::{
    BOOTSTRAP_ACCOUNT_ROLE_FAUCET, BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER, RuntimeManifest,
};

use crate::manifest::{
    EventQueryAnchorSpec, SIMULATION_KIND, SIMULATION_SCHEMA_VERSION, SimulationManifest,
    WorkloadKind,
};
use crate::provision::actor_provisioning_enabled;
use crate::target::{TargetBlockDriverMode, TargetCapabilities, TargetProviderKind};

const MAX_EVENT_QUERY_CURSOR_BYTES: usize = 256;

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

    validate_native_accounts(manifest)?;

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
    if let Some(observability) = &manifest.observability
        && let Some(interval) = &observability.resource_sample_interval
    {
        interval
            .parse_millis()
            .map_err(|source| ValidationError::InvalidResourceSampleInterval { source })?;
    }
    validate_target_block_driver(manifest)?;
    validate_client(manifest)?;
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

fn validate_client(manifest: &SimulationManifest) -> Result<(), ValidationError> {
    if let Some(client) = &manifest.client {
        if let Some(max_concurrent_requests) = client.max_concurrent_requests
            && max_concurrent_requests == 0
        {
            return Err(ValidationError::InvalidMaxRpcClientConcurrency);
        }
        if let Some(pool_size) = client.pool_size
            && pool_size == 0
        {
            return Err(ValidationError::InvalidRpcClientPoolSize);
        }
        if let (Some(target), Some(client)) = (
            manifest.target.max_rpc_client_concurrency,
            client.max_concurrent_requests,
        ) && target != client
        {
            return Err(ValidationError::ConflictingRpcClientConcurrency { target, client });
        }
    }
    Ok(())
}

fn validate_target_block_driver(manifest: &SimulationManifest) -> Result<(), ValidationError> {
    if let Some(block_interval_ms) = manifest.target.block_interval_ms
        && block_interval_ms == 0
    {
        return Err(ValidationError::InvalidBlockIntervalMs);
    }
    if let Some(max_rpc_connections) = manifest.target.max_rpc_connections
        && max_rpc_connections == 0
    {
        return Err(ValidationError::InvalidMaxRpcConnections);
    }
    if let Some(max_rpc_client_concurrency) = manifest.target.max_rpc_client_concurrency
        && max_rpc_client_concurrency == 0
    {
        return Err(ValidationError::InvalidMaxRpcClientConcurrency);
    }
    if manifest.target.provider != TargetProviderKind::Local {
        if manifest.target.block_driver.is_some() {
            return Err(ValidationError::NonLocalRejectsBlockDriverField {
                field: "target.blockDriver",
            });
        }
        if manifest.target.block_interval_ms.is_some() {
            return Err(ValidationError::NonLocalRejectsBlockDriverField {
                field: "target.blockIntervalMs",
            });
        }
        if manifest.target.max_rpc_connections.is_some() {
            return Err(ValidationError::NonLocalRejectsRpcConnectionField {
                field: "target.maxRpcConnections",
            });
        }
        return Ok(());
    }
    if manifest.target.block_interval_ms.is_some()
        && !matches!(
            manifest.target.block_driver,
            Some(TargetBlockDriverMode::Interval)
        )
    {
        return Err(ValidationError::BlockIntervalRequiresIntervalDriver);
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
        if artifact.artifact.path.as_os_str().is_empty() {
            return Err(ValidationError::EmptyContractArtifactPath {
                name: name.to_owned(),
            });
        }
    }

    validate_view_calls(manifest, &artifact_names)?;
    validate_contract_calls(manifest, &artifact_names)?;
    validate_storage_reads(manifest, &artifact_names)?;
    validate_event_queries(manifest, &artifact_names)?;
    validate_read_after_write(manifest, &artifact_names)?;

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

    if manifest
        .workloads
        .iter()
        .any(|workload| workload.name == WorkloadKind::StorageRead)
    {
        if manifest.contract_artifacts.is_empty() {
            return Err(ValidationError::MissingContractArtifactsForStorageRead);
        }
        if manifest.storage_reads.is_empty() {
            return Err(ValidationError::MissingStorageReadsForStorageRead);
        }
    }

    if manifest
        .workloads
        .iter()
        .any(|workload| workload.name == WorkloadKind::ContractCall)
    {
        if manifest.contract_artifacts.is_empty() {
            return Err(ValidationError::MissingContractArtifactsForContractCall);
        }
        if manifest.contract_calls.is_empty() {
            return Err(ValidationError::MissingContractCallsForContractCall);
        }
    }

    if manifest
        .workloads
        .iter()
        .any(|workload| workload.name == WorkloadKind::EventsQuery)
    {
        if manifest.contract_artifacts.is_empty() {
            return Err(ValidationError::MissingContractArtifactsForEventsQuery);
        }
        if manifest.event_queries.is_empty() {
            return Err(ValidationError::MissingEventQueriesForEventsQuery);
        }
    }

    if manifest
        .workloads
        .iter()
        .any(|workload| workload.name == WorkloadKind::ReadAfterWrite)
    {
        if manifest.contract_artifacts.is_empty() {
            return Err(ValidationError::MissingContractArtifactsForReadAfterWrite);
        }
        if manifest.read_after_write.is_empty() {
            return Err(ValidationError::MissingReadAfterWriteEntries);
        }
    }

    Ok(())
}

fn validate_read_after_write(
    manifest: &SimulationManifest,
    artifact_names: &HashSet<String>,
) -> Result<(), ValidationError> {
    let mut names = HashSet::new();
    for entry in &manifest.read_after_write {
        let name = entry.name.trim();
        if name.is_empty() {
            return Err(ValidationError::EmptyReadAfterWriteName);
        }
        if name != entry.name {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "readAfterWrite[].name",
                value: entry.name.clone(),
            });
        }
        if entry.contract.trim() != entry.contract {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "readAfterWrite[].contract",
                value: entry.contract.clone(),
            });
        }
        if !names.insert(name.to_owned()) {
            return Err(ValidationError::DuplicateReadAfterWriteName {
                name: name.to_owned(),
            });
        }
        if !artifact_names.contains(entry.contract.trim()) {
            return Err(ValidationError::UnknownReadAfterWriteContract {
                entry: name.to_owned(),
                contract: entry.contract.clone(),
            });
        }
        validate_target_weight("readAfterWrite", name, entry.weight)?;
        entry.write.entrypoint.resolve().map_err(|source| {
            ValidationError::InvalidReadAfterWriteEntrypoint {
                entry: name.to_owned(),
                field: "write.entrypoint",
                reason: source.to_string(),
            }
        })?;
        validate_read_after_write_hex(&entry.write.calldata, None, "write.calldata", name)?;
        entry.read.entrypoint.resolve().map_err(|source| {
            ValidationError::InvalidReadAfterWriteEntrypoint {
                entry: name.to_owned(),
                field: "read.entrypoint",
                reason: source.to_string(),
            }
        })?;
        validate_read_after_write_hex(&entry.read.calldata, None, "read.calldata", name)?;
    }
    Ok(())
}

fn validate_read_after_write_hex(
    value: &str,
    expected_len: Option<usize>,
    field: &'static str,
    entry: &str,
) -> Result<(), ValidationError> {
    let Some(stripped) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    else {
        return Err(ValidationError::InvalidReadAfterWriteHex {
            entry: entry.to_owned(),
            field,
            reason: "hex value must be 0x-prefixed".into(),
        });
    };
    if stripped.len() % 2 != 0 {
        return Err(ValidationError::InvalidReadAfterWriteHex {
            entry: entry.to_owned(),
            field,
            reason: "hex value must contain an even number of digits".into(),
        });
    }
    if let Err(error) = hex::decode(stripped) {
        return Err(ValidationError::InvalidReadAfterWriteHex {
            entry: entry.to_owned(),
            field,
            reason: error.to_string(),
        });
    }
    if let Some(expected_len) = expected_len
        && stripped.len() / 2 != expected_len
    {
        return Err(ValidationError::InvalidReadAfterWriteHex {
            entry: entry.to_owned(),
            field,
            reason: format!("expected {expected_len} bytes"),
        });
    }
    Ok(())
}

fn validate_view_calls(
    manifest: &SimulationManifest,
    artifact_names: &HashSet<String>,
) -> Result<(), ValidationError> {
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
        validate_target_weight("viewCalls", name, view.weight)?;
        view.entrypoint
            .resolve()
            .map_err(|source| ValidationError::InvalidViewCallEntrypoint {
                view: name.to_owned(),
                reason: source.to_string(),
            })?;
        validate_hex_len(&view.calldata, None, "calldata", name)?;
        if let Some(expected) = &view.expect_return_data {
            validate_hex_len(expected, None, "expectReturnData", name)?;
        }
    }
    Ok(())
}

fn validate_storage_reads(
    manifest: &SimulationManifest,
    artifact_names: &HashSet<String>,
) -> Result<(), ValidationError> {
    let mut storage_names = HashSet::new();
    for read in &manifest.storage_reads {
        let name = read.name.trim();
        if name.is_empty() {
            return Err(ValidationError::EmptyStorageReadName);
        }
        if name != read.name {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "storageReads[].name",
                value: read.name.clone(),
            });
        }
        if read.contract.trim() != read.contract {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "storageReads[].contract",
                value: read.contract.clone(),
            });
        }
        if !storage_names.insert(name.to_owned()) {
            return Err(ValidationError::DuplicateStorageReadName {
                name: name.to_owned(),
            });
        }
        if !artifact_names.contains(read.contract.trim()) {
            return Err(ValidationError::UnknownStorageReadContract {
                read: name.to_owned(),
                contract: read.contract.clone(),
            });
        }
        validate_target_weight("storageReads", name, read.weight)?;
        validate_storage_hex_len(&read.key, Some(32), "key", name)?;
    }
    Ok(())
}

fn validate_contract_calls(
    manifest: &SimulationManifest,
    artifact_names: &HashSet<String>,
) -> Result<(), ValidationError> {
    let mut call_names = HashSet::new();
    for call in &manifest.contract_calls {
        let name = call.name.trim();
        if name.is_empty() {
            return Err(ValidationError::EmptyContractCallName);
        }
        if name != call.name {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "contractCalls[].name",
                value: call.name.clone(),
            });
        }
        if call.contract.trim() != call.contract {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "contractCalls[].contract",
                value: call.contract.clone(),
            });
        }
        if !call_names.insert(name.to_owned()) {
            return Err(ValidationError::DuplicateContractCallName {
                name: name.to_owned(),
            });
        }
        if !artifact_names.contains(call.contract.trim()) {
            return Err(ValidationError::UnknownContractCallContract {
                call: name.to_owned(),
                contract: call.contract.clone(),
            });
        }
        validate_target_weight("contractCalls", name, call.weight)?;
        call.entrypoint.resolve().map_err(|source| {
            ValidationError::InvalidContractCallEntrypoint {
                call: name.to_owned(),
                reason: source.to_string(),
            }
        })?;
        validate_contract_call_hex_len(&call.calldata, None, "calldata", name)?;
    }
    Ok(())
}

fn validate_event_queries(
    manifest: &SimulationManifest,
    artifact_names: &HashSet<String>,
) -> Result<(), ValidationError> {
    let mut query_names = HashSet::new();
    for query in &manifest.event_queries {
        let name = query.name.trim();
        if name.is_empty() {
            return Err(ValidationError::EmptyEventQueryName);
        }
        if name != query.name {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "eventQueries[].name",
                value: query.name.clone(),
            });
        }
        if query.contract.trim() != query.contract {
            return Err(ValidationError::InvalidContractNameWhitespace {
                field: "eventQueries[].contract",
                value: query.contract.clone(),
            });
        }
        if !query_names.insert(name.to_owned()) {
            return Err(ValidationError::DuplicateEventQueryName {
                name: name.to_owned(),
            });
        }
        if !artifact_names.contains(query.contract.trim()) {
            return Err(ValidationError::UnknownEventQueryContract {
                query: name.to_owned(),
                contract: query.contract.clone(),
            });
        }
        validate_target_weight("eventQueries", name, query.weight)?;
        validate_event_query_cursor(&query.cursor, "cursor", name)?;
        if query.limit == 0 || query.limit > 200 {
            return Err(ValidationError::InvalidEventQueryLimit {
                query: name.to_owned(),
                limit: query.limit,
            });
        }
        if let EventQueryAnchorSpec::AtHeight { height } = query.anchor
            && height < 0
        {
            return Err(ValidationError::InvalidEventQueryAnchorHeight {
                query: name.to_owned(),
                height,
            });
        }
    }
    Ok(())
}

fn validate_target_weight(
    field: &'static str,
    name: &str,
    weight: u32,
) -> Result<(), ValidationError> {
    if weight == 0 {
        return Err(ValidationError::ZeroTargetWeight {
            field,
            name: name.to_owned(),
        });
    }
    Ok(())
}

fn validate_contract_call_hex_len(
    value: &str,
    expected_len: Option<usize>,
    field: &'static str,
    call: &str,
) -> Result<(), ValidationError> {
    let Some(stripped) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    else {
        return Err(ValidationError::InvalidContractCallHex {
            call: call.to_owned(),
            field,
            reason: "hex value must be 0x-prefixed".into(),
        });
    };
    if stripped.len() % 2 != 0 {
        return Err(ValidationError::InvalidContractCallHex {
            call: call.to_owned(),
            field,
            reason: "hex value must contain an even number of digits".into(),
        });
    }
    if let Err(error) = hex::decode(stripped) {
        return Err(ValidationError::InvalidContractCallHex {
            call: call.to_owned(),
            field,
            reason: error.to_string(),
        });
    }
    if let Some(expected_len) = expected_len
        && stripped.len() / 2 != expected_len
    {
        return Err(ValidationError::InvalidContractCallHex {
            call: call.to_owned(),
            field,
            reason: format!("expected {expected_len} bytes"),
        });
    }
    Ok(())
}

fn validate_event_query_cursor(
    value: &str,
    field: &'static str,
    query: &str,
) -> Result<(), ValidationError> {
    let Some(stripped) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    else {
        return Err(ValidationError::InvalidEventQueryHex {
            query: query.to_owned(),
            field,
            reason: "hex value must be 0x-prefixed".into(),
        });
    };
    if stripped.len() % 2 != 0 {
        return Err(ValidationError::InvalidEventQueryHex {
            query: query.to_owned(),
            field,
            reason: "hex value must contain an even number of digits".into(),
        });
    }
    if let Err(error) = hex::decode(stripped) {
        return Err(ValidationError::InvalidEventQueryHex {
            query: query.to_owned(),
            field,
            reason: error.to_string(),
        });
    }
    if stripped.len() / 2 > MAX_EVENT_QUERY_CURSOR_BYTES {
        return Err(ValidationError::InvalidEventQueryHex {
            query: query.to_owned(),
            field,
            reason: format!("expected at most {MAX_EVENT_QUERY_CURSOR_BYTES} bytes"),
        });
    }
    Ok(())
}

fn validate_storage_hex_len(
    value: &str,
    expected_len: Option<usize>,
    field: &'static str,
    read: &str,
) -> Result<(), ValidationError> {
    let Some(stripped) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    else {
        return Err(ValidationError::InvalidStorageReadHex {
            read: read.to_owned(),
            field,
            reason: "hex value must be 0x-prefixed".into(),
        });
    };
    if stripped.len() % 2 != 0 {
        return Err(ValidationError::InvalidStorageReadHex {
            read: read.to_owned(),
            field,
            reason: "hex value must contain an even number of digits".into(),
        });
    }
    if let Err(error) = hex::decode(stripped) {
        return Err(ValidationError::InvalidStorageReadHex {
            read: read.to_owned(),
            field,
            reason: error.to_string(),
        });
    }
    if let Some(expected_len) = expected_len
        && stripped.len() / 2 != expected_len
    {
        return Err(ValidationError::InvalidStorageReadHex {
            read: read.to_owned(),
            field,
            reason: format!("expected {expected_len} bytes"),
        });
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

fn positive_raw(raw: &str) -> bool {
    raw.bytes().all(|byte| byte.is_ascii_digit())
        && raw
            .parse::<u128>()
            .is_ok_and(|value| value > 0 && value.to_string() == raw)
}
pub(crate) fn requires_signing_accounts(manifest: &SimulationManifest) -> bool {
    !manifest.contract_artifacts.is_empty()
        || manifest.workloads.iter().any(|w| {
            matches!(
                w.name,
                WorkloadKind::WalletSubmitSmoke
                    | WorkloadKind::ReceiptPoll
                    | WorkloadKind::StatusPoll
                    | WorkloadKind::ContractCall
                    | WorkloadKind::ReadAfterWrite
            )
        })
}

fn validate_native_accounts(manifest: &SimulationManifest) -> Result<(), ValidationError> {
    if manifest.target.provider == TargetProviderKind::Local
        && manifest.accounts.is_none()
        && (requires_signing_accounts(manifest) || actor_provisioning_enabled(manifest))
    {
        return Err(ValidationError::NativeAccounts { reason: "local signing/provisioning requires explicit native accounts; runtime manifests no longer supply keys".into() });
    }
    if manifest.accounts.is_none() {
        let raw = manifest
            .bootstrap
            .as_ref()
            .and_then(|b| b.deployment_collateral_raw.as_deref());
        if raw.is_some_and(|value| !positive_raw(value)) {
            return Err(ValidationError::NativeAccounts {
                reason: "bootstrap.deploymentCollateralRaw must be a positive u128 decimal".into(),
            });
        }
        if !manifest.contract_artifacts.is_empty() && raw.is_none() {
            return Err(ValidationError::NativeAccounts { reason: "bootstrap contract deployment requires explicit bootstrap.deploymentCollateralRaw".into() });
        }
    }

    if let Some(accounts) = &manifest.accounts {
        if accounts.seed.trim().is_empty() || accounts.seed.len() > 1024 {
            return Err(ValidationError::NativeAccounts {
                reason: "seed must contain 1..1024 bytes".into(),
            });
        }
        if !positive_raw(&accounts.minimum_balance_raw) {
            return Err(ValidationError::NativeAccounts {
                reason: "minimumBalanceRaw must be a positive u128 decimal".into(),
            });
        }
        if let Some(raw) = &accounts.deployment_collateral_raw
            && !positive_raw(raw)
        {
            return Err(ValidationError::NativeAccounts {
                reason: "deploymentCollateralRaw must be a positive u128 decimal".into(),
            });
        }
        if !manifest.contract_artifacts.is_empty() && accounts.deployment_collateral_raw.is_none() {
            return Err(ValidationError::NativeAccounts { reason: "native contract deployment requires explicit deploymentCollateralRaw for each predicted contract address".into() });
        }
        if actor_provisioning_enabled(manifest) {
            return Err(ValidationError::NativeAccounts {
                reason: "explicit accounts cannot use bootstrap.actorProvisioning".into(),
            });
        }
        if accounts
            .funding_amount_raw
            .as_deref()
            .is_some_and(|raw| !positive_raw(raw))
        {
            return Err(ValidationError::NativeAccounts {
                reason: "fundingAmountRaw must be a positive canonical u128 decimal".into(),
            });
        }
        if manifest.target.provider == TargetProviderKind::Local
            && accounts.funding == crate::manifest::NativeAccountFunding::Faucet
            && accounts.funding_amount_raw.is_none()
        {
            return Err(ValidationError::NativeAccounts {
                reason: "local faucet setup requires explicit accounts.fundingAmountRaw".into(),
            });
        }
        if manifest.load_shape.iter().any(|phase| phase.users > 1000) {
            return Err(ValidationError::NativeAccounts {
                reason: "native account setup is bounded to 1000 actors".into(),
            });
        }
    }

    Ok(())
}

pub fn validate_against_runtime(
    manifest: &SimulationManifest,
    runtime: &RuntimeManifest,
) -> Result<(), ValidationError> {
    if runtime.kind == thruput_json_rpc_types::runtime_manifest::HOGD_DEV_RUNTIME_MANIFEST_KIND
        && manifest.accounts.is_none()
        && requires_signing_accounts(manifest)
    {
        return Err(ValidationError::NativeAccounts {
            reason:
                "local signing workloads require explicit accounts.kind=native with a fixture seed"
                    .into(),
        });
    }
    if let Some(accounts) = &manifest.accounts {
        let local = runtime.kind
            == thruput_json_rpc_types::runtime_manifest::HOGD_DEV_RUNTIME_MANIFEST_KIND;
        if (local
            && accounts.funding == crate::manifest::NativeAccountFunding::Faucet
            && accounts.funding_amount_raw.is_none())
            || (!local && accounts.funding_amount_raw.is_some())
        {
            return Err(ValidationError::NativeAccounts { reason: "local faucet funding requires fundingAmountRaw; hosted faucets retain their fixed amount".into() });
        }
    }
    let account_count = u32::try_from(
        runtime
            .bootstrap_accounts
            .iter()
            .filter(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let max_users = manifest
        .load_shape
        .iter()
        .map(|phase| phase.users)
        .max()
        .unwrap_or(0);
    let has_provisioning_faucet = runtime
        .bootstrap_accounts
        .iter()
        .any(|account| account.index == Some(0) && account.role == BOOTSTRAP_ACCOUNT_ROLE_FAUCET);
    if runtime.kind == thruput_json_rpc_types::runtime_manifest::HOSTED_DEVNET_RUNTIME_MANIFEST_KIND
        && manifest.accounts.is_none()
        && max_users > account_count
        && actor_provisioning_enabled(manifest)
        && !has_provisioning_faucet
    {
        return Err(ValidationError::ActorProvisioningRequiresSponsor);
    }
    if runtime.kind == thruput_json_rpc_types::runtime_manifest::HOSTED_DEVNET_RUNTIME_MANIFEST_KIND
        && manifest.accounts.is_none()
        && max_users > account_count
        && !actor_provisioning_enabled(manifest)
    {
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
    #[error("invalid native accounts: {reason}")]
    NativeAccounts { reason: String },
    #[error(
        "unsupported simulation schemaVersion {actual}; update the scenario to schema 3 native bootstrap accounts, remove devWalletProfile/artifact fields and set explicit deploymentCollateralRaw for contract deployments"
    )]
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
    #[error("observability.resourceSampleInterval is invalid: {source}")]
    InvalidResourceSampleInterval {
        #[source]
        source: crate::manifest::DurationParseError,
    },
    #[error("load phase {index} must have at least one user")]
    InvalidUsers { index: usize },
    #[error("load phase {index} ratePerUserHz must be positive")]
    InvalidRate { index: usize },
    #[error("workload {workload:?} must have a nonzero weight")]
    ZeroWeight { workload: WorkloadKind },
    #[error("{field} target `{name}` must have a nonzero weight")]
    ZeroTargetWeight { field: &'static str, name: String },
    #[error("contract artifact name must not be empty")]
    EmptyContractArtifactName,
    #[error("contract artifact {name} path must not be empty")]
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
    #[error("view call `{view}` entrypoint is invalid: {reason}")]
    InvalidViewCallEntrypoint { view: String, reason: String },
    #[error("view-call workload requires at least one contract artifact")]
    MissingContractArtifactsForViewCall,
    #[error("view-call workload requires at least one viewCalls entry")]
    MissingViewCallsForViewCall,
    #[error("contract call name must not be empty")]
    EmptyContractCallName,
    #[error("duplicate contract call name `{name}`")]
    DuplicateContractCallName { name: String },
    #[error("contract call `{call}` references unknown contract artifact `{contract}`")]
    UnknownContractCallContract { call: String, contract: String },
    #[error("contract call `{call}` field `{field}` is invalid: {reason}")]
    InvalidContractCallHex {
        call: String,
        field: &'static str,
        reason: String,
    },
    #[error("contract call `{call}` entrypoint is invalid: {reason}")]
    InvalidContractCallEntrypoint { call: String, reason: String },
    #[error("contract-call workload requires at least one contract artifact")]
    MissingContractArtifactsForContractCall,
    #[error("contract-call workload requires at least one contractCalls entry")]
    MissingContractCallsForContractCall,
    #[error("storage read name must not be empty")]
    EmptyStorageReadName,
    #[error("duplicate storage read name `{name}`")]
    DuplicateStorageReadName { name: String },
    #[error("storage read `{read}` references unknown contract artifact `{contract}`")]
    UnknownStorageReadContract { read: String, contract: String },
    #[error("storage read `{read}` field `{field}` is invalid: {reason}")]
    InvalidStorageReadHex {
        read: String,
        field: &'static str,
        reason: String,
    },
    #[error("storage-read workload requires at least one contract artifact")]
    MissingContractArtifactsForStorageRead,
    #[error("storage-read workload requires at least one storageReads entry")]
    MissingStorageReadsForStorageRead,
    #[error("event query name must not be empty")]
    EmptyEventQueryName,
    #[error("duplicate event query name `{name}`")]
    DuplicateEventQueryName { name: String },
    #[error("event query `{query}` references unknown contract artifact `{contract}`")]
    UnknownEventQueryContract { query: String, contract: String },
    #[error("event query `{query}` field `{field}` is invalid: {reason}")]
    InvalidEventQueryHex {
        query: String,
        field: &'static str,
        reason: String,
    },
    #[error("event query `{query}` limit {limit} must be between 1 and 200")]
    InvalidEventQueryLimit { query: String, limit: u32 },
    #[error("event query `{query}` anchor height {height} must be non-negative")]
    InvalidEventQueryAnchorHeight { query: String, height: i64 },
    #[error("events-query workload requires at least one contract artifact")]
    MissingContractArtifactsForEventsQuery,
    #[error("events-query workload requires at least one eventQueries entry")]
    MissingEventQueriesForEventsQuery,
    #[error("read-after-write name must not be empty")]
    EmptyReadAfterWriteName,
    #[error("duplicate read-after-write name `{name}`")]
    DuplicateReadAfterWriteName { name: String },
    #[error("read-after-write `{entry}` references unknown contract artifact `{contract}`")]
    UnknownReadAfterWriteContract { entry: String, contract: String },
    #[error("read-after-write `{entry}` field `{field}` is invalid: {reason}")]
    InvalidReadAfterWriteHex {
        entry: String,
        field: &'static str,
        reason: String,
    },
    #[error("read-after-write `{entry}` field `{field}` entrypoint is invalid: {reason}")]
    InvalidReadAfterWriteEntrypoint {
        entry: String,
        field: &'static str,
        reason: String,
    },
    #[error("read-after-write workload requires at least one contract artifact")]
    MissingContractArtifactsForReadAfterWrite,
    #[error("read-after-write workload requires at least one readAfterWrite entry")]
    MissingReadAfterWriteEntries,
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
    #[error("blockIntervalMs must be at least 1")]
    InvalidBlockIntervalMs,
    #[error("blockIntervalMs requires target.blockDriver to be interval")]
    BlockIntervalRequiresIntervalDriver,
    #[error("non-local target does not accept local block-driver field {field}")]
    NonLocalRejectsBlockDriverField { field: &'static str },
    #[error("maxRpcConnections must be at least 1")]
    InvalidMaxRpcConnections,
    #[error("client.maxConcurrentRequests must be at least 1")]
    InvalidMaxRpcClientConcurrency,
    #[error("client.poolSize must be at least 1")]
    InvalidRpcClientPoolSize,
    #[error(
        "target.maxRpcClientConcurrency ({target}) conflicts with client.maxConcurrentRequests ({client})"
    )]
    ConflictingRpcClientConcurrency { target: u32, client: u32 },
    #[error("non-local target does not accept local RPC connection field {field}")]
    NonLocalRejectsRpcConnectionField { field: &'static str },
    #[error(
        "scenario requests {requested} actors but runtime manifest advertises {available} bootstrap accounts"
    )]
    TooManyActors { requested: u32, available: u32 },
    #[error("actor provisioning requires runtime bootstrap account 0 with role `faucet`")]
    ActorProvisioningRequiresSponsor,
    #[error("workload {workload:?} requires runtime manifest endpoint {endpoint}")]
    MissingReadPlane {
        workload: WorkloadKind,
        endpoint: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use thruput_json_rpc_types::base58::Base58;
    use thruput_json_rpc_types::runtime_manifest::RuntimeManifest;
    use thruput_signing::derive_dev_account_signer;
    use thruput_xdr::types::Ed25519PublicKey;

    use super::*;
    use crate::manifest::{
        ActorProvisioningSpec, BootstrapSpec, DurationSpec, EventQueryAnchorSpec,
        EventQueryOrderSpec, EventQuerySpec, LoadPhase, WorkloadSpec,
    };
    use crate::target::TargetProviderSpec;

    #[test]
    fn old_schema_and_missing_bootstrap_deployment_amount_are_rejected() {
        let mut manifest = basic_manifest();
        manifest.schema_version = 2;
        let error = validate_manifest(&manifest).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("update the scenario to schema 3")
        );
        manifest.schema_version = SIMULATION_SCHEMA_VERSION;
        manifest.contract_artifacts.push(
            serde_json::from_value(
                json!({"name":"counter","artifact":{"kind":"rv32","path":"counter.rv32a"}}),
            )
            .unwrap(),
        );
        manifest
            .bootstrap
            .as_mut()
            .unwrap()
            .deployment_collateral_raw = None;
        let error = validate_manifest(&manifest).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("bootstrap.deploymentCollateralRaw")
        );
        manifest
            .bootstrap
            .as_mut()
            .unwrap()
            .deployment_collateral_raw = Some("10000000".into());
        validate_manifest(&manifest).unwrap();
    }

    #[test]
    fn native_accounts_ignore_legacy_inventory_but_reject_unsafe_configuration() {
        let mut manifest = basic_manifest();
        manifest.accounts = Some(
            serde_json::from_value(json!({"kind":"native","seed":"test"})).expect("native config"),
        );
        let mut runtime = runtime_manifest();
        runtime.bootstrap_accounts.clear();
        validate_manifest(&manifest).expect("native manifest");
        validate_against_runtime(&manifest, &runtime)
            .expect("native actors need no legacy accounts");
        manifest
            .accounts
            .as_mut()
            .expect("native config")
            .minimum_balance_raw = "0".into();
        assert!(matches!(
            validate_manifest(&manifest),
            Err(ValidationError::NativeAccounts { .. })
        ));
        manifest
            .accounts
            .as_mut()
            .expect("native config")
            .minimum_balance_raw = "1".into();
        manifest
            .contract_artifacts
            .push(contract_artifact("example", "example.rv32a"));
        assert!(matches!(
            validate_manifest(&manifest),
            Err(ValidationError::NativeAccounts { .. })
        ));
        manifest
            .accounts
            .as_mut()
            .expect("native config")
            .deployment_collateral_raw = Some("10000000".into());
        validate_manifest(&manifest).expect("explicit deploy collateral");
        manifest.target.provider = TargetProviderKind::Local;
        validate_manifest(&manifest).expect("local explicit native accounts are supported");
        let accounts = manifest.accounts.as_mut().unwrap();
        accounts.funding = crate::manifest::NativeAccountFunding::Faucet;
        assert!(matches!(
            validate_manifest(&manifest),
            Err(ValidationError::NativeAccounts { .. })
        ));
        manifest.accounts.as_mut().unwrap().funding_amount_raw = Some("1000000000".into());
        validate_manifest(&manifest).expect("explicit local transfer amount");
    }

    #[test]
    fn native_account_settings_reject_unknown_modes_and_fields() {
        for value in [
            json!({"kind":"legacy","seed":"test"}),
            json!({"kind":"native","seed":"test","funding":"automatic"}),
            json!({"kind":"native","seed":"test","walletArtifact":"old.rv32a"}),
        ] {
            assert!(serde_json::from_value::<crate::manifest::NativeAccountsSpec>(value).is_err());
        }
    }

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
    fn rejects_invalid_resource_sample_interval() {
        let mut manifest = basic_manifest();
        manifest.observability = Some(crate::manifest::ObservabilitySpec {
            resource_sample_interval: Some(DurationSpec("0s".into())),
        });

        let err = validate_manifest(&manifest).expect_err("zero interval should reject");

        assert!(matches!(
            err,
            ValidationError::InvalidResourceSampleInterval { .. }
        ));
    }

    #[test]
    fn checked_in_contract_presets_parse_and_validate() {
        for (name, manifest_json) in [
            (
                "local-fibonacci-view-smoke",
                include_str!("../../../examples/sim/local-fibonacci-view-smoke.json"),
            ),
            (
                "local-fibonacci-contract-call-events-smoke",
                include_str!(
                    "../../../examples/sim/local-fibonacci-contract-call-events-smoke.json"
                ),
            ),
            (
                "local-fibonacci-compute-heavy",
                include_str!("../../../examples/sim/local-fibonacci-compute-heavy.json"),
            ),
            (
                "local-fibonacci-event-heavy",
                include_str!("../../../examples/sim/local-fibonacci-event-heavy.json"),
            ),
            (
                "local-calldata-heavy",
                include_str!("../../../examples/sim/local-calldata-heavy.json"),
            ),
            (
                "local-nnint-heavy",
                include_str!("../../../examples/sim/local-nnint-heavy.json"),
            ),
            (
                "local-counter-read-after-write-smoke",
                include_str!("../../../examples/sim/local-counter-read-after-write-smoke.json"),
            ),
            (
                "local-multi-contract-soak",
                include_str!("../../../examples/sim/local-multi-contract-soak.json"),
            ),
            (
                "local-realistic-soak",
                include_str!("../../../examples/sim/local-realistic-soak.json"),
            ),
            (
                "local-realistic-soak-short",
                include_str!("../../../examples/sim/local-realistic-soak-short.json"),
            ),
        ] {
            let manifest: SimulationManifest = serde_json::from_str(manifest_json)
                .unwrap_or_else(|error| panic!("{name} should parse: {error}"));
            validate_manifest(&manifest)
                .unwrap_or_else(|error| panic!("{name} should validate: {error}"));
        }
    }

    #[test]
    fn validates_view_call_contract_references_and_hex() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.view_calls = vec![crate::manifest::ViewCallSpec {
            name: "fib-10".into(),
            contract: "fibonacci".into(),
            weight: 1,
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
    fn validates_rv32_contract_artifact_kind() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![crate::manifest::ContractArtifactSpec {
            name: "counter".into(),
            artifact: crate::manifest::ContractArtifactRef {
                kind: crate::manifest::ContractArtifactKind::Rv32,
                path: "counter.rv32a".into(),
            },
        }];

        validate_manifest(&manifest).expect("rv32 contract artifact should validate");
    }

    #[test]
    fn validates_contract_call_contract_references_and_hex() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("events", "events.rv32a")];
        manifest.contract_calls = vec![crate::manifest::ContractCallSpec {
            name: "events-store-value".into(),
            contract: "events".into(),
            weight: 1,
            entrypoint: "0x0c012d3b21f5676d".into(),
            calldata: "0x2a64".into(),
        }];
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::ContractCall,
            weight: 1,
        }];

        validate_manifest(&manifest).expect("contract-call manifest should validate");
    }

    #[test]
    fn rejects_empty_contract_call_name() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("events", "events.rv32a")];
        manifest.contract_calls = vec![crate::manifest::ContractCallSpec {
            name: " ".into(),
            contract: "events".into(),
            weight: 1,
            entrypoint: "0x0c012d3b21f5676d".into(),
            calldata: "0x2a64".into(),
        }];

        let err = validate_manifest(&manifest).expect_err("empty name should reject");

        assert!(matches!(err, ValidationError::EmptyContractCallName));
    }

    #[test]
    fn rejects_duplicate_contract_call_name() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("events", "events.rv32a")];
        manifest.contract_calls = vec![
            crate::manifest::ContractCallSpec {
                name: "events-store-value".into(),
                contract: "events".into(),
                weight: 1,
                entrypoint: "0x0c012d3b21f5676d".into(),
                calldata: "0x2a64".into(),
            },
            crate::manifest::ContractCallSpec {
                name: "events-store-value".into(),
                contract: "events".into(),
                weight: 1,
                entrypoint: "0x0c012d3b21f5676d".into(),
                calldata: "0x2a64".into(),
            },
        ];

        let err = validate_manifest(&manifest).expect_err("duplicate name should reject");

        assert!(matches!(
            err,
            ValidationError::DuplicateContractCallName { .. }
        ));
    }

    #[test]
    fn validates_multiple_contract_calls_for_weighted_selection() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("events", "events.rv32a")];
        manifest.contract_calls = vec![
            crate::manifest::ContractCallSpec {
                name: "events-store-value".into(),
                contract: "events".into(),
                weight: 1,
                entrypoint: "0x0c012d3b21f5676d".into(),
                calldata: "0x2a64".into(),
            },
            crate::manifest::ContractCallSpec {
                name: "events-checkpoint".into(),
                contract: "events".into(),
                weight: 1,
                entrypoint: "0xae92627315b69337".into(),
                calldata: "0x00".into(),
            },
        ];
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::ContractCall,
            weight: 1,
        }];

        validate_manifest(&manifest).expect("multiple contract calls should validate");
    }

    #[test]
    fn rejects_zero_contract_call_weight() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("events", "events.rv32a")];
        manifest.contract_calls = vec![crate::manifest::ContractCallSpec {
            name: "events-store-value".into(),
            contract: "events".into(),
            weight: 0,
            entrypoint: "0x0c012d3b21f5676d".into(),
            calldata: "0x2a64".into(),
        }];

        let err = validate_manifest(&manifest).expect_err("zero target weight should reject");

        assert!(matches!(
            err,
            ValidationError::ZeroTargetWeight {
                field: "contractCalls",
                ..
            }
        ));
    }

    #[test]
    fn rejects_contract_call_without_matching_contract_artifact() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("events", "events.rv32a")];
        manifest.contract_calls = vec![crate::manifest::ContractCallSpec {
            name: "events-store-value".into(),
            contract: "missing".into(),
            weight: 1,
            entrypoint: "0x0c012d3b21f5676d".into(),
            calldata: "0x2a64".into(),
        }];

        let err = validate_manifest(&manifest).expect_err("unknown contract should reject");

        assert!(matches!(
            err,
            ValidationError::UnknownContractCallContract { .. }
        ));
    }

    #[test]
    fn rejects_contract_call_entrypoint_that_is_not_eight_bytes() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("events", "events.rv32a")];
        manifest.contract_calls = vec![crate::manifest::ContractCallSpec {
            name: "events-store-value".into(),
            contract: "events".into(),
            weight: 1,
            entrypoint: "0x1111".into(),
            calldata: "0x2a64".into(),
        }];

        let err = validate_manifest(&manifest).expect_err("short entrypoint should reject");

        assert!(matches!(
            err,
            ValidationError::InvalidContractCallEntrypoint { .. }
        ));
    }

    #[test]
    fn rejects_contract_call_workload_without_contract_calls() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("events", "events.rv32a")];
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::ContractCall,
            weight: 1,
        }];

        let err = validate_manifest(&manifest).expect_err("empty contractCalls should reject");

        assert!(matches!(
            err,
            ValidationError::MissingContractCallsForContractCall
        ));
    }

    #[test]
    fn validates_storage_read_contract_references_and_key() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.storage_reads = vec![crate::manifest::StorageReadSpec {
            name: "fib-zero-key".into(),
            contract: "fibonacci".into(),
            weight: 1,
            key: format!("0x{}", "00".repeat(32)),
            expect_present: Some(false),
        }];
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::StorageRead,
            weight: 1,
        }];

        validate_manifest(&manifest).expect("storage-read manifest should validate");
    }

    #[test]
    fn rejects_storage_read_without_matching_contract_artifact() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.storage_reads = vec![crate::manifest::StorageReadSpec {
            name: "fib-zero-key".into(),
            contract: "missing".into(),
            weight: 1,
            key: format!("0x{}", "00".repeat(32)),
            expect_present: None,
        }];

        let err = validate_manifest(&manifest).expect_err("unknown contract should reject");

        assert!(matches!(
            err,
            ValidationError::UnknownStorageReadContract { .. }
        ));
    }

    #[test]
    fn rejects_storage_read_key_that_is_not_32_bytes() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.storage_reads = vec![crate::manifest::StorageReadSpec {
            name: "fib-zero-key".into(),
            contract: "fibonacci".into(),
            weight: 1,
            key: "0x1111".into(),
            expect_present: None,
        }];

        let err = validate_manifest(&manifest).expect_err("short key should reject");

        assert!(matches!(err, ValidationError::InvalidStorageReadHex { .. }));
    }

    #[test]
    fn rejects_storage_read_contract_reference_whitespace() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.storage_reads = vec![crate::manifest::StorageReadSpec {
            name: "fib-zero-key".into(),
            contract: " fibonacci ".into(),
            weight: 1,
            key: format!("0x{}", "00".repeat(32)),
            expect_present: None,
        }];

        let err = validate_manifest(&manifest).expect_err("padded reference should reject");

        assert!(matches!(
            err,
            ValidationError::InvalidContractNameWhitespace { .. }
        ));
    }

    #[test]
    fn rejects_view_call_without_matching_contract_artifact() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.view_calls = vec![crate::manifest::ViewCallSpec {
            name: "fib-10".into(),
            contract: "missing".into(),
            weight: 1,
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
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.view_calls = vec![crate::manifest::ViewCallSpec {
            name: "fib-10".into(),
            contract: "fibonacci".into(),
            weight: 1,
            entrypoint: "0x1111".into(),
            calldata: "0x0a".into(),
            expect_return_data: None,
        }];

        let err = validate_manifest(&manifest).expect_err("short entrypoint should reject");

        assert!(matches!(
            err,
            ValidationError::InvalidViewCallEntrypoint { .. }
        ));
    }

    #[test]
    fn rejects_view_call_contract_reference_whitespace() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.view_calls = vec![crate::manifest::ViewCallSpec {
            name: "fib-10".into(),
            contract: " fibonacci ".into(),
            weight: 1,
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
    fn rejects_storage_read_workload_without_db_gateway() {
        let mut manifest = basic_manifest();
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::StorageRead,
            weight: 1,
        }];
        let mut runtime = runtime_manifest();
        runtime.endpoints.db_gateway = None;

        let err = validate_against_runtime(&manifest, &runtime)
            .expect_err("storage-read should require db gateway");

        assert!(matches!(
            err,
            ValidationError::MissingReadPlane {
                workload: WorkloadKind::StorageRead,
                endpoint: "dbGateway"
            }
        ));
    }

    #[test]
    fn validates_events_query_manifest() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.event_queries = vec![EventQuerySpec {
            name: "fib-events".into(),
            contract: "fibonacci".into(),
            weight: 1,
            order: EventQueryOrderSpec::Descending,
            anchor: EventQueryAnchorSpec::AtEnd,
            cursor: "0x".into(),
            limit: 10,
            expect_min_events: Some(0),
        }];
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::EventsQuery,
            weight: 1,
        }];

        validate_manifest(&manifest).expect("event query manifest should validate");
    }

    #[test]
    fn rejects_events_query_unknown_contract() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.event_queries = vec![EventQuerySpec {
            name: "fib-events".into(),
            contract: "missing".into(),
            weight: 1,
            order: EventQueryOrderSpec::Descending,
            anchor: EventQueryAnchorSpec::AtEnd,
            cursor: "0x".into(),
            limit: 10,
            expect_min_events: Some(0),
        }];

        let err = validate_manifest(&manifest).expect_err("unknown contract should reject");

        assert!(matches!(
            err,
            ValidationError::UnknownEventQueryContract { .. }
        ));
    }

    #[test]
    fn rejects_events_query_invalid_cursor_and_anchor() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.event_queries = vec![EventQuerySpec {
            name: "fib-events".into(),
            contract: "fibonacci".into(),
            weight: 1,
            order: EventQueryOrderSpec::Descending,
            anchor: EventQueryAnchorSpec::AtEnd,
            cursor: "0x0".into(),
            limit: 10,
            expect_min_events: Some(0),
        }];

        let err = validate_manifest(&manifest).expect_err("odd cursor should reject");
        assert!(matches!(err, ValidationError::InvalidEventQueryHex { .. }));

        manifest.event_queries[0].cursor = "0x".into();
        manifest.event_queries[0].anchor = EventQueryAnchorSpec::AtHeight { height: -1 };
        let err = validate_manifest(&manifest).expect_err("negative height should reject");
        assert!(matches!(
            err,
            ValidationError::InvalidEventQueryAnchorHeight { .. }
        ));

        manifest.event_queries[0].anchor = EventQueryAnchorSpec::AtEnd;
        manifest.event_queries[0].cursor = format!("0x{}", "00".repeat(257));
        let err = validate_manifest(&manifest).expect_err("oversized cursor should reject");
        assert!(matches!(err, ValidationError::InvalidEventQueryHex { .. }));
    }

    #[test]
    fn rejects_events_query_invalid_limit() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.event_queries = vec![EventQuerySpec {
            name: "fib-events".into(),
            contract: "fibonacci".into(),
            weight: 1,
            order: EventQueryOrderSpec::Descending,
            anchor: EventQueryAnchorSpec::AtEnd,
            cursor: "0x".into(),
            limit: 0,
            expect_min_events: Some(0),
        }];

        let err = validate_manifest(&manifest).expect_err("zero limit should reject");
        assert!(matches!(
            err,
            ValidationError::InvalidEventQueryLimit { .. }
        ));

        manifest.event_queries[0].limit = 201;
        let err = validate_manifest(&manifest).expect_err("oversized limit should reject");
        assert!(matches!(
            err,
            ValidationError::InvalidEventQueryLimit { .. }
        ));
    }

    #[test]
    fn rejects_events_query_workload_without_event_queries() {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts = vec![contract_artifact("fibonacci", "fibonacci.rv32a")];
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::EventsQuery,
            weight: 1,
        }];

        let err = validate_manifest(&manifest).expect_err("empty eventQueries should reject");

        assert!(matches!(
            err,
            ValidationError::MissingEventQueriesForEventsQuery
        ));
    }

    #[test]
    fn rejects_events_query_workload_without_indexer() {
        let mut manifest = basic_manifest();
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::EventsQuery,
            weight: 1,
        }];
        let mut runtime = runtime_manifest();
        runtime.endpoints.indexer = None;

        let err = validate_against_runtime(&manifest, &runtime)
            .expect_err("events-query should require indexer");

        assert!(matches!(
            err,
            ValidationError::MissingReadPlane {
                workload: WorkloadKind::EventsQuery,
                endpoint: "indexer"
            }
        ));
    }

    fn read_after_write_entry() -> crate::manifest::ReadAfterWriteSpec {
        crate::manifest::ReadAfterWriteSpec {
            name: "counter-roundtrip".into(),
            contract: "caller-counter".into(),
            weight: 1,
            write: crate::manifest::RawWriteSpec {
                entrypoint: "0x1111111111111111".into(),
                calldata: "0x".into(),
            },
            read: crate::manifest::RawReadSpec {
                entrypoint: "0x2222222222222222".into(),
                calldata: "0x".into(),
            },
        }
    }

    fn read_after_write_manifest() -> SimulationManifest {
        let mut manifest = basic_manifest();
        manifest.contract_artifacts =
            vec![contract_artifact("caller-counter", "caller-counter.rv32a")];
        manifest.read_after_write = vec![read_after_write_entry()];
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::ReadAfterWrite,
            weight: 1,
        }];
        manifest
    }

    #[test]
    fn validates_read_after_write_references_and_hex() {
        validate_manifest(&read_after_write_manifest())
            .expect("read-after-write manifest should validate");
    }

    #[test]
    fn rejects_read_after_write_unknown_contract() {
        let mut manifest = read_after_write_manifest();
        manifest.read_after_write[0].contract = "missing".into();

        let err = validate_manifest(&manifest).expect_err("unknown contract should reject");

        assert!(matches!(
            err,
            ValidationError::UnknownReadAfterWriteContract { .. }
        ));
    }

    #[test]
    fn rejects_read_after_write_short_entrypoint() {
        let mut manifest = read_after_write_manifest();
        manifest.read_after_write[0].write.entrypoint = "0x1111".into();

        let err = validate_manifest(&manifest).expect_err("short entrypoint should reject");

        assert!(matches!(
            err,
            ValidationError::InvalidReadAfterWriteEntrypoint {
                field: "write.entrypoint",
                ..
            }
        ));
    }

    #[test]
    fn rejects_duplicate_read_after_write_name() {
        let mut manifest = read_after_write_manifest();
        manifest.read_after_write = vec![read_after_write_entry(), read_after_write_entry()];

        let err = validate_manifest(&manifest).expect_err("duplicate name should reject");

        // Duplicate names are detected before the single-entry cap.
        assert!(matches!(
            err,
            ValidationError::DuplicateReadAfterWriteName { .. }
        ));
    }

    #[test]
    fn validates_multiple_read_after_write_entries_for_weighted_selection() {
        let mut manifest = read_after_write_manifest();
        let mut second = read_after_write_entry();
        second.name = "second-roundtrip".into();
        manifest.read_after_write = vec![read_after_write_entry(), second];

        validate_manifest(&manifest).expect("multiple read-after-write entries should validate");
    }

    #[test]
    fn rejects_read_after_write_workload_without_entries() {
        let mut manifest = read_after_write_manifest();
        manifest.read_after_write = Vec::new();

        let err = validate_manifest(&manifest).expect_err("empty readAfterWrite should reject");

        assert!(matches!(err, ValidationError::MissingReadAfterWriteEntries));
    }

    #[test]
    fn rejects_read_after_write_workload_without_view_gateway() {
        let mut manifest = basic_manifest();
        manifest.workloads = vec![WorkloadSpec {
            name: WorkloadKind::ReadAfterWrite,
            weight: 1,
        }];
        let runtime = runtime_manifest();
        assert!(runtime.endpoints.view_gateway.is_none());

        let err = validate_against_runtime(&manifest, &runtime)
            .expect_err("read-after-write should require view gateway");

        assert!(matches!(
            err,
            ValidationError::MissingReadPlane {
                workload: WorkloadKind::ReadAfterWrite,
                endpoint: "viewGateway"
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
    fn faucet_does_not_count_as_an_actor_account() {
        let mut manifest = basic_manifest();
        manifest.load_shape[0].users = 2;
        let mut runtime = runtime_manifest();
        let mut faucet = runtime.bootstrap_accounts[0].clone();
        faucet.index = Some(9);
        faucet.role = "faucet".into();
        runtime.bootstrap_accounts.push(faucet);

        let err = validate_against_runtime(&manifest, &runtime)
            .expect_err("faucet should not satisfy actor count");

        assert!(matches!(
            err,
            ValidationError::TooManyActors {
                requested: 2,
                available: 1
            }
        ));
    }

    #[test]
    fn allows_actor_count_above_bootstrap_accounts_when_provisioning_enabled() {
        let mut manifest = basic_manifest();
        manifest.load_shape[0].users = 2;
        manifest.bootstrap = Some(BootstrapSpec {
            deployment_collateral_raw: None,
            accounts: 2,
            account_seed: "seed".into(),
            initial_storage_tokens: "1000000000".into(),
            storage_cost_profile: "default".into(),
            actor_provisioning: Some(ActorProvisioningSpec {
                enabled: true,
                airdrop_hog: None,
            }),
        });
        let runtime = runtime_manifest();
        let mut runtime = runtime;
        let mut faucet = runtime.bootstrap_accounts[0].clone();
        faucet.index = Some(0);
        faucet.role = BOOTSTRAP_ACCOUNT_ROLE_FAUCET.into();
        runtime.bootstrap_accounts.push(faucet);

        validate_against_runtime(&manifest, &runtime)
            .expect("actor provisioning should allow creating missing accounts");
    }

    #[test]
    fn allows_faucet_only_runtime_to_provision_first_workload_actor() {
        let mut manifest = basic_manifest();
        manifest.bootstrap = Some(BootstrapSpec {
            deployment_collateral_raw: None,
            accounts: 1,
            account_seed: "seed".into(),
            initial_storage_tokens: "1000000000".into(),
            storage_cost_profile: "hogd-default".into(),
            actor_provisioning: Some(ActorProvisioningSpec {
                enabled: true,
                airdrop_hog: None,
            }),
        });
        let mut runtime = runtime_manifest();
        runtime.bootstrap_accounts[0].index = Some(0);
        runtime.bootstrap_accounts[0].role = BOOTSTRAP_ACCOUNT_ROLE_FAUCET.into();

        validate_against_runtime(&manifest, &runtime)
            .expect("index-zero faucet should sponsor the first workload actor");
    }

    #[test]
    fn rejects_actor_provisioning_without_runtime_sponsor() {
        let mut manifest = basic_manifest();
        manifest.load_shape[0].users = 2;
        manifest.bootstrap = Some(BootstrapSpec {
            deployment_collateral_raw: None,
            accounts: 1,
            account_seed: "seed".into(),
            initial_storage_tokens: "1000000000".into(),
            storage_cost_profile: "default".into(),
            actor_provisioning: Some(ActorProvisioningSpec {
                enabled: true,
                airdrop_hog: None,
            }),
        });
        let mut runtime = runtime_manifest();
        runtime.bootstrap_accounts[0].role = BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER.into();

        let err = validate_against_runtime(&manifest, &runtime)
            .expect_err("actor provisioning should require a sponsor account");

        assert!(matches!(
            err,
            ValidationError::ActorProvisioningRequiresSponsor
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
    fn validate_manifest_accepts_local_block_driver_options() {
        let mut manifest = basic_manifest();
        manifest.target.provider = TargetProviderKind::Local;
        manifest.target.rpc_url = None;
        manifest.target.runtime_manifest = None;
        manifest.target.block_driver = Some(crate::target::TargetBlockDriverMode::Interval);
        manifest.target.block_interval_ms = Some(250);
        manifest.target.max_rpc_connections = Some(512);

        validate_manifest(&manifest).expect("local block driver options should be valid");
    }

    #[test]
    fn validate_manifest_rejects_non_local_max_rpc_connections() {
        let mut manifest = basic_manifest();
        manifest.target.max_rpc_connections = Some(512);

        let err = validate_manifest(&manifest).expect_err("existing target should reject field");

        assert!(matches!(
            err,
            ValidationError::NonLocalRejectsRpcConnectionField {
                field: "target.maxRpcConnections"
            }
        ));
    }

    #[test]
    fn validate_manifest_rejects_zero_max_rpc_connections() {
        let mut manifest = basic_manifest();
        manifest.target.provider = TargetProviderKind::Local;
        manifest.target.rpc_url = None;
        manifest.target.runtime_manifest = None;
        manifest.target.max_rpc_connections = Some(0);

        let err = validate_manifest(&manifest).expect_err("zero connection cap should be invalid");

        assert!(matches!(err, ValidationError::InvalidMaxRpcConnections));
    }

    #[test]
    fn validate_manifest_accepts_existing_rpc_client_concurrency() {
        let mut manifest = basic_manifest();
        manifest.client = Some(crate::manifest::ClientSpec {
            max_concurrent_requests: Some(512),
            pool_size: Some(8),
        });

        validate_manifest(&manifest).expect("existing target client concurrency should be valid");
    }

    #[test]
    fn validate_manifest_rejects_zero_rpc_client_concurrency() {
        let mut manifest = basic_manifest();
        manifest.client = Some(crate::manifest::ClientSpec {
            max_concurrent_requests: Some(0),
            pool_size: None,
        });

        let err =
            validate_manifest(&manifest).expect_err("zero client concurrency should be invalid");

        assert!(matches!(
            err,
            ValidationError::InvalidMaxRpcClientConcurrency
        ));
    }

    #[test]
    fn validate_manifest_rejects_zero_rpc_client_pool_size() {
        let mut manifest = basic_manifest();
        manifest.client = Some(crate::manifest::ClientSpec {
            max_concurrent_requests: None,
            pool_size: Some(0),
        });

        let err = validate_manifest(&manifest).expect_err("zero client pool should be invalid");

        assert!(matches!(err, ValidationError::InvalidRpcClientPoolSize));
    }

    #[test]
    fn validate_manifest_accepts_legacy_target_rpc_client_concurrency() {
        let mut manifest = basic_manifest();
        manifest.target.max_rpc_client_concurrency = Some(512);

        validate_manifest(&manifest).expect("legacy target client concurrency should be valid");
    }

    #[test]
    fn validate_manifest_rejects_conflicting_rpc_client_concurrency() {
        let mut manifest = basic_manifest();
        manifest.target.max_rpc_client_concurrency = Some(512);
        manifest.client = Some(crate::manifest::ClientSpec {
            max_concurrent_requests: Some(256),
            pool_size: None,
        });

        let err = validate_manifest(&manifest).expect_err("conflicting cap should be invalid");

        assert!(matches!(
            err,
            ValidationError::ConflictingRpcClientConcurrency {
                target: 512,
                client: 256
            }
        ));
    }

    #[test]
    fn validate_manifest_rejects_non_local_block_driver_options() {
        let mut manifest = basic_manifest();
        manifest.target.block_driver = Some(crate::target::TargetBlockDriverMode::Interval);

        let err = validate_manifest(&manifest).expect_err("existing target should reject field");

        assert!(matches!(
            err,
            ValidationError::NonLocalRejectsBlockDriverField {
                field: "target.blockDriver"
            }
        ));
    }

    #[test]
    fn validate_manifest_rejects_zero_block_interval() {
        let mut manifest = basic_manifest();
        manifest.target.provider = TargetProviderKind::Local;
        manifest.target.rpc_url = None;
        manifest.target.runtime_manifest = None;
        manifest.target.block_interval_ms = Some(0);

        let err = validate_manifest(&manifest).expect_err("zero interval should be invalid");

        assert!(matches!(err, ValidationError::InvalidBlockIntervalMs));
    }

    #[test]
    fn validate_manifest_rejects_block_interval_without_interval_driver() {
        let mut manifest = basic_manifest();
        manifest.target.provider = TargetProviderKind::Local;
        manifest.target.rpc_url = None;
        manifest.target.runtime_manifest = None;
        manifest.target.block_interval_ms = Some(250);

        let err = validate_manifest(&manifest)
            .expect_err("local interval without interval driver should be invalid");

        assert!(matches!(
            err,
            ValidationError::BlockIntervalRequiresIntervalDriver
        ));

        manifest.target.block_driver = Some(crate::target::TargetBlockDriverMode::Instant);
        let err = validate_manifest(&manifest)
            .expect_err("instant block driver interval should be invalid");

        assert!(matches!(
            err,
            ValidationError::BlockIntervalRequiresIntervalDriver
        ));
    }

    #[test]
    fn required_bootstrap_accounts_uses_max_phase_users() {
        let mut manifest = basic_manifest();
        manifest.load_shape = vec![
            LoadPhase {
                duration: DurationSpec("1s".into()),
                users: 2,
                rate_per_user_hz: 1.0,
                stagger_actors: false,
            },
            LoadPhase {
                duration: DurationSpec("1s".into()),
                users: 5,
                rate_per_user_hz: 1.0,
                stagger_actors: false,
            },
            LoadPhase {
                duration: DurationSpec("1s".into()),
                users: 3,
                rate_per_user_hz: 1.0,
                stagger_actors: false,
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
                block_driver: None,
                block_interval_ms: None,
                max_rpc_connections: None,
                max_rpc_client_concurrency: None,
            },
            client: None,
            bootstrap: Some(serde_json::from_value(json!({"accounts":1,"accountSeed":"seed","initialStorageTokens":"1000000000","storageCostProfile":"hog-devnode-default","deploymentCollateralRaw":"10000000"})).unwrap()),
            accounts: None,
            load_shape: vec![LoadPhase {
                duration: DurationSpec("1s".into()),
                users: 1,
                rate_per_user_hz: 1.0,
                stagger_actors: false,
            }],
            workloads: vec![WorkloadSpec {
                name: WorkloadKind::ChainTip,
                weight: 1,
            }],
            contract_artifacts: Vec::new(),
            view_calls: Vec::new(),
            contract_calls: Vec::new(),
            storage_reads: Vec::new(),
            event_queries: Vec::new(),
            read_after_write: Vec::new(),
            assertions: crate::manifest::AssertionSpec::default(),
            chaos: Vec::new(),
            observability: None,
            baseline: None,
            resolved: None,
        }
    }

    fn contract_artifact(name: &str, path: &str) -> crate::manifest::ContractArtifactSpec {
        crate::manifest::ContractArtifactSpec {
            name: name.into(),
            artifact: crate::manifest::ContractArtifactRef {
                kind: crate::manifest::ContractArtifactKind::Rv32,
                path: path.into(),
            },
        }
    }

    fn runtime_manifest() -> RuntimeManifest {
        let signer = derive_dev_account_signer("seed", 0);
        let public_key = Base58(Ed25519PublicKey::from(signer.verifying_key_bytes()));
        serde_json::from_value(json!({
            "schemaVersion": 4,
            "kind": "hosted-devnet-runtime",
            "chainState": "fresh",
            "networkHash": "1".repeat(32),
            "protocol": "hogp/test00000000000",
            "producer": {
                "binary": "hog-devnode",
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
                "walletAddress": identifier(0x22),
                "initialStorageTokens": "1000000000"
            }],
            "bootstrap": {
                "storageCostModel": {
                    "hash": identifier(0x55),
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

    fn identifier(byte: u8) -> String {
        thruput_json_rpc_types::NetworkHashText::from([byte; 32]).to_string()
    }
}
