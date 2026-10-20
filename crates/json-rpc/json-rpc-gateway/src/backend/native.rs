use super::{
    ContractAddress, GatewayBackend, RpcRequestContext, RpcServerError, async_trait, handlers,
};
use thruput_json_rpc_server::{
    ViewBatchConfig,
    native::read::{NativeSimulationPlane, NativeStatePlane, StateKey},
};
use thruput_json_rpc_types::{requests as input, responses as out};

#[async_trait]
impl NativeStatePlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn native_storage(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: StateKey,
    ) -> Result<out::StorageResult, RpcServerError> {
        let context = self.bounded_context(context);
        self.with_deadline(
            context,
            self.ctx
                .storage_batcher
                .native_storage(context, address, key),
        )
        .await
    }
    #[tracing::instrument(skip_all)]
    async fn native_registered_auth(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::RegisteredAuthResult, RpcServerError> {
        let context = self.bounded_context(context);
        self.with_deadline(
            context,
            self.ctx
                .storage_batcher
                .native_registered_auth(context, address),
        )
        .await
    }
}

#[async_trait]
impl NativeSimulationPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn simulate(
        &self,
        context: RpcRequestContext,
        request: input::SimulateInvocationRequest,
        gas_limit: u64,
    ) -> Result<out::SimulationResult, RpcServerError> {
        let context = self.bounded_context(context);
        self.with_deadline(
            context,
            handlers::view::native_simulate(&self.ctx, context, request, gas_limit),
        )
        .await
    }
    #[tracing::instrument(skip_all)]
    async fn simulate_batch(
        &self,
        context: RpcRequestContext,
        request: input::SimulateInvocationsRequest,
        gas_limit: u64,
        limits: ViewBatchConfig,
    ) -> Result<out::SimulationResults, RpcServerError> {
        let context = self.bounded_context(context);
        self.with_deadline(
            context,
            handlers::view::native_simulate_batch(&self.ctx, context, request, gas_limit, limits),
        )
        .await
    }
}
