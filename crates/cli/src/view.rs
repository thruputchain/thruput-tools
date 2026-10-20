mod native;
pub use native::run;

use std::path::Path;
use thruput_json_rpc_types::hex::prefixed_hex;

use thruput_json_rpc_types::{ContractAddress, hex::HexBytes, view::ViewRequest};

use crate::CliError;
use crate::account_resolution::{resolve_address_target, resolve_contract_target};
use crate::artifacts::ArtifactOptions;
use crate::call::{ResolvedCall, resolve_call, selector_json};
use crate::codec::{encode_args, format_contract_address};
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};

#[derive(Debug, Clone)]
struct PreparedView {
    rpc_url: String,
    native_profile: crate::config::ResolvedProfile,
    method: String,
    resolved: ResolvedCall,
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
) -> Result<PreparedView, CliError> {
    let resolved_profile = resolve_profile_with_overrides(project_start, profile, overrides)?;
    let rpc_url = resolved_profile.rpc_endpoint()?;

    let contract_address =
        ContractAddress::from(resolve_contract_target(&resolved_profile, contract)?);
    let resolved = resolve_call(project_start, method, package, artifacts)?;
    let calldata_bytes = resolve_calldata(args, calldata, &resolved)?;
    let caller = from
        .map(|target| resolve_address_target(&resolved_profile, target).map(ContractAddress::from))
        .transpose()?;
    let entrypoint_bytes = resolved.entrypoint.into_inner().to_vec();

    Ok(PreparedView {
        rpc_url: rpc_url.to_owned(),
        native_profile: resolved_profile.clone(),
        method: method.to_owned(),
        resolved,
        request: ViewRequest {
            auth_claim: None,
            min_height: None,
            contract: contract_address,
            entrypoint: HexBytes(entrypoint_bytes),
            calldata: HexBytes(calldata_bytes),
            caller,
            gas_limit: None,
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
        assert!(err.to_string().contains("invalid view calldata"));
    }
}
