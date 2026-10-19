use super::{
    HogdDevViewBackend, RpcRequestContext, RpcServerError, VIEW_GATEWAY, async_trait, shared_view,
    within_request_deadline,
};
use thruput_hogd_backend::native as decode;
use thruput_json_rpc_server::{ViewBatchConfig, native::read::NativeSimulationPlane};
use thruput_json_rpc_types::{requests as input, responses as out};

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
        } = self
        else {
            return Err(RpcServerError::NotImplemented("simulateInvocation".into()));
        };
        let min_height = request.min_state_block_height.clone();
        let query = shared_view::view_query(decode::simulation_request(request)?, gas_limit)?;
        let result = within_request_deadline(context.capped(*request_timeout), async {
            client.viewgateway_view(&query).await.map_err(|source| {
                RpcServerError::BackendUnavailable(format!("{VIEW_GATEWAY}: {source}"))
            })
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
        } = self
        else {
            return Err(RpcServerError::NotImplemented("simulateInvocations".into()));
        };
        let context = context.capped(*request_timeout);
        let query = shared_view::view_batch_query(
            decode::simulations_request(request)?,
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
        decode::simulations(&query, result)
    }
}
