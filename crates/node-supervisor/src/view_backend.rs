//! hogd-dev ViewGateway-backed view plane adapter.

use std::{fmt, net::SocketAddr, time::Duration};

use async_trait::async_trait;
use thruput_hogd_backend::{intro, view as shared_view};
use thruput_json_rpc_server::{RpcRequestContext, RpcServerError, ViewPlane};
use thruput_json_rpc_types::protocol_token::protocol_from_text;
use thruput_json_rpc_types::view::{ViewRequest, ViewResponse};
use thruput_view::ViewGateway_ViewGateway1_Client;
use thruput_x23_transport::connect;

use crate::{
    NodeSupervisorError, request_deadline::within_request_deadline, service::VIEW_GATEWAY,
    stub_backend::StubBackend,
};

const HOGD_DEV_POOL_SIZE: usize = 2;

/// Local-node view plane, backed by hogd-dev's optional `ViewGateway`.
#[derive(Clone)]
pub(crate) enum HogdDevViewBackend {
    Gateway {
        client: ViewGateway_ViewGateway1_Client,
        request_timeout: Duration,
    },
    Stub(StubBackend),
}

impl fmt::Debug for HogdDevViewBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gateway { .. } => f.debug_tuple("HogdDevViewBackend::Gateway").finish(),
            Self::Stub(_) => f.debug_tuple("HogdDevViewBackend::Stub").finish(),
        }
    }
}

impl HogdDevViewBackend {
    pub(crate) async fn connect_optional(
        endpoint: Option<SocketAddr>,
        protocol_version: &str,
        expected_protocol: Option<&str>,
        request_timeout: Duration,
    ) -> Result<Self, NodeSupervisorError> {
        let Some(endpoint) = endpoint else {
            tracing::info!(
                "hogd-dev did not advertise a ViewGateway endpoint; view is unavailable"
            );
            return Ok(Self::Stub(StubBackend::new()));
        };

        tracing::debug!(%endpoint, "connecting to hogd-dev ViewGateway");
        let client = connect(
            "hogd-dev ViewGateway",
            &endpoint.to_string(),
            HOGD_DEV_POOL_SIZE,
            ViewGateway_ViewGateway1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::ViewGatewayConnect { endpoint, source })?;

        let protocol = protocol_from_text(protocol_version)?;
        let intro_outcome = client.viewgateway_intro(&protocol).await.map_err(|e| {
            NodeSupervisorError::ViewGatewayRpc {
                op: "viewgateway_intro",
                error: e.to_string(),
            }
        })?;
        let intro = intro::view_gateway(intro_outcome);
        if !intro.accepted() {
            return Err(NodeSupervisorError::intro_rejected(
                intro,
                protocol_version,
                expected_protocol,
            ));
        }

        Ok(Self::Gateway {
            client,
            request_timeout,
        })
    }

    pub(crate) const fn plane_status(&self) -> crate::server::PlaneStatus {
        match self {
            Self::Gateway { .. } => crate::server::PlaneStatus::ViewGateway,
            Self::Stub(_) => crate::server::PlaneStatus::Stub,
        }
    }
}

#[async_trait]
impl ViewPlane for HogdDevViewBackend {
    #[tracing::instrument(skip_all)]
    async fn view_batch(
        &self,
        context: RpcRequestContext,
        request: thruput_json_rpc_types::view::ViewBatchRequest,
        gas_limit: u64,
        limits: thruput_json_rpc_server::ViewBatchConfig,
    ) -> Result<thruput_json_rpc_types::view::ViewBatchResponse, RpcServerError> {
        let Self::Gateway {
            client,
            request_timeout,
        } = self
        else {
            return Err(RpcServerError::NotImplemented("viewBatch".into()));
        };
        let context = context.capped(*request_timeout);
        let query = shared_view::view_batch_query(
            request,
            gas_limit,
            context.remaining().ok_or(RpcServerError::BackendTimeout)?,
            limits,
        )?;
        let result = within_request_deadline(context, async {
            client
                .viewgateway_view_batch(&query)
                .await
                .map_err(|source| {
                    RpcServerError::BackendUnavailable(format!("{VIEW_GATEWAY}: {source}"))
                })
        })
        .await?;
        shared_view::view_batch_response(VIEW_GATEWAY, &query, result)
    }

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
    async fn view(
        &self,
        context: RpcRequestContext,
        request: ViewRequest,
        gas_limit: u64,
    ) -> Result<ViewResponse, RpcServerError> {
        let Self::Gateway {
            client,
            request_timeout,
        } = self
        else {
            return Err(RpcServerError::NotImplemented("view".into()));
        };

        let query = shared_view::view_query(request, gas_limit)?;
        let result = within_request_deadline(context.capped(*request_timeout), async {
            client.viewgateway_view(&query).await.map_err(|source| {
                RpcServerError::BackendUnavailable(format!("{VIEW_GATEWAY}: {source}"))
            })
        })
        .await?;

        let outcome = shared_view::view_outcome(VIEW_GATEWAY, result)?;
        if outcome.dropped_events > 0 {
            tracing::warn!(
                dropped = outcome.dropped_events,
                "dropped malformed events from hogd-dev ViewGateway",
            );
        }
        Ok(outcome.response)
    }
}

#[cfg(test)]
mod tests;

mod native;
