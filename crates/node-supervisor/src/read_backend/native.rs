use super::{
    ContractAddress, DbGateway_DbGateway1_Client, HogdDevReadBackend, HogdDevStorageReadBackend,
    RpcRequestContext, RpcServerError, StorageKey, async_trait, get_payload_from_db_gateway,
    shared_db_gateway, within_request_deadline,
};
use crate::service::SCHEDULER_GATEWAY;
use thruput_hogd_backend::native as decode;
use thruput_hogd_backend::{native::transaction as native_transaction, receipt::tx_hash_to_wire};
use thruput_json_rpc_server::native::read::{NativeBlockPlane, NativeHistoryPlane};
use thruput_json_rpc_server::native::read::{NativeStatePlane, StateKey};
use thruput_json_rpc_server::native::transaction::{
    NativeReadError, NativeTransactionReadPlane, within_native_read_deadline,
};
use thruput_json_rpc_types::requests as input;
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

async fn deployment(
    client: &DbGateway_DbGateway1_Client,
    context: RpcRequestContext,
    address: ContractAddress,
) -> Result<out::ContractDeploymentResult, RpcServerError> {
    within_request_deadline(context, async {
        let (height, item) = decode::single_item(
            get_payload_from_db_gateway(client, address, StorageKey::Metadata).await?,
        )?;
        decode::contract_deployment(address, height, item)
    })
    .await
}

async fn artifact(
    client: &DbGateway_DbGateway1_Client,
    context: RpcRequestContext,
    hash: thruput_json_rpc_types::SHA256Hash,
) -> Result<out::ContractArtifactResult, RpcServerError> {
    within_request_deadline(context, async {
        let result = client
            .dbgateway_get_contract(&hash.0)
            .await
            .map_err(|error| RpcServerError::BackendUnavailable(format!("DbGateway: {error}")))?;
        decode::contract_artifact(hash, result)
    })
    .await
}
#[async_trait]
impl NativeStatePlane for HogdDevReadBackend {
    async fn native_contract_deployment(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::ContractDeploymentResult, RpcServerError> {
        let client = self.db_gateway.as_ref().ok_or_else(|| {
            RpcServerError::BackendUnavailable("DbGateway is not configured".into())
        })?;
        deployment(client, context, address).await
    }
    async fn native_contract_artifact(
        &self,
        context: RpcRequestContext,
        hash: thruput_json_rpc_types::SHA256Hash,
    ) -> Result<out::ContractArtifactResult, RpcServerError> {
        let client = self.db_gateway.as_ref().ok_or_else(|| {
            RpcServerError::BackendUnavailable("DbGateway is not configured".into())
        })?;
        artifact(client, context, hash).await
    }
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
    async fn native_contract_deployment(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::ContractDeploymentResult, RpcServerError> {
        deployment(&self.db_gateway, context, address).await
    }
    async fn native_contract_artifact(
        &self,
        context: RpcRequestContext,
        hash: thruput_json_rpc_types::SHA256Hash,
    ) -> Result<out::ContractArtifactResult, RpcServerError> {
        artifact(&self.db_gateway, context, hash).await
    }
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
    async fn native_input(
        &self,
        context: RpcRequestContext,
        hash: TxHash,
    ) -> Result<out::TransactionInputResult, NativeReadError> {
        within_native_read_deadline(context, async {
            native_transaction::input_lookup(hash, self.transaction_info(hash).await?)
        })
        .await
    }
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
    async fn native_input(
        &self,
        _: RpcRequestContext,
        _: TxHash,
    ) -> Result<out::TransactionInputResult, NativeReadError> {
        Err(RpcServerError::NotImplemented("getTransactionInput".into()).into())
    }
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

#[async_trait]
impl thruput_json_rpc_server::native::read::NativeBlockPlane for HogdDevReadBackend {
    #[tracing::instrument(skip_all)]
    async fn native_block(
        &self,
        context: RpcRequestContext,
        height: thruput_json_rpc_types::BlockHeight,
    ) -> Result<out::BlockResult, RpcServerError> {
        within_request_deadline(context, async {
            let result = self
                .indexer
                .get_block_by_height(&height)
                .await
                .map_err(|error| {
                    RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {error}"))
                })?;
            decode::block_lookup(&height, result)
        })
        .await
    }
}

#[async_trait]
impl thruput_json_rpc_server::native::read::NativeHistoryPlane for HogdDevReadBackend {
    #[tracing::instrument(skip_all, fields(filters_len = request.filters.len(), limit = ?request.limit, has_cursor = request.cursor.is_some()))]
    async fn native_events(
        &self,
        context: RpcRequestContext,
        request: thruput_json_rpc_types::requests::EventQuery,
    ) -> Result<out::EventPage, RpcServerError> {
        within_request_deadline(context, async {
            let query = decode::history::event_query(&request)?;
            let result = self.indexer.get_events(&query).await.map_err(|error| {
                RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {error}"))
            })?;
            decode::history::event_page(result)
        })
        .await
    }
    #[tracing::instrument(skip_all, fields(address = ?request.address, limit = ?request.limit, has_cursor = request.cursor.is_some()))]
    async fn native_root_transactions(
        &self,
        context: RpcRequestContext,
        request: thruput_json_rpc_types::requests::RootAddressQuery,
    ) -> Result<out::TransactionRefPage, RpcServerError> {
        within_request_deadline(context, async {
            let query = decode::history::root_query(&request)?;
            let result = self
                .indexer
                .get_transactions_by_address(&query)
                .await
                .map_err(|error| {
                    RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {error}"))
                })?;
            decode::history::transaction_page(result)
        })
        .await
    }
    #[tracing::instrument(skip_all, fields(block_height = ?request.block_height, limit = ?request.limit, has_cursor = request.cursor.is_some()))]
    async fn native_block_transactions(
        &self,
        context: RpcRequestContext,
        request: thruput_json_rpc_types::requests::BlockPageRequest,
    ) -> Result<out::BlockTransactionPage, RpcServerError> {
        within_request_deadline(context, async {
            let query = decode::history::block_query(&request);
            let result = self
                .indexer
                .get_transactions_by_block(&query)
                .await
                .map_err(|error| {
                    RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {error}"))
                })?;
            decode::history::transaction_page(result)
        })
        .await
    }
}

#[async_trait]
impl NativeBlockPlane for HogdDevStorageReadBackend {
    #[tracing::instrument(skip_all)]
    async fn native_block(
        &self,
        _context: RpcRequestContext,
        _height: thruput_json_rpc_types::BlockHeight,
    ) -> Result<out::BlockResult, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBlockByHeight".into()))
    }
}

#[async_trait]
impl NativeHistoryPlane for HogdDevStorageReadBackend {
    #[tracing::instrument(skip_all)]
    async fn native_events(
        &self,
        _context: RpcRequestContext,
        _request: input::EventQuery,
    ) -> Result<out::EventPage, RpcServerError> {
        Err(RpcServerError::NotImplemented("getEvents".into()))
    }
    #[tracing::instrument(skip_all)]
    async fn native_root_transactions(
        &self,
        _context: RpcRequestContext,
        _request: input::RootAddressQuery,
    ) -> Result<out::TransactionRefPage, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionsByRootAddress".into(),
        ))
    }
    #[tracing::instrument(skip_all)]
    async fn native_block_transactions(
        &self,
        _context: RpcRequestContext,
        _request: input::BlockPageRequest,
    ) -> Result<out::BlockTransactionPage, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionsByBlock".into(),
        ))
    }
}
