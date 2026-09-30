use groundhog_json_rpc_types::hex::prefixed_hex;
use std::path::{Path, PathBuf};

use groundhog_client::codec::method_entrypoint_id;
use groundhog_client::{CallRequest, PollOptions, SignedCall, UnsignedCall};
use groundhog_json_rpc_types::receipt::ExecStatus;
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::LocalSigner;
use groundhog_xdr::contract::ContractEntrypointID;
use serde_json::json;

use crate::account_resolution::{resolve_contract_target, resolve_transaction_sender};
use crate::artifacts::ArtifactOptions;
use crate::balance::fetch_balance_blocking;
use crate::build::{cargo_metadata_with_artifacts, resolve_package};
use crate::codec::{
    CallMetadata, SpecMethodMetadata, decode_return, encode_args, format_contract_address,
    parse_contract_address,
};
use crate::config::{ProfileOverrides, ResolvedProfile, resolve_profile_with_overrides};
use crate::contract_spec::load_built_contract_spec;
use crate::dev_detail::{fetch_transaction_detail_blocking, print_transaction_detail};
use crate::network_descriptor::resolve_wallet_signing_context;
use crate::result::CommandResult;
use crate::tx_utils::{
    command_error_for_exec_status, decode_receipt_abort, format_tx_hash, nonce_from_timestamp,
    print_failed_receipt_data, resolve_block_expiry_height, summarize_exec_status,
};
use crate::{CliError, Print};

/// Default gas budget for a contract call and its inner invocation.
pub const DEFAULT_CALL_GAS_LIMIT: u64 = 25_000_000;

/// Current schema version for `call --json` data payloads.
pub const CALL_JSON_SCHEMA_VERSION: u32 = 1;

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
    signed: SignedCall,
}

#[derive(Debug)]
struct CallPlan {
    rpc_url: String,
    profile_name: String,
    contract_address_hex: String,
    wallet_address_hex: String,
    wallet_entrypoint_id: String,
    wallet_entrypoint_source: &'static str,
    method: String,
    resolved: ResolvedCall,
    calldata: Vec<u8>,
    transaction: UnsignedCall,
    signer: LocalSigner,
}

/// Call a method on a deployed contract.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
pub fn run(
    contract: &str,
    method: &str,
    args: &[String],
    package: Option<&str>,
    dry_run: bool,
    gas_limit: u64,
    force: bool,
    wallet: Option<&str>,
    artifacts: &ArtifactOptions,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let resolved_profile = resolve_profile_with_overrides(&cwd, profile, overrides)?;
    if !dry_run {
        print.require_confirmation_available(force, "call a contract")?;
    }
    let plan = prepare_call_plan(
        &cwd,
        resolved_profile,
        contract,
        method,
        args,
        package,
        artifacts,
        wallet,
        gas_limit,
    )?;
    if !dry_run {
        print.detail("contract", &plan.contract_address_hex);
        print.detail("method", &plan.method);
        print.detail("wallet", &plan.wallet_address_hex);
        print.detail("profile", &plan.profile_name);
        if !print.confirm(force, "call a contract", "Submit this contract call?")? {
            print.note("Cancelled");
            return Ok(());
        }
        // An underfunded call can be dropped without a receipt; give funding
        // guidance here instead of waiting for a timeout.
        check_call_gas_balance(&plan.rpc_url, &plan.wallet_address_hex, gas_limit)?;
    }
    let prepared = sign_call_plan(plan)?;
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
            print.detail("gas used", receipt.gas_used);
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
        "schemaVersion": CALL_JSON_SCHEMA_VERSION,
        "txHash": tx_hash_formatted,
        "contract": prepared.contract_address_hex,
        "method": prepared.method,
        "height": receipt.height,
        "status": receipt.status,
        "gasUsed": receipt.gas_used,
        "statusSummary": status_summary,
        "returnData": receipt.return_data,
        "abortData": receipt.abort_data,
        "returnValue": decoded_return,
        "transactionDetail": transaction_detail,
        "diagnostics": diagnostics,
        "walletEntrypointId": prepared.wallet_entrypoint_id,
        "walletEntrypointSource": prepared.wallet_entrypoint_source,
    });
    crate::tx_utils::insert_abort_json(&mut data, decoded_abort.as_ref());
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
            "entrypointId": prepared.wallet_entrypoint_id,
            "entrypointSource": prepared.wallet_entrypoint_source,
        },
        "target": {
            "contract": prepared.contract_address_hex,
            "method": prepared.method,
            "entrypointId": format_entrypoint_id(prepared.resolved.entrypoint),
        },
        "transactionDetail": {
            "rpcMethod": "groundhog_dev_getTransactionDetail",
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
    let calldata_hex = prefixed_hex(&prepared.calldata);

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
            "schemaVersion": CALL_JSON_SCHEMA_VERSION,
            "dryRun": true,
            "submitted": false,
            "rpcUrl": prepared.rpc_url,
            "contract": prepared.contract_address_hex,
            "method": prepared.method,
            "entrypointId": entrypoint_id,
            "walletEntrypointId": prepared.wallet_entrypoint_id,
            "walletEntrypointSource": prepared.wallet_entrypoint_source,
            "calldata": calldata_hex,
            "signedTx": prepared.tx_hex,
            "selector": selector_json(&prepared.resolved),
        }),
    ))?;

    Ok(())
}

