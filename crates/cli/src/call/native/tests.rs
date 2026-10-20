use super::*;
use crate::call::ResolvedCall;
use jsonrpsee::{
    RpcModule,
    http_client::{HttpClient, HttpClientBuilder},
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use thruput_json_rpc_client::native::receipt::{ReceiptPollCause, ReceiptPollError};
use thruput_json_rpc_types::NetworkHash;
use thruput_json_rpc_types::{BlockHeight, responses::EmittedEvent, transaction::TxHash};
use thruput_signing::{LocalSigner, PubkeyAccount, Signer};
use thruput_xdr::types::{
    ContractAddress as XdrAddress, NetworkHash as XdrNetworkHash, SHA256Hash,
};

type Calls = Arc<Mutex<Vec<(&'static str, Value)>>>;
fn network() -> XdrNetworkHash {
    XdrNetworkHash::from(SHA256Hash::from([9; 32]))
}
fn sender() -> ResolvedTransactionSender {
    let signer = LocalSigner::from_secret_key([8; 32]);
    let account = PubkeyAccount::new(signer.public_key(), 0);
    ResolvedTransactionSender {
        wallet_address: account.address(),
        native_account: Some(account),
        signer: signer.into(),
        key_name: "fixture".into(),
        wallet_name: None,
        network_hash: Some(network()),
    }
}
fn prepared() -> PreparedCall {
    let sender = sender();
    let address = XdrAddress::from([3; 32]);
    let entrypoint = thruput_xdr::contract::ContractEntrypointID::from([4; 8]);
    let signed = thruput_client::prepare_call(
        thruput_client::CallRequest {
            wallet: thruput_client::Wallet::Native(sender.native_account.as_ref().unwrap()),
            target_address: address,
            target_entrypoint: entrypoint,
            target_calldata: &[5],
            gas_limit: 1000,
            target_gas_limit: 500,
            block_expiry_height: 100,
            nonce: 1,
        },
        network(),
    )
    .unwrap()
    .sign(&sender.signer)
    .unwrap();
    PreparedCall {
        rpc_url: "http://127.0.0.1:1".into(),
        contract_address_hex: ContractAddress::from(address).to_string(),
        wallet_address_hex: ContractAddress::from(sender.wallet_address).to_string(),
        wallet_entrypoint_id: format_entrypoint_id(
            thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
        ),
        wallet_entrypoint_source: "native_default_wallet",
        method: "fixture".into(),
        resolved: ResolvedCall {
            entrypoint,
            metadata: None,
        },
        calldata: vec![5],
        tx_hex: signed.tx_hex().to_owned(),
        signed,
        network_hash: network(),
    }
}
fn receipt(prepared: &PreparedCall, execution: ReceiptExecution) -> TransactionReceipt {
    TransactionReceipt {
        transaction_hash: prepared.signed.tx_hash(),
        block_height: BlockHeight::from(9_007_199_254_740_993),
        execution,
        events: vec![EmittedEvent {
            address: ContractAddress::from(XdrAddress::from([3; 32])),
            topics: vec![],
            data: HexBytes(vec![6]),
        }],
        gas_used: u64::MAX,
    }
}
fn discovery() -> Value {
    json!({"wireFormat":"14.0.0","supportedMethods":REQUIRED_METHODS,"implementation":"test"})
}
fn network_result() -> Value {
    json!({"networkHash":NetworkHash::from(network().into_inner())})
}
async fn endpoint(
    answers: Vec<(&'static str, Result<Value, ErrorObjectOwned>)>,
) -> (NativeClient<HttpClient>, String, ServerHandle, Calls) {
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    for (method, answer) in answers {
        module
            .register_method(method, move |params, calls, _| {
                calls
                    .lock()
                    .unwrap()
                    .push((method, params.parse::<Value>()?));
                answer.clone()
            })
            .unwrap();
    }
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let handle = server.start(module);
    (
        NativeClient::new(HttpClientBuilder::default().build(&url).unwrap()),
        url,
        handle,
        calls,
    )
}

fn profile(url: &str) -> crate::config::ResolvedProfile {
    serde_json::from_value(json!({"name":"fixture", "rpc_url":url, "contracts":{}, "wallets":{}}))
        .unwrap()
}

#[test]
fn preparation_checks_native_discovery_identity_and_expiry() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_, url, handle, calls) = rt.block_on(endpoint(vec![
        ("getVersionInfo", Ok(discovery())),
        ("getNetworkInfo", Ok(network_result())),
        (
            "getChainTip",
            Ok(json!({"blockHeight":"42","blockHash":NetworkHash::from(SHA256Hash::from([7;32]))})),
        ),
    ]));
    let (context, expiry) = prepare_context(&sender(), &profile(&url), &url, &rt).unwrap();
    assert_eq!(context.network_hash, network());
    assert_eq!(expiry, 1042);
    assert_eq!(
        context.entrypoints.do_action,
        thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| *method)
            .collect::<Vec<_>>(),
        ["getVersionInfo", "getNetworkInfo", "getChainTip"]
    );
    handle.stop().unwrap();
}

