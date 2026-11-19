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
        let stated = Self::view_timepoint(clock);
        let query = shared_view::view_query(request.clone(), gas_limit, stated.clone())?;
        let result = within_request_deadline(context.capped(*request_timeout), async {
            let view = |query| async move {
                client.viewgateway_view(&query).await.map_err(|source| {
                    RpcServerError::BackendUnavailable(format!("{VIEW_GATEWAY}: {source}"))
                })
            };
            let result = view(query).await?;
            if matches!(result, ViewGatewayResult::ViewGatewayInvalidInput { .. })
                && let Some(retry) = Self::retry_timepoint(clock, scheduler.as_ref(), &stated).await
            {
                return view(shared_view::view_query(request, gas_limit, retry)?).await;
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
        let stated = Self::view_timepoint(clock);
        let (query, result) = within_request_deadline(context, async {
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
            if matches!(
                result,
                ViewGatewayBatchResult::Default { outcome }
                    if outcome == ViewGatewayBatchOutcome::ViewGatewayBatchInvalidRequest
            ) && let Some(retry) =
                Self::retry_timepoint(clock, scheduler.as_ref(), &stated).await
            {
                return view(batch(&retry)?).await;
            }
            Ok((query, result))
        })
        .await?;
        decode::simulations(&query, result)
    }
}
