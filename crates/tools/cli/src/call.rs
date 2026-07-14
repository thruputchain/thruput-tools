use std::path::Path;

use groundhog_contract_util::NormalizedMethodSignature;
use groundhog_json_rpc_types::receipt::ExecStatus;
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams,
    build_dev_wallet_do_action_transaction, sign_and_encode_transaction_hex,
};
use groundhog_xdr::contract::ContractEntrypointID;
use serde_json::json;

use crate::account_resolution::resolve_wallet_address;
use crate::artifacts::ArtifactOptions;
use crate::build::{cargo_metadata_with_artifacts, resolve_package};
use crate::codec::{
    CallMetadata, ContractMetadata, MethodMetadata, SpecMethodMetadata, decode_return, encode_args,
    format_contract_address, parse_contract_address, spec_string_text,
};
use crate::config::{
    ProfileOverrides, find_project_config, resolve_default_key_for_profile,
    resolve_profile_with_overrides,
};
use crate::contract_spec::{InterfaceSpec, load_contract_spec, load_registered_interfaces};
use crate::dev_detail::{fetch_transaction_detail_blocking, print_transaction_detail};
use crate::keys::load_local_signer;
use crate::network_descriptor::resolve_wallet_entrypoints;
use crate::result::CommandResult;
use crate::tx_utils::{
    command_error_for_exec_status, decode_receipt_abort, format_tx_hash, nonce_from_timestamp,
    print_failed_receipt_data, resolve_block_expiry_height, submit_and_poll, summarize_exec_status,
};
use crate::{CliError, Print};

/// Resolved contract entrypoint plus any metadata used for codec work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedCall {
    pub(crate) entrypoint: ContractEntrypointID,
    pub(crate) metadata: Option<CallMetadata>,
}

/// Fully prepared call submission inputs shared by live execution and dry-run flows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCall {
    pub(crate) rpc_url: String,
    pub(crate) contract_address_hex: String,
    pub(crate) wallet_address_hex: String,
    pub(crate) wallet_entrypoint_id: String,
    pub(crate) wallet_entrypoint_source: &'static str,
    pub(crate) method: String,
    pub(crate) resolved: ResolvedCall,
    pub(crate) calldata: Vec<u8>,
    pub(crate) tx_hex: String,
}

/// Call a method on a deployed contract.
#[allow(clippy::too_many_arguments)]
pub fn run(
    contract: &str,
    method: &str,
    args: &[String],
    dry_run: bool,
    artifacts: &ArtifactOptions,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let prepared = prepare_call(contract, method, args, artifacts, profile, overrides)?;
    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    let task = print.start_task(format_args!("Calling `{}`", prepared.method));

    let (tx_hash, receipt) = submit_prepared_call(&prepared)?;
    task.clear();
    let transaction_detail = fetch_transaction_detail_blocking(&prepared.rpc_url, tx_hash, print);
    let tx_hash_formatted = format_tx_hash(&tx_hash);
    let status_summary = summarize_exec_status(&receipt.status);
    let diagnostics = (!matches!(receipt.status, ExecStatus::Ok))
        .then(|| call_diagnostics_json(&prepared, transaction_detail.is_some()));
    let decoded_return = receipt
        .return_data
        .as_ref()
        .and_then(|data| decode_return(data.0.as_slice(), prepared.resolved.metadata.as_ref()));
    let decoded_abort = decode_receipt_abort(&receipt, prepared.resolved.metadata.as_ref());

    // Print result
    match &receipt.status {
        ExecStatus::Ok => {
            print.success("Call succeeded");
            if prepared.resolved.metadata.is_some() {
                print.detail("method", &prepared.method);
                if let Some(ref value) = decoded_return {
                    print.detail("value", value);
                }
            } else {
                print.detail("entrypoint", &prepared.method);
            }
            print.detail("contract", &prepared.contract_address_hex);
            print.detail("tx", &tx_hash_formatted);
            print.detail("height", *receipt.height);
            if decoded_return.is_none()
                && let Some(ref data) = receipt.return_data
                && !data.0.is_empty()
            {
                print.detail("return", format_args!("0x{}", hex::encode(&data.0)));
            }
        }
        ExecStatus::Reverted { .. } => {
            print.warn(format_args!("Call {status_summary}"));
            print.detail("method", &prepared.method);
            print.detail("contract", &prepared.contract_address_hex);
            print.detail("tx", &tx_hash_formatted);
            print.detail("height", *receipt.height);
            print_failed_receipt_data(&receipt, decoded_abort.as_ref(), print);
            print_failed_call_diagnostics(&prepared, transaction_detail.is_some(), print);
        }
        other => {
            print.warn(format_args!("Call {}", summarize_exec_status(other)));
            print.detail("method", &prepared.method);
            print.detail("contract", &prepared.contract_address_hex);
            print.detail("tx", &tx_hash_formatted);
            print.detail("height", *receipt.height);
            print_failed_receipt_data(&receipt, decoded_abort.as_ref(), print);
            print_failed_call_diagnostics(&prepared, transaction_detail.is_some(), print);
        }
    }

    if let Some(detail) = transaction_detail.as_ref() {
        print_transaction_detail(detail, print);
    }

    let mut data = json!({
        "tx_hash": tx_hash_formatted,
        "contract": prepared.contract_address_hex,
        "method": prepared.method,
        "height": receipt.height,
        "status": receipt.status,
        "status_summary": status_summary,
        "return_data": receipt.return_data,
        "abort_data": receipt.abort_data,
        "return_value": decoded_return,
        "transaction_detail": transaction_detail,
        "diagnostics": diagnostics,
        "wallet_entrypoint_id": prepared.wallet_entrypoint_id,
        "wallet_entrypoint_source": prepared.wallet_entrypoint_source,
    });
    match decoded_abort.as_ref() {
        Some(Ok(abort)) => {
            data["abort_exception"] = json!(abort.exception);
            data["abort_value"] = json!(abort.value);
        }
        Some(Err(error)) => {
            data["abort_decode_error"] = json!(error.to_string());
        }
        None => {}
    }
    if let Some(error) = command_error_for_exec_status(&receipt.status) {
        print.json(&CommandResult::failure_with_error(cmd, data, error))?;
        return Err(CliError::CommandReportedFailure);
    }
    print.json(&CommandResult::new(cmd, data))?;

    Ok(())
}

fn print_failed_call_diagnostics(
    prepared: &PreparedCall,
    transaction_detail_available: bool,
    print: &Print,
) {
    print.detail(
        "wallet",
        format_args!(
            "{} entrypoint={} source={}",
            prepared.wallet_address_hex,
            prepared.wallet_entrypoint_id,
            prepared.wallet_entrypoint_source
        ),
    );
    print.detail(
        "target entrypoint",
        format_entrypoint_id(prepared.resolved.entrypoint),
    );
    if !transaction_detail_available {
        print.detail(
            "transaction detail",
            "unavailable from groundhog_dev_getTransactionDetail",
        );
    }
}

fn call_diagnostics_json(
    prepared: &PreparedCall,
    transaction_detail_available: bool,
) -> serde_json::Value {
    json!({
        "wallet": {
            "address": prepared.wallet_address_hex,
            "entrypoint_id": prepared.wallet_entrypoint_id,
            "entrypoint_source": prepared.wallet_entrypoint_source,
        },
        "target": {
            "contract": prepared.contract_address_hex,
            "method": prepared.method,
            "entrypoint_id": format_entrypoint_id(prepared.resolved.entrypoint),
        },
        "transaction_detail": {
            "rpc_method": "groundhog_dev_getTransactionDetail",
            "available": transaction_detail_available,
        },
    })
}

