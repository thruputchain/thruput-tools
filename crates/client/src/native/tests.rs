use super::*;
use crate::{CallRequest, Wallet, prepare_call};
use jsonrpsee::{
    RpcModule,
    http_client::{HttpClient, HttpClientBuilder},
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use thruput_signing::LocalSigner;
use thruput_xdr::{
    contract::ContractEntrypointID,
    types::{ContractAddress, SHA256Hash},
};

type Calls = Arc<Mutex<Vec<(&'static str, Value)>>>;

fn signed() -> SignedCall {
    prepare_call(
        CallRequest {
            wallet: Wallet::Legacy {
                address: ContractAddress::from([1; 32]),
                entrypoint: ContractEntrypointID::from([2; 8]),
            },
            target_address: ContractAddress::from([3; 32]),
            target_entrypoint: ContractEntrypointID::from([4; 8]),
            target_calldata: &[5, 6],
            gas_limit: 1000,
            target_gas_limit: 500,
            block_expiry_height: 100,
            nonce: 1,
        },
        SigningNetworkHash::from(SHA256Hash::from([9; 32])),
    )
    .unwrap()
    .sign(&LocalSigner::from_secret_key([8; 32]))
    .unwrap()
}

async fn endpoint(
    answers: Vec<(&'static str, Result<Value, ErrorObjectOwned>)>,
) -> (NativeClient<HttpClient>, ServerHandle, Calls) {
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    for (method, answer) in answers {
        module
            .register_method(method, move |params, calls, _| {
                let value: Value = params.parse()?;
                calls.lock().unwrap().push((method, value));
                answer.clone()
            })
            .unwrap();
    }
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let handle = server.start(module);
    let client = NativeClient::new(HttpClientBuilder::default().build(url).unwrap());
    (client, handle, calls)
}

fn version(wire: &str, methods: &[&str]) -> Value {
    json!({"wireFormat":wire,"supportedMethods":methods,"implementation":"test"})
}
fn tip(height: i64) -> Value {
    json!({"blockHeight":height.to_string(),"blockHash":NetworkHash::from(SHA256Hash::from([7; 32]))})
}

#[test]
fn wire_compatibility_requires_a_canonical_version_and_supported_major() {
    for wire in ["14.0.0", "14.1.7", "14.99.99"] {
        assert!(compatible_wire_format(wire), "{wire}");
    }
    for wire in [
        "13.0.0",
        "15.0.0",
        "14",
        "14.0",
        "14.0.0.0",
        "014.0.0",
        "14.00.0",
        "14.0.0-beta",
        "14.-1.0",
        "14. 0.0",
        "14.0.18446744073709551616",
    ] {
        assert!(!compatible_wire_format(wire), "{wire}");
    }
}

#[tokio::test]
async fn endpoint_preparation_uses_only_native_reads() {
    let network = NetworkHash::from(SHA256Hash::from([9; 32]));
    let (client, handle, calls) = endpoint(vec![
        (
            "getVersionInfo",
            Ok(version(
                "14.1.0",
                &["sendTransaction", "getTransactionReceipt"],
            )),
        ),
        ("getNetworkInfo", Ok(json!({"networkHash":network}))),
        ("getChainTip", Ok(tip(42))),
    ])
    .await;
    check_endpoint(&client, &["sendTransaction", "getTransactionReceipt"])
        .await
        .unwrap();
    assert_eq!(
        validate_network(
            &client,
            &SigningNetworkHash::from(SHA256Hash::from([9; 32]))
        )
        .await
        .unwrap()
        .network_hash,
        network
    );
    assert_eq!(
        resolve_block_expiry_height(&client, 1000).await.unwrap(),
        1042
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
        ["getVersionInfo", "getNetworkInfo", "getChainTip"]
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, params)| params == &json!({}))
    );
    handle.stop().unwrap();
}

#[tokio::test]
async fn missing_methods_or_incompatible_wire_stop_preparation_without_submission() {
    for (wire, missing) in [("13.0.0", false), ("14.0.0", true)] {
        let (client, handle, calls) = endpoint(vec![(
            "getVersionInfo",
            Ok(version(wire, &["getNetworkInfo"])),
        )])
        .await;
        let result = check_endpoint(
            &client,
            &[
                "sendTransaction",
                "getTransactionReceipt",
                "sendTransaction",
            ],
        )
        .await;
        match result.unwrap_err() {
            NativeRpcError::MissingMethods { methods } if missing => {
                assert_eq!(methods, ["getTransactionReceipt", "sendTransaction"]);
            }
            NativeRpcError::WireFormat { actual } if !missing => assert_eq!(actual, wire),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn malformed_discovery_preserves_original_parse_error_without_fallback() {
    let (client, handle, calls) = endpoint(vec![(
        "getVersionInfo",
        Ok(json!({"version":"stellar","supportedMethods":[],"implementation":"test"})),
    )])
    .await;
    match check_endpoint(&client, &[]).await.unwrap_err() {
        NativeRpcError::InvalidDiscovery { source } => {
            let NativeClientError::InvalidResponse {
                method,
                raw,
                source,
            } = *source
            else {
                panic!("malformed discovery must preserve its raw result");
            };
            assert_eq!(method, "getVersionInfo");
            assert_eq!(raw["version"], "stellar");
            assert!(source.to_string().contains("wireFormat"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn mismatching_pinned_network_is_rejected_without_submission() {
    let (client, handle, calls) = endpoint(vec![(
        "getNetworkInfo",
        Ok(json!({"networkHash":NetworkHash::from(SHA256Hash::from([8; 32]))})),
    )])
    .await;
    match validate_network(
        &client,
        &SigningNetworkHash::from(SHA256Hash::from([9; 32])),
    )
    .await
    .unwrap_err()
    {
        NativeRpcError::NetworkMismatch { expected, actual } => {
            assert_eq!(
                expected,
                SigningNetworkHash::from(SHA256Hash::from([9; 32]))
            );
            assert_eq!(actual, SigningNetworkHash::from(SHA256Hash::from([8; 32])));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn expiry_rejects_overflow_and_nonpositive_offsets() {
    for (height, offset) in [(i64::MAX, 1), (42, 0), (42, -1)] {
        let (client, handle, _) = endpoint(vec![("getChainTip", Ok(tip(height)))]).await;
        assert!(
            matches!(resolve_block_expiry_height(&client,offset).await,Err(NativeRpcError::InvalidExpiry {tip,offset:returned}) if tip==height && returned==offset)
        );
        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn native_admission_and_failed_receipt_submit_exactly_once() {
    let signed = signed();
    let hash = signed.tx_hash();
    let (client, handle, calls) = endpoint(vec![
        ("sendTransaction",Ok(json!({"transactionHash":hash,"admission":"accepted"}))),
        ("getTransactionReceipt",Ok(json!({"transactionHash":hash,"blockHeight":"42","execution":{"result":"abort","abortData":"0x0102"},"events":[],"gasUsed":"200"}))),
    ]).await;
    let admitted = signed.submit(&client).await.unwrap();
    assert_eq!(admitted.tx_hash, hash);
    let receipt = admitted
        .wait(
            &client,
            NetworkHash::from(SHA256Hash::from([9; 32])),
            ReceiptPollConfig::default(),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert!(
        matches!(receipt.execution,responses::ReceiptExecution::Abort{abort_data} if abort_data.0==[1,2])
    );
    assert_eq!(receipt.transaction_hash, hash);
    assert_eq!(receipt.gas_used, 200);
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["sendTransaction", "getTransactionReceipt"]
    );
    let bytes = hex::decode(
        signed
            .tx_hex()
            .strip_prefix("0x")
            .unwrap_or(signed.tx_hex()),
    )
    .unwrap();
    assert_eq!(calls[0].1, json!({"signedTransaction":HexBytes(bytes)}));
    handle.stop().unwrap();
}

#[tokio::test]
async fn native_submission_preserves_unknown_result_and_canonical_hash() {
    let signed = signed();
    let raw = json!({"transactionHash":signed.tx_hash(),"admission":"futureAdmission","extra":{"keep":true}});
    let (client, handle, calls) = endpoint(vec![("sendTransaction", Ok(raw.clone()))]).await;
    match signed.submit(&client).await.unwrap_err() {
        SubmitCallError::Unsupported {
            tx_hash,
            raw: actual,
        } => {
            assert_eq!(tx_hash, signed.tx_hash());
            assert_eq!(actual, raw);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn native_submission_does_not_accept_a_different_hash() {
    let signed = signed();
    let returned = TxHash(NetworkHash::from(SHA256Hash::from([2; 32])));
    let (client, handle, calls) = endpoint(vec![(
        "sendTransaction",
        Ok(json!({"transactionHash":returned,"admission":"accepted"})),
    )])
    .await;
    assert!(
        matches!(signed.submit(&client).await,Err(SubmitCallError::HashMismatch{tx_hash,returned:actual}) if tx_hash==signed.tx_hash() && actual==returned)
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn malformed_native_acknowledgement_stays_uncertain_and_is_not_retried() {
    let signed = signed();
    let (client, handle, calls) = endpoint(vec![(
        "sendTransaction",
        Ok(json!({"transactionHash":signed.tx_hash(),"admission":42})),
    )])
    .await;
    match signed.submit(&client).await.unwrap_err() {
        SubmitCallError::Rpc { tx_hash, source } => {
            assert_eq!(tx_hash, signed.tx_hash());
            let NativeClientError::InvalidResponse {
                method,
                raw,
                source,
            } = *source
            else {
                panic!("malformed acknowledgement must preserve its raw result");
            };
            assert_eq!(method, "sendTransaction");
            assert_eq!(
                *raw,
                json!({"transactionHash":signed.tx_hash(),"admission":42})
            );
            assert!(source.to_string().contains("string"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn native_submission_keeps_the_original_application_error() {
    let signed = signed();
    let data = json!({"kind":"backendUnavailable","admission":"unknown","transactionHash":signed.tx_hash(),"newField":"retained"});
    let original = ErrorObjectOwned::owned(-32050, "lost backend response", Some(data.clone()));
    let (client, handle, calls) = endpoint(vec![("sendTransaction", Err(original))]).await;
    match signed.submit(&client).await.unwrap_err() {
        SubmitCallError::Rpc { tx_hash, source } => {
            assert_eq!(tx_hash, signed.tx_hash());
            let NativeClientError::Application { source, .. } = *source else {
                panic!("expected application error")
            };
            assert_eq!(source.code(), -32050);
            assert_eq!(source.message(), "lost backend response");
            assert_eq!(
                serde_json::from_str::<Value>(source.data().unwrap().get()).unwrap(),
                data
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn cancelled_native_wait_retains_hash_without_resubmitting() {
    let signed = signed();
    let (client, handle, calls) = endpoint(vec![(
        "sendTransaction",
        Ok(json!({"transactionHash":signed.tx_hash(),"admission":"accepted"})),
    )])
    .await;
    let admitted = signed.submit(&client).await.unwrap();
    let error = admitted
        .wait(
            &client,
            NetworkHash::from(SHA256Hash::from([9; 32])),
            ReceiptPollConfig::default(),
            std::future::ready(()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.evidence.transaction_hash, signed.tx_hash());
    assert!(matches!(
        error.cause,
        thruput_json_rpc_client::native::receipt::ReceiptPollCause::Cancelled
    ));
    assert_eq!(calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn missing_discovery_is_incompatible_but_backend_failure_remains_a_call_error() {
    for (code, incompatible) in [(-32601, true), (-32050, false)] {
        let (client, handle, calls) = endpoint(vec![(
            "getVersionInfo",
            Err(ErrorObjectOwned::owned(code, "unavailable", None::<()>)),
        )])
        .await;
        match check_endpoint(&client, &[]).await.unwrap_err() {
            NativeRpcError::InvalidDiscovery { source } if incompatible => {
                assert!(matches!(*source, NativeClientError::Application { .. }));
            }
            NativeRpcError::Call { method, source } if !incompatible => {
                assert_eq!(method, "getVersionInfo");
                assert!(matches!(*source, NativeClientError::Application { .. }));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn timed_out_wait_retains_canonical_hash_and_network_without_resubmitting() {
    let signed = signed();
    let (client, handle, calls) = endpoint(vec![
        (
            "sendTransaction",
            Ok(json!({"transactionHash":signed.tx_hash(),"admission":"accepted"})),
        ),
        ("getTransactionReceipt", Ok(Value::Null)),
        ("getTransactionStatus", Ok(json!({"status":"unknown"}))),
    ])
    .await;
    let admitted = signed.submit(&client).await.unwrap();
    let network = NetworkHash::from(SHA256Hash::from([9; 32]));
    let error = admitted
        .wait(
            &client,
            network,
            ReceiptPollConfig {
                timeout: std::time::Duration::from_millis(1),
                ..ReceiptPollConfig::default()
            },
            std::future::pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.evidence.transaction_hash, signed.tx_hash());
    assert_eq!(error.evidence.network_hash, network);
    assert!(matches!(
        error.cause,
        thruput_json_rpc_client::native::receipt::ReceiptPollCause::Timeout
    ));
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| *method == "sendTransaction")
            .count(),
        1
    );
    handle.stop().unwrap();
}
