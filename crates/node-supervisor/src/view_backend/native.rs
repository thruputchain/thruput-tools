use super::{
    HogdDevViewBackend, RpcRequestContext, RpcServerError, StaleView, VIEW_GATEWAY, async_trait,
    shared_view, within_request_deadline,
};
use thruput_hogd_backend::native as decode;
use thruput_json_rpc_server::{ViewBatchConfig, native::read::NativeSimulationPlane};
use thruput_json_rpc_types::{requests as input, responses as out};
use thruput_view::{
    ViewGateway_ViewGateway1_Client, ViewGatewayBatchOutcome, ViewGatewayBatchQuery,
    ViewGatewayBatchResult, ViewGatewayResult,
};
use thruput_xdr::view::TransactionViewQuery;

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
            let mut query = shared_view::view_query(request, gas_limit, stated.clone())?;
            let result = view(client, &query).await?;
            let refused = matches!(result, ViewGatewayResult::ViewGatewayInvalidInput { .. });
            let Some(stale) = stale_view(refused, snapshot_height(&result), parent_height) else {
                return Ok(result);
            };
            let Some(rerun) = Self::rerun_time(clock, scheduler.as_ref(), &stated, stale).await?
            else {
                return Ok(result);
            };
            query.block_timepoint = rerun.timepoint;
            let result = view(client, &query).await?;
            check_rerun(snapshot_height(&result), rerun.parent_height)?;
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
        let remaining = || context.remaining().ok_or(RpcServerError::BackendTimeout);
        let (query, result) = within_request_deadline(context, async {
            let (stated, parent_height) = Self::view_time(clock, scheduler.as_ref()).await;
            let mut query =
                shared_view::view_batch_query(request, gas_limit, remaining()?, limits, &stated)?;
            let result = view_batch(client, &query).await?;
            let refused = matches!(
                result,
                ViewGatewayBatchResult::Default { outcome }
                    if outcome == ViewGatewayBatchOutcome::ViewGatewayBatchInvalidRequest
            );
            let Some(stale) = stale_view(refused, batch_snapshot_height(&result), parent_height)
            else {
                return Ok((query, result));
            };
            let Some(rerun) = Self::rerun_time(clock, scheduler.as_ref(), &stated, stale).await?
            else {
                return Ok((query, result));
            };
            shared_view::restate_view_batch(&mut query, &rerun.timepoint, remaining()?)?;
            let result = view_batch(client, &query).await?;
            check_rerun(batch_snapshot_height(&result), rerun.parent_height)?;
            Ok((query, result))
        })
        .await?;
        decode::simulations(&query, result)
    }
}

async fn view(
    client: &ViewGateway_ViewGateway1_Client,
    query: &TransactionViewQuery,
) -> Result<ViewGatewayResult, RpcServerError> {
    client
        .viewgateway_view(query)
        .await
        .map_err(|source| RpcServerError::BackendUnavailable(format!("{VIEW_GATEWAY}: {source}")))
}

async fn view_batch(
    client: &ViewGateway_ViewGateway1_Client,
    query: &ViewGatewayBatchQuery,
) -> Result<ViewGatewayBatchResult, RpcServerError> {
    client
        .viewgateway_view_batch(query)
        .await
        .map_err(|source| RpcServerError::BackendUnavailable(format!("{VIEW_GATEWAY}: {source}")))
}

fn snapshot_height(result: &ViewGatewayResult) -> Option<i64> {
    match result {
        ViewGatewayResult::ViewGatewayOk { payload } => Some(*payload.height),
        _ => None,
    }
}

fn batch_snapshot_height(result: &ViewGatewayBatchResult) -> Option<i64> {
    match result {
        ViewGatewayBatchResult::ViewGatewayBatchOk { payload } => Some(*payload.height),
        _ => None,
    }
}

/// Whether a block committed after the view's time was chosen may have made
/// that time stale: hogd refused it, or ran the view on a snapshot newer than
/// the tip the time came from.
fn stale_view(
    refused: bool,
    snapshot_height: Option<i64>,
    parent_height: Option<i64>,
) -> Option<StaleView> {
    if refused {
        return Some(StaleView::Refused);
    }
    let parent_height = parent_height?;
    snapshot_height
        .is_some_and(|height| height > parent_height)
        .then_some(StaleView::NewerSnapshot)
}

/// A view reruns once. If another block committed before hogd opened the
/// rerun's snapshot, its time is stale too, so fail instead of returning a
/// result at the wrong time.
fn check_rerun(snapshot_height: Option<i64>, parent_height: i64) -> Result<(), RpcServerError> {
    if snapshot_height.is_some_and(|height| height > parent_height) {
        return Err(RpcServerError::BackendUnavailable(format!(
            "{VIEW_GATEWAY}: blocks committed during the view and its retry; retry the view"
        )));
    }
    Ok(())
}
