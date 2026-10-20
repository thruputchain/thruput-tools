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
    wrong_block_network: bool,
    history_refs: Option<out::TransactionRefPage>,
    history_error: Option<RpcServerError>,
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
type AssembledService = crate::native::service::NativeService<Backend, Backend, Backend, Backend>;
fn assembled_service(
    backend: Backend,
    capabilities: crate::native::service::NativeCapabilities,
) -> AssembledService {
    AssembledService::new(
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend,
        NetworkHash::from(thruput_xdr::types::SHA256Hash::from([9; 32])),
        RpcServerConfig::default().with_tip_refresh_interval_ms(0),
        BuildVersion::new("test-build", None),
        capabilities,
    )
}

#[tokio::test]
async fn assembly_registers_each_configured_plane_independently() {
    use crate::native::service::NativeCapabilities;
    for mask in 0..16 {
        let scheduler = mask & 1 != 0;
        let indexer = mask & 2 != 0;
        let db = mask & 4 != 0;
        let simulation = mask & 8 != 0;
        let capabilities = NativeCapabilities::new(scheduler, indexer, db, simulation);
        let mut expected = vec!["getVersionInfo", "getNetworkInfo", "getServerLimits"];
        if scheduler {
            expected.push("sendTransaction");
        }
        if indexer {
            expected.extend([
                "getTransactionReceipt",
                "getBlockByHeight",
                "getEvents",
                "getTransactionsByRootAddress",
                "getTransactionsByBlock",
            ]);
        }
        if scheduler && indexer {
            expected.push("getTransactionStatus");
        }
        if db {
            expected.extend([
                "getChainTip",
                "getChainConfig",
                "getStorage",
                "getBalance",
                "getRegisteredAuth",
            ]);
        }
        if simulation {
            expected.extend(["simulateInvocation", "simulateInvocations"]);
        }
        expected.sort_unstable();
        let backend = Backend::default();
        let module = assembled_service(backend.clone(), capabilities)
            .into_module()
            .unwrap();
        assert_eq!(module.supported_methods(), expected, "configuration {mask}");
        let module = module.into_rpc();
        let mut registered: Vec<_> = module.method_names().collect();
        registered.sort_unstable();
        assert_eq!(registered, expected);
        let version_request = r#"{"jsonrpc":"2.0","id":1,"method":"getVersionInfo","params":{}}"#;
        let (before, _) = module.raw_json_request(version_request, 1).await.unwrap();
        let before: serde_json::Value = serde_json::from_str(before.get()).unwrap();
        assert_eq!(
            before["result"]["supportedMethods"],
            serde_json::json!(expected)
        );
        assert_eq!(before["result"]["wireFormat"], "14.0.0");
        assert_eq!(before["result"]["implementation"], "test-build");
        assert!(before["result"].get("releaseVersion").is_none());
        assert!(before["result"].get("bundleManifestDigest").is_none());
        backend.unavailable.store(true, Ordering::SeqCst);
        let (after, _) = module.raw_json_request(version_request, 1).await.unwrap();
        assert_eq!(
            before,
            serde_json::from_str::<serde_json::Value>(after.get()).unwrap()
        );
        for name in thruput_json_rpc_api::native::METHOD_NAMES
            .iter()
            .copied()
            .chain([
                "groundhog_networkInfo",
                "groundhog_sendRawTransaction",
                "groundhog_view",
            ])
            .filter(|name| !expected.contains(name))
        {
            let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":name,"params":{}});
            let (response, _) = module
                .raw_json_request(&request.to_string(), 1)
                .await
                .unwrap();
            let response: serde_json::Value = serde_json::from_str(response.get()).unwrap();
            assert_eq!(response["error"]["code"], -32601, "{mask}: {name}");
        }
        assert_eq!(
            backend.calls.load(Ordering::SeqCst),
            0,
            "discovery must not probe backends"
        );
    }
}