#[tokio::test]
async fn balance_checks_account_identity_and_capacity_without_writes() {
    let wallet = prepared().wallet_address_hex;
    let address = ContractAddress::from(parse_contract_address(&wallet).unwrap());
    for (reported, raw, success) in [
        (address, 1000, true),
        (address, 999, false),
        (
            ContractAddress::from(XdrAddress::from([7; 32])),
            1000,
            false,
        ),
    ] {
        let (client, _, handle, calls) = endpoint(vec![(
            "getBalance",
            Ok(json!({"address":reported,"stateBlockHeight":"42","rawBalance":raw.to_string()})),
        )])
        .await;
        assert_eq!(fetch_balance(&client, &wallet, 1000).await.is_ok(), success);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [("getBalance", json!({"address":address}))]
        );
        handle.stop().unwrap();
    }
}

#[test]
fn native_report_retains_root_receipt_and_decodes_single_child_return() {
    let prepared = prepared();
    let root = xdrs::cxs::encode(&vec![vec![7_u8]]).unwrap();
    let report = NativeCallReport::new(
        NativeCallCommon::new(&prepared).unwrap(),
        &prepared,
        receipt(
            &prepared,
            ReceiptExecution::Return {
                return_data: HexBytes(root.clone()),
            },
        ),
    );
    assert!(report.execution_failure().is_none());
    let value = serde_json::to_value(CommandResult::new("call", report)).unwrap();
    let data = &value["data"];
    assert_eq!(data["schemaVersion"], 4);
    assert_eq!(data["blockHeight"], "9007199254740993");
    assert_eq!(data["gasUsed"], u64::MAX.to_string());
    assert_eq!(data["childReturnData"], "0x07");
    assert_eq!(
        data["execution"],
        json!({"result":"return","returnData":HexBytes(root)})
    );
    assert_eq!(data["events"][0]["data"], "0x06");
    assert!(data["events"][0]["address"].is_string());
    assert_eq!(data["transactionHash"], json!(prepared.signed.tx_hash()));
    for absent in [
        "height",
        "status",
        "txHash",
        "contract",
        "transactionDetail",
    ] {
        assert!(data.get(absent).is_none(), "{absent}");
    }
    assert!(
        serde_json::to_value(schemars::schema_for!(NativeCallReport))
            .unwrap()
            .to_string()
            .contains("blockHeight")
    );
}

#[test]
fn failed_execution_and_invalid_wallet_return_keep_the_entire_receipt() {
    for execution in [
        ReceiptExecution::Abort {
            abort_data: HexBytes(vec![0xab]),
        },
        ReceiptExecution::Fault {},
        ReceiptExecution::NonDeterministicError {},
        ReceiptExecution::FailedGasSponsorship {},
        ReceiptExecution::Return {
            return_data: HexBytes(vec![0xff]),
        },
    ] {
        let prepared = prepared();
        let report = NativeCallReport::new(
            NativeCallCommon::new(&prepared).unwrap(),
            &prepared,
            receipt(&prepared, execution.clone()),
        );
        let error = report.execution_failure().unwrap();
        let value =
            serde_json::to_value(CommandResult::failure_with_error("call", report, error)).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["data"]["execution"], json!(execution));
        assert_eq!(value["data"]["events"][0]["data"], "0x06");
        assert_eq!(value["data"]["gasUsed"], u64::MAX.to_string());
    }
}

#[tokio::test]
async fn native_call_submits_once_and_waits_read_only() {
    let prepared = prepared();
    let hash = prepared.signed.tx_hash();
    let returned = receipt(&prepared, ReceiptExecution::Fault {});
    let (client, _, handle, calls) = endpoint(vec![
        ("getVersionInfo", Ok(discovery())),
        ("getNetworkInfo", Ok(network_result())),
        (
            "sendTransaction",
            Ok(json!({"transactionHash":hash,"admission":"accepted"})),
        ),
        (
            "getTransactionReceipt",
            Ok(serde_json::to_value(returned).unwrap()),
        ),
    ])
    .await;
    let received = submit_and_wait(
        &client,
        &prepared,
        ReceiptPollConfig::default(),
        std::future::pending(),
    )
    .await
    .unwrap();
    assert!(matches!(received.execution, ReceiptExecution::Fault {}));
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| *method)
            .collect::<Vec<_>>(),
        [
            "getVersionInfo",
            "getNetworkInfo",
            "sendTransaction",
            "getTransactionReceipt"
        ]
    );
    handle.stop().unwrap();
}

