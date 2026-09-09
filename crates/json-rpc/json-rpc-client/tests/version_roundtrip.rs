use groundhog_json_rpc_api::GroundhogApiServer;
use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_server::{
    BuildInfo, ChainPlane, GroundhogRpcServer, ReadPlane, RpcRequestContext, RpcServerConfig,
    RpcServerError, SubmitPlane, TipReading, ValidatedTransaction, ViewPlane,
};
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress,
    block::BlockInfo,
    events::{EventPage, EventQuery},
    network_info::NetworkInfo,
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash},
};
use jsonrpsee::server::Server;

#[derive(Clone, Copy)]
struct VersionOnlyBackend;

#[async_trait::async_trait]
impl SubmitPlane for VersionOnlyBackend {
    async fn submit_raw_transaction(
        &self,
        _context: RpcRequestContext,
        _tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        Err(RpcServerError::NotImplemented("sendRawTransaction".into()))
    }
}

#[async_trait::async_trait]
impl ChainPlane for VersionOnlyBackend {
    async fn chain_tip(&self, _context: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        Err(RpcServerError::NotImplemented("chainTip".into()))
    }

    async fn get_block_by_number(
        &self,
        _context: RpcRequestContext,
        _height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBlockByNumber".into()))
    }

    async fn pending_transaction_count(
        &self,
        _context: RpcRequestContext,
    ) -> Result<u64, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "pendingTransactionCount".into(),
        ))
    }
}

#[async_trait::async_trait]
impl ReadPlane for VersionOnlyBackend {
    async fn transaction_receipt(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionReceipt".into(),
        ))
    }

    async fn transaction_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionStatus".into(),
        ))
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
}

#[async_trait::async_trait]
impl ViewPlane for VersionOnlyBackend {}

#[tokio::test]
async fn version_roundtrips_with_camel_case_json_shape() {
    let server = Server::builder()
        .build("127.0.0.1:0")
        .await
        .expect("bug: test server should bind");
    let addr = server
        .local_addr()
        .expect("bug: test server local address should be available");
    let rpc = GroundhogRpcServer::unified(
        VersionOnlyBackend,
        NetworkInfo::new(groundhog_json_rpc_types::NetworkHash::from(
            groundhog_xdr::types::SHA256Hash::from([0x07; 32]),
        )),
        RpcServerConfig::default(),
        BuildInfo::for_tests(),
    );
    let handle = server.start(rpc.into_rpc());

    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .expect("bug: test client should build");
    let version = client
        .version()
        .await
        .expect("bug: version call should succeed");
    let network_info = client
        .network_info()
        .await
        .expect("bug: networkInfo call should succeed");
    client
        .chain_tip()
        .await
        .expect_err("bug: backend-dependent calls should remain unavailable in this fixture");

    assert_eq!(
        version.wire_format,
        groundhog_json_rpc_api::WIRE_FORMAT_VERSION
    );
    assert_eq!(version.implementation, "test");
    assert_eq!(version.platform_version, None);
    assert_eq!(version.release_version, None);
    assert_eq!(version.bundle_manifest_digest, None);
    assert_eq!(version.commit_hash.as_deref(), Some("test-commit"));
    assert_eq!(network_info.network_hash.as_ref(), &[0x07; 32]);

    let json = serde_json::to_value(&version).expect("bug: version info should serialize");
    assert_eq!(
        json["wireFormat"],
        groundhog_json_rpc_api::WIRE_FORMAT_VERSION
    );
    assert_eq!(json["implementation"], "test");
    assert_eq!(json["platformVersion"], serde_json::Value::Null);
    assert_eq!(json["releaseVersion"], serde_json::Value::Null);
    assert_eq!(json["bundleManifestDigest"], serde_json::Value::Null);
    assert_eq!(json["commitHash"], "test-commit");
    assert_eq!(json["wire_format"], serde_json::Value::Null);
    assert_eq!(json["commit_hash"], serde_json::Value::Null);

    handle.stop().expect("bug: test server should stop");
    handle.stopped().await;
}
