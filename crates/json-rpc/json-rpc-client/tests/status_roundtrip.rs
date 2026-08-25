use groundhog_json_rpc_api::GroundhogApiServer;
use groundhog_json_rpc_client::{
    GroundhogApiClient, HttpClientBuilder, PollTransactionReceiptError,
    poll_transaction_receipt_with_config,
};
use groundhog_json_rpc_server::{
    BuildInfo, ChainPlane, GroundhogRpcServer, ReadPlane, RpcRequestContext, RpcServerConfig,
    RpcServerError, SubmitPlane, ValidatedTransaction, ViewPlane,
};
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress, NetworkHash,
    block::{BlockInfo, ChainTip},
    events::{EventPage, EventQuery},
    hex::{Hex0x, HexBytes},
    network_info::NetworkInfo,
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{
        AddressTransactionQuery, ConflictCode, ConflictDiagnostic, TransactionRefPage,
        TransactionStatus, TxHash,
    },
};
use jsonrpsee::server::Server;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const ATTEMPT_HEIGHT: i64 = 42;
const CONFLICT_ADDRESS: [u8; 32] = [0x0D; 32];
const CONFLICT_KEY: &[u8] = &[0, 0, 0, 1];

fn test_network_info() -> NetworkInfo {
    NetworkInfo::new(NetworkHash::from(groundhog_xdr::types::SHA256Hash::from(
        [0x07; 32],
    )))
}

#[derive(Clone)]
struct RecentFailureBackend {
    receipt_calls: Arc<AtomicUsize>,
    commit_on_second_receipt: bool,
}

impl RecentFailureBackend {
    fn always_failing() -> Self {
        Self {
            receipt_calls: Arc::new(AtomicUsize::new(0)),
            commit_on_second_receipt: false,
        }
    }

    fn eventually_committed() -> Self {
        Self {
            receipt_calls: Arc::new(AtomicUsize::new(0)),
            commit_on_second_receipt: true,
        }
    }
}

fn canned_recent_failure() -> TransactionStatus {
    TransactionStatus::RecentFailure {
        attempt_height: BlockHeight::from(ATTEMPT_HEIGHT),
        conflict: ConflictDiagnostic {
            address: Hex0x(groundhog_xdr::types::ContractAddress::from(
                CONFLICT_ADDRESS,
            )),
            key: HexBytes(CONFLICT_KEY.to_vec()),
            code: ConflictCode::ExclusiveLock,
        },
    }
}

#[async_trait::async_trait]
impl SubmitPlane for RecentFailureBackend {
    async fn submit_raw_transaction(
        &self,
        _context: RpcRequestContext,
        _tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        Err(RpcServerError::NotImplemented("sendRawTransaction".into()))
    }
}

#[async_trait::async_trait]
impl ChainPlane for RecentFailureBackend {
    async fn chain_tip(&self, _context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
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
impl ReadPlane for RecentFailureBackend {
    async fn transaction_receipt(
        &self,
        _context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        let receipt_call = self.receipt_calls.fetch_add(1, Ordering::Relaxed);
        if self.commit_on_second_receipt && receipt_call > 0 {
            return Ok(Some(TransactionReceipt {
                tx_hash,
                height: BlockHeight::from(ATTEMPT_HEIGHT + 1),
                status: groundhog_json_rpc_types::receipt::ExecStatus::Ok,
                events: Vec::new(),
                return_data: None,
                abort_data: None,
            }));
        }
        Ok(None)
    }

    async fn transaction_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Ok(canned_recent_failure())
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

#[tokio::test]
async fn receipt_polling_surfaces_recent_failure_status() {
    let server = Server::builder()
        .build("127.0.0.1:0")
        .await
        .expect("bug: test server should bind");
    let addr = server
        .local_addr()
        .expect("bug: test server local address should be available");
    let rpc = GroundhogRpcServer::unified(
        RecentFailureBackend::always_failing(),
        test_network_info(),
        RpcServerConfig::default(),
        BuildInfo::for_tests(),
    );
    let handle = server.start(rpc.into_rpc());
    let tx_hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x33; 32])));

    let error = poll_transaction_receipt_with_config(
        &format!("http://{addr}"),
        tx_hash,
        3,
        std::time::Duration::from_millis(1),
    )
    .await
    .expect_err("recent failure must be returned after receipt polling is exhausted");

    match error {
        PollTransactionReceiptError::TransactionStatus {
            tx_hash: actual_hash,
            status:
                TransactionStatus::RecentFailure {
                    attempt_height,
                    conflict,
                },
        } => {
            assert_eq!(actual_hash, tx_hash);
            assert_eq!(*attempt_height, ATTEMPT_HEIGHT);
            assert_eq!(conflict.code, ConflictCode::ExclusiveLock);
        }
        other => panic!("expected recent failure polling error, got {other:?}"),
    }

    handle.stop().expect("bug: test server should stop");
    handle.stopped().await;
}

#[tokio::test]
async fn receipt_polling_prefers_a_later_receipt_to_recent_failure() {
    let server = Server::builder()
        .build("127.0.0.1:0")
        .await
        .expect("bug: test server should bind");
    let addr = server
        .local_addr()
        .expect("bug: test server local address should be available");
    let rpc = GroundhogRpcServer::unified(
        RecentFailureBackend::eventually_committed(),
        test_network_info(),
        RpcServerConfig::default(),
        BuildInfo::for_tests(),
    );
    let handle = server.start(rpc.into_rpc());
    let tx_hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x44; 32])));

    let receipt = poll_transaction_receipt_with_config(
        &format!("http://{addr}"),
        tx_hash,
        3,
        std::time::Duration::from_millis(1),
    )
    .await
    .expect("a receipt that appears after an advisory failure must win");

    assert_eq!(receipt.tx_hash, tx_hash);
    assert_eq!(*receipt.height, ATTEMPT_HEIGHT + 1);

    handle.stop().expect("bug: test server should stop");
    handle.stopped().await;
}

#[async_trait::async_trait]
impl ViewPlane for RecentFailureBackend {}

#[tokio::test]
async fn recent_failure_status_roundtrips_through_client() {
    let server = Server::builder()
        .build("127.0.0.1:0")
        .await
        .expect("bug: test server should bind");
    let addr = server
        .local_addr()
        .expect("bug: test server local address should be available");
    let rpc = GroundhogRpcServer::unified(
        RecentFailureBackend::always_failing(),
        test_network_info(),
        RpcServerConfig::default(),
        BuildInfo::for_tests(),
    );
    let handle = server.start(rpc.into_rpc());

    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .expect("bug: test client should build");
    let tx_hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x22; 32])));
    let status = client
        .get_transaction_status(tx_hash)
        .await
        .expect("bug: status call should succeed");

    match status {
        TransactionStatus::RecentFailure {
            attempt_height,
            conflict,
        } => {
            assert_eq!(*attempt_height, ATTEMPT_HEIGHT);
            assert_eq!(
                conflict.address.0,
                groundhog_xdr::types::ContractAddress::from(CONFLICT_ADDRESS)
            );
            assert_eq!(conflict.key.0, CONFLICT_KEY);
            assert_eq!(conflict.code, ConflictCode::ExclusiveLock);
        }
        other => panic!("expected RecentFailure, got {other:?}"),
    }

    handle.stop().expect("bug: test server should stop");
    handle.stopped().await;
}
