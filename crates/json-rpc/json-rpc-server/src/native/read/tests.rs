use super::*;
use crate::{BuildVersion, ChainPlane, RpcServerConfig, TipReading};
use jsonrpsee::{
    core::client::ClientT, core::params::ObjectParams, http_client::HttpClientBuilder,
    server::ServerBuilder,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use thruput_json_rpc_types::{BlockHeight, NetworkHash, block::BlockInfo, hex::HexBytes};

#[derive(Clone, Default)]
struct Backend {
    calls: Arc<AtomicUsize>,
    unavailable: Arc<AtomicBool>,
    slow: bool,
    height: Option<i64>,
    balance_item: Option<out::StorageItem>,
    wrong_auth_address: bool,
}
impl Backend {
    async fn check(&self) -> Result<(), RpcServerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.slow {
            std::future::pending::<()>().await;
        }
        if self.unavailable.load(Ordering::SeqCst) {
            Err(RpcServerError::BackendUnavailable("offline".into()))
        } else {
            Ok(())
        }
    }
}
#[async_trait]
impl ChainPlane for Backend {
    async fn chain_tip(&self, _: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        self.check().await?;
        Ok(TipReading::new(
            thruput_json_rpc_types::block::ChainTip {
                height: 42.into(),
                block_hash: thruput_json_rpc_types::base58::Base58(
                    thruput_xdr::types::SHA256Hash::from([8; 32]),
                ),
            },
            thruput_xdr::network_config::NetworkConfig::_0 {
                v0: thruput_xdr::network_config::NetworkConfigV0 {
                    params: xdr_common::VecM(vec![]),
                },
            },
        ))
    }
    async fn get_block_by_number(
        &self,
        _: RpcRequestContext,
        _: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        panic!("out of slice")
    }
    async fn pending_transaction_count(&self, _: RpcRequestContext) -> Result<u64, RpcServerError> {
        panic!("out of slice")
    }
}
#[async_trait]
impl NativeStatePlane for Backend {
    async fn native_storage(
        &self,
        _: RpcRequestContext,
        _: ContractAddress,
        key: StateKey,
    ) -> Result<out::StorageResult, RpcServerError> {
        self.check().await?;
        Ok(out::StorageResult {
            state_block_height: self.height.unwrap_or(42).into(),
            item: match key {
                StateKey::ContractData(_) => Some(out::StorageItem::Bytestring {
                    bytes: HexBytes(vec![1, 2]),
                }),
                StateKey::FreeNativeTokenBalance => self.balance_item.clone(),
            },
        })
    }
    async fn native_registered_auth(
        &self,
        _: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::RegisteredAuthResult, RpcServerError> {
        self.check().await?;
        Ok(out::RegisteredAuthResult {
            address: if self.wrong_auth_address {
                thruput_json_rpc_types::base58::Base58(thruput_xdr::types::ContractAddress::from(
                    [99; 32],
                ))
            } else {
                address
            },
            state_block_height: self.height.unwrap_or(42).into(),
            configuration: None,
        })
    }
}
#[async_trait]
impl NativeSimulationPlane for Backend {
    async fn simulate(
        &self,
        _: RpcRequestContext,
        _: input::SimulateInvocationRequest,
        gas_limit: u64,
    ) -> Result<out::SimulationResult, RpcServerError> {
        self.check().await?;
        assert!(gas_limit > 0);
        Ok(out::SimulationResult {
            state_block_height: self.height.unwrap_or(42).into(),
            output: out::InvocationOutput::Return {
                return_data: HexBytes(vec![3]),
            },
            events: vec![],
            gas_used: 5,
        })
    }
    async fn simulate_batch(
        &self,
        _: RpcRequestContext,
        request: input::SimulateInvocationsRequest,
        _: u64,
        _: ViewBatchConfig,
    ) -> Result<out::SimulationResults, RpcServerError> {
        self.check().await?;
        Ok(out::SimulationResults {
            state_block_height: self.height.unwrap_or(42).into(),
            results: request
                .invocations
                .iter()
                .map(|_| out::InvocationResult {
                    output: out::InvocationOutput::Fault {},
                    events: vec![],
                    gas_used: 5,
                })
                .collect(),
        })
    }
}
type Service = NativeReadService<Backend, Backend, Backend>;
fn service(
    backend: Backend,
    config: RpcServerConfig,
    capabilities: NativeReadCapabilities,
) -> Service {
    NativeReadService::new(
        backend.clone(),
        backend.clone(),
        backend,
        NetworkHash::from(thruput_xdr::types::SHA256Hash::from([9; 32])),
        config,
        BuildVersion::new("test-build", None),
        capabilities,
    )
}
fn invocation() -> input::SimulateInvocationRequest {
    input::SimulateInvocationRequest {
        address: thruput_json_rpc_types::base58::Base58(thruput_xdr::types::ContractAddress::from(
            [1; 32],
        )),
        entrypoint: HexBytes(vec![2; 8]),
        calldata: HexBytes(vec![3]),
        caller: None,
        assumed_auth: None,
        gas_limit: Some(100),
        min_state_block_height: None,
    }
}
fn call() -> input::Invocation {
    let request = invocation();
    input::Invocation {
        address: request.address,
        entrypoint: request.entrypoint,
        calldata: request.calldata,
        caller: None,
        assumed_auth: None,
        gas_limit: request.gas_limit,
    }
}
#[tokio::test]
async fn typed_requests_enforce_bounds_before_backend_dispatch() {
    let backend = Backend::default();
    let config = RpcServerConfig {
        view_gas_limit: 100,
        max_view_calldata_bytes: 4,
        view_batch: ViewBatchConfig {
            max_calls: 2,
            max_gas: 150,
            max_calldata_bytes: 3,
            ..ViewBatchConfig::default()
        },
        ..RpcServerConfig::default()
    };
    let service = service(
        backend.clone(),
        config,
        NativeReadCapabilities::new(true, true),
    );
    let mut invalid = vec![];
    let mut request = invocation();
    request.entrypoint = HexBytes(vec![0; 7]);
    invalid.push(request);
    let mut request = invocation();
    request.calldata = HexBytes(vec![0; 5]);
    invalid.push(request);
    for gas in [0, 101] {
        let mut request = invocation();
        request.gas_limit = Some(gas);
        invalid.push(request);
    }
    let mut request = invocation();
    request.min_state_block_height = Some((-1).into());
    invalid.push(request);
    for request in invalid {
        assert_eq!(
            service
                .simulate_invocation(request)
                .await
                .unwrap_err()
                .code(),
            codes::INVALID_PARAMS
        );
    }
    for calls in [vec![], vec![call(); 3], vec![call(); 2]] {
        assert_eq!(
            service
                .simulate_invocations(input::SimulateInvocationsRequest {
                    invocations: calls,
                    min_state_block_height: None
                })
                .await
                .unwrap_err()
                .code(),
            codes::INVALID_PARAMS
        );
    }
    let mut large = call();
    large.gas_limit = Some(1);
    large.calldata = HexBytes(vec![0; 2]);
    assert_eq!(
        service
            .simulate_invocations(input::SimulateInvocationsRequest {
                invocations: vec![large; 2],
                min_state_block_height: None
            })
            .await
            .unwrap_err()
            .code(),
        codes::INVALID_PARAMS
    );
    assert_eq!(
        service
            .get_storage(input::StorageQuery {
                address: thruput_json_rpc_types::base58::Base58(
                    thruput_xdr::types::ContractAddress::from([1; 32])
                ),
                key: HexBytes(vec![0; 31])
            })
            .await
            .unwrap_err()
            .code(),
        codes::INVALID_PARAMS
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    service.simulate_invocation(invocation()).await.unwrap();
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn read_deadlines_do_not_add_admission_claims() {
    let backend = Backend {
        slow: true,
        ..Default::default()
    };
    let config = RpcServerConfig {
        request_timeout_ms: Some(10),
        ..RpcServerConfig::default()
    };
    let service = service(backend, config, NativeReadCapabilities::new(true, true));
    let error = service
        .get_balance(input::AddressQuery {
            address: thruput_json_rpc_types::base58::Base58(
                thruput_xdr::types::ContractAddress::from([1; 32]),
            ),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::BACKEND_TIMEOUT);
    let data: serde_json::Value = serde_json::from_str(error.data().unwrap().get()).unwrap();
    assert_eq!(data["kind"], "backendTimeout");
    assert!(data.get("admission").is_none());
}
#[test]
fn native_invalid_data_has_evidence_while_wire13_error_is_unchanged() {
    let error = invalid("bad result");
    let legacy = ErrorObjectOwned::from(error.clone());
    assert_eq!(
        legacy.code(),
        thruput_json_rpc_types::error::codes::BACKEND_UNAVAILABLE
    );
    assert_eq!(legacy.message(), "backend unavailable: bad result");
    assert!(legacy.data().is_none());
    let native = rpc_error(&error);
    assert_eq!(native.code(), codes::BACKEND_DATA_INVALID);
    let data: serde_json::Value = serde_json::from_str(native.data().unwrap().get()).unwrap();
    assert_eq!(data["reason"], "inconsistentResult");
    assert!(data.get("admission").is_none());
    assert!(data.get("observation").is_none());
}

#[async_trait]
impl crate::native::transaction::NativeSubmitPlane for Backend {
    async fn native_submit(
        &self,
        _: RpcRequestContext,
        _: crate::ValidatedTransaction,
        attempt: crate::native::transaction::SubmissionAttempt,
    ) -> Result<(), RpcServerError> {
        self.check().await?;
        assert!(attempt.begin_dispatch());
        Ok(())
    }
}
#[async_trait]
impl crate::native::transaction::NativeTransactionReadPlane for Backend {
    async fn native_receipt(
        &self,
        _: RpcRequestContext,
        hash: thruput_json_rpc_types::transaction::TxHash,
    ) -> Result<out::ReceiptResult, crate::native::transaction::NativeReadError> {
        self.check().await?;
        Ok(Some(out::TransactionReceipt {
            transaction_hash: hash,
            block_height: 42.into(),
            execution: out::ReceiptExecution::Return {
                return_data: HexBytes(vec![]),
            },
            events: vec![],
            gas_used: 99,
        }))
    }
    async fn native_status(
        &self,
        _: RpcRequestContext,
        _: thruput_json_rpc_types::transaction::TxHash,
    ) -> Result<out::TransactionStatus, crate::native::transaction::NativeReadError> {
        self.check().await?;
        Ok(out::TransactionStatus::Committed {
            block_height: 42.into(),
        })
    }
}
type Transactions = crate::GroundhogRpcServer<Backend, Backend, Backend, ()>;
fn transactions(backend: Backend) -> Transactions {
    crate::GroundhogRpcServer::new(
        backend.clone(),
        backend.clone(),
        backend,
        (),
        thruput_json_rpc_types::network_info::NetworkInfo::new(
            thruput_json_rpc_types::base58::Base58([9; 32].into()),
        ),
        RpcServerConfig {
            tip_refresh_interval_ms: 0,
            ..Default::default()
        },
        crate::BuildInfo::new("test-build", None),
    )
}

// Candidate HTTP composition exists only in tests. Unimplemented methods must
// be removed before serving; accidentally dispatching one fails this test.
struct Candidate(Service, Transactions);
macro_rules! dispatch {
    (get_version_info, $s:expr, $r:expr) => {
        $s.0.get_version_info($r).await
    };
    (get_network_info, $s:expr, $r:expr) => {
        $s.0.get_network_info($r).await
    };
    (get_server_limits, $s:expr, $r:expr) => {
        $s.0.get_server_limits($r).await
    };
    (get_chain_tip, $s:expr, $r:expr) => {
        $s.0.get_chain_tip($r).await
    };
    (get_chain_config, $s:expr, $r:expr) => {
        $s.0.get_chain_config($r).await
    };
    (get_storage, $s:expr, $r:expr) => {
        $s.0.get_storage($r).await
    };
    (get_balance, $s:expr, $r:expr) => {
        $s.0.get_balance($r).await
    };
    (get_registered_auth, $s:expr, $r:expr) => {
        $s.0.get_registered_auth($r).await
    };
    (simulate_invocation, $s:expr, $r:expr) => {
        $s.0.simulate_invocation($r).await
    };
    (simulate_invocations, $s:expr, $r:expr) => {
        $s.0.simulate_invocations($r).await
    };
    (send_transaction, $s:expr, $r:expr) => {
        $s.1.native_send_transaction($r).await
    };
    (get_transaction_status, $s:expr, $r:expr) => {
        $s.1.native_transaction_status($r).await
    };
    (get_transaction_receipt, $s:expr, $r:expr) => {
        $s.1.native_transaction_receipt($r).await
    };
    ($name:ident, $s:expr, $r:expr) => {{
        let _ = $r;
        panic!("unregistered method reached service")
    }};
}
macro_rules! implement {
    ($( $(#[$doc:meta])* $name:ident($wire:literal,$parse:ident,$decorate:ident): $request:ty => $response:ty; )*) => {
        impl thruput_json_rpc_api::native::NativeApi for Candidate { $(async fn $name(&self, request: $request) -> jsonrpsee::core::RpcResult<$response> { dispatch!($name, self, request) })* }
    };
}
thruput_json_rpc_api::native_methods!(implement);
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the HTTP capability and outage scenario together"
)]
async fn candidate_http_serves_native_shapes_and_fixed_capabilities_during_outages() {
    for db in [false, true] {
        let backend = Backend::default();
        let service = service(
            backend.clone(),
            RpcServerConfig::default(),
            NativeReadCapabilities::new(db, db),
        );
        let mut expected = service.method_names();
        if db {
            expected.extend([
                "sendTransaction",
                "getTransactionStatus",
                "getTransactionReceipt",
            ]);
            expected.sort_unstable();
            assert_eq!(expected.len(), 13);
        }
        let mut module =
            crate::native::NativeModule::new(Candidate(service, transactions(backend.clone())))
                .unwrap();
        for name in thruput_json_rpc_api::native::METHOD_NAMES {
            if !expected.contains(name) {
                assert!(module.remove_method(name));
            }
        }
        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let handle = server.start(module.finalize().into_rpc());
        let client = HttpClientBuilder::default().build(url).unwrap();
        let version: serde_json::Value = client
            .request("getVersionInfo", ObjectParams::new())
            .await
            .unwrap();
        assert_eq!(version["wireFormat"], "14.0.0");
        assert_eq!(version["supportedMethods"], serde_json::json!(expected));
        let limits: serde_json::Value = client
            .request("getServerLimits", ObjectParams::new())
            .await
            .unwrap();
        assert_eq!(limits.get("simulationBatch").is_some(), db);
        let network: serde_json::Value = client
            .request("getNetworkInfo", ObjectParams::new())
            .await
            .unwrap();
        assert!(network["networkHash"].is_string());
        if db {
            let config: serde_json::Value = client
                .request("getChainConfig", ObjectParams::new())
                .await
                .unwrap();
            assert_eq!(config["stated"], serde_json::json!({}));
            assert!(config.get("execution").is_none());
            let tip: serde_json::Value = client
                .request("getChainTip", ObjectParams::new())
                .await
                .unwrap();
            assert_eq!(tip["blockHeight"], "42");
            let mut address = ObjectParams::new();
            address
                .insert(
                    "address",
                    ContractAddress::from(thruput_xdr::types::ContractAddress::from([1; 32])),
                )
                .unwrap();
            let balance: serde_json::Value = client.request("getBalance", address).await.unwrap();
            assert_eq!(balance["rawBalance"], "0");
            assert_eq!(balance["stateBlockHeight"], "42");
            let address = ContractAddress::from(thruput_xdr::types::ContractAddress::from([1; 32]));
            let mut auth = ObjectParams::new();
            auth.insert("address", address).unwrap();
            let auth: serde_json::Value = client.request("getRegisteredAuth", auth).await.unwrap();
            assert_eq!(auth["stateBlockHeight"], "42");
            assert!(auth["configuration"].is_null());
            let mut storage = ObjectParams::new();
            storage.insert("address", address).unwrap();
            storage.insert("key", HexBytes(vec![0; 32])).unwrap();
            let storage: serde_json::Value = client.request("getStorage", storage).await.unwrap();
            assert_eq!(storage["stateBlockHeight"], "42");
            assert_eq!(storage["item"]["bytes"], "0x0102");
            let mut single = ObjectParams::new();
            single.insert("address", address).unwrap();
            single.insert("entrypoint", "0x0202020202020202").unwrap();
            single.insert("calldata", "0x03").unwrap();
            let single: serde_json::Value =
                client.request("simulateInvocation", single).await.unwrap();
            assert_eq!(single["stateBlockHeight"], "42");
            assert_eq!(single["gasUsed"], "5");
            let mut batch = ObjectParams::new();
            batch.insert("invocations", vec![call(), call()]).unwrap();
            let batch: serde_json::Value =
                client.request("simulateInvocations", batch).await.unwrap();
            assert_eq!(batch["results"].as_array().unwrap().len(), 2);
            assert_eq!(batch["stateBlockHeight"], "42");
            let signed = crate::tx::test_fixtures::canonical_signed_transaction();
            let mut submit = ObjectParams::new();
            submit
                .insert(
                    "signedTransaction",
                    HexBytes(xdrs::txs::encode(&signed).unwrap()),
                )
                .unwrap();
            let sent: serde_json::Value = client.request("sendTransaction", submit).await.unwrap();
            assert_eq!(sent["admission"], "accepted");
            assert!(sent.get("txHash").is_none());
            let mut query = ObjectParams::new();
            query
                .insert("transactionHash", sent["transactionHash"].as_str().unwrap())
                .unwrap();
            let status: serde_json::Value =
                client.request("getTransactionStatus", query).await.unwrap();
            assert_eq!(
                status,
                serde_json::json!({"status":"committed","blockHeight":"42"})
            );
            let mut query = ObjectParams::new();
            query
                .insert("transactionHash", sent["transactionHash"].as_str().unwrap())
                .unwrap();
            let receipt: serde_json::Value = client
                .request("getTransactionReceipt", query)
                .await
                .unwrap();
            assert_eq!(receipt["transactionHash"], sent["transactionHash"]);
            assert_eq!(receipt["blockHeight"], "42");
            assert_eq!(
                receipt["execution"],
                serde_json::json!({"result":"return","returnData":"0x"})
            );
            assert_eq!(receipt["gasUsed"], "99");
            assert!(receipt.get("status").is_none());
            assert!(receipt.get("height").is_none());
            backend.unavailable.store(true, Ordering::SeqCst);
            assert!(
                client
                    .request::<serde_json::Value, _>("getChainTip", ObjectParams::new())
                    .await
                    .is_err()
            );
            let after: serde_json::Value = client
                .request("getVersionInfo", ObjectParams::new())
                .await
                .unwrap();
            assert_eq!(version, after);
        }
        assert!(
            client
                .request::<serde_json::Value, _>("sendTransaction", ObjectParams::new())
                .await
                .is_err()
        );
        assert!(
            client
                .request::<serde_json::Value, _>("groundhog_getVersionInfo", ObjectParams::new())
                .await
                .is_err()
        );
        handle.stop().unwrap();
        handle.stopped().await;
    }
    assert_eq!(thruput_json_rpc_api::WIRE_FORMAT_VERSION, "13.0.0");
}

#[test]
fn native_error_evidence_distinguishes_freshness_corruption_and_absent_methods() {
    let height_error = |requested: i64, observed: i64| {
        rpc_error(&RpcServerError::ViewHeightUnavailable(
            thruput_json_rpc_types::view::ViewHeightUnavailable {
                requested: requested.into(),
                observed: observed.into(),
            },
        ))
    };
    let error = height_error(43, 42);
    assert_eq!(error.code(), codes::STATE_HEIGHT_UNAVAILABLE);
    let data: serde_json::Value = serde_json::from_str(error.data().unwrap().get()).unwrap();
    assert_eq!(data["requestedStateBlockHeight"], "43");
    assert_eq!(data["observedStateBlockHeight"], "42");
    assert!(data.get("admission").is_none());
    assert_eq!(height_error(42, 42).code(), codes::BACKEND_DATA_INVALID);
    assert_eq!(height_error(42, -1).code(), codes::BACKEND_UNAVAILABLE);
    assert_eq!(height_error(42, -2).code(), codes::BACKEND_DATA_INVALID);
    assert_eq!(
        rpc_error(&RpcServerError::NotImplemented("test".into())).code(),
        -32601
    );
    assert_eq!(
        rpc_error(&invalid("broken")).message(),
        "backend data invalid: broken"
    );
}

#[tokio::test]
async fn custom_planes_cannot_return_mismatched_auth_or_invalid_balances() {
    let address = input::AddressQuery {
        address: thruput_json_rpc_types::base58::Base58(thruput_xdr::types::ContractAddress::from(
            [1; 32],
        )),
    };
    for item in [
        out::StorageItem::Bytestring {
            bytes: HexBytes(vec![]),
        },
        out::StorageItem::NonnegativeInt { value: -1 },
    ] {
        let service = service(
            Backend {
                balance_item: Some(item),
                ..Default::default()
            },
            RpcServerConfig::default(),
            NativeReadCapabilities::new(true, true),
        );
        assert_eq!(
            service
                .get_balance(address.clone())
                .await
                .unwrap_err()
                .code(),
            codes::BACKEND_DATA_INVALID
        );
    }
    let service = service(
        Backend {
            wrong_auth_address: true,
            ..Default::default()
        },
        RpcServerConfig::default(),
        NativeReadCapabilities::new(true, true),
    );
    assert_eq!(
        service
            .get_registered_auth(address)
            .await
            .unwrap_err()
            .code(),
        codes::BACKEND_DATA_INVALID
    );
}
#[tokio::test]
async fn custom_plane_snapshot_sentinel_and_corruption_are_distinct() {
    for (height, code) in [
        (-1, codes::BACKEND_UNAVAILABLE),
        (-2, codes::BACKEND_DATA_INVALID),
    ] {
        let service = service(
            Backend {
                height: Some(height),
                ..Default::default()
            },
            RpcServerConfig::default(),
            NativeReadCapabilities::new(true, true),
        );
        assert_eq!(
            service
                .simulate_invocation(invocation())
                .await
                .unwrap_err()
                .code(),
            code
        );
        assert_eq!(
            service
                .get_balance(input::AddressQuery {
                    address: thruput_json_rpc_types::base58::Base58(
                        thruput_xdr::types::ContractAddress::from([1; 32])
                    )
                })
                .await
                .unwrap_err()
                .code(),
            code
        );
    }
}
#[tokio::test]
async fn bundle_identity_keeps_diagnostics_from_the_same_build() {
    let build =
        BuildVersion::new("new-build", Some("new-platform")).with_source_commit(Some("new-commit"));
    let service = service(
        Backend::default(),
        RpcServerConfig::default(),
        NativeReadCapabilities::new(false, false),
    )
    .with_bundle_identity(&thruput_bundle_identity::BundleIdentity::unbundled(build));
    let version = service
        .get_version_info(input::EmptyRequest {})
        .await
        .unwrap();
    assert_eq!(version.implementation, "new-build");
    assert_eq!(version.commit_hash.as_deref(), Some("new-commit"));
    assert_eq!(version.platform_version.as_deref(), Some("new-platform"));
    assert!(version.release_version.is_none());
    assert!(version.bundle_manifest_digest.is_none());
}