#[tokio::test]
async fn direct_unconfigured_service_calls_return_method_not_found() {
    use thruput_json_rpc_api::native::NativeApi;
    let backend = Backend::default();
    let service = assembled_service(
        backend.clone(),
        crate::native::service::NativeCapabilities::new(false, false, false, false),
    );
    let error = service
        .get_transaction_receipt(input::TransactionQuery {
            transaction_hash: thruput_json_rpc_types::transaction::TxHash(
                thruput_xdr::types::SHA256Hash::from([1; 32]).into(),
            ),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), -32601);
    let error = service.simulate_invocation(invocation()).await.unwrap_err();
    assert_eq!(error.code(), -32601);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}

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
                "getEvents",
                "getTransactionsByRootAddress",
                "getTransactionsByBlock",
                "getBlockByHeight",
                "getTransactionStatus",
                "getTransactionReceipt",
            ]);
            expected.sort_unstable();
            assert_eq!(expected.len(), 17);
        }
        let module = assembled_service(
            backend.clone(),
            crate::native::service::NativeCapabilities::new(db, db, db, db),
        )
        .into_module()
        .unwrap();
        assert_eq!(module.supported_methods(), expected);
        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let handle = server.start(module.into_rpc());
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

#[async_trait]
impl NativeBlockPlane for Backend {
    async fn native_block(
        &self,
        _: RpcRequestContext,
        height: BlockHeight,
    ) -> Result<out::BlockResult, RpcServerError> {
        self.check().await?;
        if *height == 0 || *height > 42 {
            return Ok(None);
        }
        let hash = thruput_xdr::types::SHA256Hash::from([9; 32]);
        Ok(Some(out::BlockInfo {
            block_hash: hash.into(),
            protocol: 1.into(),
            network_hash: if self.wrong_block_network {
                thruput_xdr::types::SHA256Hash::from([7; 32]).into()
            } else {
                hash.into()
            },
            config_hash: hash.into(),
            proposer: thruput_json_rpc_types::base58::Base58([2; 32].into()),
            block_timepoint: 123.into(),
            block_height: self.height.unwrap_or(*height).into(),
            txs_hash: hash.into(),
            storage_hash: hash.into(),
            events_hash: hash.into(),
            prev_block_hash: hash.into(),
            proposal_txs_hash: hash.into(),
            upgrades: vec![],
        }))
    }
}

#[tokio::test]
async fn block_metadata_strict_params_indexer_only_and_outages() {
    use crate::native::service::NativeCapabilities;
    let backend = Backend::default();
    let module = assembled_service(
        backend.clone(),
        NativeCapabilities::new(false, true, false, false),
    )
    .into_module()
    .unwrap()
    .into_rpc();
    for params in [
        serde_json::json!({"blockHeight":1}),
        serde_json::json!({"blockHeight":"01"}),
        serde_json::json!({"blockHeight":"-1"}),
        serde_json::json!({"blockHeight":"1","extra":true}),
        serde_json::json!(["1"]),
        serde_json::json!({"height":"1"}),
    ] {
        let request =
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getBlockByHeight","params":params});
        let (response, _) = module
            .raw_json_request(&request.to_string(), 1)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(response.get()).unwrap();
        assert_eq!(response["error"]["code"], -32602, "{params}");
    }
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    for height in [0, 1, 43] {
        let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getBlockByHeight","params":{"blockHeight":height.to_string()}});
        let (response, _) = module
            .raw_json_request(&request.to_string(), 1)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(response.get()).unwrap();
        if height == 1 {
            assert_eq!(response["result"]["blockHeight"], "1");
            assert_eq!(response["result"]["blockTimepoint"], "123");
        } else {
            assert!(response.get("result").unwrap().is_null());
        }
    }
    backend.unavailable.store(true, Ordering::SeqCst);
    let (response, _) = module
        .raw_json_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"getBlockByHeight","params":{"blockHeight":"0"}}"#,
            1,
        )
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(response.get()).unwrap();
    assert_eq!(response["error"]["code"], codes::BACKEND_UNAVAILABLE);
    assert!(response["error"]["data"].get("admission").is_none());
}