#[tokio::test]
async fn submission_rechecks_identity_and_never_sends_on_mismatch() {
    let prepared = prepared();
    let (client, _, handle, calls) = endpoint(vec![
        ("getVersionInfo", Ok(discovery())),
        (
            "getNetworkInfo",
            Ok(json!({"networkHash":NetworkHash::from(SHA256Hash::from([7;32]))})),
        ),
    ])
    .await;
    let failure = submit_and_wait(
        &client,
        &prepared,
        ReceiptPollConfig::default(),
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.phase, SubmissionPhase::NotSent);
    assert_eq!(calls.lock().unwrap().len(), 2);
    handle.stop().unwrap();
}

#[tokio::test]
async fn uncertain_submission_preserves_original_error_and_does_not_poll_or_retry() {
    let prepared = prepared();
    let hash = prepared.signed.tx_hash();
    let original = json!({"kind":"backendUnavailable","admission":"unknown","transactionHash":hash,"extra":"retained"});
    let (client, _, handle, calls) = endpoint(vec![
        ("getVersionInfo", Ok(discovery())),
        ("getNetworkInfo", Ok(network_result())),
        (
            "sendTransaction",
            Err(ErrorObjectOwned::owned(
                -32050,
                "lost reply",
                Some(original.clone()),
            )),
        ),
    ])
    .await;
    let error = submit_and_wait(
        &client,
        &prepared,
        ReceiptPollConfig::default(),
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.phase, SubmissionPhase::Uncertain);
    let CliError::NativeRpcFailure { evidence, .. } = error.error else {
        panic!("expected native error")
    };
    assert_eq!(evidence["transactionHash"], json!(hash));
    assert_eq!(evidence["cause"]["rpcError"]["data"], original);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| *method)
            .collect::<Vec<_>>(),
        ["getVersionInfo", "getNetworkInfo", "sendTransaction"]
    );
    handle.stop().unwrap();
}

#[tokio::test]
async fn unsupported_receipt_preserves_raw_result_and_commitment() {
    let prepared = prepared();
    let hash = prepared.signed.tx_hash();
    let raw = json!({"transactionHash":hash,"blockHeight":"42","execution":{"result":"future","diagnostic":[1,2]},"events":[],"gasUsed":"1","extra":true});
    let (client, _, handle, _) = endpoint(vec![
        ("getVersionInfo", Ok(discovery())),
        ("getNetworkInfo", Ok(network_result())),
        (
            "sendTransaction",
            Ok(json!({"transactionHash":hash,"admission":"accepted"})),
        ),
        ("getTransactionReceipt", Ok(raw.clone())),
    ])
    .await;
    let error = submit_and_wait(
        &client,
        &prepared,
        ReceiptPollConfig::default(),
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.phase, SubmissionPhase::Acknowledged);
    let CliError::NativeRpcFailure { evidence, .. } = error.error else {
        panic!("expected native error")
    };
    assert_eq!(evidence["cause"]["result"], raw);
    assert_eq!(evidence["commitment"]["blockHeight"], "42");
    assert_eq!(evidence["transactionHash"], json!(hash));
    handle.stop().unwrap();
}

#[test]
fn poll_error_preserves_sticky_commitment_and_original_lookup_error() {
    use thruput_json_rpc_client::native::{
        NativeClientError,
        receipt::{KnownCommitment, ReceiptEvidence},
    };
    let hash = TxHash(NetworkHash::from(SHA256Hash::from([1; 32])));
    let original = ErrorObjectOwned::owned(
        -32050,
        "offline",
        Some(json!({"kind":"backendUnavailable","extra":17})),
    );
    let source = NativeClientError::Application {
        data: Box::new(thruput_json_rpc_types::error::native::DecodedError::decode(
            "getTransactionReceipt",
            json!(original),
        )),
        source: original.clone(),
    };
    let error = native_rpc::poll_error(&ReceiptPollError {
        cause: ReceiptPollCause::Timeout,
        evidence: Box::new(ReceiptEvidence {
            network_hash: NetworkHash::from(network().into_inner()),
            transaction_hash: hash,
            commitment: Some(KnownCommitment {
                network_hash: NetworkHash::from(network().into_inner()),
                transaction_hash: hash,
                block_height: 42.into(),
            }),
            last_status: Some(
                thruput_json_rpc_types::responses::TransactionStatus::Committed {
                    block_height: 42.into(),
                },
            ),
            last_lookup_error: Some(source),
        }),
    });
    let CliError::NativeRpcFailure { evidence, .. } = error else {
        panic!("expected native error")
    };
    assert_eq!(evidence["cause"]["kind"], "timeout");
    assert_eq!(evidence["commitment"]["blockHeight"], "42");
    assert_eq!(
        evidence["lastStatus"],
        json!({"status":"committed","blockHeight":"42"})
    );
    assert_eq!(evidence["lastLookupError"]["rpcError"], json!(original));
}

