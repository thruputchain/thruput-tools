use super::{
    HogdDevViewBackend, RpcRequestContext, RpcServerError, VIEW_GATEWAY, async_trait, shared_view,
    within_request_deadline,
};
use thruput_hogd_backend::native as decode;
use thruput_json_rpc_server::{ViewBatchConfig, native::read::NativeSimulationPlane};
use thruput_json_rpc_types::{requests as input, responses as out};
use thruput_view::{
    ViewGatewayBatchOutcome, ViewGatewayBatchQuery, ViewGatewayBatchResult, ViewGatewayResult,
};
use thruput_xdr::types::TimePoint;

#[async_trait]
impl NativeSimulationPlane for HogdDevViewBackend {
    #[tracing::instrument(skip_all)]
    async fn simulate(
        &self,
        context: RpcRequestContext,
        request: input::SimulateInvocationRequest,
        gas_limit: u64,
    ) -> Result<out::SimulationResult, RpcServerError> {
        let Self::Gateway {
            client,
            request_timeout,
            clock,
            scheduler,
        } = self
        else {
            return Err(RpcServerError::NotImplemented("simulateInvocation".into()));
        };
        let min_height = request.min_state_block_height.clone();
        let request = decode::simulation_request(request)?;
        let result = within_request_deadline(context.capped(*request_timeout), async {
            let (stated, parent_height) = Self::view_time(clock, scheduler.as_ref()).await;
            let view = |timepoint| {
                let query = shared_view::view_query(request.clone(), gas_limit, timepoint);
                async move {
                    client.viewgateway_view(&query?).await.map_err(|source| {
                        RpcServerError::BackendUnavailable(format!("{VIEW_GATEWAY}: {source}"))
                    })
                }
            };
            let result = view(stated.clone()).await?;
            let stale = match &result {
                ViewGatewayResult::ViewGatewayInvalidInput { .. } => true,
                ViewGatewayResult::ViewGatewayOk { payload } => {
                    parent_height.is_some_and(|height| *payload.height != height)
                }
                _ => false,
            };
            if stale
                && let Some(fresh) = Self::fresh_view_time(clock, scheduler.as_ref()).await
                && fresh.timepoint != stated
            {
                return view(fresh.timepoint).await;
            }
            Ok(result)
        })
        .await?;
        decode::simulation(result, min_height)
    }
    #[tracing::instrument(skip_all)]
    async fn simulate_batch(
        &self,
        context: RpcRequestContext,
        request: input::SimulateInvocationsRequest,
        gas_limit: u64,
        limits: ViewBatchConfig,
    ) -> Result<out::SimulationResults, RpcServerError> {
        let Self::Gateway {
            client,
            request_timeout,
            clock,
            scheduler,
        } = self
        else {
            return Err(RpcServerError::NotImplemented("simulateInvocations".into()));
        };
        let context = context.capped(*request_timeout);
        let request = decode::simulations_request(request)?;
        let batch = |timepoint: &TimePoint| {
            shared_view::view_batch_query(
                request.clone(),
                gas_limit,
                context.remaining().ok_or(RpcServerError::BackendTimeout)?,
                limits,
                timepoint,
            )
        };
        let (query, result) = within_request_deadline(context, async {
            let (stated, parent_height) = Self::view_time(clock, scheduler.as_ref()).await;
            let view = |query: ViewGatewayBatchQuery| async move {
                let result = client
                    .viewgateway_view_batch(&query)
                    .await
                    .map_err(|source| {
                        RpcServerError::BackendUnavailable(format!("{VIEW_GATEWAY}: {source}"))
                    })?;
                Ok::<_, RpcServerError>((query, result))
            };
            let (query, result) = view(batch(&stated)?).await?;
            let stale = match &result {
                ViewGatewayBatchResult::Default { outcome } => {
                    *outcome == ViewGatewayBatchOutcome::ViewGatewayBatchInvalidRequest
                }
                ViewGatewayBatchResult::ViewGatewayBatchOk { payload } => {
                    parent_height.is_some_and(|height| *payload.height != height)
                }
                _ => false,
            };
            if stale
                && let Some(fresh) = Self::fresh_view_time(clock, scheduler.as_ref()).await
                && fresh.timepoint != stated
            {
                return view(batch(&fresh.timepoint)?).await;
            }
            Ok((query, result))
        })
        .await?;
        decode::simulations(&query, result)
    }
}
