use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use thruput_json_rpc_types::ContractAddress;
use thruput_json_rpc_types::base58::Base58;
use thruput_json_rpc_types::network_descriptor::{
    NETWORK_DESCRIPTOR_SCHEMA_VERSION, NetworkDescriptor,
};
use thruput_json_rpc_types::responses::{ReceiptExecution, TransactionReceipt, TransactionStatus};
use thruput_json_rpc_types::transaction::TxHash;
use tower::ServiceExt;

use thruput_faucet::{DEFAULT_LISTEN_ADDRESS, FaucetConfig, airdrop, app, config, policy};

#[test]
fn default_config_uses_local_listen_address() {
    let config = test_config();

    assert_eq!(config.listen_address, DEFAULT_LISTEN_ADDRESS);
}

#[tokio::test]
async fn serve_stops_when_shutdown_future_resolves() {
    let mut config = test_config();
    config.listen_address = "127.0.0.1:0".into();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server = tokio::spawn(thruput_faucet::serve_with_shutdown(config, async move {
        let _ = shutdown_rx.await;
    }));

    shutdown_tx
        .send(())
        .expect("bug: shutdown receiver should be alive");
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("bug: server should stop after shutdown future resolves")
        .expect("bug: server task should join")
        .expect("bug: server should shut down cleanly");
}

#[tokio::test]
async fn health_returns_ok() {
    let response = app(test_config())
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("bug: health request should build"),
        )
        .await
        .expect("bug: health request should complete");

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .expect("bug: health body should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: health body should be JSON");
    assert_eq!(json, serde_json::json!({ "status": "ok" }));
}

#[tokio::test]
async fn network_returns_public_descriptor() {
    let descriptor = test_network_descriptor();
    let response = app(FaucetConfig::new(
        descriptor.clone(),
        Arc::new(MockAirdropSubmitter::success()),
    )
    .expect("bug: test faucet config should build"))
    .oneshot(
        Request::builder()
            .uri("/v1/network")
            .body(Body::empty())
            .expect("bug: network request should build"),
    )
    .await
    .expect("bug: network request should complete");

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bug: network body should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: network body should be JSON");
    assert_eq!(json["schemaVersion"], NETWORK_DESCRIPTOR_SCHEMA_VERSION);
    assert_eq!(json["networkId"], "devnet-preview");
    assert!(json["endpoints"].get("faucetUrl").is_none());
    assert_eq!(
        json["nativeToken"]["transferEntrypoint"],
        "0x63285b3523f9dc3f"
    );
    assert_eq!(json["airdrop"]["rawAmount"], "100000000");
    assert!(json["airdrop"].get("amountTokens").is_none());
    assert_eq!(json["airdrop"]["recipientCooldownSeconds"], "43200");
    assert_eq!(json["airdrop"]["clientQuotaMax"], 10);
    assert_eq!(json["airdrop"]["clientQuotaWindowSeconds"], "3600");
    assert_eq!(json["airdrop"]["maxPendingPerRecipient"], 1);
    assert!(json.get("bootstrapAccountDerivation").is_none());
    assert!(json.get("bootstrapAccounts").is_none());
    assert!(json.get("privateKey").is_none());
    assert!(json.get("walletCreate").is_none());
    assert!(json.get("devWallet").is_none());

    let returned: NetworkDescriptor =
        serde_json::from_slice(&body).expect("bug: network body should deserialize");
    assert_eq!(returned, descriptor);
}

#[tokio::test]
async fn wallet_creation_endpoint_is_retired() {
    let response = app(test_config())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/create-wallet")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn airdrop_validates_request_then_submits() {
    let response = app(test_config())
        .oneshot(request_with_connect_info(
            Request::builder()
                .method("POST")
                .uri("/v1/airdrop")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "recipient": identifier(0x11),
                        "clientNonce": "0x01",
                        "networkId": "devnet-preview",
                        "networkHash": identifier(0),
                        "genesisHash": identifier(0)
                    })
                    .to_string(),
                ))
                .expect("bug: airdrop request should build"),
        ))
        .await
        .expect("bug: airdrop request should complete");

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bug: airdrop body should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
    assert_eq!(
        json,
        serde_json::json!({
            "attemptStatus": "succeeded",
            "recipient": identifier(0x11),
            "txHash": identifier(0x22),
            "rawAmount": "100000000",
            "height": "42",
            "status": { "result": "ok" },
            "returnData": "0x"
        })
    );
}

#[tokio::test]
async fn airdrop_rejects_invalid_request_fields_with_structured_error() {
    struct Case {
        name: &'static str,
        field: &'static str,
        value: serde_json::Value,
        code: &'static str,
    }
    let cases = [
        Case {
            name: "mismatched genesis hash",
            field: "genesisHash",
            value: serde_json::json!(identifier(0x99)),
            code: "airdrop_network_mismatch",
        },
        Case {
            name: "mismatched network hash",
            field: "networkHash",
            value: serde_json::json!(identifier(0x99)),
            code: "airdrop_network_mismatch",
        },
        Case {
            name: "empty client nonce",
            field: "clientNonce",
            value: serde_json::json!("0x"),
            code: "airdrop_invalid_client_nonce",
        },
        Case {
            name: "client nonce over 32 bytes",
            field: "clientNonce",
            value: serde_json::json!(format!("0x{}", "01".repeat(33))),
            code: "airdrop_invalid_client_nonce",
        },
        Case {
            name: "odd-length client nonce",
            field: "clientNonce",
            value: serde_json::json!("0x1"),
            code: "airdrop_invalid_client_nonce",
        },
        Case {
            name: "non-string client nonce",
            field: "clientNonce",
            value: serde_json::json!(1),
            code: "airdrop_invalid_client_nonce",
        },
    ];

    for case in cases {
        let mut body = airdrop_body();
        body[case.field] = case.value;

        let response = airdrop_request_with_body(&test_config(), body).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{}", case.name);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap_or_else(|error| panic!("{}: airdrop body should read: {error}", case.name));
        let json: serde_json::Value = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("{}: airdrop body should be JSON: {error}", case.name));
        assert_eq!(json["error"]["code"], case.code, "{}", case.name);
    }
}

#[tokio::test]
async fn airdrop_retry_returns_recorded_success() {
    let airdrop = Arc::new(MockAirdropSubmitter::success());
    let config = test_config_with_submitters(airdrop.clone());
    let response = airdrop_request(&config).await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(airdrop.calls(), 1);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bug: airdrop body should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
    assert_eq!(json["attemptStatus"], "succeeded");
    assert_eq!(json["txHash"], identifier(0x22));
}

#[test]
fn airdrop_submitter_amount_must_match_network_descriptor() {
    let error = FaucetConfig::new(
        test_network_descriptor(),
        Arc::new(MockAirdropSubmitter::success_with_raw_amount("200000000")),
    )
    .expect_err("bug: mismatched airdrop amount should fail");

    assert!(matches!(
        error,
        thruput_faucet::config::FaucetConfigError::SubmitterAmountMismatch {
            submitter: "airdrop submitter",
            expected: 100_000_000,
            actual: 200_000_000,
        }
    ));
}

#[test]
fn faucet_config_requires_airdrop_descriptor() {
    let mut descriptor = test_network_descriptor();
    descriptor.airdrop = None;

    let error = FaucetConfig::new(descriptor, Arc::new(MockAirdropSubmitter::success()))
        .expect_err("bug: missing airdrop descriptor should fail");

    assert!(matches!(
        error,
        thruput_faucet::config::FaucetConfigError::MissingAirdropDescriptor
    ));
}

