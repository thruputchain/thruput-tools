use std::path::Path;

use groundhog_rpc_api_types::{
    ContractAddress,
    hex::HexBytes,
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
};
use groundhog_rpc_client::{GroundhogApiClient, GroundhogDevApiClient, HttpClientBuilder};
use jsonrpsee::core::ClientError;
use jsonrpsee::types::error::METHOD_NOT_FOUND_CODE;
use serde_json::json;

use crate::artifacts::ArtifactOptions;
use crate::call::{
    ResolvedCall, format_entrypoint_id, format_hex_bytes, resolve_call, selector_json,
};
use crate::codec::{decode_return, encode_args, parse_contract_address};
use crate::config::resolve_profile;
use crate::dev_keys;
use crate::result::CommandResult;
use crate::{CliError, Print};

/// Execute a read-only contract invocation.
#[allow(clippy::too_many_arguments)]
pub fn run(
    contract: &str,
    method: &str,
    args: &[String],
    calldata: Option<&str>,
    from: Option<&str>,
    artifacts: &ArtifactOptions,
    project_start: &Path,
    profile: &str,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let prepared = prepare_view(
        contract,
        method,
        args,
        calldata,
        from,
        artifacts,
        project_start,
        profile,
    )?;

    print.status(format_args!(
        "Viewing {} on {} via {}...",
        prepared.method, prepared.contract_address_hex, prepared.rpc_url
    ));

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let response = rt.block_on(fetch_view(&prepared.rpc_url, prepared.request.clone()))?;

    print_view_response(&prepared, &response, print);
    print.json(&CommandResult::new(
        cmd,
        json!({
            "rpc_url": prepared.rpc_url,
            "contract": prepared.contract_address_hex,
            "method": prepared.method,
            "entrypoint_id": prepared.entrypoint_id,
            "calldata": prepared.calldata_hex,
            "caller": prepared.request.caller,
            "selector": selector_json(&prepared.resolved),
            "height": response.height,
            "output": response.output,
            "events": response.events,
            "return_value": decoded_return_value(&prepared, &response),
        }),
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
    artifacts: &ArtifactOptions,
    project_start: &Path,
    profile: &str,
) -> Result<PreparedView, CliError> {
    let resolved_profile = resolve_profile(project_start, Some(profile))?;
    let rpc_url = resolved_profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?;

    let contract_address_hex = resolved_profile.resolve_contract(contract);
    let contract_address = ContractAddress::from(parse_contract_address(&contract_address_hex)?);
    let resolved = resolve_call(project_start, method, artifacts)?;
    let calldata_bytes = resolve_calldata(args, calldata, &resolved)?;
    let caller = resolve_caller(rpc_url, from)?;
    let entrypoint_bytes = resolved.entrypoint.into_inner().to_vec();
    let entrypoint_id = format_entrypoint_id(resolved.entrypoint);
    let calldata_hex = format_hex_bytes(&calldata_bytes);

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
            return Err(CliError::Other(
                "`--calldata` can only be used with an explicit 0x-prefixed entrypoint ID".into(),
            ));
        }
        if !args.is_empty() {
            return Err(CliError::Other(
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
    hex::decode(stripped).map_err(|e| CliError::Other(format!("invalid view calldata hex: {e}")))
}

fn resolve_caller(rpc_url: &str, from: Option<&str>) -> Result<Option<ContractAddress>, CliError> {
    let Some(from) = from else {
        return Ok(None);
    };
    if let Some(index) = dev_keys::dev_key_index(from) {
        let rt =
            tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
        return rt
            .block_on(resolve_dev_caller(rpc_url, from, index))
            .map(Some);
    }
    parse_contract_address(from)
        .map(ContractAddress::from)
        .map(Some)
}

async fn resolve_dev_caller(
    rpc_url: &str,
    from: &str,
    index: usize,
) -> Result<ContractAddress, CliError> {
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|error| {
            dev_caller_error(from, format!("could not connect to {rpc_url}: {error}"))
        })?;

    let accounts = match client.get_accounts().await {
        Ok(accounts) => accounts,
        Err(error) if is_method_not_found(&error) => {
            return Err(dev_caller_error(
                from,
                "RPC endpoint does not expose groundhog_dev_getAccounts",
            ));
        }
        Err(error) => {
            return Err(dev_caller_error(
                from,
                format!("groundhog_dev_getAccounts failed: {error}"),
            ));
        }
    };

    let Some(account) = accounts.get(index) else {
        return Err(dev_caller_error(
            from,
            format!(
                "groundhog_dev_getAccounts returned {} accounts, missing dev-{index}",
                accounts.len()
            ),
        ));
    };

    Ok(account.address)
}

fn dev_caller_error(from: &str, message: impl Into<String>) -> CliError {
    CliError::LocalDevAccountUnavailable {
        key_name: from.to_owned(),
        message: message.into(),
    }
}

fn is_method_not_found(error: &ClientError) -> bool {
    matches!(error, ClientError::Call(object) if object.code() == METHOD_NOT_FOUND_CODE)
}

async fn fetch_view(rpc_url: &str, request: ViewRequest) -> Result<ViewResponse, CliError> {
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|e| CliError::RpcConnect(format!("{rpc_url}: {e}")))?;
    client
        .view(request)
        .await
        .map_err(|e| CliError::RpcMethodFailed {
            method: "view",
            message: e.to_string(),
        })
}

fn print_view_response(prepared: &PreparedView, response: &ViewResponse, print: &Print) {
    match &response.output {
        RootInvocationOutput::Return { return_data } => {
            if let Some(value) = decoded_return_value(prepared, response) {
                print.status(format_args!(
                    "View returned: {} -> {value}",
                    prepared.method
                ));
            } else {
                print.status(format_args!("View returned: {}", prepared.method));
            }
            print.status(format_args!(
                "  contract: {}",
                prepared.contract_address_hex
            ));
            print.status(format_args!("  height: {}", *response.height));
            if !return_data.0.is_empty() && decoded_return_value(prepared, response).is_none() {
                print.status(format_args!(
                    "  return: {}",
                    format_hex_bytes(&return_data.0)
                ));
            }
        }
        RootInvocationOutput::Abort { abort_data } => {
            print.warn("View aborted.");
            print.status(format_args!("  method: {}", prepared.method));
            print.status(format_args!(
                "  contract: {}",
                prepared.contract_address_hex
            ));
            print.status(format_args!("  height: {}", *response.height));
            print.status(format_args!("  abort: {}", format_hex_bytes(&abort_data.0)));
        }
        RootInvocationOutput::Fault => {
            print.warn("View faulted.");
            print.status(format_args!("  method: {}", prepared.method));
            print.status(format_args!(
                "  contract: {}",
                prepared.contract_address_hex
            ));
            print.status(format_args!("  height: {}", *response.height));
        }
        _ => {
            print.warn("View returned an unsupported output variant.");
            print.status(format_args!("  method: {}", prepared.method));
            print.status(format_args!(
                "  contract: {}",
                prepared.contract_address_hex
            ));
            print.status(format_args!("  height: {}", *response.height));
        }
    }

    if !response.events.is_empty() {
        print.status(format_args!("  events: {}", response.events.len()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_calldata_accepts_prefixed_hex() {
        assert_eq!(parse_calldata("0x0102ff").unwrap(), vec![1, 2, 255]);
    }

    #[test]
    fn parse_calldata_rejects_non_hex() {
        let err = parse_calldata("0xnot-hex").unwrap_err();
        assert!(err.to_string().contains("invalid view calldata hex"));
    }
}
