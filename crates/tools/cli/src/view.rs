use groundhog_json_rpc_types::hex::prefixed_hex;
use std::path::Path;

use groundhog_json_rpc_client::GroundhogApiClient;
use groundhog_json_rpc_types::{
    ContractAddress,
    hex::HexBytes,
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
};

use crate::account_resolution::{lookup_dev_account_address, resolve_contract_target};
use crate::artifacts::ArtifactOptions;
use crate::call::{ResolvedCall, format_entrypoint_id, resolve_call, selector_json};
use crate::codec::{
    AbortDecodeError, DecodedAbort, decode_abort, decode_return, encode_args,
    format_contract_address, parse_contract_address,
};
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::dev_detail::format_event;
use crate::dev_keys;
use crate::result::CommandResult;
use crate::view_json::{VIEW_JSON_SCHEMA_VERSION, ViewJsonPayload};
use crate::{CliError, Print};

/// Resolve inputs, call `groundhog_view`, and print the view output.
#[allow(clippy::too_many_arguments)]
pub fn run(
    contract: &str,
    method: &str,
    args: &[String],
    calldata: Option<&str>,
    from: Option<&str>,
    package: Option<&str>,
    artifacts: &ArtifactOptions,
    project_start: &Path,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let prepared = prepare_view(
        contract,
        method,
        args,
        calldata,
        from,
        package,
        artifacts,
        project_start,
        profile,
        overrides,
        &rt,
    )?;

    let task = print.start_task(format_args!("Viewing `{}`", prepared.method));

    let response = rt.block_on(fetch_view(&prepared.rpc_url, prepared.request.clone()))?;
    let return_value = decoded_return_value(&prepared, &response);
    let decoded_abort = decoded_abort_value(&prepared, &response);
    let abort = decoded_abort
        .as_ref()
        .and_then(|result| result.as_ref().ok());

    if !print.is_json() {
        task.clear();
        print_view_response(
            &prepared,
            &response,
            return_value.as_ref(),
            decoded_abort.as_ref(),
            print,
        );
    }
    print.json(&CommandResult::new(
        cmd,
        ViewJsonPayload {
            schema_version: VIEW_JSON_SCHEMA_VERSION,
            rpc_url: &prepared.rpc_url,
            contract: &prepared.contract_address_hex,
            method: &prepared.method,
            entrypoint_id: &prepared.entrypoint_id,
            calldata: &prepared.calldata_hex,
            caller: &prepared.request.caller,
            selector: selector_json(&prepared.resolved),
            height: &response.height,
            output: &response.output,
            events: &response.events,
            return_value: return_value.as_ref(),
            abort_exception: abort.and_then(|abort| abort.exception.as_deref()),
            abort_exception_crate: abort.and_then(|abort| abort.exception_crate),
            abort_exception_version: abort.and_then(|abort| abort.exception_version),
            abort_value: abort.map(|abort| &abort.value),
            abort_decode_error: decoded_abort
                .as_ref()
                .and_then(|result| result.as_ref().err())
                .map(ToString::to_string),
        },
    ))?;

    Ok(())
}

#[derive(Debug, Clone)]
struct PreparedView {
    rpc_url: String,
    contract_address_hex: String,
    method: String,
    resolved: ResolvedCall,
    entrypoint_id: String,
    calldata_hex: String,
    request: ViewRequest,
}

#[allow(clippy::too_many_arguments)]
fn prepare_view(
    contract: &str,
    method: &str,
    args: &[String],
    calldata: Option<&str>,
    from: Option<&str>,
    package: Option<&str>,
    artifacts: &ArtifactOptions,
    project_start: &Path,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    rt: &tokio::runtime::Runtime,
) -> Result<PreparedView, CliError> {
    let resolved_profile = resolve_profile_with_overrides(project_start, profile, overrides)?;
    let rpc_url = resolved_profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?;

    let contract_address =
        ContractAddress::from(resolve_contract_target(&resolved_profile, contract)?);
    let contract_address_hex = format_contract_address(&contract_address);
    let resolved = resolve_call(project_start, method, package, artifacts)?;
    let calldata_bytes = resolve_calldata(args, calldata, &resolved)?;
    let caller = resolve_caller(&resolved_profile, from, rt)?;
    let entrypoint_bytes = resolved.entrypoint.into_inner().to_vec();
    let entrypoint_id = format_entrypoint_id(resolved.entrypoint);
    let calldata_hex = prefixed_hex(&calldata_bytes);

    Ok(PreparedView {
        rpc_url: rpc_url.to_owned(),
        contract_address_hex,
        method: method.to_owned(),
        resolved,
        entrypoint_id,
        calldata_hex,
        request: ViewRequest {
            contract: contract_address,
            entrypoint: HexBytes(entrypoint_bytes),
            calldata: HexBytes(calldata_bytes),
            caller,
        },
    })
}

fn resolve_calldata(
    args: &[String],
    calldata: Option<&str>,
    resolved: &ResolvedCall,
) -> Result<Vec<u8>, CliError> {
    if let Some(raw) = calldata {
        if resolved.metadata.is_some() {
            return Err(CliError::InvalidViewArguments(
                "`--calldata` can only be used with an explicit 0x-prefixed entrypoint ID".into(),
            ));
        }
        if !args.is_empty() {
            return Err(CliError::InvalidViewArguments(
                "`--calldata` cannot be combined with positional view arguments".into(),
            ));
        }
        return parse_calldata(raw);
    }

    encode_args(args, resolved.metadata.as_ref())
}

fn parse_calldata(raw: &str) -> Result<Vec<u8>, CliError> {
    let stripped = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    hex::decode(stripped).map_err(|e| CliError::InvalidViewCalldata(e.to_string()))
}

