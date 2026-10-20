use super::{
    ContractAddress, DbGateway_DbGateway1_Client, HogdDevReadBackend, HogdDevStorageReadBackend,
    RpcRequestContext, RpcServerError, StorageKey, async_trait, get_payload_from_db_gateway,
    shared_db_gateway, within_request_deadline,
};
use crate::service::SCHEDULER_GATEWAY;
use thruput_hogd_backend::native as decode;
use thruput_hogd_backend::{native::transaction as native_transaction, receipt::tx_hash_to_wire};
use thruput_json_rpc_server::native::read::{NativeStatePlane, StateKey};
use thruput_json_rpc_server::native::transaction::{
    NativeReadError, NativeTransactionReadPlane, within_native_read_deadline,
};
use thruput_json_rpc_types::{responses as out, transaction::TxHash};

async fn storage(
    client: &DbGateway_DbGateway1_Client,
    context: RpcRequestContext,
    address: ContractAddress,
    key: StateKey,
) -> Result<out::StorageResult, RpcServerError> {
    let key = match key {
        StateKey::ContractData(key) => shared_db_gateway::contract_data_key(key),
        StateKey::FreeNativeTokenBalance => StorageKey::FreeNativeTokenBalance,
    };
    within_request_deadline(context, async {
        let (height, item) =
            decode::single_item(get_payload_from_db_gateway(client, address, key).await?)?;
        decode::storage(height, item)
    })
    .await
}
async fn auth(
    client: &DbGateway_DbGateway1_Client,
    context: RpcRequestContext,
    address: ContractAddress,
) -> Result<out::RegisteredAuthResult, RpcServerError> {
    within_request_deadline(context, async {
        let (height, item) = decode::single_item(
            get_payload_from_db_gateway(client, address, StorageKey::AuthData).await?,
        )?;
        decode::registered_auth(address, height, item)
    })
    .await
}
#[async_trait]
impl NativeStatePlane for HogdDevReadBackend {
    #[tracing::instrument(skip_all)]
    async fn native_storage(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: StateKey,
    ) -> Result<out::StorageResult, RpcServerError> {
        let client = self.db_gateway.as_ref().ok_or_else(|| {
            RpcServerError::BackendUnavailable("DbGateway is not configured".into())
        })?;
        storage(client, context, address, key).await
    }
    #[tracing::instrument(skip_all)]
    async fn native_registered_auth(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::RegisteredAuthResult, RpcServerError> {
        let client = self.db_gateway.as_ref().ok_or_else(|| {
            RpcServerError::BackendUnavailable("DbGateway is not configured".into())
        })?;
        auth(client, context, address).await
    }
}
#[async_trait]
impl NativeStatePlane for HogdDevStorageReadBackend {
    #[tracing::instrument(skip_all)]
    async fn native_storage(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: StateKey,
    ) -> Result<out::StorageResult, RpcServerError> {
        storage(&self.db_gateway, context, address, key).await
    }
    #[tracing::instrument(skip_all)]
    async fn native_registered_auth(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::RegisteredAuthResult, RpcServerError> {
        auth(&self.db_gateway, context, address).await
    }
}

#[async_trait]
impl NativeTransactionReadPlane for HogdDevReadBackend {
    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn native_receipt(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<out::TransactionReceipt>, NativeReadError> {
        within_native_read_deadline(context, async {
            native_transaction::receipt_lookup(tx_hash, self.transaction_info(tx_hash).await?)
        })
        .await
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn native_status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<out::TransactionStatus, NativeReadError> {
        within_native_read_deadline(context, async {
            let indexed = match self.transaction_info(tx_hash).await {
                Ok(result) => native_transaction::indexed_status(tx_hash, result),
                Err(error) => Err(error.into()),
            };
            native_transaction::sequence_status(indexed, async {
                // An expired budget must not start a late fallback RPC.
                if context
                    .remaining()
                    .is_some_and(|remaining| remaining.is_zero())
                {
                    return Err(RpcServerError::BackendTimeout.into());
                }
                let hash = tx_hash_to_wire(tx_hash);
                let results = self
                    .scheduler_gateway
                    .schedgateway_lookup_transaction_outcomes(std::slice::from_ref(&hash))
                    .await
                    .map_err(|error| {
                        RpcServerError::BackendUnavailable(format!("{SCHEDULER_GATEWAY}: {error}"))
                    })?;
                native_transaction::scheduler_status(&results)
            })
            .await
        })
        .await
    }
}

#[async_trait]
impl NativeTransactionReadPlane for HogdDevStorageReadBackend {
    #[tracing::instrument(skip_all)]
    async fn native_receipt(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<Option<out::TransactionReceipt>, NativeReadError> {
        Err(RpcServerError::NotImplemented("getTransactionReceipt".into()).into())
    }

    #[tracing::instrument(skip_all)]
    async fn native_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<out::TransactionStatus, NativeReadError> {
        Err(RpcServerError::NotImplemented("getTransactionStatus".into()).into())
    }
}