#[test]
fn faucet_config_rejects_invalid_airdrop_descriptor_amount() {
    let mut descriptor = test_network_descriptor();
    descriptor
        .airdrop
        .as_mut()
        .expect("bug: test descriptor should advertise airdrops")
        .raw_amount = "invalid".into();

    let error = FaucetConfig::new(descriptor, Arc::new(MockAirdropSubmitter::success()))
        .expect_err("bug: invalid airdrop descriptor amount should fail");

    assert!(matches!(
        error,
        thruput_faucet::config::FaucetConfigError::InvalidDescriptorAmount {
            field: "airdrop.rawAmount",
            ref value,
        } if value == "invalid"
    ));
}

#[tokio::test]
async fn airdrop_rejects_mismatched_accepted_amount_without_resubmitting() {
    let airdrop = Arc::new(MockAirdropSubmitter::success_with_mismatched_accepted_amount());
    let config = test_config_with_submitters(airdrop.clone());

    let response = airdrop_request(&config).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(airdrop.calls(), 1);
    assert_eq!(airdrop.poll_calls(), 0);
    assert!(airdrop.broadcasts.lock().unwrap().is_empty());
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(
        attempt.error_code.as_deref(),
        Some("airdrop_submit_amount_mismatch")
    );
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::FailedTerminal);
}

#[tokio::test]
async fn airdrop_final_store_failure_retains_accepted_transaction_for_retry() {
    let mut config = test_config_with_submitters(Arc::new(MockAirdropSubmitter::success()));
    config.policy_store = Arc::new(FailFinalPutPolicyStore::fail_succeeded_airdrop());

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
    assert_eq!(attempt.tx_hash.as_deref(), Some(identifier(0x22).as_str()));
    assert!(attempt.expires_at.is_none());
}

#[tokio::test]
async fn airdrop_duplicate_in_flight_returns_pending_attempt() {
    let config = test_config();
    seed_airdrop_attempt(&config, policy::AirdropAttemptStatus::Executing, None);

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bug: airdrop body should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
    assert_eq!(json["attemptStatus"], "pending");
    assert_eq!(json["recipient"], identifier(0x11));
    assert_eq!(json["retryAfterSeconds"], "5");
    assert!(json.get("txHash").is_none());
}

#[tokio::test]
async fn airdrop_pending_response_matches_shared_wire_type() {
    let config = test_config_with_submitters(Arc::new(MockAirdropSubmitter::pending()));

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bug: pending airdrop body should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: pending airdrop body should be JSON");
    assert_eq!(
        json,
        serde_json::json!({
            "attemptStatus": "pending",
            "recipient": identifier(0x11),
            "txHash": identifier(0x22),
            "rawAmount": "100000000",
            "retryAfterSeconds": "5"
        })
    );
}

#[tokio::test]
async fn configured_retained_capacity_rejects_new_keys_but_serves_retries() {
    let airdrop = Arc::new(MockAirdropSubmitter::success());
    let mut config = test_config_with_submitters(airdrop.clone());
    config.policy_config.airdrop.max_retained_attempts = 1;
    assert_eq!(airdrop_request(&config).await.status(), StatusCode::OK);
    let mut second = airdrop_body();
    second["recipient"] = serde_json::json!(identifier(0x12));
    let response = airdrop_request_with_body(&config, second.clone()).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "airdrop_capacity_exceeded");
    assert_eq!(airdrop_request(&config).await.status(), StatusCode::OK);
    assert_eq!(airdrop.calls(), 1);
    config.policy_config.airdrop.max_retained_attempts = 2;
    assert_eq!(
        airdrop_request_with_body(&config, second).await.status(),
        StatusCode::OK
    );
    assert_eq!(airdrop.calls(), 2);
}

#[tokio::test]
async fn concurrent_same_key_airdrops_submit_once_with_zero_cooldown() {
    let airdrop = Arc::new(MockAirdropSubmitter::success());
    let mut config = test_config_with_submitters(airdrop.clone());
    config.policy_config.airdrop.recipient_cooldown_secs = 0;

    let (first, second) = tokio::join!(airdrop_request(&config), airdrop_request(&config));

    assert!(
        matches!(first.status(), StatusCode::OK | StatusCode::ACCEPTED)
            && matches!(second.status(), StatusCode::OK | StatusCode::ACCEPTED)
    );
    assert_eq!(airdrop.calls(), 1);
}