fn print_dry_run(
    prepared: &PreparedCall,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let entrypoint_id = format_entrypoint_id(prepared.resolved.entrypoint);
    let calldata_hex = format_hex_bytes(&prepared.calldata);

    print.status(format_args!(
        "Prepared dry-run call {} on {} via {}.",
        prepared.method, prepared.contract_address_hex, prepared.rpc_url
    ));
    print.status(format_args!(
        "  selector: {}",
        selector_summary(&prepared.resolved)
    ));
    print.status(format_args!("  entrypoint: {entrypoint_id}"));
    print.status(format_args!("  calldata: {calldata_hex}"));
    print.status(format_args!("  signed tx: {}", prepared.tx_hex));
    print.status("  submitted: no (--dry-run)");

    print.json(&CommandResult::new(
        cmd,
        json!({
            "dry_run": true,
            "submitted": false,
            "rpc_url": prepared.rpc_url,
            "contract": prepared.contract_address_hex,
            "method": prepared.method,
            "entrypoint_id": entrypoint_id,
            "wallet_entrypoint_id": prepared.wallet_entrypoint_id,
            "wallet_entrypoint_source": prepared.wallet_entrypoint_source,
            "calldata": calldata_hex,
            "signed_tx": prepared.tx_hex,
            "selector": selector_json(&prepared.resolved),
        }),
    ))?;

    Ok(())
}

pub(crate) fn format_entrypoint_id(entrypoint: ContractEntrypointID) -> String {
    format!("0x{}", hex::encode(entrypoint.into_inner()))
}

pub(crate) fn format_hex_bytes(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn selector_summary(resolved: &ResolvedCall) -> String {
    match &resolved.metadata {
        Some(CallMetadata::Spec(metadata)) => {
            format!(
                "embedded spec {} v{}",
                metadata.interface_name, metadata.version
            )
        }
        Some(CallMetadata::Local(metadata)) => {
            format!(
                "local metadata {} v{}",
                metadata.interface_name, metadata.version
            )
        }
        None => "explicit entrypoint".into(),
    }
}

pub(crate) fn selector_json(resolved: &ResolvedCall) -> serde_json::Value {
    match &resolved.metadata {
        Some(CallMetadata::Spec(metadata)) => json!({
            "source": "embedded_spec",
            "interface": {
                "name": metadata.interface_name,
                "version": metadata.version,
            },
        }),
        Some(CallMetadata::Local(metadata)) => json!({
            "source": "local_metadata",
            "interface": {
                "name": metadata.interface_name,
                "version": metadata.version,
            },
            "source_path": metadata.source.display().to_string(),
        }),
        None => json!({
            "source": "explicit_entrypoint",
            "interface": serde_json::Value::Null,
        }),
    }
}

/// Resolve, encode, and sign a call transaction without submitting it.
pub(crate) fn prepare_call(
    contract: &str,
    method: &str,
    args: &[String],
    artifacts: &ArtifactOptions,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
) -> Result<PreparedCall, CliError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    prepare_call_with_runtime(contract, method, args, artifacts, profile, overrides, &rt)
}

#[allow(clippy::too_many_lines)]
fn prepare_call_with_runtime(
    contract: &str,
    method: &str,
    args: &[String],
    artifacts: &ArtifactOptions,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    rt: &tokio::runtime::Runtime,
) -> Result<PreparedCall, CliError> {
    let cwd = std::env::current_dir()?;
    let resolved_profile = resolve_profile_with_overrides(&cwd, profile, overrides)?;
    let key_name = resolve_default_key_for_profile(&resolved_profile)?;
    if resolved_profile.rpc_url.is_none() && key_name.is_none() {
        return Err(CliError::MissingRpcUrl);
    }
    let key_name = key_name.ok_or(CliError::MissingSigningKey)?;
    let rpc_url = resolved_profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?;

    let contract_address_hex = resolved_profile.resolve_contract(contract);
    let contract_address = parse_contract_address(&contract_address_hex)?;

    let signer = load_local_signer(&key_name)?;

    let resolved = resolve_call(&cwd, method, artifacts)?;
    let calldata = encode_args(args, resolved.metadata.as_ref())?;

    let wallet_address = resolve_wallet_address(&resolved_profile, &key_name, &signer)?;
    let wallet_address_hex = format_contract_address(&wallet_address);

    let wallet_entrypoints = resolve_wallet_entrypoints(&resolved_profile, &key_name)?;
    let wallet_entrypoint_id = format_entrypoint_id(wallet_entrypoints.do_action);
    let block_expiry_height = rt.block_on(async { resolve_block_expiry_height(rpc_url).await })?;

    let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
        wallet_address,
        wallet_entrypoint: wallet_entrypoints.do_action,
        target_address: contract_address,
        target_entrypoint: resolved.entrypoint,
        target_calldata: calldata.as_slice(),
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        target_gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height,
        nonce: nonce_from_timestamp(),
    })
    .map_err(|e| CliError::Other(e.to_string()))?;

    let tx_hex = sign_and_encode_transaction_hex(tx, &signer, SignTransactionParams::default())
        .map_err(|e| CliError::Other(e.to_string()))?;

    Ok(PreparedCall {
        rpc_url: rpc_url.to_owned(),
        contract_address_hex,
        wallet_address_hex,
        wallet_entrypoint_id,
        wallet_entrypoint_source: wallet_entrypoints.source,
        method: method.to_owned(),
        resolved,
        calldata,
        tx_hex,
    })
}

/// Submit a previously prepared call transaction and wait for its receipt.
pub(crate) fn submit_prepared_call(
    prepared: &PreparedCall,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    submit_prepared_call_with_runtime(prepared, &rt)
}

fn submit_prepared_call_with_runtime(
    prepared: &PreparedCall,
    rt: &tokio::runtime::Runtime,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    rt.block_on(async { submit_and_poll(&prepared.rpc_url, &prepared.tx_hex).await })
}

fn explicit_entrypoint_id(method: &str) -> Result<Option<ContractEntrypointID>, CliError> {
    let Some(hex_str) = method
        .strip_prefix("0x")
        .or_else(|| method.strip_prefix("0X"))
    else {
        return Ok(None);
    };
    if hex_str.len() != 16 {
        return Err(CliError::Other(
            "explicit entrypoint IDs must be 8 bytes / 16 hex characters".into(),
        ));
    }
    let bytes = hex::decode(hex_str)
        .map_err(|e| CliError::Other(format!("invalid explicit entrypoint hex: {e}")))?;
    let id: [u8; 8] = bytes
        .try_into()
        .map_err(|_| CliError::Other("entrypoint ID must be exactly 8 bytes".into()))?;
    Ok(Some(ContractEntrypointID::from(id)))
}

/// Compute the canonical entrypoint ID for a method using the shared
/// `groundhog-contract-util` implementation.  The returned type is from
/// the workspace `groundhog-xdr` version; the utility crate may lag
/// behind, so we round-trip through raw bytes.
fn method_entrypoint_id(interface_name: &str, version: &str, method: &str) -> ContractEntrypointID {
    let sig = NormalizedMethodSignature::new(method);
    let id = groundhog_contract_util::contract_entrypoint_id(Some(interface_name), version, &sig);
    let bytes: [u8; 8] = id.as_ref()[..8]
        .try_into()
        .expect("bug: entrypoint ID is always 8 bytes");
    ContractEntrypointID::from(bytes)
}