#[tokio::test]
async fn block_metadata_rejects_inconsistent_values_and_deadlines() {
    for (backend, expected) in [
        (
            Backend {
                wrong_block_network: true,
                ..Backend::default()
            },
            codes::BACKEND_DATA_INVALID,
        ),
        (
            Backend {
                height: Some(2),
                ..Backend::default()
            },
            codes::BACKEND_DATA_INVALID,
        ),
        (
            Backend {
                slow: true,
                ..Backend::default()
            },
            codes::BACKEND_TIMEOUT,
        ),
    ] {
        let service = service(
            backend,
            RpcServerConfig::default().with_request_timeout_ms(5),
            NativeReadCapabilities::new(false, false),
        );
        let error = service
            .get_block_by_height(input::BlockQuery {
                block_height: 1.into(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), expected);
    }
    let backend = Backend::default();
    let service = service(
        backend.clone(),
        RpcServerConfig::default(),
        NativeReadCapabilities::new(false, false),
    );
    assert_eq!(
        service
            .get_block_by_height(input::BlockQuery {
                block_height: (-1).into()
            })
            .await
            .unwrap_err()
            .code(),
        codes::INVALID_PARAMS
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}

#[async_trait]
impl NativeHistoryPlane for Backend {
    async fn native_events(
        &self,
        _: RpcRequestContext,
        request: input::EventQuery,
    ) -> Result<out::EventPage, RpcServerError> {
        self.check().await?;
        assert!(request.limit.is_some_and(|value| value > 0 && value <= 200));
        if let Some(error) = &self.history_error {
            return Err(error.clone());
        }
        Ok(out::EventPage {
            events: vec![],
            next_cursor: None,
            oldest_indexed_block_height: 0.into(),
            latest_indexed_block_height: 42.into(),
        })
    }
    async fn native_root_transactions(
        &self,
        _: RpcRequestContext,
        request: input::RootAddressQuery,
    ) -> Result<out::TransactionRefPage, RpcServerError> {
        self.check().await?;
        assert!(request.limit.is_some_and(|value| value > 0 && value <= 200));
        if let Some(error) = &self.history_error {
            return Err(error.clone());
        }
        if let Some(result) = &self.history_refs {
            return Ok(result.clone());
        }
        Ok(out::TransactionRefPage {
            transactions: vec![],
            next_cursor: None,
            oldest_indexed_block_height: 0.into(),
            latest_indexed_block_height: 42.into(),
        })
    }
    async fn native_block_transactions(
        &self,
        _: RpcRequestContext,
        request: input::BlockPageRequest,
    ) -> Result<out::BlockTransactionPage, RpcServerError> {
        self.check().await?;
        assert!(request.limit.is_some_and(|value| value > 0 && value <= 200));
        if let Some(error) = &self.history_error {
            return Err(error.clone());
        }
        if let Some(result) = &self.history_refs {
            return Ok(result.clone());
        }
        Ok(out::TransactionRefPage {
            transactions: vec![],
            next_cursor: None,
            oldest_indexed_block_height: 1.into(),
            latest_indexed_block_height: 42.into(),
        })
    }
}

#[test]
fn history_errors_preserve_native_cause_without_changing_legacy_codes() {
    for (error, code, kind) in [
        (
            RpcServerError::InvalidCursor("bad token".into()),
            -32602,
            "invalidCursor",
        ),
        (
            RpcServerError::HistoryUnavailable("watermark behind".into()),
            -32013,
            "historyUnavailable",
        ),
    ] {
        let legacy: ErrorObjectOwned = error.clone().into();
        assert_eq!(
            legacy.code(),
            thruput_json_rpc_types::error::codes::BACKEND_UNAVAILABLE
        );
        let native = rpc_error(&error);
        assert_eq!(native.code(), code);
        let data: serde_json::Value = serde_json::from_str(native.data().unwrap().get()).unwrap();
        assert_eq!(data["kind"], kind);
        assert!(data.get("reason").is_none());
        assert!(data.get("admission").is_none());
        assert!(data.get("oldestIndexedBlockHeight").is_none());
    }
}

#[tokio::test]
async fn history_rejects_direct_invalid_requests_before_dispatch_and_caps_defaults() {
    let backend = Backend::default();
    let service = service(
        backend.clone(),
        RpcServerConfig::default().with_max_events_per_page(17),
        NativeReadCapabilities::new(false, false),
    );
    let mut request: input::EventQuery = serde_json::from_value(serde_json::json!({
        "filters": [{"addresses": [invocation().address]}],
        "start": {"order": "ascending", "anchor": {"kind": "atBeginning"}}
    }))
    .unwrap();
    service.get_events(request.clone()).await.unwrap();
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    request.limit = Some(18);
    assert_eq!(
        service
            .get_events(request.clone())
            .await
            .unwrap_err()
            .code(),
        -32602
    );
    request.limit = None;
    request.filters.clear();
    assert_eq!(
        service.get_events(request).await.unwrap_err().code(),
        -32602
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}

fn reference_page() -> out::TransactionRefPage {
    out::TransactionRefPage {
        transactions: [0x7f, 0x80]
            .into_iter()
            .map(|byte| out::TransactionRef {
                transaction_hash: thruput_json_rpc_types::transaction::TxHash(
                    thruput_json_rpc_types::base58::Base58([byte; 32].into()),
                ),
                block_height: 9.into(),
            })
            .collect(),
        next_cursor: Some(HexBytes(vec![2])),
        oldest_indexed_block_height: 1.into(),
        latest_indexed_block_height: 10.into(),
    }
}

#[tokio::test]
async fn reference_pages_enforce_order_height_and_progress() {
    let query = || input::BlockPageRequest {
        block_height: 9.into(),
        start: Some(input::BlockStart {
            order: thruput_json_rpc_types::events::IterationOrder::Ascending,
        }),
        cursor: None,
        limit: Some(2),
    };
    let run = |page| {
        service(
            Backend {
                history_refs: Some(page),
                ..Backend::default()
            },
            RpcServerConfig::default(),
            NativeReadCapabilities::new(false, false),
        )
    };
    run(reference_page())
        .get_transactions_by_block(query())
        .await
        .unwrap();
    let mut reversed = reference_page();
    reversed.transactions.reverse();
    assert_eq!(
        run(reversed.clone())
            .get_transactions_by_block(query())
            .await
            .unwrap_err()
            .code(),
        -32054
    );
    let mut resumed = query();
    resumed.start = None;
    resumed.cursor = Some(HexBytes(vec![1]));
    run(reversed)
        .get_transactions_by_block(resumed.clone())
        .await
        .unwrap();
    resumed.cursor = Some(HexBytes(vec![2]));
    assert_eq!(
        run(reference_page())
            .get_transactions_by_block(resumed)
            .await
            .unwrap_err()
            .code(),
        -32054
    );
    for change in 0..5 {
        let mut page = reference_page();
        match change {
            0 => page.transactions[0].block_height = 8.into(),
            1 => page.transactions[1] = page.transactions[0].clone(),
            2 => page.transactions.clear(),
            3 => page.oldest_indexed_block_height = 0.into(),
            _ => page.latest_indexed_block_height = 8.into(),
        }
        assert_eq!(
            run(page)
                .get_transactions_by_block(query())
                .await
                .unwrap_err()
                .code(),
            -32054
        );
    }
    let mut terminal = reference_page();
    terminal.transactions.clear();
    terminal.next_cursor = None;
    run(terminal.clone())
        .get_transactions_by_block(query())
        .await
        .unwrap();
    terminal.latest_indexed_block_height = 8.into();
    run(terminal.clone())
        .get_transactions_by_block(query())
        .await
        .unwrap();
    let mut resumed = query();
    resumed.start = None;
    resumed.cursor = Some(HexBytes(vec![1]));
    assert_eq!(
        run(terminal)
            .get_transactions_by_block(resumed)
            .await
            .unwrap_err()
            .code(),
        -32054
    );
}

#[tokio::test]
async fn root_reference_pages_enforce_the_inclusive_anchor() {
    let run = |page| {
        service(
            Backend {
                history_refs: Some(page),
                ..Backend::default()
            },
            RpcServerConfig::default(),
            NativeReadCapabilities::new(false, false),
        )
    };
    let root = input::RootAddressQuery {
        address: invocation().address,
        start: Some(input::HistoryStart::Ascending {
            anchor: input::AscendingAnchor::AtHeight {
                block_height: 9.into(),
            },
        }),
        cursor: None,
        limit: Some(2),
    };
    run(reference_page())
        .get_transactions_by_root_address(root.clone())
        .await
        .unwrap();
    let mut wrong = reference_page();
    wrong.transactions[0].block_height = 8.into();
    assert_eq!(
        run(wrong)
            .get_transactions_by_root_address(root)
            .await
            .unwrap_err()
            .code(),
        -32054
    );
}

#[tokio::test]
async fn history_preserves_backend_cursor_outcomes_and_deadlines() {
    let query = || input::BlockPageRequest {
        block_height: 9.into(),
        start: None,
        cursor: Some(HexBytes(vec![1])),
        limit: None,
    };
    for (error, code) in [
        (
            RpcServerError::InvalidCursor("invalid token".into()),
            -32602,
        ),
        (RpcServerError::HistoryUnavailable("behind".into()), -32013),
    ] {
        let service = service(
            Backend {
                history_error: Some(error),
                ..Backend::default()
            },
            RpcServerConfig::default(),
            NativeReadCapabilities::new(false, false),
        );
        assert_eq!(
            service
                .get_transactions_by_block(query())
                .await
                .unwrap_err()
                .code(),
            code
        );
    }
    let service = service(
        Backend {
            slow: true,
            ..Backend::default()
        },
        RpcServerConfig {
            request_timeout_ms: Some(1),
            ..RpcServerConfig::default()
        },
        NativeReadCapabilities::new(false, false),
    );
    assert_eq!(
        service
            .get_transactions_by_block(query())
            .await
            .unwrap_err()
            .code(),
        -32051
    );
}

#[tokio::test]
async fn native_event_limit_advertises_and_enforces_backend_ceiling() {
    let backend = Backend::default();
    let service = service(
        backend.clone(),
        RpcServerConfig::default().with_max_events_per_page(u32::MAX),
        NativeReadCapabilities::new(false, false),
    );
    let limits = service
        .get_server_limits(input::EmptyRequest {})
        .await
        .unwrap();
    assert_eq!(limits.max_events_per_page, 200);
    let mut request: input::EventQuery = serde_json::from_value(serde_json::json!({
        "filters": [{"addresses": [invocation().address]}],
        "start": {"order": "ascending", "anchor": {"kind": "atBeginning"}},
        "limit": 200
    }))
    .unwrap();
    service.get_events(request.clone()).await.unwrap();
    request.limit = Some(201);
    assert_eq!(
        service.get_events(request).await.unwrap_err().code(),
        -32602
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Copy)]
struct Funding(crate::native::funding::FundingCapabilities);
#[async_trait]
impl crate::native::funding::NativeFundingPlane for Funding {
    fn capabilities(&self) -> crate::native::funding::FundingCapabilities {
        self.0
    }
    async fn request_airdrop(
        &self,
        _: input::AirdropRequest,
    ) -> jsonrpsee::core::RpcResult<out::AirdropAttempt> {
        Err(crate::native::funding::rpc_error(
            &RpcServerError::BackendUnavailable("offline".into()),
        ))
    }
    async fn fund_account(
        &self,
        _: input::FundAccountRequest,
    ) -> jsonrpsee::core::RpcResult<out::FundingSubmission> {
        Err(crate::native::funding::rpc_error(
            &RpcServerError::BackendUnavailable("offline".into()),
        ))
    }
}

#[tokio::test]
async fn optional_funding_registration_is_configured_not_health_driven() {
    use crate::native::{funding::FundingCapabilities, service::NativeCapabilities};
    for (capabilities, local, names) in [
        (FundingCapabilities::None, false, vec![]),
        (
            FundingCapabilities::Faucet,
            false,
            vec!["faucet_requestAirdrop"],
        ),
        (
            FundingCapabilities::Local,
            true,
            vec![
                "dev_fundAccount",
                "dev_getPendingTransactionCount",
                "faucet_requestAirdrop",
            ],
        ),
    ] {
        let service = assembled_service(
            Backend::default(),
            NativeCapabilities::new(false, false, false, false),
        )
        .with_funding(Funding(capabilities));
        let service = if local {
            service.with_local_pending_count()
        } else {
            service
        };
        let module = service.into_module().unwrap().into_rpc();
        for name in [
            "faucet_requestAirdrop",
            "dev_fundAccount",
            "dev_getPendingTransactionCount",
        ] {
            assert_eq!(
                module.method_names().any(|method| method == name),
                names.contains(&name)
            );
        }
        let version = r#"{"jsonrpc":"2.0","id":1,"method":"getVersionInfo","params":{}}"#;
        let (before, _) = module.raw_json_request(version, 1).await.unwrap();
        if !names.is_empty() {
            let request = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"faucet_requestAirdrop","params":{"recipient":thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from([1;32])),"clientNonce":"0x01"}});
            let (failed, _) = module
                .raw_json_request(&request.to_string(), 1)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_str(failed.get()).unwrap();
            assert_eq!(value["error"]["data"]["kind"], "backendUnavailable");
        }
        let (after, _) = module.raw_json_request(version, 1).await.unwrap();
        assert_eq!(before.get(), after.get());
    }
}