#[tokio::test]
async fn concurrent_same_key_pending_retries_poll_once_without_consuming_capacity() {
    let poll_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut submitter = MockAirdropSubmitter::success();
    submitter.poll_gate = Some(poll_gate.clone());
    let airdrop = Arc::new(submitter);
    let config = test_config_with_submitters(airdrop.clone());
    seed_airdrop_attempt(
        &config,
        policy::AirdropAttemptStatus::Pending,
        Some(identifier(0x22)),
    );
    let owner = {
        let config = config.clone();
        tokio::spawn(async move { airdrop_request(&config).await })
    };
    while airdrop.poll_calls() == 0 {
        tokio::task::yield_now().await;
    }

    let mut retries = Vec::new();
    for _ in 0..=64 {
        let config = config.clone();
        retries.push(tokio::spawn(async move { airdrop_request(&config).await }));
    }
    for retry in retries {
        let response = tokio::time::timeout(Duration::from_secs(1), retry)
            .await
            .expect("bug: same-key retry should not wait for execution capacity")
            .expect("bug: same-key retry task should complete");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    assert_eq!(airdrop.calls(), 0);
    assert_eq!(airdrop.poll_calls(), 1);

    poll_gate.add_permits(1);
    let response = owner.await.expect("bug: poll owner task should complete");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn airdrop_execution_capacity_bounds_detached_workers() {
    let mut submitter = MockAirdropSubmitter::success();
    let submit_gate = Arc::new(tokio::sync::Semaphore::new(0));
    submitter.submit_gate = Some(submit_gate.clone());
    let airdrop = Arc::new(submitter);
    let mut config = test_config_with_submitters(airdrop.clone());
    config.policy_config.airdrop.ip_quota_max = 65;
    let policy_store = config.policy_store.clone();
    let router = app(config);
    let mut tasks = Vec::new();
    for value in 1_u8..=64 {
        let mut body = airdrop_body();
        body["recipient"] = serde_json::json!(identifier(value));
        body["clientNonce"] = serde_json::json!(format!("0x{value:02x}"));
        let request = request_with_connect_info(
            Request::builder()
                .method("POST")
                .uri("/v1/airdrop")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("bug: airdrop request should build"),
        );
        let router = router.clone();
        tasks.push(tokio::spawn(async move { router.oneshot(request).await }));
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while airdrop.calls() < 64 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bug: detached workers should reach the submitter");
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        assert!(
            task.await
                .expect_err("canceled request task should not complete")
                .is_cancelled()
        );
    }
    let mut overflow_body = airdrop_body();
    overflow_body["recipient"] = serde_json::json!(identifier(65));
    overflow_body["clientNonce"] = serde_json::json!("0x41");

    let response = router
        .clone()
        .oneshot(request_with_connect_info(
            Request::builder()
                .method("POST")
                .uri("/v1/airdrop")
                .header("content-type", "application/json")
                .body(Body::from(overflow_body.to_string()))
                .expect("bug: airdrop request should build"),
        ))
        .await
        .expect("bug: capacity request should complete");

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bug: capacity response should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: capacity response should be JSON");
    assert_eq!(json["error"]["code"], "airdrop_capacity_exceeded");
    let overflow_key = airdrop_attempt_key_for(65, "41");
    assert!(
        policy_store
            .get_airdrop_attempt(&overflow_key)
            .expect("bug: capacity-rejected attempt lookup should succeed")
            .is_none()
    );
    submit_gate.add_permits(64);
    tokio::time::timeout(Duration::from_secs(1), async {
        while airdrop.poll_calls() < 64 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bug: detached workers should finish after capacity is released");

    submit_gate.add_permits(1);
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        router.oneshot(request_with_connect_info(
            Request::builder()
                .method("POST")
                .uri("/v1/airdrop")
                .header("content-type", "application/json")
                .body(Body::from(overflow_body.to_string()))
                .expect("bug: retry request should build"),
        )),
    )
    .await
    .expect("bug: capacity-rejected request should remain retryable")
    .expect("bug: retry request should complete");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn airdrop_execution_timeout_releases_capacity_without_resubmitting() {
    let mut submitter = MockAirdropSubmitter::success();
    submitter.submit_gate = Some(Arc::new(tokio::sync::Semaphore::new(0)));
    let airdrop = Arc::new(submitter);
    let config = test_config_with_submitters(airdrop.clone());
    let request = {
        let config = config.clone();
        tokio::spawn(async move { airdrop_request(&config).await })
    };
    while airdrop.calls() == 0 {
        tokio::task::yield_now().await;
    }

    tokio::time::advance(Duration::from_mins(10) + Duration::from_secs(1)).await;
    let response = request
        .await
        .expect("bug: timed-out request should complete");

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be retained");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Ambiguous);
    assert_eq!(airdrop.calls(), 1);

    let response = airdrop_request(&config).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(airdrop.calls(), 1);
}

#[tokio::test]
async fn canceled_request_does_not_strand_pending_airdrop() {
    let mut submitter = MockAirdropSubmitter::success();
    submitter.submit_delay = Some(Duration::from_millis(25));
    let airdrop = Arc::new(submitter);
    let config = test_config_with_submitters(airdrop.clone());
    let request = {
        let config = config.clone();
        tokio::spawn(async move { airdrop_request(&config).await })
    };
    while airdrop.calls() == 0 {
        tokio::task::yield_now().await;
    }

    request.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Succeeded);
    assert_eq!(airdrop.calls(), 1);
}

#[tokio::test]
async fn canceled_request_still_recovers_panicking_submission_task() {
    let config = test_config_with_submitters(Arc::new(PanickingAirdropSubmitter));
    let request = {
        let config = config.clone();
        tokio::spawn(async move { airdrop_request(&config).await })
    };
    while stored_airdrop_attempt(&config).is_none() {
        tokio::task::yield_now().await;
    }

    request.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::FailedTerminal);
    assert_eq!(attempt.error_code.as_deref(), Some("airdrop_submit_failed"));
}

#[tokio::test]
async fn panicking_poll_does_not_overwrite_accepted_transaction() {
    let config = test_config_with_submitters(Arc::new(PanickingPollAirdropSubmitter));

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
    assert_eq!(attempt.tx_hash, Some(identifier(0x22)));
}

#[tokio::test]
async fn terminal_same_key_attempt_is_retained_with_zero_cooldown() {
    let mut config = test_config();
    config.policy_config.airdrop.recipient_cooldown_secs = 0;
    seed_airdrop_attempt(
        &config,
        policy::AirdropAttemptStatus::FailedTerminal,
        Some(identifier(0x22)),
    );

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        stored_airdrop_attempt(&config).map(|attempt| attempt.status),
        Some(policy::AirdropAttemptStatus::FailedTerminal)
    );
}

#[tokio::test]
async fn airdrop_accepted_transaction_is_polled_without_resubmission() {
    let airdrop = Arc::new(MockAirdropSubmitter::success());
    let config = test_config_with_submitters(airdrop.clone());
    seed_airdrop_attempt(
        &config,
        policy::AirdropAttemptStatus::Pending,
        Some(identifier(0x22)),
    );

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(airdrop.calls(), 0);
    assert_eq!(airdrop.poll_calls(), 1);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Succeeded);
}

#[tokio::test]
async fn airdrop_poll_retry_reuses_accepted_transaction() {
    let airdrop = Arc::new(MockAirdropSubmitter::poll_fail_once());
    let config = test_config_with_submitters(airdrop.clone());

    let response = airdrop_request(&config).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
    assert!(attempt.tx_hash.is_some());
    assert!(attempt.expires_at.is_none());

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(airdrop.calls(), 1);
    assert_eq!(airdrop.poll_calls(), 2);
}

#[tokio::test]
async fn airdrop_invalid_transaction_becomes_terminal() {
    let airdrop = Arc::new(MockAirdropSubmitter::invalid());
    let config = test_config_with_submitters(airdrop.clone());

    let response = airdrop_request(&config).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::FailedTerminal);
    assert!(attempt.expires_at.is_some());

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(airdrop.calls(), 1);
    assert_eq!(airdrop.poll_calls(), 1);
}

#[tokio::test]
async fn airdrop_never_expires_executing_attempt() {
    let airdrop = Arc::new(MockAirdropSubmitter::success());
    let config = test_config_with_submitters(airdrop.clone());
    seed_airdrop_attempt_with_expiry(
        &config,
        policy::AirdropAttemptStatus::Executing,
        None,
        Some(SystemTime::UNIX_EPOCH),
    );

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(airdrop.calls(), 0);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Executing);
}

#[tokio::test]
async fn airdrop_never_reexecutes_ambiguous_attempt() {
    let airdrop = Arc::new(MockAirdropSubmitter::success());
    let config = test_config_with_submitters(airdrop.clone());
    seed_airdrop_attempt_with_expiry(
        &config,
        policy::AirdropAttemptStatus::Ambiguous,
        None,
        Some(SystemTime::UNIX_EPOCH),
    );

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(airdrop.calls(), 0);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Ambiguous);
}

#[tokio::test]
async fn airdrop_quota_rejects_before_submit() {
    let airdrop = Arc::new(MockAirdropSubmitter::success());
    let mut config = test_config_with_submitters(airdrop.clone());
    config.policy_config.airdrop.ip_quota_max = 0;

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(airdrop.calls(), 0);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bug: rejection body should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: rejection body should be JSON");
    assert_eq!(json["error"]["code"], "airdrop_quota_exceeded");
}

