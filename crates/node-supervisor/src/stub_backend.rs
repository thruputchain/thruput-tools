//! Stub backend planes that answer `NotImplemented`.
//!
//! The supervisor serves submit, chain, and read methods through real
//! hogd-dev adapters. This stub stands in only for the read plane when
//! its backing services are not connected, so those methods
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
use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};
use thruput_json_rpc_types::{
    ContractAddress,
    transaction::{TransactionStatus, TxHash},
};
use thruput_json_rpc_types::{requests as input, responses as out};

use crate::airdrop::FundingStatusPlane;

/// Backend that returns `NotImplemented` for unwired planes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct StubBackend;

impl StubBackend {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl FundingStatusPlane for StubBackend {
    async fn funding_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionStatus".into()))
    }
}

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
    async fn native_contract_deployment(
        &self,
        _: RpcRequestContext,
        _: ContractAddress,
    ) -> Result<out::ContractDeploymentResult, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getContractDeployment".into(),
        ))
    }
    async fn native_contract_artifact(
        &self,
        _: RpcRequestContext,
        _: thruput_json_rpc_types::SHA256Hash,
    ) -> Result<out::ContractArtifactResult, RpcServerError> {
        Err(RpcServerError::NotImplemented("getContractArtifact".into()))
    }
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