#[tokio::test]
async fn invalid_poll_configuration_does_not_send() {
    let prepared = prepared();
    let (client, _, handle, calls) = endpoint(vec![("getVersionInfo", Ok(discovery()))]).await;
    let failure = submit_and_wait(
        &client,
        &prepared,
        ReceiptPollConfig {
            interval: std::time::Duration::ZERO,
            ..ReceiptPollConfig::default()
        },
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.phase, SubmissionPhase::NotSent);
    assert!(calls.lock().unwrap().is_empty());
    handle.stop().unwrap();
}

#[tokio::test]
async fn cancellation_before_preflight_is_not_sent_and_makes_no_rpc_call() {
    let prepared = prepared();
    let (client, _, handle, calls) = endpoint(vec![("getVersionInfo", Ok(discovery()))]).await;
    let failure = submit_and_wait(
        &client,
        &prepared,
        ReceiptPollConfig::default(),
        std::future::ready(()),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.phase, SubmissionPhase::NotSent);
    assert!(calls.lock().unwrap().is_empty());
    let CliError::NativeRpcFailure { evidence, .. } = failure.error else {
        panic!("expected native error")
    };
    assert_eq!(
        evidence["transactionHash"],
        json!(prepared.signed.tx_hash())
    );
    let report = NativeCallFailure {
        common: NativeCallCommon::new(&prepared).unwrap(),
        transaction_hash: prepared.signed.tx_hash(),
        submission: failure.phase,
        signed_transaction: HexBytes(
            hex::decode(prepared.signed.tx_hex().trim_start_matches("0x")).unwrap(),
        ),
        evidence,
    };
    assert_eq!(
        serde_json::to_value(report).unwrap()["submission"],
        "notSent"
    );
    handle.stop().unwrap();
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep both submission cancellation boundaries and their evidence assertions together"
)]
async fn cancellation_during_preflight_send_or_receipt_preserves_phase_without_retrying() {
    for (gate_method, expected_phase, expected_wire) in [
        ("getVersionInfo", SubmissionPhase::NotSent, "notSent"),
        ("getNetworkInfo", SubmissionPhase::NotSent, "notSent"),
        ("sendTransaction", SubmissionPhase::Uncertain, "uncertain"),
        (
            "getTransactionReceipt",
            SubmissionPhase::Acknowledged,
            "acknowledged",
        ),
    ] {
        let prepared = prepared();
        let hash = prepared.signed.tx_hash();
        let calls = Calls::default();
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut module = RpcModule::new(calls.clone());
        for method in [
            "getVersionInfo",
            "getNetworkInfo",
            "sendTransaction",
            "getTransactionReceipt",
        ] {
            let reached = reached.clone();
            let release = release.clone();
            module
                .register_async_method(method, move |params, calls, _| {
                    let reached = reached.clone();
                    let release = release.clone();
                    async move {
                        calls
                            .lock()
                            .unwrap()
                            .push((method, params.parse::<Value>()?));
                        if method == gate_method {
                            reached.notify_one();
                            release.notified().await;
                        }
                        Ok::<_, ErrorObjectOwned>(match method {
                            "getVersionInfo" => discovery(),
                            "getNetworkInfo" => network_result(),
                            "sendTransaction" => {
                                json!({"transactionHash": hash, "admission": "accepted"})
                            }
                            _ => Value::Null,
                        })
                    }
                })
                .unwrap();
        }
        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let handle = server.start(module);
        let client = NativeClient::new(HttpClientBuilder::default().build(url).unwrap());
        let failure = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            submit_and_wait(
                &client,
                &prepared,
                ReceiptPollConfig::default(),
                reached.notified(),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(failure.phase, expected_phase);
        let CliError::NativeRpcFailure { evidence, .. } = failure.error else {
            panic!("expected native error")
        };
        assert_eq!(evidence["transactionHash"], json!(hash));
        assert_eq!(evidence["cause"]["kind"], "cancelled");
        assert!(evidence.get("admission").is_none());
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, _)| *method == "sendTransaction")
                .count(),
            usize::from(expected_phase != SubmissionPhase::NotSent)
        );
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, _)| *method == "getTransactionReceipt")
                .count(),
            usize::from(gate_method == "getTransactionReceipt")
        );
        let report = NativeCallFailure {
            common: NativeCallCommon::new(&prepared).unwrap(),
            transaction_hash: hash,
            submission: failure.phase,
            signed_transaction: HexBytes(
                hex::decode(prepared.signed.tx_hex().trim_start_matches("0x")).unwrap(),
            ),
            evidence,
        };
        assert_eq!(
            serde_json::to_value(report).unwrap()["submission"],
            expected_wire
        );
        release.notify_one();
        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn signal_registration_failure_is_not_cancellation() {
    let mut signal = std::pin::pin!(native_rpc::signal_cancellation(std::future::ready(Err(
        std::io::Error::other("registration failed")
    ))));
    let state = std::future::poll_fn(|cx| Poll::Ready(signal.as_mut().poll(cx))).await;
    assert!(state.is_pending());
}