#[tokio::test]
async fn airdrop_socket_ip_override_uses_finite_override() {
    let mut config = test_config();
    config.policy_config.trust_proxy_client_ip_headers = true;
    config.policy_config.airdrop.ip_quota_max = 0;
    config
        .policy_config
        .airdrop
        .client_overrides
        .push(config::AirdropClientOverride {
            ip: "127.0.0.1".parse().expect("bug: override IP should parse"),
            quota_max: 1,
            quota_window_secs: 60,
        });
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/airdrop")
        .header("content-type", "application/json")
        .header("x-forwarded-for", "203.0.113.10")
        .body(Body::from(airdrop_body().to_string()))
        .expect("bug: airdrop request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));

    let response = app(config.clone())
        .oneshot(request)
        .await
        .expect("bug: request should complete");
    let mut second_body = airdrop_body();
    second_body["recipient"] = serde_json::json!(identifier(0x12));
    second_body["clientNonce"] = serde_json::json!("0x02");
    let second_response = airdrop_request_with_body(&config, second_body).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(second_response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn airdrop_unconfigured_socket_ip_uses_default_quota() {
    let mut config = test_config();
    config.policy_config.airdrop.ip_quota_max = 0;
    config
        .policy_config
        .airdrop
        .client_overrides
        .push(config::AirdropClientOverride {
            ip: "127.0.0.2".parse().expect("bug: override IP should parse"),
            quota_max: 1,
            quota_window_secs: 60,
        });

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn airdrop_retries_failed_submit_attempt() {
    let airdrop = Arc::new(MockAirdropSubmitter::fail_once());
    let config = test_config_with_submitters(airdrop.clone());
    let response = airdrop_request(&config).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        stored_airdrop_attempt(&config)
            .expect("bug: failed attempt should be stored")
            .expires_at
            .is_some()
    );

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(airdrop.calls(), 2);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Succeeded);
}

#[tokio::test]
async fn airdrop_does_not_retry_ambiguous_submit_error() {
    let airdrop = Arc::new(MockAirdropSubmitter::ambiguous_submit());
    let config = test_config_with_submitters(airdrop.clone());

    let response = airdrop_request(&config).await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be retained");
    assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
    assert!(attempt.expires_at.is_none());

    let response = airdrop_request(&config).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(airdrop.calls(), 1);
    assert_eq!(airdrop.broadcasts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn airdrop_rejects_malformed_recipient() {
    let response = app(test_config())
        .oneshot(request_with_connect_info(
            Request::builder()
                .method("POST")
                .uri("/v1/airdrop")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "recipient": "0xdeadbeef",
                        "clientNonce": "0x01",
                        "networkId": "devnet-preview",
                        "networkHash": identifier(0),
                        "genesisHash": identifier(0)
                    })
                    .to_string(),
                ))
                .expect("bug: airdrop request should build"),
        ))
        .await
        .expect("bug: airdrop request should complete");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn airdrop_submit_failure_returns_bad_gateway() {
    let response = app(FaucetConfig::new(
        test_network_descriptor(),
        Arc::new(MockAirdropSubmitter::failure()),
    )
    .expect("bug: test faucet config should build"))
    .oneshot(request_with_connect_info(
        Request::builder()
            .method("POST")
            .uri("/v1/airdrop")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "recipient": identifier(0x11),
                    "clientNonce": "0x01",
                    "networkId": "devnet-preview",
                    "networkHash": identifier(0),
                    "genesisHash": identifier(0)
                })
                .to_string(),
            ))
            .expect("bug: airdrop request should build"),
    ))
    .await
    .expect("bug: airdrop request should complete");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bug: airdrop body should read");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
    assert_eq!(json["error"]["code"], "airdrop_submit_failed");
}

fn test_config() -> FaucetConfig {
    test_config_with_store()
}

fn test_config_with_store() -> FaucetConfig {
    let airdrop_submitter = Arc::new(MockAirdropSubmitter::success());
    FaucetConfig::new(test_network_descriptor(), airdrop_submitter)
        .expect("bug: test faucet config should build")
}

fn test_config_with_submitters(
    airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
) -> FaucetConfig {
    FaucetConfig::new(test_network_descriptor(), airdrop_submitter)
        .expect("bug: test faucet config should build")
}

async fn airdrop_request(config: &FaucetConfig) -> axum::response::Response {
    airdrop_request_with_body(config, airdrop_body()).await
}

async fn airdrop_request_with_body(
    config: &FaucetConfig,
    body: serde_json::Value,
) -> axum::response::Response {
    app(config.clone())
        .oneshot(request_with_connect_info(
            Request::builder()
                .method("POST")
                .uri("/v1/airdrop")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("bug: airdrop request should build"),
        ))
        .await
        .expect("bug: airdrop request should complete")
}

// All callers use this fixture for a 32-byte typed identifier, never raw bytes.
fn identifier(byte: u8) -> String {
    thruput_json_rpc_types::NetworkHashText::from([byte; 32]).to_string()
}

fn airdrop_body() -> serde_json::Value {
    serde_json::json!({
        "recipient": identifier(0x11),
        "clientNonce": "0x01",
        "networkId": "devnet-preview",
        "networkHash": identifier(0),
        "genesisHash": identifier(0)
    })
}

fn request_with_connect_info(mut request: Request<Body>) -> Request<Body> {
    let addr: SocketAddr = "127.0.0.1:12345"
        .parse()
        .expect("bug: test socket address should parse");
    request.extensions_mut().insert(ConnectInfo(addr));
    request
}

fn airdrop_attempt_key() -> policy::AirdropAttemptKey {
    airdrop_attempt_key_for(0x11, "01")
}

fn airdrop_attempt_key_for(recipient_byte: u8, client_nonce: &str) -> policy::AirdropAttemptKey {
    policy::AirdropAttemptKey {
        network_id: "devnet-preview".into(),
        genesis_hash: identifier(0),
        recipient: identifier(recipient_byte),
        client_nonce: client_nonce.into(),
    }
}

fn seed_airdrop_attempt(
    config: &FaucetConfig,
    status: policy::AirdropAttemptStatus,
    tx_hash: Option<String>,
) {
    seed_airdrop_attempt_with_expiry(config, status, tx_hash, None);
}

fn seed_airdrop_attempt_with_expiry(
    config: &FaucetConfig,
    status: policy::AirdropAttemptStatus,
    tx_hash: Option<String>,
    expires_at: Option<SystemTime>,
) {
    let now = SystemTime::UNIX_EPOCH;
    config
        .policy_store
        .put_airdrop_attempt(policy::AirdropAttempt {
            key: airdrop_attempt_key(),
            client_identity: policy::ClientIdentity::Unknown,
            raw_amount: "100000000".into(),
            status,
            execution_id: u64::from(status == policy::AirdropAttemptStatus::Executing),
            tx_hash,
            transaction: None,
            height: None,
            return_data: None,
            error_code: None,
            created_at: now,
            updated_at: now,
            expires_at,
        })
        .expect("bug: test airdrop attempt insert should succeed");
}

fn stored_airdrop_attempt(config: &FaucetConfig) -> Option<policy::AirdropAttempt> {
    config
        .policy_store
        .get_airdrop_attempt(&airdrop_attempt_key())
        .expect("bug: test airdrop attempt fetch should succeed")
}

struct FailFinalPutPolicyStore {
    inner: policy::InMemoryFaucetPolicyStore,
    fail_succeeded_airdrop: bool,
    fail_prepared_airdrop: bool,
}

impl FailFinalPutPolicyStore {
    fn fail_succeeded_airdrop() -> Self {
        Self {
            inner: policy::InMemoryFaucetPolicyStore::new(),
            fail_succeeded_airdrop: true,
            fail_prepared_airdrop: false,
        }
    }
}

impl policy::FaucetPolicyStore for FailFinalPutPolicyStore {
    fn get_airdrop_attempt(
        &self,
        key: &policy::AirdropAttemptKey,
    ) -> Result<Option<policy::AirdropAttempt>, policy::PolicyStoreError> {
        self.inner.get_airdrop_attempt(key)
    }

