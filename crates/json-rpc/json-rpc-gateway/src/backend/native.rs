use super::{
    ContractAddress, GatewayBackend, RpcRequestContext, RpcServerError, async_trait, handlers,
};
use thruput_json_rpc_server::native::transaction::{
    NativeReadError, NativeTransactionReadPlane, within_native_read_deadline,
};
use thruput_json_rpc_server::{
    ViewBatchConfig,
    native::read::{NativeSimulationPlane, NativeStatePlane, StateKey},
};
use thruput_json_rpc_types::{requests as input, responses as out, transaction::TxHash};

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

#[async_trait]
impl NativeTransactionReadPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn native_receipt(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<out::TransactionReceipt>, NativeReadError> {
        let context = self.bounded_context(context);
        within_native_read_deadline(
            context,
            handlers::receipt::native_transaction_receipt(&self.ctx, context, tx_hash),
        )
        .await
    }

    #[tracing::instrument(skip_all)]
    async fn native_status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<out::TransactionStatus, NativeReadError> {
        let context = self.bounded_context(context);
        within_native_read_deadline(
            context,
            handlers::receipt::native_transaction_status(&self.ctx, context, tx_hash),
        )
        .await
    }
}

#[async_trait]
impl thruput_json_rpc_server::native::transaction::NativeSubmitPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn native_submit(
        &self,
        context: RpcRequestContext,
        tx: thruput_json_rpc_server::ValidatedTransaction,
        attempt: thruput_json_rpc_server::native::transaction::SubmissionAttempt,
    ) -> Result<(), RpcServerError> {
        let context = self.bounded_context(context);
        let _guard = attempt.guard();
        self.with_deadline(
            context,
            self.ctx
                .batch_submitter
                .submit_native(context, tx.bytes, tx.hash, attempt),
        )
        .await
    }
}
