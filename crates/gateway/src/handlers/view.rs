//! `groundhog_view` handler backed by hogd `ViewGateway`.

use std::time::Duration;

use groundhog_rpc_api_types::{
    hex::HexBytes,
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
};
use groundhog_view::{
    AuthClaim, AuthClaim_source_address_claim, ContractEntrypointID, Invocation,
    InvocationFrameOutput, InvokeFlag, TransactionViewQuery, ViewGatewayResult,
};
use xdr_common::{BytesM, VecM};

use crate::{context::GatewayContext, error::GatewayError};

use super::parse_event::parse_emitted_event;

/// Execute a read-only contract invocation through hogd `ViewGateway`.
#[tracing::instrument(
    skip_all,
    fields(
        ?request.contract,
        entrypoint_len = request.entrypoint.0.len(),
        calldata_len = request.calldata.0.len(),
        caller = ?request.caller,
        gas_limit,
    )
)]
pub async fn view(
    ctx: &GatewayContext,
    request: ViewRequest,
    gas_limit: u64,
) -> Result<ViewResponse, GatewayError> {
    let client = ctx.view_gateway.read().await.clone().ok_or_else(|| {
        if ctx.config.view_gateway_address.is_some() {
            GatewayError::BackendUnavailable("view_gateway not connected".into())
        } else {
            GatewayError::NotImplemented("view".into())
        }
    })?;

    let query = view_query_from_request(request, gas_limit)?;
    let timeout = Duration::from_millis(ctx.config.view_timeout_ms);
    let result = tokio::time::timeout(timeout, client.viewgateway_view(&query))
        .await
        .map_err(|_| GatewayError::BackendTimeout)?
        .map_err(|source| GatewayError::BackendUnavailable(format!("view_gateway: {source}")))?;

    view_response_from_gateway_result(result)
}

fn view_query_from_request(
    request: ViewRequest,
    gas_limit: u64,
) -> Result<TransactionViewQuery, GatewayError> {
    let entrypoint: [u8; 8] = request.entrypoint.0.as_slice().try_into().map_err(|_| {
        GatewayError::InvalidParam("view entrypoint must be exactly 8 bytes".into())
    })?;

    Ok(TransactionViewQuery {
        invocation: Invocation {
            invoked_address: request.contract.0,
            entrypoint: ContractEntrypointID::from(entrypoint),
            gas_limit,
            calldata: BytesM(request.calldata.0),
        },
        parent_address: request.caller.map(|caller| caller.0),
        auth_claim: public_view_auth_claim(),
        forwarded_tokens: VecM(vec![]),
        invoke_flags: InvokeFlag::ReadOnly.0,
    })
}

fn public_view_auth_claim() -> AuthClaim {
    AuthClaim::kSourceAddressAuthData {
        source_address_claim: AuthClaim_source_address_claim {
            allowed_keys: 0,
            required_weight: 0,
        },
    }
}

fn view_response_from_gateway_result(
    result: ViewGatewayResult,
) -> Result<ViewResponse, GatewayError> {
    match result {
        ViewGatewayResult::ViewGatewayOk { payload } => Ok(ViewResponse {
            output: root_output_from_invocation_output(payload.output)?,
            events: payload
                .events
                .iter()
                .filter_map(parse_emitted_event)
                .collect(),
            height: payload.height,
        }),
        ViewGatewayResult::ViewGatewayAbandoned => Err(GatewayError::BackendUnavailable(
            "view_gateway abandoned the view after retrying; retry later".into(),
        )),
        ViewGatewayResult::ViewGatewayInternalError => Err(GatewayError::BackendUnavailable(
            "view_gateway reported an internal error".into(),
        )),
        ViewGatewayResult::Default { outcome } => {
            tracing::warn!(outcome, "unexpected ViewGatewayResult outcome");
            Err(GatewayError::BackendUnavailable(format!(
                "view_gateway returned unexpected outcome: {outcome}"
            )))
        }
        other => {
            tracing::warn!(result = ?other, "unknown ViewGatewayResult variant");
            Err(GatewayError::BackendUnavailable(
                "view_gateway returned an unsupported result variant; regenerate the \
                 ViewGateway client for this protocol"
                    .into(),
            ))
        }
    }
}

fn root_output_from_invocation_output(
    output: InvocationFrameOutput,
) -> Result<RootInvocationOutput, GatewayError> {
    match output {
        InvocationFrameOutput::Return { return_buffer } => Ok(RootInvocationOutput::Return {
            return_data: HexBytes(return_buffer.0),
        }),
        InvocationFrameOutput::Abort { abort_buffer } => Ok(RootInvocationOutput::Abort {
            abort_data: HexBytes(abort_buffer.0),
        }),
        InvocationFrameOutput::Fault => Ok(RootInvocationOutput::Fault),
        _ => Err(GatewayError::BackendUnavailable(
            "view_gateway returned an unsupported output variant; regenerate the ViewGateway \
             client for this protocol"
                .into(),
        )),
    }
}