    fn put_airdrop_attempt(
        &self,
        attempt: policy::AirdropAttempt,
    ) -> Result<Option<policy::AirdropAttempt>, policy::PolicyStoreError> {
        if self.fail_succeeded_airdrop && attempt.status == policy::AirdropAttemptStatus::Succeeded
        {
            return Err(policy::PolicyStoreError::LockPoisoned);
        }
        self.inner.put_airdrop_attempt(attempt)
    }

    fn admit_airdrop_attempt(
        &self,
        attempt: policy::AirdropAttempt,
        attempt_capacity: usize,
        max_pending_per_recipient: u32,
        quota_checks: &[policy::QuotaCheck],
        now: SystemTime,
    ) -> Result<policy::AdmitAirdropAttemptResult, policy::PolicyStoreError> {
        self.inner.admit_airdrop_attempt(
            attempt,
            attempt_capacity,
            max_pending_per_recipient,
            quota_checks,
            now,
        )
    }

    fn claim_airdrop_transition(
        &self,
        key: &policy::AirdropAttemptKey,
        expected: &[policy::AirdropAttemptStatus],
        expected_execution_id: Option<u64>,
        next: policy::AirdropAttemptStatus,
        now: SystemTime,
    ) -> Result<policy::AirdropAttempt, policy::PolicyStoreError> {
        self.inner
            .claim_airdrop_transition(key, expected, expected_execution_id, next, now)
    }

    fn replace_airdrop_attempt_if_status(
        &self,
        expected: &[policy::AirdropAttemptStatus],
        attempt: policy::AirdropAttempt,
    ) -> Result<policy::AirdropAttempt, policy::PolicyStoreError> {
        if self.fail_prepared_airdrop && attempt.transaction.is_some() {
            return Err(policy::PolicyStoreError::LockPoisoned);
        }
        if self.fail_succeeded_airdrop && attempt.status == policy::AirdropAttemptStatus::Succeeded
        {
            return Err(policy::PolicyStoreError::LockPoisoned);
        }
        self.inner
            .replace_airdrop_attempt_if_status(expected, attempt)
    }

    fn check_and_record_quota(
        &self,
        key: policy::QuotaKey,
        max: u32,
        window: Duration,
        now: SystemTime,
    ) -> Result<policy::QuotaDecision, policy::PolicyStoreError> {
        self.inner.check_and_record_quota(key, max, window, now)
    }

    fn check_and_record_quotas(
        &self,
        checks: &[policy::QuotaCheck],
        now: SystemTime,
    ) -> Result<policy::QuotaDecision, policy::PolicyStoreError> {
        self.inner.check_and_record_quotas(checks, now)
    }

    fn expire(&self, now: SystemTime) -> Result<(), policy::PolicyStoreError> {
        self.inner.expire(now)
    }
}

fn test_network_descriptor() -> NetworkDescriptor {
    serde_json::from_value(serde_json::json!({
        "schemaVersion": NETWORK_DESCRIPTOR_SCHEMA_VERSION,
        "kind": "groundhog-network",
        "networkId": "devnet-preview",
        "networkHash": identifier(0),
        "protocol": "x23",
        "genesisHash": identifier(0),
        "endpoints": {
            "rpcUrl": "https://rpc.devnet.example"
        },
        "nativeToken": {
            "tokenAddress": identifier(0),
            "decimals": 7,
            "transferEntrypoint": "0x63285b3523f9dc3f"
        },
        "faucet": {
            "walletAddress": identifier(0)
        },
        "airdrop": {
            "rawAmount": "100000000",
            "recipientCooldownSeconds": "43200",
            "clientQuotaMax": 10,
            "clientQuotaWindowSeconds": "3600",
            "maxPendingPerRecipient": 1
        }
    }))
    .expect("bug: test network descriptor should deserialize")
}

enum PollObservation {
    None,
    Committed,
    LookupFailed,
}

struct MockAirdropSubmitter {
    failures_remaining: Mutex<usize>,
    poll_failures_remaining: Mutex<usize>,
    calls: Mutex<usize>,
    poll_calls: Mutex<usize>,
    broadcasts: Mutex<Vec<airdrop::PreparedAirdrop>>,
    poll_status: Mutex<Option<TransactionStatus>>,
    receipt_override: Mutex<Option<TransactionReceipt>>,
    submit_error: Option<&'static str>,
    observed_heights: Mutex<Vec<Option<i64>>>,
    poll_observation: PollObservation,
    raw_amount: String,
    accepted_raw_amount: String,
    poll_times_out: bool,
    poll_invalid: bool,
    ambiguous_submit: bool,
    submit_delay: Option<Duration>,
    submit_gate: Option<Arc<tokio::sync::Semaphore>>,
    poll_gate: Option<Arc<tokio::sync::Semaphore>>,
}

struct PanickingAirdropSubmitter;
struct PanickingPollAirdropSubmitter;

#[async_trait::async_trait]
impl airdrop::AirdropSubmitter for PanickingAirdropSubmitter {
    fn raw_amount(&self) -> u128 {
        100_000_000
    }

    async fn prepare_airdrop(
        &self,
        _recipient: ContractAddress,
    ) -> Result<airdrop::PreparedAirdrop, airdrop::AirdropSubmitError> {
        tokio::time::sleep(Duration::from_millis(25)).await;
        panic!("intentional submission panic")
    }

    async fn submit_airdrop(
        &self,
        _transaction: &airdrop::PreparedAirdrop,
    ) -> Result<(), airdrop::AirdropSubmitError> {
        Ok(())
    }

    async fn poll_airdrop(
        &self,
        _tx_hash: TxHash,
        _committed_height: Option<i64>,
    ) -> Result<TransactionReceipt, airdrop::AirdropSubmitError> {
        unreachable!("panicking submitter never accepts a transaction")
    }
}

#[async_trait::async_trait]
impl airdrop::AirdropSubmitter for PanickingPollAirdropSubmitter {
    fn raw_amount(&self) -> u128 {
        100_000_000
    }

    async fn prepare_airdrop(
        &self,
        _recipient: ContractAddress,
    ) -> Result<airdrop::PreparedAirdrop, airdrop::AirdropSubmitError> {
        Ok(prepared_airdrop("100000000"))
    }

    async fn submit_airdrop(
        &self,
        _transaction: &airdrop::PreparedAirdrop,
    ) -> Result<(), airdrop::AirdropSubmitError> {
        Ok(())
    }

    async fn poll_airdrop(
        &self,
        _tx_hash: TxHash,
        _committed_height: Option<i64>,
    ) -> Result<TransactionReceipt, airdrop::AirdropSubmitError> {
        panic!("intentional polling panic")
    }
}

impl MockAirdropSubmitter {
    fn success() -> Self {
        Self::success_with_raw_amount("100000000")
    }

    fn success_with_raw_amount(raw_amount: &str) -> Self {
        Self {
            failures_remaining: Mutex::new(0),
            poll_failures_remaining: Mutex::new(0),
            calls: Mutex::new(0),
            poll_calls: Mutex::new(0),
            broadcasts: Mutex::new(Vec::new()),
            poll_status: Mutex::new(None),
            receipt_override: Mutex::new(None),
            submit_error: None,
            observed_heights: Mutex::new(Vec::new()),
            poll_observation: PollObservation::None,
            raw_amount: raw_amount.into(),
            accepted_raw_amount: raw_amount.into(),
            poll_times_out: false,
            poll_invalid: false,
            ambiguous_submit: false,
            submit_delay: None,
            submit_gate: None,
            poll_gate: None,
        }
    }

