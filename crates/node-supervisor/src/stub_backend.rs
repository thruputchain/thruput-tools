//! Stub backend planes that answer `NotImplemented`.
//!
//! The supervisor serves submit, chain, and read methods through real
//! hogd-dev adapters. This stub stands in only for the read and view
//! planes when their backing services are not connected, so those methods
//! fail with a stable wire error instead of a transport error.
//!
//! Wire-error strings carry only the JSON-RPC method name, so the public
//! protocol surface is stable. The "not connected" context goes to
//! `tracing::warn!` at supervisor startup, not into the wire error.

use async_trait::async_trait;
use thruput_json_rpc_server::native::{
    read::{NativeBlockPlane, NativeHistoryPlane, NativeStatePlane, StateKey},
    transaction::{NativeReadError, NativeTransactionReadPlane},
};
use thruput_json_rpc_server::{ReadPlane, RpcRequestContext, RpcServerError, ViewPlane};
use thruput_json_rpc_types::{
    ContractAddress,
    address_auth::AddressAuthDataResponse,
    events::{EventPage, EventQuery},
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash},
};
use thruput_json_rpc_types::{requests as input, responses as out};

/// Backend that returns `NotImplemented` for unwired planes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct StubBackend;

impl StubBackend {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ReadPlane for StubBackend {
    async fn transaction_receipt(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionReceipt".into()))
    }

    async fn transaction_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionStatus".into()))
    }

    async fn contract_events(
        &self,
        _context: RpcRequestContext,
        _query: EventQuery,
    ) -> Result<EventPage, RpcServerError> {
        Err(RpcServerError::NotImplemented("getEvents".into()))
    }

    async fn transactions_by_address(
        &self,
        _context: RpcRequestContext,
        _query: AddressTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionsByAddress".into(),
        ))
    }

    async fn get_storage_at(
        &self,
        _context: RpcRequestContext,
        _address: ContractAddress,
        _key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        Err(RpcServerError::NotImplemented("getStorageAt".into()))
    }

    async fn read_native_hog_balance(
        &self,
        _context: RpcRequestContext,
        _address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBalance".into()))
    }

    async fn read_address_auth_data(
        &self,
        _context: RpcRequestContext,
        _address: ContractAddress,
    ) -> Result<AddressAuthDataResponse, RpcServerError> {
        Err(RpcServerError::NotImplemented("getAddressAuthData".into()))
    }
}

#[async_trait]
impl ViewPlane for StubBackend {}

#[async_trait]
impl NativeBlockPlane for StubBackend {
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
impl NativeHistoryPlane for StubBackend {
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

#[async_trait]
impl NativeStatePlane for StubBackend {
    #[tracing::instrument(skip_all)]
    async fn native_storage(
        &self,
        _context: RpcRequestContext,
        _address: ContractAddress,
        _key: StateKey,
    ) -> Result<out::StorageResult, RpcServerError> {
        Err(RpcServerError::NotImplemented("getStorage".into()))
    }
    #[tracing::instrument(skip_all)]
    async fn native_registered_auth(
        &self,
        _context: RpcRequestContext,
        _address: ContractAddress,
    ) -> Result<out::RegisteredAuthResult, RpcServerError> {
        Err(RpcServerError::NotImplemented("getRegisteredAuth".into()))
    }
}

#[async_trait]
impl NativeTransactionReadPlane for StubBackend {
    #[tracing::instrument(skip_all)]
    async fn native_receipt(
        &self,
        _context: RpcRequestContext,
        _hash: TxHash,
    ) -> Result<Option<out::TransactionReceipt>, NativeReadError> {
        Err(RpcServerError::NotImplemented("getTransactionReceipt".into()).into())
    }
    #[tracing::instrument(skip_all)]
    async fn native_status(
        &self,
        _context: RpcRequestContext,
        _hash: TxHash,
    ) -> Result<out::TransactionStatus, NativeReadError> {
        Err(RpcServerError::NotImplemented("getTransactionStatus".into()).into())
    }
}