pub(crate) fn format_entrypoint_id(entrypoint: ContractEntrypointID) -> String {
    format!("0x{}", hex::encode(entrypoint.into_inner()))
}

fn selector_summary(resolved: &ResolvedCall) -> String {
    match &resolved.metadata {
        Some(CallMetadata::Spec(metadata)) => {
            format!(
                "contract spec {} v{}",
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
        Some(CallMetadata::Spec(metadata)) => {
            // Keep the legacy machine-readable value stable even though specs
            // comes from the RV32 spec sidecar.
            json!({
                "source": "embedded_spec",
                "interface": {
                    "name": metadata.interface_name,
                    "version": metadata.version,
                },
            })
        }
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

#[allow(clippy::too_many_arguments)]
fn prepare_call_plan(
    cwd: &Path,
    resolved_profile: ResolvedProfile,
    contract: &str,
    method: &str,
    args: &[String],
    package: Option<&str>,
    artifacts: &ArtifactOptions,
    wallet: Option<&str>,
    gas_limit: u64,
) -> Result<CallPlan, CliError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    prepare_call_plan_with_runtime(
        cwd,
        resolved_profile,
        contract,
        method,
        args,
        package,
        artifacts,
        wallet,
        gas_limit,
        &rt,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn prepare_call_plan_with_runtime(
    cwd: &Path,
    resolved_profile: ResolvedProfile,
    contract: &str,
    method: &str,
    args: &[String],
    package: Option<&str>,
    artifacts: &ArtifactOptions,
    wallet: Option<&str>,
    gas_limit: u64,
    rt: &tokio::runtime::Runtime,
) -> Result<CallPlan, CliError> {
    if resolved_profile.rpc_url.is_none() && resolved_profile.default_key.is_none() {
        return Err(CliError::MissingRpcUrl);
    }
    let rpc_url = resolved_profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?;

    let contract_address = resolve_contract_target(&resolved_profile, contract)?;
    let contract_address_hex = format_contract_address(&contract_address);

    let sender = resolve_transaction_sender(&resolved_profile, wallet)?;

    let resolved = resolve_call(cwd, method, package, artifacts)?;
    let calldata = encode_args(args, resolved.metadata.as_ref())?;

    let wallet_address = sender.wallet_address;
    let wallet_address_hex = format_contract_address(&wallet_address);

    let wallet_context = resolve_wallet_signing_context(
        &resolved_profile,
        &sender.key_name,
        sender.network_hash,
        rt,
    )?;
    let wallet_entrypoint_id = format_entrypoint_id(wallet_context.entrypoints.do_action);
    let block_expiry_height = rt.block_on(async { resolve_block_expiry_height(rpc_url).await })?;

    let tx = groundhog_client::prepare_call(
        CallRequest {
            wallet_address,
            wallet_entrypoint: wallet_context.entrypoints.do_action,
            target_address: contract_address,
            target_entrypoint: resolved.entrypoint,
            target_calldata: calldata.as_slice(),
            gas_limit,
            target_gas_limit: gas_limit,
            block_expiry_height,
            nonce: nonce_from_timestamp(),
        },
        wallet_context.network_hash,
    )
    .map_err(|e| CliError::Other(e.to_string()))?;

    Ok(CallPlan {
        rpc_url: rpc_url.to_owned(),
        profile_name: resolved_profile.name,
        contract_address_hex,
        wallet_address_hex,
        wallet_entrypoint_id,
        wallet_entrypoint_source: wallet_context.entrypoints.source,
        method: method.to_owned(),
        resolved,
        calldata,
        transaction: tx,
        signer: sender.signer,
    })
}

fn check_call_gas_balance(rpc_url: &str, wallet: &str, gas_limit: u64) -> Result<(), CliError> {
    let address = parse_contract_address(wallet)?;
    let balance = fetch_balance_blocking(
        rpc_url,
        groundhog_json_rpc_types::ContractAddress::from(address),
    )?;
    let available = balance.raw_balance.parse::<u128>().map_err(|error| {
        CliError::Other(format!(
            "invalid raw balance `{}` from RPC: {error}",
            balance.raw_balance
        ))
    })?;
    if available < u128::from(gas_limit) {
        return Err(CliError::CallGasBalanceInsufficient {
            wallet: wallet.to_owned(),
            available_raw: available,
            required_raw: gas_limit,
        });
    }
    Ok(())
}

fn sign_call_plan(plan: CallPlan) -> Result<PreparedCall, CliError> {
    let signed = plan
        .transaction
        .sign(&plan.signer)
        .map_err(|error| CliError::Other(error.to_string()))?;
    let tx_hex = signed.tx_hex().to_owned();
    Ok(PreparedCall {
        rpc_url: plan.rpc_url,
        contract_address_hex: plan.contract_address_hex,
        wallet_address_hex: plan.wallet_address_hex,
        wallet_entrypoint_id: plan.wallet_entrypoint_id,
        wallet_entrypoint_source: plan.wallet_entrypoint_source,
        method: plan.method,
        resolved: plan.resolved,
        calldata: plan.calldata,
        tx_hex,
        signed,
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
    rt.block_on(async {
        let client = crate::http_client::rpc_client(&prepared.rpc_url)?;
        let submitted = prepared
            .signed
            .submit(&client)
            .await
            .map_err(crate::tx_utils::map_submit_call_error)?;
        let receipt = submitted
            .wait(&client, PollOptions::default())
            .await
            .map_err(crate::tx_utils::map_poll_receipt_error)?;
        Ok((submitted.tx_hash, receipt))
    })
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

pub(crate) fn resolve_call(
    cwd: &Path,
    method: &str,
    package: Option<&str>,
    artifacts: &ArtifactOptions,
) -> Result<ResolvedCall, CliError> {
    if let Some(entrypoint) = explicit_entrypoint_id(method)? {
        return Ok(ResolvedCall {
            entrypoint,
            metadata: None,
        });
    }
    let metadata = resolve_spec_method(cwd, method, package, artifacts)?;
    Ok(ResolvedCall {
        entrypoint: method_entrypoint_id(&metadata.interface_name, &metadata.version, method),
        metadata: Some(CallMetadata::Spec(metadata)),
    })
}

fn resolve_spec_method(
    cwd: &Path,
    method: &str,
    package: Option<&str>,
    artifacts: &ArtifactOptions,
) -> Result<SpecMethodMetadata, CliError> {
    let missing_spec = || CliError::CallSpecRequired {
        method: method.to_owned(),
    };
    let selected = select_package(cwd, package, artifacts)?.ok_or_else(missing_spec)?;
    let spec = load_built_contract_spec(&selected.target_directory, &selected.name)?
        .ok_or_else(missing_spec)?;

    groundhog_client::codec::resolve_spec_method(&spec, method)?.ok_or_else(|| {
        CliError::CallMethodNotFound {
            method: method.to_owned(),
            package: selected.name,
        }
    })
}

/// Workspace package whose build output describes the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedPackage {
    name: String,
    /// Cargo target directory holding the built contract spec sidecar.
    target_directory: PathBuf,
}

/// Select the package whose built spec resolves named methods.
///
/// Outside a Cargo project, return `None` so the caller can request a build.
/// Within a workspace, require an unambiguous package selection.
fn select_package(
    cwd: &Path,
    package: Option<&str>,
    artifacts: &ArtifactOptions,
) -> Result<Option<SelectedPackage>, CliError> {
    let metadata = match cargo_metadata_with_artifacts(artifacts) {
        Ok(metadata) => metadata,
        Err(err) if package.is_some() => return Err(err),
        Err(_) => return Ok(None),
    };
    let pkg_name = resolve_package(&metadata, package, cwd)?;
    Ok(Some(SelectedPackage {
        name: pkg_name.to_owned(),
        target_directory: metadata.target_directory,
    }))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ResolvedCall, explicit_entrypoint_id, format_entrypoint_id, method_entrypoint_id,
        resolve_call, resolve_spec_method, select_package, selector_json, selector_summary,
    };
    use crate::CliError;
    use crate::artifacts::ArtifactOptions;
    use crate::codec::{
        CallMetadata, ContractMetadata, MethodMetadata, SpecMethodMetadata, build_type_def_index,
        decode_return as decode_return_value, decode_spec_value, decode_spec_value_from_reader,
        encode_args as encode_calldata, normalize_type_name, parse_contract_address,
    };
    use crate::contract_spec::{InterfaceSpec, load_built_registered_interfaces};
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
    fn selector_helpers_report_contract_spec_resolution() {
        let metadata = sample_single_arg_spec_metadata(ContractType::U32);
        let resolved = ResolvedCall {
            entrypoint: method_entrypoint_id("Counter", "0.1.0", "call"),
            metadata: Some(CallMetadata::Spec(metadata)),
        };

        assert_eq!(selector_summary(&resolved), "contract spec Counter v0.1.0");
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
    fn invalid_numeric_argument_keeps_invalid_argument_error() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec!["u64".into()],
                return_type: None,
            },
        };
        let err = encode_calldata(
            &["not-a-number".into()],
            Some(&CallMetadata::Local(metadata)),
        )
        .expect_err("bug: invalid numeric argument should fail");
        assert_eq!(err.code(), "invalid_argument");
        assert!(
            err.hint().is_some_and(|hint| hint.contains("u64 argument")),
            "hint should name the argument: {:?}",
            err.hint()
        );
        match err {
            CliError::InvalidArgument { argument, message } => {
                assert_eq!(argument, "u64 argument");
                assert!(
                    message.contains("`not-a-number` is not a valid u64"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("bug: expected CliError::InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn invalid_fixed_hex_argument_keeps_invalid_argument_error() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec!["SHA256Hash".into()],
                return_type: None,
            },
        };
        let err = encode_calldata(&["0x1234".into()], Some(&CallMetadata::Local(metadata)))
            .expect_err("bug: short hash argument should fail");
        assert_eq!(err.code(), "invalid_argument");
        match err {
            CliError::InvalidArgument { argument, message } => {
                assert_eq!(argument, "sha256 hash");
                assert_eq!(message, "must be 32 bytes, got 2");
            }
            other => panic!("bug: expected CliError::InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn rejects_multiple_raw_arguments_without_spec_metadata() {
        let err = encode_calldata(&["0x01".into(), "0x02".into()], None)
            .expect_err("bug: multiple raw arguments should fail without metadata");
        assert_call_encode_error(
            err,
            "arguments",
            "multiple call arguments require a built contract spec — run `groundhog build` and \
             select a method by name, or pass a single hex-encoded calldata argument",
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
    fn encodes_and_decodes_local_native_256_values() {
        for (ty, value) in [
            ("U256", sdk::U256::MAX.to_string()),
            ("sdk::U256", sdk::U256::MAX.to_string()),
            ("groundhog_sdk::U256", sdk::U256::MAX.to_string()),
            ("I256", sdk::I256::MIN.to_string()),
            ("sdk::I256", sdk::I256::MIN.to_string()),
            ("groundhog_sdk::I256", sdk::I256::MIN.to_string()),
        ] {
            let metadata = ContractMetadata {
                interface_name: "Counter".into(),
                version: "0.1.0".into(),
                source: Path::new("/tmp/implementation.rs").to_path_buf(),
                method: MethodMetadata {
                    arg_types: vec![ty.into()],
                    return_type: Some(ty.into()),
                },
            };

            let encoded = encode_calldata(
                std::slice::from_ref(&value),
                Some(&CallMetadata::Local(metadata.clone())),
            )
            .expect("bug: local native 256-bit calldata should encode");
            let decoded = decode_return_value(&encoded, Some(&CallMetadata::Local(metadata)))
                .expect("bug: local native 256-bit return should decode");

            assert_eq!(decoded, json!(value));
        }
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

        let u256_value = sdk::U256::MAX.to_string();
        let i256_value = sdk::I256::MIN.to_string();
        let metadata = sample_single_arg_spec_metadata(ContractType::Tuple {
            tuple: vec![ContractType::U256, ContractType::I256]
                .try_into()
                .unwrap(),
        });
        let argument = json!([u256_value, i256_value]).to_string();
        let bytes = encode_calldata(&[argument], Some(&CallMetadata::Spec(metadata.clone())))
            .expect("bug: embedded spec native 256-bit integers should encode");

        assert_eq!(
            decode_spec_arguments(&metadata, &bytes),
            vec![json!([
                sdk::U256::MAX.to_string(),
                sdk::I256::MIN.to_string()
            ])]
        );
    }

    #[test]
    fn typed_native_256_arguments_enforce_ranges() {
        const U256_OVERFLOW: &str =
            "115792089237316195423570985008687907853269984665640564039457584007913129639936";
        const I256_OVERFLOW: &str =
            "57896044618658097711785492504343953926634992332820282019728792003956564819968";
        const I256_UNDERFLOW: &str =
            "-57896044618658097711785492504343953926634992332820282019728792003956564819969";

        for (ty, value, expected) in [
            (ContractType::U256, U256_OVERFLOW, "U256"),
            (ContractType::I256, I256_OVERFLOW, "I256"),
            (ContractType::I256, I256_UNDERFLOW, "I256"),
        ] {
            let error = encode_calldata(
                &[value.to_owned()],
                Some(&CallMetadata::Spec(sample_single_arg_spec_metadata(ty))),
            )
            .expect_err("bug: out-of-range native 256-bit argument must fail");

            assert!(error.to_string().contains(expected), "{error}");
            assert!(error.to_string().contains("does not fit"), "{error}");
        }
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
    #[serial]
    fn named_calls_require_a_current_build_spec_even_when_source_matches() {
        let project = create_temp_project("demo-contract");
        let project_root = std::fs::canonicalize(project.path()).unwrap();
        std::fs::write(project_root.join("groundhog.toml"), "").unwrap();
        std::fs::write(
            project_root.join("src/lib.rs"),
            "#[contract]\nimpl Counter { pub fn get() -> u64 { 42 } }\n",
        )
        .unwrap();
        let target = project_root.join("target");
        let artifacts = ArtifactOptions::new(Some(target.clone()));
        let _cwd = CurrentDirGuard::enter(&project_root);

        for state in ["unbuilt", "missing sidecar", "stale sidecar"] {
            if state != "unbuilt" {
                let rv32 = write_built_spec(&target, "demo-contract", &sample_contract_spec());
                let sidecar = rv32.with_extension("spec.xdr");
                if state == "missing sidecar" {
                    std::fs::remove_file(sidecar).unwrap();
                } else {
                    std::fs::File::open(sidecar)
                        .unwrap()
                        .set_modified(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap();
                }
            }

            let err = resolve_call(&project_root, "get", None, &artifacts).unwrap_err();
            assert!(
                matches!(&err, CliError::CallSpecRequired { method } if method == "get"),
                "{state}: {err:?}"
            );
            assert!(err.hint().unwrap().contains("groundhog build"));
            assert_eq!(err.code(), "call_spec_required");
        }
    }

    #[test]
    #[serial]
    fn named_calls_use_the_specs_interface_name_and_version() {
        let project = create_temp_project("versioned-impl");
        let project_root = std::fs::canonicalize(project.path()).unwrap();
        let manifest = project_root.join("Cargo.toml");
        std::fs::write(
            &manifest,
            std::fs::read_to_string(&manifest)
                .unwrap()
                .replace("0.1.0", "0.2.0"),
        )
        .unwrap();
        // Cargo metadata needs a source target, but the resolver must not read
        // this alias or signature to infer the exported interface or codec.
        std::fs::write(
            project_root.join("src/lib.rs"),
            "use interfaces::VersionedCallee as Alias;\n\
             #[contract]\nimpl Alias for Contract { fn get() -> u32 { 42 } }\n",
        )
        .unwrap();
        let target = project_root.join("target");
        let artifacts = ArtifactOptions::new(Some(target.clone()));
        let _cwd = CurrentDirGuard::enter(&project_root);

        for version in ["0.1.0", "1.2.3"] {
            let mut spec = sample_contract_spec();
            spec.interfaces[0].name = spec_string_bytes("VersionedCallee");
            spec.interfaces[0].vers = spec_string_bytes(version);
            write_built_spec(&target, "versioned-impl", &spec);

            let resolved = resolve_call(&project_root, "get", None, &artifacts).unwrap();
            assert_eq!(
                resolved.entrypoint,
                method_entrypoint_id("VersionedCallee", version, "get")
            );
            assert_ne!(
                resolved.entrypoint,
                method_entrypoint_id("VersionedCallee", "0.2.0", "get")
            );
            assert_ne!(
                resolved.entrypoint,
                method_entrypoint_id("Alias", version, "get")
            );
            assert_eq!(
                selector_json(&resolved)["interface"],
                json!({ "name": "VersionedCallee", "version": version })
            );
            let value = xdrs::cxs::encode(&42_u64).unwrap();
            assert_eq!(
                decode_return_value(&value, resolved.metadata.as_ref()),
                Some(json!(42))
            );
        }
    }

    #[test]
    #[serial]
    fn methods_absent_from_the_spec_do_not_resolve_from_source() {
        let project = create_temp_project("demo-contract");
        let project_root = std::fs::canonicalize(project.path()).unwrap();
        std::fs::write(project_root.join("groundhog.toml"), "").unwrap();
        std::fs::write(
            project_root.join("src/lib.rs"),
            "#[contract]\nimpl Counter { pub fn source_only() {} }\n",
        )
        .unwrap();
        let target = project_root.join("target");
        write_built_spec(&target, "demo-contract", &sample_contract_spec());
        let artifacts = ArtifactOptions::new(Some(target));
        let _cwd = CurrentDirGuard::enter(&project_root);

        let err = resolve_call(&project_root, "source_only", None, &artifacts).unwrap_err();
        assert!(matches!(
            err,
            CliError::CallMethodNotFound { method, package }
                if method == "source_only" && package == "demo-contract"
        ));
    }

    #[test]
    #[serial]
    fn explicit_entrypoints_work_without_a_project_but_named_calls_request_a_build() {
        let directory = tempfile::tempdir().unwrap();
        let _cwd = CurrentDirGuard::enter(directory.path());
        let artifacts = ArtifactOptions::default();
        let resolved =
            resolve_call(directory.path(), "0x0102030405060708", None, &artifacts).unwrap();
        assert_eq!(
            format_entrypoint_id(resolved.entrypoint),
            "0x0102030405060708"
        );
        assert_eq!(resolved.metadata, None);
        let err = resolve_call(directory.path(), "get", None, &artifacts).unwrap_err();
        assert!(matches!(err, CliError::CallSpecRequired { .. }));
    }

    #[test]
    #[serial]
    fn select_package_reads_the_explicit_target_dir() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let rv32 = custom_target.join("groundhog").join("demo_contract.rv32a");
        std::fs::create_dir_all(rv32.parent().expect("bug: rv32 parent should exist"))
            .expect("bug: custom target dir should be created");
        std::fs::write(&rv32, b"rv32 fixture").expect("bug: rv32 fixture should be written");
        std::fs::write(
            rv32.with_extension("spec.xdr"),
            xdrs::txs::encode(&sample_contract_spec()).expect("bug: spec should encode"),
        )
        .expect("bug: rv32 spec sidecar should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let selected = select_package(&project_root, None, &artifacts)
            .expect("bug: package selection should succeed")
            .expect("bug: the project package should resolve");
        let interfaces =
            load_built_registered_interfaces(&selected.target_directory, &selected.name)
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
        let rv32 = custom_target.join("groundhog").join("demo_contract.rv32a");
        std::fs::create_dir_all(rv32.parent().expect("bug: rv32 parent should exist"))
            .expect("bug: custom target dir should be created");
        std::fs::write(&rv32, b"rv32 fixture").expect("bug: rv32 fixture should be written");
        std::fs::write(
            rv32.with_extension("spec.xdr"),
            xdrs::txs::encode(&sample_multi_interface_contract_spec())
                .expect("bug: spec should encode"),
        )
        .expect("bug: rv32 spec sidecar should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let err = resolve_spec_method(&project_root, "get", None, &artifacts)
            .expect_err("bug: ambiguous sidecar interface matches should fail");
        assert_eq!(
            err.to_string(),
            "method `get` matches multiple interfaces in the contract spec: Counter \
             v0.1.0, Other v0.1.0"
        );
    }

    #[test]
    #[serial]
    fn resolves_and_encodes_erc20_methods_from_rv32_spec_sidecar() {
        let project = create_temp_project("erc20");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let rv32 = custom_target.join("groundhog").join("erc20.rv32a");
        std::fs::create_dir_all(rv32.parent().expect("bug: rv32 parent should exist"))
            .expect("bug: custom target dir should be created");
        std::fs::write(&rv32, b"rv32 fixture").expect("bug: rv32 fixture should be written");
        std::fs::write(
            rv32.with_extension("spec.xdr"),
            xdrs::txs::encode(&sample_erc20_contract_spec()).expect("bug: spec should encode"),
        )
        .expect("bug: rv32 spec sidecar should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let resolved = resolve_call(&project_root, "mint", None, &artifacts)
            .expect("bug: mint should resolve from the rv32 spec sidecar");
        let receiver_hex = format!("0x{}", "04".repeat(32));
        let encoded = encode_calldata(
            &[receiver_hex.clone(), "55".into()],
            resolved.metadata.as_ref(),
        )
        .expect("bug: the spec should encode address and u128 arguments");
        let decoded: (groundhog_xdr::types::ContractAddress, u128) =
            xdrs::cxs::decode(&encoded).expect("bug: encoded mint arguments should decode");
        assert_eq!(
            decoded,
            (
                parse_contract_address(&receiver_hex).expect("bug: receiver should parse"),
                55,
            )
        );

        let total_supply = resolve_call(&project_root, "total_supply", None, &artifacts)
            .expect("bug: total_supply should resolve from the rv32 spec sidecar");
        let return_data = xdrs::cxs::encode(&0_u128).expect("bug: u128 should encode");
        assert_eq!(
            decode_return_value(&return_data, total_supply.metadata.as_ref()),
            Some(json!("0")),
        );
    }

    #[test]
    #[serial]
    fn workspace_package_selects_its_own_built_spec() {
        let workspace = create_temp_workspace();
        let workspace_root = std::fs::canonicalize(workspace.path())
            .expect("bug: workspace path should canonicalize");

        let _cwd = CurrentDirGuard::enter(&workspace_root);
        let metadata = resolve_spec_method(
            &workspace_root,
            "mint",
            Some("erc20"),
            &ArtifactOptions::new(Some(workspace_root.join("target"))),
        )
        .expect("bug: mint should resolve from the erc20 member spec");

        assert_eq!(metadata.interface_name, "ERC20");
        assert_eq!(metadata.version, "0.1.9");
    }

    #[test]
    #[serial]
    fn member_directory_infers_its_own_package_without_a_flag() {
        let workspace = create_temp_workspace();
        let workspace_root = std::fs::canonicalize(workspace.path())
            .expect("bug: workspace path should canonicalize");
        let member_root = workspace_root.join("erc20");

        let _cwd = CurrentDirGuard::enter(&member_root);
        let metadata = resolve_spec_method(
            &member_root,
            "mint",
            None,
            &ArtifactOptions::new(Some(workspace_root.join("target"))),
        )
        .expect("bug: mint should resolve from the surrounding member spec");

        assert_eq!(metadata.interface_name, "ERC20");
        assert_eq!(metadata.version, "0.1.9");
    }

    #[test]
    #[serial]
    fn workspace_root_without_a_package_asks_for_one() {
        let workspace = create_temp_workspace();
        let workspace_root = std::fs::canonicalize(workspace.path())
            .expect("bug: workspace path should canonicalize");

        let _cwd = CurrentDirGuard::enter(&workspace_root);
        let err = resolve_spec_method(&workspace_root, "mint", None, &ArtifactOptions::default())
            .expect_err("bug: a workspace root should not resolve a package on its own");

        assert!(matches!(err, CliError::PackageRequired), "got {err:?}");
    }

    #[test]
    #[serial]
    fn unknown_package_is_reported_instead_of_being_skipped() {
        let workspace = create_temp_workspace();
        let workspace_root = std::fs::canonicalize(workspace.path())
            .expect("bug: workspace path should canonicalize");

        let _cwd = CurrentDirGuard::enter(&workspace_root);
        let err = select_package(
            &workspace_root,
            Some("not-a-member"),
            &ArtifactOptions::default(),
        )
        .expect_err("bug: an unknown package name should fail");

        assert!(
            matches!(&err, CliError::PackageNotFound(name) if name == "not-a-member"),
            "got {err:?}"
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

    #[test]
    fn normalize_type_name_recognizes_qualified_native_256_types() {
        assert_eq!(normalize_type_name("sdk::U256"), "U256");
        assert_eq!(normalize_type_name("&sdk::I256"), "I256");
        assert_eq!(normalize_type_name("groundhog_sdk::U256"), "U256");
        assert_eq!(normalize_type_name("&'a groundhog_sdk::I256"), "I256");
        assert_eq!(normalize_type_name("other::U256"), "other::U256");
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

    /// Two-member workspace with the project config at the root, so no member
    /// contains it and `-p` is the only way to select one.  Members inherit
    /// their version from the workspace.
    fn create_temp_workspace() -> TempDir {
        let workspace = tempfile::tempdir().expect("bug: temp dir should be created");
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"erc20\", \"counter\"]\n\n             [workspace.package]\nversion = \"9.9.9\"\n",
        )
        .expect("bug: workspace manifest should be written");
        std::fs::write(workspace.path().join("groundhog.toml"), "")
            .expect("bug: groundhog.toml should be written");

        for (member, interface, method) in [
            ("erc20", "Erc20", "mint"),
            ("counter", "Counter", "increment"),
        ] {
            let src = workspace.path().join(member).join("src");
            std::fs::create_dir_all(&src).expect("bug: member src dir should be created");
            std::fs::write(
                workspace.path().join(member).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{member}\"\nversion.workspace = true\n                     edition = \"2024\"\n"
                ),
            )
            .expect("bug: member manifest should be written");
            std::fs::write(
                src.join("lib.rs"),
                format!(
                    "#[contract(throws = Error)]\nimpl {interface} {{\n                         pub fn {method}(&self, amount: u64) -> u64 {{ amount }}\n}}\n"
                ),
            )
            .expect("bug: member sources should be written");
        }
        write_built_spec(
            &workspace.path().join("target"),
            "erc20",
            &sample_erc20_contract_spec(),
        );
        write_built_spec(
            &workspace.path().join("target"),
            "counter",
            &sample_contract_spec(),
        );
        workspace
    }

    fn write_built_spec(target: &Path, package: &str, spec: &ContractSpec) -> std::path::PathBuf {
        let artifact = crate::artifacts::groundhog_rv32_output_path(target, package);
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, b"rv32 fixture").unwrap();
        std::fs::write(
            artifact.with_extension("spec.xdr"),
            xdrs::txs::encode(spec).unwrap(),
        )
        .unwrap();
        artifact
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

    fn sample_erc20_contract_spec() -> ContractSpec {
        ContractSpec {
            interfaces: vec![ContractSpecInterface {
                name: spec_string_bytes("ERC20"),
                vers: spec_string_bytes("0.1.9"),
                throws: vec![].try_into().unwrap(),
                methods: vec![
                    ContractSpecMethod {
                        name: spec_string_bytes("mint"),
                        args: vec![
                            ContractField {
                                name: spec_string_bytes("receiver"),
                                ty: ContractType::Address,
                            },
                            ContractField {
                                name: spec_string_bytes("amount"),
                                ty: ContractType::U128,
                            },
                        ]
                        .try_into()
                        .unwrap(),
                        return_type: ContractType::Void,
                    },
                    ContractSpecMethod {
                        name: spec_string_bytes("total_supply"),
                        args: vec![].try_into().unwrap(),
                        return_type: ContractType::U128,
                    },
                ]
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
                        name: spec_id("Payload"),
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
                        name: spec_id("Mode"),
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
                        name: spec_id("Envelope"),
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
                        name: spec_id("NumericEnum"),
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
                        name: spec_id("NumericUnion"),
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
            named: spec_id(name),
        }
    }

    fn spec_string_bytes(value: &str) -> ContractSpecString {
        value.try_into().expect("bug: test string should fit")
    }

    /// A fixture id: one crate, one module, so the declared name is what
    /// tells these types apart -- which is all these tests turn on.
    fn spec_id(declared: &str) -> groundhog_contract_spec::ContractSpecId {
        groundhog_contract_spec::ContractSpecId {
            crate_name: spec_string_bytes("demo"),
            major: spec_string_bytes("1"),
            module_path: spec_string_bytes("demo"),
            declared: spec_string_bytes(declared),
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