    fn success_with_mismatched_accepted_amount() -> Self {
        let mut submitter = Self::success();
        submitter.accepted_raw_amount = "200000000".into();
        submitter
    }

    fn pending() -> Self {
        let mut submitter = Self::success();
        submitter.poll_times_out = true;
        submitter
    }

    fn invalid() -> Self {
        let mut submitter = Self::success();
        submitter.poll_invalid = true;
        submitter
    }

    fn ambiguous_submit() -> Self {
        let mut submitter = Self::success();
        submitter.ambiguous_submit = true;
        submitter
    }

    fn failure() -> Self {
        Self {
            failures_remaining: Mutex::new(usize::MAX),
            poll_failures_remaining: Mutex::new(0),
            calls: Mutex::new(0),
            poll_calls: Mutex::new(0),
            broadcasts: Mutex::new(Vec::new()),
            poll_status: Mutex::new(None),
            receipt_override: Mutex::new(None),
            submit_error: None,
            observed_heights: Mutex::new(Vec::new()),
            poll_observation: PollObservation::None,
            raw_amount: "100000000".into(),
            accepted_raw_amount: "100000000".into(),
            poll_times_out: false,
            poll_invalid: false,
            ambiguous_submit: false,
            submit_delay: None,
            submit_gate: None,
            poll_gate: None,
        }
    }

    fn fail_once() -> Self {
        Self {
            failures_remaining: Mutex::new(1),
            poll_failures_remaining: Mutex::new(0),
            calls: Mutex::new(0),
            poll_calls: Mutex::new(0),
            broadcasts: Mutex::new(Vec::new()),
            poll_status: Mutex::new(None),
            receipt_override: Mutex::new(None),
            submit_error: None,
            observed_heights: Mutex::new(Vec::new()),
            poll_observation: PollObservation::None,
            raw_amount: "100000000".into(),
            accepted_raw_amount: "100000000".into(),
            poll_times_out: false,
            poll_invalid: false,
            ambiguous_submit: false,
            submit_delay: None,
            submit_gate: None,
            poll_gate: None,
        }
    }

    fn calls(&self) -> usize {
        *self.calls.lock().expect("bug: calls lock should succeed")
    }

    fn poll_fail_once() -> Self {
        Self {
            failures_remaining: Mutex::new(0),
            poll_failures_remaining: Mutex::new(1),
            calls: Mutex::new(0),
            poll_calls: Mutex::new(0),
            broadcasts: Mutex::new(Vec::new()),
            poll_status: Mutex::new(None),
            receipt_override: Mutex::new(None),
            submit_error: None,
            observed_heights: Mutex::new(Vec::new()),
            poll_observation: PollObservation::None,
            raw_amount: "100000000".into(),
            accepted_raw_amount: "100000000".into(),
            poll_times_out: false,
            poll_invalid: false,
            ambiguous_submit: false,
            submit_delay: None,
            submit_gate: None,
            poll_gate: None,
        }
    }

    fn poll_calls(&self) -> usize {
        *self
            .poll_calls
            .lock()
            .expect("bug: poll calls lock should succeed")
    }
}

#[async_trait::async_trait]
impl airdrop::AirdropSubmitter for MockAirdropSubmitter {
    fn raw_amount(&self) -> u128 {
        self.raw_amount
            .parse()
            .expect("bug: mock raw amount should be valid")
    }

    async fn prepare_airdrop(
        &self,
        _recipient: ContractAddress,
    ) -> Result<airdrop::PreparedAirdrop, airdrop::AirdropSubmitError> {
        *self.calls.lock().expect("bug: calls lock should succeed") += 1;
        if let Some(delay) = self.submit_delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(gate) = &self.submit_gate {
            gate.acquire()
                .await
                .expect("bug: submit gate should stay open")
                .forget();
        }
        let mut failures = self
            .failures_remaining
            .lock()
            .expect("bug: failures lock should succeed");
        if *failures > 0 {
            if *failures != usize::MAX {
                *failures -= 1;
            }
            return Err(airdrop::AirdropSubmitError::BuildTransaction("boom".into()));
        }
        Ok(prepared_airdrop(&self.accepted_raw_amount))
    }

    async fn submit_airdrop(
        &self,
        transaction: &airdrop::PreparedAirdrop,
    ) -> Result<(), airdrop::AirdropSubmitError> {
        use thruput_json_rpc_types::error::native::codes;

        if let Some(admission) = self.submit_error {
            if admission == "local" {
                return Err(airdrop::AirdropSubmitError::RpcConnect(
                    "invalid URL".into(),
                ));
            }
            self.broadcasts.lock().unwrap().push(transaction.clone());
            let (code, kind, value) = match admission {
                "invalid" => (
                    codes::TRANSACTION_INVALID,
                    "transactionInvalid",
                    "notAttempted",
                ),
                "overloaded" => (codes::OVERLOADED, "overloaded", "rejected"),
                "unavailable" => (codes::BACKEND_UNAVAILABLE, "backendUnavailable", "rejected"),
                "timeout" => (codes::BACKEND_TIMEOUT, "backendTimeout", "notAttempted"),
                "dataInvalid" => (
                    codes::BACKEND_DATA_INVALID,
                    "backendDataInvalid",
                    "notAttempted",
                ),
                "knownUnknown" => (codes::BACKEND_TIMEOUT, "backendTimeout", "unknown"),
                "conflict" => (
                    codes::TRANSACTION_REJECTED,
                    "transactionRejected",
                    "rejected",
                ),
                _ => (
                    codes::TRANSACTION_REJECTED,
                    "transactionRejected",
                    admission,
                ),
            };
            let mut raw = serde_json::json!({
                "code":code, "message":"refused", "data":{"kind":kind, "admission":value}
            });
            if admission == "conflict" {
                raw["data"]["transactionHash"] = serde_json::json!(TxHash(Base58(
                    thruput_xdr::types::SHA256Hash::from([0x91; 32])
                )));
            }
            return Err(airdrop::AirdropSubmitError::Rpc {
                method: "sendTransaction",
                source: Box::new(
                    thruput_json_rpc_client::native::NativeClientError::Application {
                        source: serde_json::from_value(raw.clone()).unwrap(),
                        data: Box::new(
                            thruput_json_rpc_types::error::native::DecodedError::decode(
                                "sendTransaction",
                                raw,
                            ),
                        ),
                    },
                ),
            });
        }
        self.broadcasts.lock().unwrap().push(transaction.clone());
        if self.ambiguous_submit {
            return Err(airdrop::AirdropSubmitError::Submit("timeout".into()));
        }
        Ok(())
    }

