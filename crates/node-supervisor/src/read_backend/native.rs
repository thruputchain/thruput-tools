use super::{
    ContractAddress, DbGateway_DbGateway1_Client, HogdDevReadBackend, HogdDevStorageReadBackend,
    RpcRequestContext, RpcServerError, StorageKey, async_trait, get_payload_from_db_gateway,
    shared_db_gateway, within_request_deadline,
};
use thruput_hogd_backend::native as decode;
use thruput_json_rpc_server::native::read::{NativeStatePlane, StateKey};
use thruput_json_rpc_types::responses as out;

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