pub(crate) fn resolve_call(
    cwd: &Path,
    method: &str,
    artifacts: &ArtifactOptions,
) -> Result<ResolvedCall, CliError> {
    if let Some(entrypoint) = explicit_entrypoint_id(method)? {
        return Ok(ResolvedCall {
            entrypoint,
            metadata: None,
        });
    }
    if let Some(metadata) = resolve_spec_method(cwd, method, artifacts)? {
        return Ok(ResolvedCall {
            entrypoint: method_entrypoint_id(&metadata.interface_name, &metadata.version, method),
            metadata: Some(CallMetadata::Spec(metadata)),
        });
    }
    match resolve_local_contract_metadata(cwd, method, artifacts)? {
        Some(metadata) => Ok(ResolvedCall {
            entrypoint: method_entrypoint_id(&metadata.interface_name, &metadata.version, method),
            metadata: Some(CallMetadata::Local(metadata)),
        }),
        None => Err(CliError::Other(
            "cannot resolve the contract method from the local project or embedded contract spec. \
             Run `groundhog build` from inside the project that defines the contract, or pass an \
             explicit 0x-prefixed 8-byte entrypoint ID."
                .into(),
        )),
    }
}

fn resolve_spec_method(
    cwd: &Path,
    method: &str,
    artifacts: &ArtifactOptions,
) -> Result<Option<SpecMethodMetadata>, CliError> {
    let Ok(metadata) = cargo_metadata_with_artifacts(artifacts) else {
        return Ok(None);
    };
    let Ok(pkg_name) = resolve_package(&metadata, None, cwd) else {
        return Ok(None);
    };
    let wasm = crate::artifacts::groundhog_wasm_output_path(&metadata.target_directory, pkg_name);
    let Some(spec) = load_contract_spec(&wasm)? else {
        return Ok(None);
    };

    let mut interface_matches = Vec::new();

    for interface in spec.interfaces.iter() {
        let matched_methods = interface
            .methods
            .iter()
            .filter_map(|candidate| match spec_string_text(&candidate.name) {
                Ok(name) if name == method => Some(Ok(candidate.clone())),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            })
            .collect::<Result<Vec<_>, CliError>>()?;
        if matched_methods.is_empty() {
            continue;
        }

        interface_matches.push((
            InterfaceSpec {
                interface_name: spec_string_text(&interface.name)?.to_owned(),
                interface_version: spec_string_text(&interface.vers)?.to_owned(),
            },
            interface.throws.iter().cloned().collect::<Vec<_>>(),
            matched_methods,
        ));
    }

    if interface_matches.is_empty() {
        return Ok(None);
    }
    interface_matches.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
    if interface_matches.len() > 1 {
        let descriptions = interface_matches
            .iter()
            .map(|(candidate, _, _)| {
                format!(
                    "{} v{}",
                    candidate.interface_name, candidate.interface_version
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::Other(format!(
            "method `{method}` matches multiple interfaces in the embedded contract spec: \
             {descriptions}"
        )));
    }

    let (matched, throws, mut methods) = interface_matches
        .into_iter()
        .next()
        .expect("bug: interface match count should be non-zero");
    if methods.len() > 1 {
        return Err(CliError::SpecFailed(format!(
            "embedded contract spec defines `{}` v{}::{method} multiple times",
            matched.interface_name, matched.interface_version
        )));
    }

    Ok(Some(SpecMethodMetadata {
        interface_name: matched.interface_name,
        version: matched.interface_version,
        method: methods.remove(0),
        type_defs: spec.type_defs.iter().cloned().collect(),
        throws,
    }))
}

fn resolve_local_contract_metadata(
    cwd: &Path,
    method: &str,
    artifacts: &ArtifactOptions,
) -> Result<Option<ContractMetadata>, CliError> {
    let Some(config_path) = find_project_config(cwd) else {
        return Ok(None);
    };
    let Some(project_root) = config_path.parent() else {
        return Ok(None);
    };
    let interface_specs = load_project_registered_interfaces(project_root, artifacts)?;
    resolve_local_contract_metadata_in_project(project_root, method, interface_specs.as_deref())
}

fn resolve_local_contract_metadata_in_project(
    project_root: &Path,
    method: &str,
    interface_specs: Option<&[InterfaceSpec]>,
) -> Result<Option<ContractMetadata>, CliError> {
    let package_version = load_package_version(project_root)?;
    let src_root = project_root.join("src");
    if !src_root.exists() {
        return Ok(None);
    }

    let mut candidates = Vec::new();
    collect_contract_metadata(&src_root, method, &package_version, &mut candidates)?;
    candidates = filter_candidates_by_interfaces(candidates, interface_specs);

    if candidates.is_empty() {
        return Ok(None);
    }

    candidates.sort_by(|a, b| {
        (&a.interface_name, &a.version, &a.source).cmp(&(&b.interface_name, &b.version, &b.source))
    });
    candidates.dedup();

    if candidates.len() == 1 {
        return Ok(candidates.into_iter().next());
    }

    let descriptions = candidates
        .iter()
        .map(|candidate| {
            format!(
                "{} v{} ({})",
                candidate.interface_name,
                candidate.version,
                candidate.source.display()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    Err(CliError::Other(format!(
        "method `{method}` matches multiple local contracts: {descriptions}"
    )))
}

fn load_project_registered_interfaces(
    project_root: &Path,
    artifacts: &ArtifactOptions,
) -> Result<Option<Vec<crate::contract_spec::InterfaceSpec>>, CliError> {
    let metadata = cargo_metadata_with_artifacts(artifacts)?;
    let pkg_name = resolve_package(&metadata, None, project_root)?;
    let wasm = crate::artifacts::groundhog_wasm_output_path(&metadata.target_directory, pkg_name);
    load_registered_interfaces(&wasm)
}

fn filter_candidates_by_interfaces(
    mut candidates: Vec<ContractMetadata>,
    interface_specs: Option<&[crate::contract_spec::InterfaceSpec]>,
) -> Vec<ContractMetadata> {
    let Some(interface_specs) = interface_specs else {
        return candidates;
    };
    if interface_specs.is_empty() {
        return candidates;
    }

    candidates.retain(|candidate| {
        interface_specs.iter().any(|interface| {
            interface.interface_name == candidate.interface_name
                && interface.interface_version == candidate.version
        })
    });
    candidates
}

fn load_package_version(project_root: &Path) -> Result<String, CliError> {
    let cargo_toml = project_root.join("Cargo.toml");
    let contents = std::fs::read_to_string(&cargo_toml)?;
    let value: toml::Value = toml::from_str(&contents)
        .map_err(|e| CliError::Other(format!("invalid Cargo.toml: {e}")))?;
    value
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CliError::Other("Cargo.toml is missing package.version".into()))
}

fn collect_contract_metadata(
    dir: &Path,
    method: &str,
    package_version: &str,
    out: &mut Vec<ContractMetadata>,
) -> Result<(), CliError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_contract_metadata(&path, method, package_version, out)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let contents = std::fs::read_to_string(&path)?;
        out.extend(contract_metadata_from_source(
            &contents,
            &path,
            method,
            package_version,
        ));
    }
    Ok(())
}

fn contract_metadata_from_source(
    source: &str,
    path: &Path,
    method: &str,
    package_version: &str,
) -> Vec<ContractMetadata> {
    let mut out = Vec::new();
    let mut offset = 0;

    while let Some(attr_start_rel) = find_next_contract_attr(source, offset) {
        let attr_start = attr_start_rel;
        let Some(attr_end_rel) = source[attr_start..].find(']') else {
            break;
        };
        let attr_end = attr_start + attr_end_rel;
        let attr = &source[attr_start..=attr_end];
        let item_start = attr_end + 1;
        let Some((header_start, brace_idx)) = find_item_header(source, item_start) else {
            break;
        };
        let Some(block_end) = find_matching_brace(source, brace_idx) else {
            break;
        };

        let header = &source[header_start..brace_idx];
        let body = &source[brace_idx + 1..block_end];
        if !body.contains(&format!("fn {method}")) {
            offset = block_end + 1;
            continue;
        }

        let version = parse_contract_attr_value(attr, "version")
            .unwrap_or_else(|| package_version.to_string());
        if let Some(interface_name) =
            parse_contract_attr_value(attr, "interface").or_else(|| infer_interface_name(header))
            && let Some(method_metadata) = parse_method_metadata(body, method)
        {
            out.push(ContractMetadata {
                interface_name,
                version,
                source: path.to_path_buf(),
                method: method_metadata,
            });
        }

        offset = block_end + 1;
    }

    out
}

fn find_next_contract_attr(source: &str, start: usize) -> Option<usize> {
    let mut search = start;
    while let Some(rel) = source[search..].find("#[contract") {
        let idx = search + rel;
        match source.as_bytes().get(idx + "#[contract".len()) {
            Some(b'(' | b']') => return Some(idx),
            _ => search = idx + "#[contract".len(),
        }
    }
    None
}

fn find_item_header(source: &str, start: usize) -> Option<(usize, usize)> {
    let remainder = &source[start..];
    let impl_idx = remainder.find("impl ");
    let trait_idx = remainder.find("trait ");
    let header_rel = match (impl_idx, trait_idx) {
        (Some(i), Some(t)) => i.min(t),
        (Some(i), None) => i,
        (None, Some(t)) => t,
        (None, None) => return None,
    };
    let header_start = start + header_rel;
    let brace_rel = source[header_start..].find('{')?;
    Some((header_start, header_start + brace_rel))
}

fn find_matching_brace(source: &str, open_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, ch) in source[open_idx..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open_idx + idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_contract_attr_value(attr: &str, key: &str) -> Option<String> {
    let needle = format!("{key} = \"");
    let start = attr.find(&needle)? + needle.len();
    let rest = &attr[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_method_metadata(body: &str, method: &str) -> Option<MethodMetadata> {
    let target = format!("fn {method}");
    let mut search_offset = 0;
    let fn_start = loop {
        let rel = body[search_offset..].find(&target)?;
        let abs = search_offset + rel;
        let after = abs + target.len();
        match body.as_bytes().get(after) {
            Some(b) if b.is_ascii_alphanumeric() || *b == b'_' => {
                search_offset = after;
            }
            _ => break abs,
        }
    };
    let signature = &body[fn_start..];
    let open_paren = signature.find('(')?;
    let close_paren = find_matching_delimiter(signature, open_paren, '(', ')')?;
    let args_src = &signature[open_paren + 1..close_paren];

    let after_args = &signature[close_paren + 1..];
    let signature_end = after_args.find('{').or_else(|| after_args.find(';'))?;
    let header_tail = after_args[..signature_end].trim();
    let return_type = header_tail
        .strip_prefix("->")
        .map(str::trim)
        .filter(|ty| !ty.is_empty())
        .map(ToOwned::to_owned);

    let mut arg_types = Vec::new();
    for arg in split_top_level(args_src, ',') {
        let arg = arg.trim();
        if arg.is_empty()
            || arg == "&self"
            || arg == "&mut self"
            || arg == "self"
            || arg == "mut self"
        {
            continue;
        }
        let (_, ty) = arg.rsplit_once(':')?;
        arg_types.push(ty.trim().to_string());
    }

    Some(MethodMetadata {
        arg_types,
        return_type,
    })
}

fn find_matching_delimiter(
    source: &str,
    open_idx: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, ch) in source[open_idx..].char_indices() {
        match ch {
            c if c == open => depth += 1,
            c if c == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open_idx + idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(source: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut angle = 0usize;

    for (idx, ch) in source.char_indices() {
        match ch {
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            _ if ch == delimiter && paren == 0 && bracket == 0 && angle == 0 => {
                parts.push(source[start..idx].trim());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(source[start..].trim());
    parts
}

fn infer_interface_name(header: &str) -> Option<String> {
    let header = header.trim();
    if let Some(rest) = header.strip_prefix("trait ") {
        return parse_rust_ident(rest);
    }
    if let Some(rest) = header.strip_prefix("pub trait ") {
        return parse_rust_ident(rest);
    }
    if let Some(rest) = header.strip_prefix("impl ") {
        if let Some((lhs, _rhs)) = rest.split_once(" for ") {
            return last_ident(lhs);
        }
        return parse_rust_ident(rest);
    }
    None
}

fn parse_rust_ident(input: &str) -> Option<String> {
    let ident: String = input
        .chars()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    (!ident.is_empty()).then_some(ident)
}

fn last_ident(input: &str) -> Option<String> {
    input
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .rfind(|part| !part.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ResolvedCall, contract_metadata_from_source, explicit_entrypoint_id,
        filter_candidates_by_interfaces, format_entrypoint_id, load_project_registered_interfaces,
        method_entrypoint_id, parse_contract_attr_value, resolve_spec_method, selector_json,
        selector_summary,
    };
    use crate::CliError;
    use crate::artifacts::ArtifactOptions;
    use crate::codec::{
        CallMetadata, ContractMetadata, MethodMetadata, SpecMethodMetadata, build_type_def_index,
        decode_return as decode_return_value, decode_spec_value, decode_spec_value_from_reader,
        encode_args as encode_calldata, normalize_type_name,
    };
    use crate::contract_spec::InterfaceSpec;
    use crate::tx_utils::summarize_exec_status;
    use groundhog_contract_spec::{
        ContractArrayType, ContractEnumDef, ContractEnumVariant, ContractField, ContractSpec,
        ContractSpecInterface, ContractSpecMethod, ContractSpecString, ContractStructDef,
        ContractType, ContractTypeDef, ContractUnionCase, ContractUnionCaseType, ContractUnionDef,
        ContractVectorType,
    };
    use groundhog_json_rpc_types::receipt::ExecStatus;
    use serde_json::json;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn parses_contract_attr_values() {
        let attr = r#"#[contract(interface = "Interface1", version = "1.2.3")]"#;
        assert_eq!(
            parse_contract_attr_value(attr, "interface").as_deref(),
            Some("Interface1")
        );
        assert_eq!(
            parse_contract_attr_value(attr, "version").as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn extracts_metadata_from_bare_impl() {
        let source = r"
            #[contract(throws = Error, client = CounterClient)]
            impl Counter {
                pub fn get(&self) -> u64 { 0 }
            }
        ";
        let metadata = contract_metadata_from_source(
            source,
            Path::new("/tmp/implementation.rs"),
            "get",
            "0.1.0",
        );
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].interface_name, "Counter");
        assert_eq!(metadata[0].version, "0.1.0");
        assert_eq!(metadata[0].method.arg_types, Vec::<String>::new());
        assert_eq!(metadata[0].method.return_type.as_deref(), Some("u64"));
    }

    #[test]
    fn method_metadata_does_not_match_prefix_methods() {
        let source = r"
            #[contract(throws = Error, client = CounterClient)]
            impl Counter {
                pub fn get_all(&self, count: u64) -> Vec<u64> { vec![] }
                pub fn get(&self) -> u64 { 0 }
            }
        ";
        let metadata = contract_metadata_from_source(
            source,
            Path::new("/tmp/implementation.rs"),
            "get",
            "0.1.0",
        );
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].method.arg_types, Vec::<String>::new());
        assert_eq!(metadata[0].method.return_type.as_deref(), Some("u64"));
    }

    #[test]
    fn extracts_metadata_from_trait_impl_with_explicit_interface_and_version() {
        let source = r#"
            #[contract(interface = "Interface1", version = "1.2.3")]
            impl Interface for Stateless {
                fn perform_task() -> u32 { 1 }
            }
        "#;
        let metadata = contract_metadata_from_source(
            source,
            Path::new("/tmp/interface.rs"),
            "perform_task",
            "0.1.0",
        );
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].interface_name, "Interface1");
        assert_eq!(metadata[0].version, "1.2.3");
        assert_eq!(metadata[0].method.arg_types, Vec::<String>::new());
        assert_eq!(metadata[0].method.return_type.as_deref(), Some("u32"));
    }

    #[test]
    fn formats_entrypoint_ids_as_prefixed_hex() {
        let entrypoint = explicit_entrypoint_id("0x0123456789abcdef")
            .expect("bug: valid entrypoint hex should parse")
            .expect("bug: explicit entrypoint should be returned");
        assert_eq!(format_entrypoint_id(entrypoint), "0x0123456789abcdef");
    }

    #[test]
    fn method_entrypoint_id_produces_known_hash() {
        let id = method_entrypoint_id("ERC20", "0.1.0", "transfer");
        assert_eq!(format_entrypoint_id(id), "0x63285b3523f9dc3f");
    }

    #[test]
    fn selector_helpers_report_explicit_entrypoint_resolution() {
        let resolved = ResolvedCall {
            entrypoint: method_entrypoint_id("Counter", "0.1.0", "get"),
            metadata: None,
        };

        assert_eq!(selector_summary(&resolved), "explicit entrypoint");
        assert_eq!(
            selector_json(&resolved),
            json!({
                "source": "explicit_entrypoint",
                "interface": null,
            })
        );
    }

    #[test]
    fn selector_helpers_report_embedded_spec_resolution() {
        let metadata = sample_single_arg_spec_metadata(ContractType::U32);
        let resolved = ResolvedCall {
            entrypoint: method_entrypoint_id("Counter", "0.1.0", "call"),
            metadata: Some(CallMetadata::Spec(metadata)),
        };

        assert_eq!(selector_summary(&resolved), "embedded spec Counter v0.1.0");
        assert_eq!(
            selector_json(&resolved),
            json!({
                "source": "embedded_spec",
                "interface": {
                    "name": "Counter",
                    "version": "0.1.0",
                },
            })
        );
    }

    #[test]
    fn selector_helpers_report_local_metadata_resolution() {
        let resolved = ResolvedCall {
            entrypoint: method_entrypoint_id("Counter", "0.1.0", "get"),
            metadata: Some(CallMetadata::Local(ContractMetadata {
                interface_name: "Counter".into(),
                version: "0.1.0".into(),
                source: Path::new("/tmp/local.rs").to_path_buf(),
                method: MethodMetadata {
                    arg_types: vec!["u64".into()],
                    return_type: Some("u64".into()),
                },
            })),
        };

        assert_eq!(selector_summary(&resolved), "local metadata Counter v0.1.0");
        assert_eq!(
            selector_json(&resolved),
            json!({
                "source": "local_metadata",
                "interface": {
                    "name": "Counter",
                    "version": "0.1.0",
                },
                "source_path": "/tmp/local.rs",
            })
        );
    }

    #[test]
    fn parses_explicit_entrypoint_id() {
        let entrypoint = explicit_entrypoint_id("0x0123456789abcdef")
            .expect("bug: valid entrypoint hex should parse")
            .expect("bug: explicit entrypoint should be returned");
        assert_eq!(
            entrypoint.into_inner(),
            0x0123_4567_89AB_CDEF_u64.to_be_bytes()
        );
    }

    #[test]
    fn encodes_single_typed_argument_from_local_metadata() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec!["u64".into()],
                return_type: None,
            },
        };
        let encoded = encode_calldata(&["5".into()], Some(&CallMetadata::Local(metadata)))
            .expect("bug: typed calldata should encode");
        let decoded: u64 =
            xdrs::cxs::decode(&encoded).expect("bug: encoded calldata should decode as u64");
        assert_eq!(decoded, 5);
    }

    #[test]
    fn rejects_multiple_raw_arguments_without_local_metadata() {
        let err = encode_calldata(&["0x01".into(), "0x02".into()], None)
            .expect_err("bug: multiple raw arguments should fail without metadata");
        assert_call_encode_error(
            err,
            "arguments",
            "multiple call arguments require local contract metadata — run `groundhog call` from \
             inside the project that defines the contract, or pass a single hex-encoded calldata \
             argument",
        );
    }

    #[test]
    fn decodes_simple_return_value() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec![],
                return_type: Some("u64".into()),
            },
        };
        let bytes = xdrs::cxs::encode(&7_u64).expect("bug: u64 should encode");
        let decoded = decode_return_value(&bytes, Some(&CallMetadata::Local(metadata)))
            .expect("bug: simple return value should decode");
        assert_eq!(decoded, serde_json::json!(7));
    }

    #[test]
    fn encodes_and_decodes_local_str_values() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec!["&str".into()],
                return_type: Some("&str".into()),
            },
        };
        let encoded = encode_calldata(
            &["hello".into()],
            Some(&CallMetadata::Local(metadata.clone())),
        )
        .expect("bug: local string calldata should encode");
        let decoded = decode_return_value(&encoded, Some(&CallMetadata::Local(metadata)))
            .expect("bug: local string return should decode");
        assert_eq!(decoded, serde_json::json!("hello"));
    }

    #[test]
    fn reports_unsupported_local_type_name_in_encode_error() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec!["Vec<String>".into()],
                return_type: None,
            },
        };
        let err = encode_calldata(&["hello".into()], Some(&CallMetadata::Local(metadata)))
            .expect_err("bug: unsupported local type should fail");
        assert_call_encode_error(
            err,
            "argument 1",
            "typed call encoding for `Vec<String>` is not implemented yet; pass an explicit \
             0x-prefixed entrypoint ID and raw calldata instead",
        );
    }

    #[test]
    fn encodes_and_decodes_embedded_spec_structured_arguments() {
        let metadata = sample_structured_spec_metadata(
            vec![
                contract_field("payload", named_type("Payload")),
                contract_field("mode", named_type("Mode")),
                contract_field(
                    "maybe_count",
                    ContractType::Option {
                        option_inner: Box::new(ContractType::U32),
                    },
                ),
                contract_field(
                    "digest",
                    ContractType::Array {
                        array: ContractArrayType {
                            element: Box::new(ContractType::U8),
                            len: 4,
                        },
                    },
                ),
                contract_field(
                    "flags",
                    ContractType::Vector {
                        vector: ContractVectorType {
                            element: Box::new(ContractType::Bool),
                            max_len: 8,
                        },
                    },
                ),
                contract_field(
                    "pair",
                    ContractType::Tuple {
                        tuple: vec![ContractType::U32, ContractType::String]
                            .try_into()
                            .unwrap(),
                    },
                ),
            ],
            ContractType::Void,
        );

        let encoded = encode_calldata(
            &[
                r#"{"count":7,"bytes":"hi"}"#.into(),
                "Ready".into(),
                "5".into(),
                "0x01020304".into(),
                "[true,false,true]".into(),
                r#"[7,"ok"]"#.into(),
            ],
            Some(&CallMetadata::Spec(metadata.clone())),
        )
        .expect("bug: embedded spec calldata should encode");

        assert_eq!(
            decode_spec_arguments(&metadata, &encoded),
            vec![
                json!({"count": 7, "bytes": "0x6869"}),
                json!("Ready"),
                json!(5),
                json!("0x01020304"),
                json!([true, false, true]),
                json!([7, "ok"]),
            ]
        );
    }

    #[test]
    fn encodes_and_decodes_embedded_spec_unions() {
        let metadata = sample_structured_spec_metadata(
            vec![contract_field("envelope", named_type("Envelope"))],
            named_type("Envelope"),
        );

        let full_case = json!({
            "case": "Full",
            "value": {
                "payload": {
                    "count": 9,
                    "bytes": "0xaabb"
                },
                "enabled": true
            }
        });
        let encoded = encode_calldata(
            &[full_case.to_string()],
            Some(&CallMetadata::Spec(metadata.clone())),
        )
        .expect("bug: union calldata should encode");

        let type_defs =
            build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
        let mut reader = xdrs::cxs::buf_reader::BufReader::new(&encoded);
        let decoded = decode_spec_value_from_reader(
            &metadata.method.args[0].ty,
            &mut reader,
            &type_defs,
            "arg0",
        )
        .expect("bug: union arg should decode");
        assert_eq!(decoded, full_case);
        assert_eq!(reader.remaining(), 0);

        let pair_case = json!({
            "case": "Pair",
            "value": ["Busy", 9]
        });
        let pair_bytes = encode_calldata(
            &[pair_case.to_string()],
            Some(&CallMetadata::Spec(metadata.clone())),
        )
        .expect("bug: tuple union case should encode");
        let decoded_pair =
            decode_return_value(&pair_bytes, Some(&CallMetadata::Spec(metadata.clone())))
                .expect("bug: union return should decode");
        assert_eq!(decoded_pair, pair_case);

        let empty_bytes = encode_calldata(
            &["Empty".into()],
            Some(&CallMetadata::Spec(metadata.clone())),
        )
        .expect("bug: void union shorthand should encode");
        let decoded_empty = decode_return_value(&empty_bytes, Some(&CallMetadata::Spec(metadata)))
            .expect("bug: void union return should decode");
        assert_eq!(decoded_empty, json!({"case": "Empty"}));
    }

    #[test]
    fn encodes_and_decodes_embedded_spec_large_integers() {
        let u128_value = u128::MAX.to_string();
        let u128_metadata = sample_single_arg_spec_metadata(ContractType::U128);
        let u128_bytes = encode_calldata(
            std::slice::from_ref(&u128_value),
            Some(&CallMetadata::Spec(u128_metadata.clone())),
        )
        .expect("bug: embedded spec u128 should encode");
        assert_eq!(
            decode_spec_arguments(&u128_metadata, &u128_bytes),
            vec![json!(u128_value.clone())]
        );
        assert_eq!(
            decode_return_value(&u128_bytes, Some(&CallMetadata::Spec(u128_metadata)))
                .expect("bug: embedded spec u128 return should decode"),
            json!(u128_value)
        );

        let i128_value = i128::MIN.to_string();
        let i128_metadata = sample_single_arg_spec_metadata(ContractType::I128);
        let i128_bytes = encode_calldata(
            std::slice::from_ref(&i128_value),
            Some(&CallMetadata::Spec(i128_metadata.clone())),
        )
        .expect("bug: embedded spec i128 should encode");
        assert_eq!(
            decode_spec_arguments(&i128_metadata, &i128_bytes),
            vec![json!(i128_value.clone())]
        );
        assert_eq!(
            decode_return_value(&i128_bytes, Some(&CallMetadata::Spec(i128_metadata)))
                .expect("bug: embedded spec i128 return should decode"),
            json!(i128_value)
        );
    }

    #[test]
    fn encodes_and_decodes_embedded_spec_addresses() {
        let address = format!("0x{}", "11".repeat(32));
        let metadata = sample_single_arg_spec_metadata(ContractType::Address);
        let encoded = encode_calldata(
            std::slice::from_ref(&address),
            Some(&CallMetadata::Spec(metadata.clone())),
        )
        .expect("bug: embedded spec address should encode");

        assert_eq!(
            decode_spec_arguments(&metadata, &encoded),
            vec![json!(address.clone())]
        );
        assert_eq!(
            decode_return_value(&encoded, Some(&CallMetadata::Spec(metadata)))
                .expect("bug: embedded spec address return should decode"),
            json!(address)
        );
    }

    #[test]
    fn rejects_byte_vectors_exceeding_max_length() {
        let metadata = sample_single_arg_spec_metadata(ContractType::Vector {
            vector: ContractVectorType {
                element: Box::new(ContractType::U8),
                max_len: 2,
            },
        });
        let err = encode_calldata(&["0x010203".into()], Some(&CallMetadata::Spec(metadata)))
            .expect_err("bug: oversized byte vectors should fail");
        assert_call_encode_error(
            err,
            "argument 1 (`value`)",
            "byte vector exceeds max length 2 (got 3)",
        );
    }

    #[test]
    fn rejects_array_length_mismatches() {
        let metadata = sample_single_arg_spec_metadata(ContractType::Array {
            array: ContractArrayType {
                element: Box::new(ContractType::Bool),
                len: 2,
            },
        });
        let err = encode_calldata(&["[true]".into()], Some(&CallMetadata::Spec(metadata)))
            .expect_err("bug: array length mismatches should fail");
        assert_call_encode_error(
            err,
            "argument 1 (`value`)",
            "array expects length 2 but got 1",
        );
    }

    #[test]
    fn rejects_struct_array_field_count_mismatches() {
        let metadata = sample_structured_spec_metadata(
            vec![contract_field("payload", named_type("Payload"))],
            ContractType::Void,
        );
        let err = encode_calldata(&["[7]".into()], Some(&CallMetadata::Spec(metadata)))
            .expect_err("bug: struct tuple field count mismatches should fail");
        assert_call_encode_error(
            err,
            "argument 1 (`payload`)",
            "expected 2 field value(s) but got 1",
        );
    }

    #[test]
    fn rejects_unexpected_struct_fields() {
        let metadata = sample_structured_spec_metadata(
            vec![contract_field("payload", named_type("Payload"))],
            ContractType::Void,
        );
        let err = encode_calldata(
            &[r#"{"count":7,"bytes":"hi","extra":1}"#.into()],
            Some(&CallMetadata::Spec(metadata)),
        )
        .expect_err("bug: unexpected struct fields should fail");
        assert_call_encode_error(err, "argument 1 (`payload`)", "unexpected field `extra`");
    }

    #[test]
    fn rejects_missing_struct_fields() {
        let metadata = sample_structured_spec_metadata(
            vec![contract_field("payload", named_type("Payload"))],
            ContractType::Void,
        );
        let err = encode_calldata(
            &[r#"{"count":7}"#.into()],
            Some(&CallMetadata::Spec(metadata)),
        )
        .expect_err("bug: missing struct fields should fail");
        assert_call_encode_error(err, "argument 1 (`payload`)", "missing field `bytes`");
    }

    #[test]
    fn rejects_payloads_for_void_union_cases() {
        let metadata = sample_structured_spec_metadata(
            vec![contract_field("envelope", named_type("Envelope"))],
            ContractType::Void,
        );
        let err = encode_calldata(
            &[r#"{"case":"Empty","value":true}"#.into()],
            Some(&CallMetadata::Spec(metadata)),
        )
        .expect_err("bug: void union cases should reject payloads");
        assert_call_encode_error(
            err,
            "argument 1 (`envelope`)",
            "union case `Empty` does not take a payload",
        );
    }

    #[test]
    fn rejects_trailing_bytes_in_decoded_spec_returns() {
        let metadata = sample_single_arg_spec_metadata(ContractType::U32);
        let type_defs =
            build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
        let mut bytes = xdrs::cxs::encode(&7_u32).expect("bug: u32 should encode");
        bytes.push(0);

        let err = decode_spec_value(&metadata.method.return_type, &bytes, &type_defs)
            .expect_err("bug: trailing return bytes should fail");
        assert_call_decode_error(
            err,
            "return value",
            "1 unread byte(s) remain after decoding",
        );
    }

    #[test]
    fn rejects_decoded_vectors_exceeding_max_length() {
        let metadata = sample_single_arg_spec_metadata(ContractType::Vector {
            vector: ContractVectorType {
                element: Box::new(ContractType::Bool),
                max_len: 1,
            },
        });
        let type_defs =
            build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
        let bytes = xdrs::cxs::encode(&2_u32).expect("bug: vector length should encode");

        let err = decode_spec_value(&metadata.method.return_type, &bytes, &type_defs)
            .expect_err("bug: oversized decoded vectors should fail");
        assert_call_decode_error(err, "return value", "decoded vector length 2 exceeds max 1");
    }

    #[test]
    fn prefers_named_enum_variants_over_numeric_string_discriminants() {
        let metadata = sample_numeric_name_spec_metadata(named_type("NumericEnum"));
        let encoded = encode_calldata(
            &[r#""1""#.into()],
            Some(&CallMetadata::Spec(metadata.clone())),
        )
        .expect("bug: numeric-string enum variant should encode by name");

        let decoded = decode_return_value(&encoded, Some(&CallMetadata::Spec(metadata)))
            .expect("bug: numeric-string enum variant should decode");
        assert_eq!(decoded, json!("1"));
    }

    #[test]
    fn prefers_named_union_cases_over_numeric_string_discriminants() {
        let metadata = sample_numeric_name_spec_metadata(named_type("NumericUnion"));
        let encoded = encode_calldata(
            &[r#""1""#.into()],
            Some(&CallMetadata::Spec(metadata.clone())),
        )
        .expect("bug: numeric-string union case should encode by name");

        let decoded = decode_return_value(&encoded, Some(&CallMetadata::Spec(metadata)))
            .expect("bug: numeric-string union case should decode");
        assert_eq!(decoded, json!({"case": "1"}));
    }

    #[test]
    fn filters_candidates_by_interfaces() {
        let candidates = vec![
            ContractMetadata {
                interface_name: "Counter".into(),
                version: "0.1.0".into(),
                source: Path::new("/tmp/a.rs").to_path_buf(),
                method: MethodMetadata {
                    arg_types: vec![],
                    return_type: Some("u64".into()),
                },
            },
            ContractMetadata {
                interface_name: "Other".into(),
                version: "0.1.0".into(),
                source: Path::new("/tmp/b.rs").to_path_buf(),
                method: MethodMetadata {
                    arg_types: vec![],
                    return_type: Some("u64".into()),
                },
            },
        ];
        let interfaces = vec![InterfaceSpec {
            interface_name: "Counter".into(),
            interface_version: "0.1.0".into(),
        }];

        let filtered = filter_candidates_by_interfaces(candidates, Some(&interfaces));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].interface_name, "Counter");
    }

    #[test]
    #[serial]
    fn load_project_registered_interfaces_uses_explicit_target_dir() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let wasm = custom_target.join("groundhog").join("demo_contract.wasm");
        std::fs::create_dir_all(wasm.parent().expect("bug: wasm parent should exist"))
            .expect("bug: custom target dir should be created");
        std::fs::write(&wasm, wasm_with_embedded_spec(&sample_contract_spec()))
            .expect("bug: wasm fixture should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let interfaces = load_project_registered_interfaces(&project_root, &artifacts)
            .expect("bug: registered interface lookup should succeed")
            .expect("bug: registered interfaces should resolve from explicit target dir");

        assert_eq!(
            interfaces,
            vec![InterfaceSpec {
                interface_name: "Counter".into(),
                interface_version: "0.1.0".into(),
            }]
        );
    }

    #[test]
    #[serial]
    fn resolve_spec_method_reports_multiple_interface_matches() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let wasm = custom_target.join("groundhog").join("demo_contract.wasm");
        std::fs::create_dir_all(wasm.parent().expect("bug: wasm parent should exist"))
            .expect("bug: custom target dir should be created");
        std::fs::write(
            &wasm,
            wasm_with_embedded_spec(&sample_multi_interface_contract_spec()),
        )
        .expect("bug: wasm fixture should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let err = resolve_spec_method(&project_root, "get", &artifacts)
            .expect_err("bug: ambiguous embedded interface matches should fail");
        assert_eq!(
            err.to_string(),
            "method `get` matches multiple interfaces in the embedded contract spec: Counter \
             v0.1.0, Other v0.1.0"
        );
    }

    #[test]
    fn summarizes_revert_without_message() {
        assert_eq!(
            summarize_exec_status(&ExecStatus::Reverted { message: None }),
            "reverted"
        );
    }

    #[test]
    fn normalize_type_name_strips_references_without_clobbering_prefixes() {
        assert_eq!(normalize_type_name("&str"), "str");
        assert_eq!(normalize_type_name("&mut str"), "str");
        assert_eq!(normalize_type_name("&'a str"), "str");
        assert_eq!(normalize_type_name("&String"), "String");
        assert_eq!(normalize_type_name("&strange"), "strange");
    }

    fn create_temp_project(package_name: &str) -> TempDir {
        let project = tempfile::tempdir().expect("bug: temp dir should be created");
        write_project_manifest(project.path(), package_name);
        std::fs::create_dir_all(project.path().join("src"))
            .expect("bug: src dir should be created");
        std::fs::write(project.path().join("src/lib.rs"), "pub fn demo() {}\n")
            .expect("bug: lib.rs should be written");
        project
    }

    fn write_project_manifest(project_root: &Path, package_name: &str) {
        std::fs::write(
            project_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
            ),
        )
        .expect("bug: Cargo.toml should be written");
    }

    fn sample_contract_spec() -> ContractSpec {
        ContractSpec {
            interfaces: vec![ContractSpecInterface {
                name: spec_string_bytes("Counter"),
                vers: spec_string_bytes("0.1.0"),
                throws: vec![].try_into().unwrap(),
                methods: vec![ContractSpecMethod {
                    name: spec_string_bytes("get"),
                    args: vec![].try_into().unwrap(),
                    return_type: ContractType::U64,
                }]
                .try_into()
                .unwrap(),
            }]
            .try_into()
            .unwrap(),
            type_defs: vec![].try_into().unwrap(),
            events: vec![].try_into().unwrap(),
        }
    }

    fn sample_multi_interface_contract_spec() -> ContractSpec {
        ContractSpec {
            interfaces: vec![
                ContractSpecInterface {
                    name: spec_string_bytes("Other"),
                    vers: spec_string_bytes("0.1.0"),
                    throws: vec![].try_into().unwrap(),
                    methods: vec![ContractSpecMethod {
                        name: spec_string_bytes("get"),
                        args: vec![].try_into().unwrap(),
                        return_type: ContractType::U64,
                    }]
                    .try_into()
                    .unwrap(),
                },
                ContractSpecInterface {
                    name: spec_string_bytes("Counter"),
                    vers: spec_string_bytes("0.1.0"),
                    throws: vec![].try_into().unwrap(),
                    methods: vec![ContractSpecMethod {
                        name: spec_string_bytes("get"),
                        args: vec![].try_into().unwrap(),
                        return_type: ContractType::U64,
                    }]
                    .try_into()
                    .unwrap(),
                },
            ]
            .try_into()
            .unwrap(),
            type_defs: vec![].try_into().unwrap(),
            events: vec![].try_into().unwrap(),
        }
    }

    fn sample_structured_spec_metadata(
        args: Vec<ContractField>,
        return_type: ContractType,
    ) -> SpecMethodMetadata {
        SpecMethodMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            method: ContractSpecMethod {
                name: spec_string_bytes("call"),
                args: args.try_into().unwrap(),
                return_type,
            },
            type_defs: vec![
                ContractTypeDef::TypeDefStruct {
                    struct_def: ContractStructDef {
                        name: spec_string_bytes("Payload"),
                        fields: vec![
                            contract_field("count", ContractType::U32),
                            contract_field(
                                "bytes",
                                ContractType::Vector {
                                    vector: ContractVectorType {
                                        element: Box::new(ContractType::U8),
                                        max_len: u32::MAX,
                                    },
                                },
                            ),
                        ]
                        .try_into()
                        .unwrap(),
                    },
                },
                ContractTypeDef::TypeDefEnum {
                    enum_def: ContractEnumDef {
                        name: spec_string_bytes("Mode"),
                        variants: vec![
                            ContractEnumVariant {
                                name: spec_string_bytes("Ready"),
                                value: 1,
                            },
                            ContractEnumVariant {
                                name: spec_string_bytes("Busy"),
                                value: 2,
                            },
                        ]
                        .try_into()
                        .unwrap(),
                    },
                },
                ContractTypeDef::TypeDefUnion {
                    union_def: ContractUnionDef {
                        name: spec_string_bytes("Envelope"),
                        cases: vec![
                            ContractUnionCase {
                                name: spec_string_bytes("Empty"),
                                discriminant: 0,
                                case_type: ContractUnionCaseType::UnionCaseVoid,
                            },
                            ContractUnionCase {
                                name: spec_string_bytes("Full"),
                                discriminant: 1,
                                case_type: ContractUnionCaseType::UnionCaseStruct {
                                    struct_fields: vec![
                                        contract_field("payload", named_type("Payload")),
                                        contract_field("enabled", ContractType::Bool),
                                    ]
                                    .try_into()
                                    .unwrap(),
                                },
                            },
                            ContractUnionCase {
                                name: spec_string_bytes("Pair"),
                                discriminant: 2,
                                case_type: ContractUnionCaseType::UnionCaseTuple {
                                    tuple: vec![named_type("Mode"), ContractType::U32]
                                        .try_into()
                                        .unwrap(),
                                },
                            },
                        ]
                        .try_into()
                        .unwrap(),
                    },
                },
            ],
            throws: vec![],
        }
    }

    fn sample_numeric_name_spec_metadata(return_type: ContractType) -> SpecMethodMetadata {
        SpecMethodMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            method: ContractSpecMethod {
                name: spec_string_bytes("call"),
                args: vec![contract_field("value", return_type.clone())]
                    .try_into()
                    .unwrap(),
                return_type,
            },
            type_defs: vec![
                ContractTypeDef::TypeDefEnum {
                    enum_def: ContractEnumDef {
                        name: spec_string_bytes("NumericEnum"),
                        variants: vec![
                            ContractEnumVariant {
                                name: spec_string_bytes("1"),
                                value: 7,
                            },
                            ContractEnumVariant {
                                name: spec_string_bytes("2"),
                                value: 8,
                            },
                        ]
                        .try_into()
                        .unwrap(),
                    },
                },
                ContractTypeDef::TypeDefUnion {
                    union_def: ContractUnionDef {
                        name: spec_string_bytes("NumericUnion"),
                        cases: vec![
                            ContractUnionCase {
                                name: spec_string_bytes("1"),
                                discriminant: 7,
                                case_type: ContractUnionCaseType::UnionCaseVoid,
                            },
                            ContractUnionCase {
                                name: spec_string_bytes("2"),
                                discriminant: 8,
                                case_type: ContractUnionCaseType::UnionCaseVoid,
                            },
                        ]
                        .try_into()
                        .unwrap(),
                    },
                },
            ],
            throws: vec![],
        }
    }

    fn sample_single_arg_spec_metadata(arg_type: ContractType) -> SpecMethodMetadata {
        SpecMethodMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            method: ContractSpecMethod {
                name: spec_string_bytes("call"),
                args: vec![contract_field("value", arg_type.clone())]
                    .try_into()
                    .unwrap(),
                return_type: arg_type,
            },
            type_defs: vec![],
            throws: vec![],
        }
    }

    fn decode_spec_arguments(
        metadata: &SpecMethodMetadata,
        encoded: &[u8],
    ) -> Vec<serde_json::Value> {
        let type_defs =
            build_type_def_index(&metadata.type_defs).expect("bug: typedef index should build");
        let mut reader = xdrs::cxs::buf_reader::BufReader::new(encoded);
        let values = metadata
            .method
            .args
            .iter()
            .enumerate()
            .map(|(index, field)| {
                decode_spec_value_from_reader(
                    &field.ty,
                    &mut reader,
                    &type_defs,
                    &format!("arg{index}"),
                )
                .expect("bug: spec argument should decode")
            })
            .collect();
        assert_eq!(reader.remaining(), 0);
        values
    }

    fn assert_call_encode_error(err: CliError, path: &str, expected_message: &str) {
        match err {
            CliError::CallEncode {
                path: actual_path,
                message,
            } => {
                assert_eq!(actual_path, path);
                assert_eq!(message, expected_message);
            }
            other => panic!("bug: expected CliError::CallEncode, got {other:?}"),
        }
    }

    fn assert_call_decode_error(err: CliError, path: &str, expected_message: &str) {
        match err {
            CliError::CallDecode {
                path: actual_path,
                message,
            } => {
                assert_eq!(actual_path, path);
                assert_eq!(message, expected_message);
            }
            other => panic!("bug: expected CliError::CallDecode, got {other:?}"),
        }
    }

    fn contract_field(name: &str, ty: ContractType) -> ContractField {
        ContractField {
            name: spec_string_bytes(name),
            ty,
        }
    }

    fn named_type(name: &str) -> ContractType {
        ContractType::Named {
            named: spec_string_bytes(name),
        }
    }

    fn spec_string_bytes(value: &str) -> ContractSpecString {
        value.try_into().expect("bug: test string should fit")
    }

    fn wasm_with_embedded_spec(spec: &ContractSpec) -> Vec<u8> {
        wasm_with_custom_section(
            groundhog_spec_tool::FINAL_SECTION_NAME,
            &xdrs::txs::encode(spec).expect("bug: contract spec fixture should encode"),
        )
    }

    fn wasm_with_custom_section(name: &str, data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        encode_uleb128(name.len(), &mut payload);
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(data);

        let mut wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x00];
        encode_uleb128(payload.len(), &mut wasm);
        wasm.extend_from_slice(&payload);
        wasm
    }

    fn encode_uleb128(mut value: usize, out: &mut Vec<u8>) {
        loop {
            let mut byte = u8::try_from(value & 0x7f).expect("bug: uleb128 byte should fit");
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
    }

    impl CurrentDirGuard {
        fn enter(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("bug: current dir should resolve");
            std::env::set_current_dir(path).expect("bug: current dir should change");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).expect("bug: current dir should restore");
        }
    }
}