    async fn poll_airdrop(
        &self,
        tx_hash: TxHash,
        committed_height: Option<i64>,
    ) -> Result<TransactionReceipt, airdrop::AirdropSubmitError> {
        self.observed_heights.lock().unwrap().push(committed_height);
        *self
            .poll_calls
            .lock()
            .expect("bug: poll calls lock should succeed") += 1;
        if let Some(gate) = &self.poll_gate {
            gate.acquire()
                .await
                .expect("bug: poll gate should stay open")
                .forget();
        }
        let mut failures = self
            .poll_failures_remaining
            .lock()
            .expect("bug: poll failures lock should succeed");
        if *failures > 0 {
            *failures -= 1;
            return Err(airdrop::AirdropSubmitError::RpcConnect("boom".into()));
        }
        if let Some(status) = self.poll_status.lock().unwrap().clone() {
            let mut error = poll_timeout(tx_hash, Some(status));
            if let airdrop::AirdropSubmitError::Poll(error) = &mut error {
                if matches!(self.poll_observation, PollObservation::Committed)
                    || committed_height.is_some()
                {
                    error.evidence.commitment =
                        Some(thruput_json_rpc_client::native::receipt::KnownCommitment {
                            network_hash: Base58(thruput_xdr::types::SHA256Hash::from([0x42; 32])),
                            transaction_hash: tx_hash,
                            block_height: committed_height.unwrap_or(42).into(),
                        });
                }
                if matches!(self.poll_observation, PollObservation::LookupFailed) {
                    error.evidence.last_lookup_error = Some(
                        thruput_json_rpc_client::native::NativeClientError::Transport(
                            jsonrpsee::core::client::Error::RequestTimeout,
                        ),
                    );
                }
            }
            return Err(error);
        }
        if self.poll_times_out {
            return Err(poll_timeout(tx_hash, None));
        }
        if self.poll_invalid {
            let mut receipt = successful_receipt(tx_hash);
            receipt.execution = serde_json::from_value(serde_json::json!({
                "result":"abort", "abortData":"0x01"
            }))
            .unwrap();
            return Ok(receipt);
        }
        Ok(self
            .receipt_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| successful_receipt(tx_hash)))
    }
}

fn test_tx_hash() -> TxHash {
    TxHash(Base58(thruput_xdr::types::SHA256Hash::from([0x22; 32])))
}

fn poll_timeout(tx_hash: TxHash, status: Option<TransactionStatus>) -> airdrop::AirdropSubmitError {
    use thruput_json_rpc_client::native::receipt::{
        ReceiptEvidence, ReceiptPollCause, ReceiptPollError,
    };
    airdrop::AirdropSubmitError::Poll(Box::new(ReceiptPollError {
        evidence: Box::new(ReceiptEvidence {
            network_hash: Base58(thruput_xdr::types::SHA256Hash::from([0x42; 32])),
            transaction_hash: tx_hash,
            commitment: None,
            last_status: status,
            last_lookup_error: None,
        }),
        cause: ReceiptPollCause::Timeout,
    }))
}

fn successful_receipt(tx_hash: TxHash) -> TransactionReceipt {
    serde_json::from_value(
        serde_json::json!({"transactionHash":tx_hash, "blockHeight":"42",
        "execution":{"result":"return","returnData":"0x"}, "events":[], "gasUsed":"1500"}),
    )
    .unwrap()
}

fn prepared_airdrop(raw_amount: &str) -> airdrop::PreparedAirdrop {
    airdrop::PreparedAirdrop {
        raw_amount: raw_amount.into(),
        tx_hash: test_tx_hash(),
        signed_transaction: "0x010203".into(),
        block_expiry_height: 1000,
    }
}

fn scheduler_error() -> thruput_json_rpc_types::responses::SchedulerError {
    serde_json::from_value(serde_json::json!({"source":"executor", "code":7})).unwrap()
}