#[test]
fn human_failure_observations_include_safe_commitment_and_status() {
    let fields = failure_observations(
        SubmissionPhase::Acknowledged,
        &json!({
            "commitment": {"blockHeight": "42"}, "lastStatus": {"status": "committed\u{1b}[2J"},
        }),
    );
    assert_eq!(fields[0], ("Submission", "admission acknowledged".into()));
    assert_eq!(fields[1], ("Committed block height", "42".into()));
    assert_eq!(fields[2].0, "Last status");
    assert!(fields[2].1.starts_with("committed"));
    assert!(!fields[2].1.contains('\u{1b}'));
}

#[test]
fn native_rpc_accepts_contract_wallets_without_reinterpreting_their_address() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_, url, handle, calls) = rt.block_on(endpoint(vec![
        ("getVersionInfo", Ok(discovery())),
        ("getNetworkInfo", Ok(network_result())),
        (
            "getChainTip",
            Ok(json!({"blockHeight":"42","blockHash":NetworkHash::from(SHA256Hash::from([7;32]))})),
        ),
    ]));
    let mut sender = sender();
    sender.native_account = None;
    sender.wallet_address = XdrAddress::from([0x37; 32]);
    let (context, expiry) = prepare_context(&sender, &profile(&url), &url, &rt).unwrap();
    assert_eq!(context.entrypoints.source, "legacy_default");
    assert_eq!(expiry, 1042);
    assert!(
        matches!(sender.wallet(context.entrypoints.do_action), thruput_client::Wallet::Legacy { address, .. } if address == sender.wallet_address)
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _)| !method.starts_with("groundhog_"))
    );
    handle.stop().unwrap();
}

#[tokio::test]
async fn authoritative_refusal_is_not_admitted_and_retains_the_signed_envelope() {
    let prepared = prepared();
    let hash = prepared.signed.tx_hash();
    for (admission, returned_hash, phase) in [
        ("rejected", hash, SubmissionPhase::NotAdmitted),
        ("notAttempted", hash, SubmissionPhase::NotAdmitted),
        (
            "rejected",
            thruput_json_rpc_types::transaction::TxHash(SHA256Hash::from([99; 32]).into()),
            SubmissionPhase::Uncertain,
        ),
    ] {
        let original = json!({"kind":"backendUnavailable","admission":admission,"transactionHash":returned_hash});
        let (client, _, handle, calls) = endpoint(vec![
            ("getVersionInfo", Ok(discovery())),
            ("getNetworkInfo", Ok(network_result())),
            (
                "sendTransaction",
                Err(ErrorObjectOwned::owned(
                    -32050,
                    "refused",
                    Some(original.clone()),
                )),
            ),
        ])
        .await;
        let failure = submit_and_wait(
            &client,
            &prepared,
            ReceiptPollConfig::default(),
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.phase, phase);
        let report = NativeCallFailure {
            common: NativeCallCommon::new(&prepared).unwrap(),
            transaction_hash: hash,
            submission: failure.phase,
            signed_transaction: HexBytes(
                hex::decode(prepared.signed.tx_hex().trim_start_matches("0x")).unwrap(),
            ),
            evidence: failure.error.details_json().unwrap(),
        };
        let report = serde_json::to_value(report).unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(report["signedTransaction"], calls[2].1["signedTransaction"]);
        assert_eq!(report["evidence"]["cause"]["rpcError"]["data"], original);
        handle.stop().unwrap();
    }
}