fn resolve_caller(
    profile: &crate::config::ResolvedProfile,
    from: Option<&str>,
    _rt: &tokio::runtime::Runtime,
) -> Result<Option<ContractAddress>, CliError> {
    let Some(from) = from else {
        return Ok(None);
    };
    if let Some(index) = dev_keys::dev_key_index(from) {
        return lookup_dev_account_address(profile, from, index).map(Some);
    }
    parse_contract_address(from)
        .map(ContractAddress::from)
        .map(Some)
}

async fn fetch_view(rpc_url: &str, request: ViewRequest) -> Result<ViewResponse, CliError> {
    let client = crate::http_client::rpc_client(rpc_url)?;
    client
        .view(request)
        .await
        .map_err(|e| CliError::RpcMethodFailed {
            method: "view",
            message: e.to_string(),
        })
}

fn print_view_response(
    prepared: &PreparedView,
    response: &ViewResponse,
    decoded_return_value: Option<&serde_json::Value>,
    decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>,
    print: &Print,
) {
    match &response.output {
        RootInvocationOutput::Return { return_data } => {
            print.success("View succeeded");
            if prepared.resolved.metadata.is_some() {
                print.detail("method", &prepared.method);
                if let Some(value) = decoded_return_value {
                    print.detail("value", value);
                }
            } else {
                print.detail("entrypoint", &prepared.method);
            }
            print.detail("contract", &prepared.contract_address_hex);
            print.detail("height", *response.height);
            if !return_data.0.is_empty() && decoded_return_value.is_none() {
                print.detail("return", prefixed_hex(&return_data.0));
            }
        }
        RootInvocationOutput::Abort { abort_data } => {
            print.warn("View aborted.");
            print.detail("method", &prepared.method);
            print.detail("contract", &prepared.contract_address_hex);
            print.detail("height", *response.height);
            match decoded_abort {
                Some(Ok(abort)) => {
                    if let Some(label) = abort.exception_label() {
                        print.detail("abort type", label);
                    }
                    print.detail("abort value", &abort.value);
                }
                Some(Err(error)) => {
                    print.detail("abort", prefixed_hex(&abort_data.0));
                    print.detail("abort decode error", error);
                }
                None => print.detail("abort", prefixed_hex(&abort_data.0)),
            }
        }
        RootInvocationOutput::Fault => {
            print.warn("View faulted.");
            print.detail("method", &prepared.method);
            print.detail("contract", &prepared.contract_address_hex);
            print.detail("height", *response.height);
        }
        _ => {
            print.warn("View returned an unsupported output variant.");
            print.detail("method", &prepared.method);
            print.detail("contract", &prepared.contract_address_hex);
            print.detail("height", *response.height);
        }
    }

    if !response.events.is_empty() {
        print.detail("events", response.events.len());
        for (index, event) in response.events.iter().enumerate() {
            print.status(format_args!("    [{index}] {}", format_event(event)));
        }
    }
}

fn decoded_return_value(
    prepared: &PreparedView,
    response: &ViewResponse,
) -> Option<serde_json::Value> {
    let RootInvocationOutput::Return { return_data } = &response.output else {
        return None;
    };
    decode_return(
        return_data.0.as_slice(),
        prepared.resolved.metadata.as_ref(),
    )
}

fn decoded_abort_value(
    prepared: &PreparedView,
    response: &ViewResponse,
) -> Option<Result<DecodedAbort, AbortDecodeError>> {
    let RootInvocationOutput::Abort { abort_data } = &response.output else {
        return None;
    };
    Some(decode_abort(
        abort_data.0.as_slice(),
        prepared.resolved.metadata.as_ref(),
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::codec::{CallMetadata, ContractMetadata, MethodMetadata};

    #[test]
    fn parse_calldata_accepts_prefixed_hex() {
        assert_eq!(parse_calldata("0x0102ff").unwrap(), vec![1, 2, 255]);
    }

    #[test]
    fn parse_calldata_rejects_non_hex() {
        let err = parse_calldata("0xnot-hex").unwrap_err();
        assert!(err.to_string().contains("invalid view calldata"));
    }

    #[test]
    fn decoded_return_value_uses_resolved_metadata() {
        let metadata = ContractMetadata {
            interface_name: "Counter".into(),
            version: "0.1.0".into(),
            source: Path::new("/tmp/implementation.rs").to_path_buf(),
            method: MethodMetadata {
                arg_types: vec![],
                return_type: Some("u64".into()),
            },
        };
        let prepared = PreparedView {
            rpc_url: "http://127.0.0.1:1".into(),
            contract_address_hex: format!("0x{}", "11".repeat(32)),
            method: "get".into(),
            resolved: ResolvedCall {
                entrypoint: groundhog_xdr::contract::ContractEntrypointID::from([0u8; 8]),
                metadata: Some(CallMetadata::Local(metadata)),
            },
            entrypoint_id: "0x0000000000000000".into(),
            calldata_hex: "0x".into(),
            request: ViewRequest {
                contract: ContractAddress::from(groundhog_xdr::types::ContractAddress::from(
                    [0u8; 32],
                )),
                entrypoint: HexBytes(vec![0u8; 8]),
                calldata: HexBytes(vec![]),
                caller: None,
            },
        };
        let return_data = xdrs::cxs::encode(&7_u64).expect("bug: u64 should encode");
        let response = ViewResponse {
            output: RootInvocationOutput::Return {
                return_data: HexBytes(return_data),
            },
            events: vec![],
            height: groundhog_json_rpc_types::BlockHeight::from(12),
        };

        assert_eq!(
            decoded_return_value(&prepared, &response),
            Some(serde_json::json!(7))
        );
    }
}