#[tokio::test]
async fn scheduler_retries_only_reconcile_the_retained_transaction() {
    let submitter = Arc::new(MockAirdropSubmitter::success());
    *submitter.poll_status.lock().unwrap() = Some(TransactionStatus::Retry {
        recent_failure: None,
        error: scheduler_error(),
    });
    let config = test_config_with_submitters(submitter.clone());
    for _ in 0..2 {
        assert_eq!(
            airdrop_request(&config).await.status(),
            StatusCode::ACCEPTED
        );
        assert_eq!(submitter.calls(), 1);
        assert_eq!(submitter.broadcasts.lock().unwrap().len(), 1);
        let attempt = stored_airdrop_attempt(&config).unwrap();
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
        assert!(attempt.expires_at.is_none());
        assert_eq!(attempt.transaction, Some(prepared_airdrop("100000000")));
    }
    let mut other_nonce = airdrop_body();
    other_nonce["clientNonce"] = serde_json::json!("0x02");
    assert_eq!(
        airdrop_request_with_body(&config, other_nonce)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    *submitter.poll_status.lock().unwrap() = None;
    assert_eq!(airdrop_request(&config).await.status(), StatusCode::OK);
    let broadcasts = submitter.broadcasts.lock().unwrap();
    assert_eq!(broadcasts.len(), 1);
    assert!(broadcasts.iter().all(|tx| tx == &broadcasts[0]));
}

#[tokio::test]
async fn dropped_airdrop_remains_pending_until_a_receipt_resolves_it() {
    let submitter = Arc::new(MockAirdropSubmitter::success());
    *submitter.poll_status.lock().unwrap() = Some(TransactionStatus::Dropped {
        recent_failure: None,
        error: scheduler_error(),
    });
    let config = test_config_with_submitters(submitter.clone());
    for _ in 0..2 {
        assert_eq!(
            airdrop_request(&config).await.status(),
            StatusCode::ACCEPTED
        );
        let attempt = stored_airdrop_attempt(&config).unwrap();
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
        assert!(attempt.expires_at.is_none());
        assert_eq!(submitter.broadcasts.lock().unwrap().len(), 1);
    }
    *submitter.poll_status.lock().unwrap() = None;
    assert_eq!(airdrop_request(&config).await.status(), StatusCode::OK);
    assert_eq!(submitter.broadcasts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn retention_failure_prevents_broadcast() {
    let submitter = Arc::new(MockAirdropSubmitter::success());
    let mut config = test_config_with_submitters(submitter.clone());
    config.policy_store = Arc::new(FailFinalPutPolicyStore {
        inner: policy::InMemoryFaucetPolicyStore::new(),
        fail_succeeded_airdrop: false,
        fail_prepared_airdrop: true,
    });
    assert_eq!(
        airdrop_request(&config).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(submitter.broadcasts.lock().unwrap().is_empty());
    let attempt = stored_airdrop_attempt(&config).unwrap();
    assert!(attempt.tx_hash.is_none());
    assert!(attempt.transaction.is_none());
}

#[tokio::test]
async fn retry_without_retained_bytes_never_signs_a_replacement() {
    let submitter = Arc::new(MockAirdropSubmitter::success());
    *submitter.poll_status.lock().unwrap() = Some(TransactionStatus::Retry {
        recent_failure: None,
        error: scheduler_error(),
    });
    let config = test_config_with_submitters(submitter.clone());
    seed_airdrop_attempt(
        &config,
        policy::AirdropAttemptStatus::Pending,
        Some(identifier(0x22)),
    );
    assert_eq!(
        airdrop_request(&config).await.status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(submitter.calls(), 0);
    assert!(submitter.broadcasts.lock().unwrap().is_empty());
    assert_eq!(
        stored_airdrop_attempt(&config).unwrap().status,
        policy::AirdropAttemptStatus::Pending
    );
}

#[tokio::test]
async fn concurrent_same_key_retry_never_rebroadcasts() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut submitter = MockAirdropSubmitter::success();
    submitter.poll_gate = Some(gate.clone());
    *submitter.poll_status.lock().unwrap() = Some(TransactionStatus::Retry {
        recent_failure: None,
        error: scheduler_error(),
    });
    let submitter = Arc::new(submitter);
    let config = test_config_with_submitters(submitter.clone());
    seed_airdrop_attempt(
        &config,
        policy::AirdropAttemptStatus::Pending,
        Some(identifier(0x22)),
    );
    let mut attempt = stored_airdrop_attempt(&config).unwrap();
    attempt.transaction = Some(prepared_airdrop("100000000"));
    config.policy_store.put_airdrop_attempt(attempt).unwrap();
    let worker_config = config.clone();
    let first = tokio::spawn(async move { airdrop_request(&worker_config).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while submitter.poll_calls() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        airdrop_request(&config).await.status(),
        StatusCode::ACCEPTED
    );
    assert!(submitter.broadcasts.lock().unwrap().is_empty());
    gate.add_permits(1);
    assert_eq!(first.await.unwrap().status(), StatusCode::ACCEPTED);
    assert!(submitter.broadcasts.lock().unwrap().is_empty());
    assert_eq!(submitter.calls(), 0);
}

#[tokio::test]
async fn observed_receipt_identity_and_output_are_preserved() {
    for invalid in 0..4 {
        let submitter = MockAirdropSubmitter::success();
        let mut receipt = successful_receipt(test_tx_hash());
        receipt.execution = ReceiptExecution::Return {
            return_data: thruput_json_rpc_types::hex::HexBytes(vec![1, 2]),
        };
        match invalid {
            1 => receipt.transaction_hash = TxHash(Base58([3; 32].into())),
            2 => receipt.block_height = (-1).into(),
            3 => receipt.execution = ReceiptExecution::Other {},
            _ => {}
        }
        *submitter.receipt_override.lock().unwrap() = Some(receipt);
        let submitter = Arc::new(submitter);
        let config = test_config_with_submitters(submitter.clone());
        let response = airdrop_request(&config).await;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if invalid == 0 {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["returnData"], "0x0102");
        } else {
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(body["error"]["code"], "airdrop_receipt_invalid");
            *submitter.receipt_override.lock().unwrap() = Some(successful_receipt(test_tx_hash()));
            let corrected = airdrop_request(&config).await;
            assert_eq!(corrected.status(), StatusCode::OK);
            assert_eq!(submitter.poll_calls(), 2);
            assert_eq!(submitter.broadcasts.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn commitment_and_failed_lookups_prevent_scheduler_replay_or_terminal_caching() {
    for committed in [true, false] {
        for status in [
            TransactionStatus::Retry {
                recent_failure: None,
                error: scheduler_error(),
            },
            TransactionStatus::Dropped {
                recent_failure: None,
                error: scheduler_error(),
            },
            serde_json::from_value(serde_json::json!({
                "status":"recentFailure", "attemptBlockHeight":"42",
                "cause":{"kind":"contractRejected", "contractIndex":0, "code":"invalidProgram"}
            }))
            .unwrap(),
        ] {
            let mut submitter = MockAirdropSubmitter::success();
            submitter.poll_observation = if committed {
                PollObservation::Committed
            } else {
                PollObservation::LookupFailed
            };
            *submitter.poll_status.lock().unwrap() = Some(status);
            let submitter = Arc::new(submitter);
            let config = test_config_with_submitters(submitter.clone());
            for _ in 0..2 {
                assert_eq!(
                    airdrop_request(&config).await.status(),
                    StatusCode::ACCEPTED
                );
                let attempt = stored_airdrop_attempt(&config).unwrap();
                assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
                assert_eq!(attempt.height, committed.then_some(42));
                assert_eq!(submitter.broadcasts.lock().unwrap().len(), 1);
                assert_eq!(submitter.calls(), 1);
            }
            *submitter.poll_status.lock().unwrap() = None;
            assert_eq!(airdrop_request(&config).await.status(), StatusCode::OK);
            assert_eq!(submitter.broadcasts.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn retained_commitment_survives_service_recreation_with_the_same_store() {
    let mut first = MockAirdropSubmitter::success();
    first.poll_observation = PollObservation::Committed;
    *first.poll_status.lock().unwrap() = Some(TransactionStatus::Retry {
        recent_failure: None,
        error: scheduler_error(),
    });
    let first = Arc::new(first);
    let config = test_config_with_submitters(first.clone());
    assert_eq!(
        airdrop_request(&config).await.status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(stored_airdrop_attempt(&config).unwrap().height, Some(42));
    assert_eq!(first.broadcasts.lock().unwrap().len(), 1);

    let mut restarted = MockAirdropSubmitter::success();
    restarted.poll_observation = PollObservation::LookupFailed;
    *restarted.poll_status.lock().unwrap() = Some(TransactionStatus::Retry {
        recent_failure: None,
        error: scheduler_error(),
    });
    let restarted = Arc::new(restarted);
    let mut restarted_config = test_config_with_submitters(restarted.clone());
    restarted_config.policy_store = config.policy_store.clone();
    for status in [
        TransactionStatus::Retry {
            recent_failure: None,
            error: scheduler_error(),
        },
        TransactionStatus::Dropped {
            recent_failure: None,
            error: scheduler_error(),
        },
    ] {
        *restarted.poll_status.lock().unwrap() = Some(status);
        assert_eq!(
            airdrop_request(&restarted_config).await.status(),
            StatusCode::ACCEPTED
        );
        assert_eq!(
            stored_airdrop_attempt(&restarted_config).unwrap().height,
            Some(42)
        );
        assert!(restarted.broadcasts.lock().unwrap().is_empty());
        assert_eq!(restarted.calls(), 0);
        assert!(
            restarted
                .observed_heights
                .lock()
                .unwrap()
                .iter()
                .all(|height| *height == Some(42))
        );
    }
    *restarted.poll_status.lock().unwrap() = None;
    assert_eq!(
        airdrop_request(&restarted_config).await.status(),
        StatusCode::OK
    );
    assert!(restarted.broadcasts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn first_send_refusals_end_only_when_admission_is_proven_absent() {
    for admission in [
        "local",
        "notAttempted",
        "rejected",
        "invalid",
        "overloaded",
        "unavailable",
        "timeout",
        "dataInvalid",
        "unknown",
        "futureAdmission",
        "knownUnknown",
        "conflict",
    ] {
        let mut submitter = MockAirdropSubmitter::success();
        submitter.submit_error = Some(admission);
        let submitter = Arc::new(submitter);
        let config = test_config_with_submitters(submitter.clone());
        assert_eq!(
            airdrop_request(&config).await.status(),
            StatusCode::BAD_GATEWAY
        );
        let attempt = stored_airdrop_attempt(&config).unwrap();
        let terminal = matches!(
            admission,
            "local"
                | "notAttempted"
                | "rejected"
                | "invalid"
                | "overloaded"
                | "unavailable"
                | "timeout"
                | "dataInvalid"
        );
        assert_eq!(
            attempt.status,
            if terminal {
                policy::AirdropAttemptStatus::FailedTerminal
            } else {
                policy::AirdropAttemptStatus::Pending
            }
        );
        assert_eq!(attempt.expires_at.is_some(), terminal);
        assert!(attempt.transaction.is_some());
        assert_eq!(
            airdrop_request(&config).await.status(),
            if terminal {
                StatusCode::CONFLICT
            } else {
                StatusCode::OK
            }
        );
        assert_eq!(submitter.calls(), 1);
        assert_eq!(
            submitter.broadcasts.lock().unwrap().len(),
            usize::from(admission != "local")
        );
    }
}
