use std::{
    fs,
    time::{Duration, Instant},
};

use jsonrpsee::core::{ClientError, client::ClientT};
use jsonrpsee::http_client::HttpClientBuilder;
use serial_test::serial;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thruput_bundle_identity::{BundleIdentity, BundleSubject};
use thruput_json_rpc_api::params::NamedParams;
use thruput_json_rpc_server::BuildVersion;
use thruput_json_rpc_types::responses as out;
use thruput_json_rpc_types::{
    airdrop::ClientNonce, base58::Base58, error::native::codes, requests::AirdropRequest,
};

use super::*;
use crate::{
    BlockDriverConfig, BlockProductionError, HogdDevConfig, LocalAirdropConfig,
    test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
};

/// Env var the fake hogd-dev reads to write its PID before printing
/// readiness. Tests use this to verify cleanup via `kill(pid, 0)`.
const FAKE_HOGD_DEV_PID_FILE_ENV: &str = "THRUPUT_FAKE_HOGD_DEV_PID_FILE";
const FAKE_COMMIT_ENTERED_FILE_ENV: &str = "GROUNDHOG_FAKE_COMMIT_ENTERED_FILE";
// Canonical XDR/hash vector only. This transaction is structurally valid
// and has a stable hash, but it is not auth-valid against empty hogd-dev
// genesis.
const CANONICAL_HASH_VECTOR_SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
13df30e7ceeb7c4c78ba6287e9d8830cabf5a744f3d6be3139f2428d7bb4f9e7000000000000000000000001\
000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695031c9d3fe2554e5c\
e62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d000000010000000050ec72dbafd4898c\
5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a";
const CANONICAL_HASH_VECTOR_TX_HASH_JSON: &str = "4La3QgA156iFSxfp58Du2hwQt6cX52QisyZKqaJGQ4rT";

fn write_matching_client_manifest() -> (TempDir, BundleIdentity, String) {
    const VERSION: &str = "9.8.7-test";
    const COMMIT: &str = "373bec4b9dd9b99abae4f23b82f4e35df61273d1";
    const RELEASE: &str = "0.1.25-rc.4";

    let canonical = format!(
        "{{\"bundle\":{{\"version\":\"{RELEASE}\"}},\"clients\":{{\"thruput\":{{\"assetRole\":\"cli\",\"component\":\"groundhog-rpc\",\"sourceCommit\":\"{COMMIT}\",\"version\":\"{VERSION}\"}}}},\"components\":{{\"groundhog-rpc\":{{\"active\":true,\"sourceCommit\":\"{COMMIT}\",\"version\":\"{VERSION}\"}}}},\"schema\":\"groundhog-bundle-v1\"}}\n"
    );
    let digest = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    );
    let mut manifest: serde_json::Value =
        serde_json::from_str(&canonical).expect("bug: canonical manifest should parse");
    manifest["bundle"]["manifestDigest"] = serde_json::Value::String(digest.clone());

    let directory = TempDir::new().expect("bug: tempdir creates");
    let path = directory.path().join("bundle-manifest.json");
    fs::write(
        &path,
        serde_json::to_vec(&manifest).expect("bug: manifest should serialize"),
    )
    .expect("bug: manifest should write");
    let identity = BundleIdentity::from_manifest_path(
        BundleSubject::client("thruput", "groundhog-rpc", "cli"),
        BuildVersion::new(VERSION, None).with_source_commit(Some(COMMIT)),
        &path,
    );
    assert_eq!(identity.release_version(), Some(RELEASE));
    (directory, identity, digest)
}

#[tokio::test]
async fn invalid_local_airdrop_config_is_rejected_before_child_start() {
    let local_airdrop = LocalAirdropConfig {
        attempt_capacity: 0,
        ..LocalAirdropConfig::default()
    };
    let config = NodeSupervisorConfig::default()
        .with_hogd_dev_binary("/definitely/missing/hogd-dev")
        .with_local_airdrop(local_airdrop);

    let error = NodeSupervisorHandle::start(config)
        .await
        .expect_err("invalid local airdrop config must fail before resolving hogd-dev");

    assert!(matches!(
        error,
        NodeSupervisorError::InvalidLocalAirdropConfig { .. }
    ));
}

/// Set an env var for the duration of a test that needs the fake child
/// to enter `ready` mode. Restored on drop. `#[serial]` ensures we are
/// the only test mutating env at a time, per workspace convention.
struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: `#[serial]` serializes all env-mutating tests in this
        // process; no other thread is reading or writing env concurrently.
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see EnvGuard::set.
        match self.prev.take() {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// Poll `kill(pid, 0)` until ESRCH or until `deadline` passes. Returns
/// `true` if the OS confirmed the process is gone within the deadline.
async fn wait_pid_gone(pid: i32, deadline: Instant) -> bool {
    loop {
        // SAFETY: `kill(pid, 0)` does not deliver a signal; it only
        // tests whether `pid` is a known process. Always safe.
        let rc = unsafe { libc::kill(pid, 0) };
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn read_pid_file(path: &std::path::Path) -> Option<i32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
}

async fn wait_path_exists(path: &std::path::Path, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    path.exists()
}

async fn start_fake_supervisor_expect_error(mode: &str) -> NodeSupervisorError {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, mode);
    let pid_dir = TempDir::new().expect("bug: tempdir creates");
    let pid_path = pid_dir.path().join("hogd.pid");
    let _pid_guard = EnvGuard::set(
        FAKE_HOGD_DEV_PID_FILE_ENV,
        pid_path.to_str().expect("bug: tempdir path is utf-8"),
    );

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);

    let result = tokio::time::timeout(Duration::from_secs(10), NodeSupervisorHandle::start(config))
        .await
        .expect("bug: startup error should return promptly");

    let err = result.expect_err("bug: fake mode should fail startup");
    let pid = read_pid_file(&pid_path).expect("bug: fake should have written pid before readiness");
    assert!(
        wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
        "startup failure leaked fake hogd-dev pid {pid}",
    );
    err
}

async fn start_fake_with_block_driver(
    mode: &str,
    block_driver: BlockDriverConfig,
) -> (
    EnvGuard,
    NodeSupervisorHandle,
    jsonrpsee::http_client::HttpClient,
) {
    let mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, mode);
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd)
        .with_block_driver(block_driver);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", handle.rpc_addr()))
        .expect("bug: jsonrpsee http client builds");
    (mode_guard, handle, client)
}

#[tokio::test]
#[serial]
async fn start_serves_json_rpc_with_scheduler_chain_backend() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
    let pid_dir = TempDir::new().expect("bug: tempdir creates");
    let pid_path = pid_dir.path().join("hogd.pid");
    let _pid_guard = EnvGuard::set(
        FAKE_HOGD_DEV_PID_FILE_ENV,
        pid_path.to_str().expect("bug: tempdir path is utf-8"),
    );

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let (_manifest_directory, bundle_identity, manifest_digest) = write_matching_client_manifest();
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd)
        .with_bundle_identity(bundle_identity);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");
    assert_eq!(
        handle.backend_planes(),
        BackendPlanes::hogd_dev_storage_only(PlaneStatus::Stub)
    );

    let url = format!("http://{}", handle.rpc_addr());
    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");

    let network_info = client
        .request::<out::NetworkInfo, _>("getNetworkInfo", NamedParams(serde_json::json!({})))
        .await
        .expect("bug: networkInfo should use immutable manifest identity");
    assert_eq!(network_info.network_hash.as_ref(), &[0; 32]);

    let version = client
        .request::<out::VersionInfo, _>("getVersionInfo", NamedParams(serde_json::json!({})))
        .await
        .expect("bug: version should report the embedding client identity");
    assert_eq!(version.wire_format, "14.0.0");
    let expected = [
        "dev_getPendingTransactionCount",
        "getBalance",
        "getChainConfig",
        "getChainTip",
        "getNetworkInfo",
        "getRegisteredAuth",
        "getServerLimits",
        "getStorage",
        "getVersionInfo",
        "sendTransaction",
    ];
    assert_eq!(version.supported_methods, expected);
    for absent in [
        "getTransactionReceipt",
        "getTransactionStatus",
        "getEvents",
        "getTransactionsByRootAddress",
        "getTransactionsByBlock",
        "getBlockByHeight",
        "simulateInvocation",
        "simulateInvocations",
        "faucet_requestAirdrop",
        "dev_fundAccount",
    ] {
        let error = client
            .request::<serde_json::Value, _>(absent, NamedParams(serde_json::json!({})))
            .await
            .unwrap_err();
        assert!(
            matches!(error, jsonrpsee::core::client::Error::Call(ref error) if error.code() == -32601),
            "{absent}: {error}"
        );
    }
    assert_eq!(version.implementation, "9.8.7-test");
    assert_eq!(
        version.commit_hash.as_deref(),
        Some("373bec4b9dd9b99abae4f23b82f4e35df61273d1")
    );
    assert_eq!(version.release_version.as_deref(), Some("0.1.25-rc.4"));
    assert_eq!(
        version.bundle_manifest_digest.as_deref(),
        Some(manifest_digest.as_str())
    );

    let tip = client
        .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
        .await
        .expect("bug: chainTip should dispatch to fake scheduler");
    assert_eq!(
        *tip.block_height, 0,
        "chainTip should report the genesis block loaded during startup"
    );
    assert_eq!(
        serde_json::to_value(tip.block_hash).expect("bug: block hash should serialize"),
        "G86DxMgpV6arteLsJ4J5UxdRLvFfH5yPAo2G8hNpFPcc"
    );
    let chain_config = client
        .request::<out::ChainConfig, _>("getChainConfig", NamedParams(serde_json::json!({})))
        .await
        .expect("bug: chainConfig should dispatch to fake scheduler");
    assert_eq!(
        chain_config.stated.max_sha256_buffers,
        Some(0),
        "chainConfig should report the parameters the fake scheduler's genesis config states"
    );
    assert_eq!(chain_config.stated.max_cross_invoke_depth, Some(0));
    assert_eq!(
        chain_config.stated.max_contract_size_bytes, None,
        "a parameter the genesis config does not state is absent, not defaulted"
    );
    assert_eq!(
        client
            .request::<out::PendingTransactionCount, _>(
                "dev_getPendingTransactionCount",
                NamedParams(serde_json::json!({}))
            )
            .await
            .expect("bug: pendingTransactionCount should use SchedGateway")
            .count,
        0
    );

    let airdrop_error = client
        .request::<out::AirdropAttempt, _>(
            "faucet_requestAirdrop",
            NamedParams(AirdropRequest {
                recipient: Base58(thruput_xdr::types::ContractAddress::from([0x88; 32])),
                client_nonce: ClientNonce::new(vec![0x99])
                    .expect("bug: test client nonce should validate"),
            }),
        )
        .await
        .expect_err("bug: scheduler-only runtime must not expose airdrop");
    assert!(
        matches!(&airdrop_error, ClientError::Call(object) if object.code() == jsonrpsee::types::error::METHOD_NOT_FOUND_CODE),
        "scheduler-only runtime should omit airdrop without a receipt backend, got {airdrop_error:?}",
    );

    let tx_hash = client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: sendRawTransaction should dispatch to fake scheduler");
    assert_eq!(
        serde_json::to_value(tx_hash.transaction_hash).expect("bug: tx hash should serialize"),
        CANONICAL_HASH_VECTOR_TX_HASH_JSON
    );

    let err = client
        .request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": "not-valid-hex"})),
        )
        .await
        .expect_err("bug: invalid hex should fail before fake scheduler dispatch");
    assert!(
        matches!(&err, ClientError::Call(object) if object.code() == codes::INVALID_PARAMS),
        "expected INVALID_PARAMS JSON-RPC error, got {err:?}",
    );

    let pid = read_pid_file(&pid_path).expect("bug: fake should have written pid");
    let status = handle.stop().await.expect("bug: clean shutdown");

    // Tighter than `!success() || code == 0`: a clean SIGTERM-handled
    // exit either returns code 0 or the SIGTERM signal. Anything else
    // (SIGKILL, abnormal exit) means the supervisor escalated past
    // graceful shutdown.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let clean = status.code() == Some(0) || status.signal() == Some(libc::SIGTERM);
        assert!(
            clean,
            "fake hogd-dev should exit on code 0 or SIGTERM, got {status:?}"
        );
    }

    assert!(
        wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
        "fake hogd-dev pid {pid} still alive after stop()",
    );
}

#[tokio::test]
#[serial]
async fn scheduler_gateway_rejection_surfaces_tx_rejected_without_coordinator_fallback() {
    let (_mode_guard, handle, client) = start_fake_with_block_driver(
        "scheduler-gateway-submit-rejected",
        BlockDriverConfig::manual(),
    )
    .await;

    let error = client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect_err("bug: typed scheduler gateway rejection should fail submission");

    assert!(
        matches!(&error, ClientError::Call(object) if object.code() == codes::TRANSACTION_REJECTED),
        "expected TRANSACTION_REJECTED from SchedGateway without coordinator fallback, got {error:?}",
    );
    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn submit_wakes_hogd_dev_block_driver() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "automine");

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");

    let url = format!("http://{}", handle.rpc_addr());
    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");

    client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: sendRawTransaction should dispatch to fake scheduler");

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let tip = client
            .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
            .await
            .expect("bug: chainTip should dispatch to fake scheduler");
        if *tip.block_height == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "block driver did not advance fake scheduler tip before timeout; tip={}",
            *tip.block_height
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn concurrent_public_submissions_are_batched_for_scheduler() {
    const SUBMIT_LOG_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_SUBMIT_LOG";
    let directory = tempfile::tempdir().expect("bug: tempdir");
    let submit_log = directory.path().join("submit-batches.log");
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "submit-batch-probe");
    let _log_guard = EnvGuard::set(
        SUBMIT_LOG_ENV,
        submit_log.to_str().expect("bug: tempdir path is utf-8"),
    );
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", handle.rpc_addr()))
        .expect("bug: jsonrpsee http client builds");
    let tx_hex = format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}");

    let (a, b, c, d, e) = tokio::join!(
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()}))
        ),
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()}))
        ),
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()}))
        ),
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()}))
        ),
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex}))
        ),
    );

    for result in [a, b, c, d, e] {
        result.expect("bug: concurrent transaction should be submitted");
    }
    let sizes: Vec<usize> = std::fs::read_to_string(submit_log)
        .expect("bug: fake scheduler should write submit log")
        .lines()
        .map(|line| line.parse().expect("bug: submit batch size should parse"))
        .collect();
    assert!(
        sizes.iter().any(|size| *size > 1),
        "concurrent public submissions should produce a multi-transaction batch: {sizes:?}"
    );
    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn rpc_array_limit_rejects_oversized_submissions_before_dispatch() {
    use jsonrpsee::core::{client::ClientT, params::BatchRequestBuilder};

    let directory = tempfile::tempdir().unwrap();
    let submit_log = directory.path().join("submit-batches.log");
    let _mode = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "submit-batch-probe");
    let _log = EnvGuard::set(
        "GROUNDHOG_FAKE_HOGD_DEV_SUBMIT_LOG",
        submit_log.to_str().unwrap(),
    );
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let handle = NodeSupervisorHandle::start(
        NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd),
    )
    .await
    .unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", handle.rpc_addr()))
        .unwrap();
    let mut allowed = BatchRequestBuilder::new();
    for _ in 0..MAX_RPC_BATCH_REQUESTS {
        allowed
            .insert("getVersionInfo", jsonrpsee::rpc_params![])
            .unwrap();
    }
    let responses = client
        .batch_request::<serde_json::Value>(allowed)
        .await
        .unwrap();
    assert_eq!(responses.len(), MAX_RPC_BATCH_REQUESTS as usize);
    assert!(responses.into_iter().all(|response| response.is_ok()));

    let oversized: Vec<_> = (0..=MAX_RPC_BATCH_REQUESTS)
        .map(|id| {
            serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "sendTransaction",
                "params": {"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")},
            })
        })
        .collect();
    // Use raw HTTP: jsonrpsee's batch client expects an array and cannot
    // decode the single null-ID error returned for a rejected whole batch.
    let error = tokio::time::timeout(Duration::from_secs(5), async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let body = serde_json::to_string(&oversized).unwrap();
        let mut socket = tokio::net::TcpStream::connect(handle.rpc_addr())
            .await
            .unwrap();
        let request = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        serde_json::from_str::<serde_json::Value>(response.split_once("\r\n\r\n").unwrap().1)
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(error["error"]["code"], -32010);
    assert!(
        !submit_log.exists(),
        "oversized array must not submit any transactions"
    );
    handle.stop().await.unwrap();
}

#[tokio::test]
#[serial]
async fn homogeneous_rpc_array_is_batched_for_scheduler() {
    use jsonrpsee::core::{client::ClientT, params::BatchRequestBuilder};

    for concurrency in [1, 4, 128] {
        let directory = tempfile::tempdir().expect("bug: tempdir");
        let submit_log = directory.path().join("submit-batches.log");
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "submit-batch-probe");
        let _log_guard = EnvGuard::set(
            "GROUNDHOG_FAKE_HOGD_DEV_SUBMIT_LOG",
            submit_log.to_str().expect("bug: tempdir path is utf-8"),
        );
        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_rpc_batch_concurrency(concurrency.try_into().unwrap())
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config)
            .await
            .expect("bug: supervisor should start against fake hogd-dev");
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.rpc_addr()))
            .expect("bug: jsonrpsee http client builds");
        let mut batch = BatchRequestBuilder::new();
        for _ in 0..35 {
            batch
                .insert(
                    "sendTransaction",
                    NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})),
                )
                .expect("bug: batch entry should serialize");
        }
        let responses = client
            .batch_request::<out::SendTransactionResult>(batch)
            .await
            .expect("bug: submission array should succeed");
        assert_eq!(responses.len(), 35);
        assert!(responses.into_iter().all(|response| response.is_ok()));
        let sizes: Vec<usize> = std::fs::read_to_string(submit_log)
            .expect("bug: fake scheduler should write submit log")
            .lines()
            .map(|line| line.parse().expect("bug: submit batch size should parse"))
            .collect();
        assert_eq!(sizes.iter().sum::<usize>(), 35);
        if concurrency > 1 {
            assert!(
                sizes.iter().any(|size| *size > 1),
                "array should coalesce: {sizes:?}"
            );
        }
        assert!(sizes.iter().all(|size| *size <= concurrency));
        handle.stop().await.expect("bug: clean shutdown");
    }
}

#[tokio::test]
#[serial]
async fn block_driver_coalesces_rapid_submit_wakes() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "automine-slow-commit");

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");

    let url = format!("http://{}", handle.rpc_addr());
    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");
    let tx_hex = format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}");

    let (a, b, c, d, e) = tokio::join!(
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()}))
        ),
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()}))
        ),
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()}))
        ),
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()}))
        ),
        client.request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex}))
        ),
    );
    for result in [a, b, c, d, e] {
        result.expect("bug: sendRawTransaction should dispatch to fake scheduler");
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let tip = client
            .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
            .await
            .expect("bug: chainTip should dispatch to fake scheduler");
        if *tip.block_height >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "block driver did not commit any block before timeout; tip={}",
            *tip.block_height
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    tokio::time::sleep(Duration::from_millis(200)).await;
    let final_tip = client
        .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
        .await
        .expect("bug: chainTip should dispatch to fake scheduler");
    assert!(
        *final_tip.block_height <= 2,
        "coalesced submit wakes should not commit one block per submit; tip={}",
        *final_tip.block_height
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn interval_block_driver_paces_submit_commit() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "automine");

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd)
        .with_block_driver(BlockDriverConfig::interval(Duration::from_millis(500)));
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");

    let url = format!("http://{}", handle.rpc_addr());
    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");

    client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: sendRawTransaction should dispatch to fake scheduler");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let early_tip = client
        .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
        .await
        .expect("bug: chainTip should dispatch to fake scheduler");
    assert_eq!(
        *early_tip.block_height, 0,
        "interval mode should not commit immediately from a submit wake"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let tip = client
            .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
            .await
            .expect("bug: chainTip should dispatch to fake scheduler");
        if *tip.block_height == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "interval block driver did not advance fake scheduler tip before timeout; tip={}",
            *tip.block_height
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn manual_block_driver_only_commits_on_explicit_request() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "automine");

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd)
        .with_block_driver(BlockDriverConfig::manual().with_interval(Duration::from_millis(25)));
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");
    let producer = handle
        .manual_block_producer()
        .expect("bug: manual mode should expose a producer");

    let url = format!("http://{}", handle.rpc_addr());
    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");
    let tx_hex = format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}");
    client
        .request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()})),
        )
        .await
        .expect("bug: first submit should reach fake scheduler");
    client
        .request::<out::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(serde_json::json!({"signedTransaction": tx_hex})),
        )
        .await
        .expect("bug: second submit should reach fake scheduler");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = client
        .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
        .await
        .expect("bug: chainTip should dispatch to fake scheduler");
    assert_eq!(
        *before.block_height, 0,
        "manual mode must not automine submits"
    );

    let produced = producer
        .produce_block()
        .await
        .expect("bug: explicit production should commit one block");
    assert_eq!(produced.height, 1);
    let after = client
        .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
        .await
        .expect("bug: chainTip should dispatch to fake scheduler");
    assert_eq!(*after.block_height, 1);

    let empty_error = producer
        .produce_block()
        .await
        .expect_err("bug: fake scheduler rejects nomination without pending submits");
    assert!(matches!(
        empty_error,
        BlockProductionError::SchedulerRejected {
            op: "schedulercoord_propose_block",
            ..
        }
    ));

    handle.stop().await.expect("bug: clean shutdown");
    let stopped = producer
        .produce_block()
        .await
        .expect_err("bug: producer retained after shutdown must fail");
    assert!(matches!(stopped, BlockProductionError::DriverStopped));
}

#[tokio::test]
#[serial]
async fn manual_block_requests_are_serialized_without_coalescing() {
    let (_mode_guard, handle, client) =
        start_fake_with_block_driver("automine-serialize-probe", BlockDriverConfig::manual()).await;
    let producer = handle
        .manual_block_producer()
        .expect("bug: manual mode should expose a producer");
    let tx_hex = format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}");
    for _ in 0..2 {
        client
            .request::<out::SendTransactionResult, _>(
                "sendTransaction",
                NamedParams(serde_json::json!({"signedTransaction": tx_hex.clone()})),
            )
            .await
            .expect("bug: submit should reach fake scheduler");
    }

    let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(producer.produce_block(), producer.produce_block())
    })
    .await
    .expect("bug: serialized requests should complete before timeout");
    let mut heights = [
        first
            .expect("bug: first serialized request should commit")
            .height,
        second
            .expect("bug: second serialized request should commit")
            .height,
    ];
    heights.sort_unstable();
    assert_eq!(heights, [1, 2]);

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn malformed_scheduler_proposal_returns_typed_error_without_panicking_driver() {
    let (_mode_guard, handle, client) =
        start_fake_with_block_driver("automine-malformed-proposal", BlockDriverConfig::manual())
            .await;
    let producer = handle
        .manual_block_producer()
        .expect("bug: manual mode should expose a producer");
    client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: submit should reach fake scheduler");

    let error = producer
        .produce_block()
        .await
        .expect_err("bug: malformed scheduler proposal should fail");
    assert!(matches!(
        error,
        BlockProductionError::MalformedSchedulerProposal { batch: "hash", .. }
    ));
    let stopped = producer
        .produce_block()
        .await
        .expect_err("bug: malformed retained proposal should stop manual producer");
    assert!(matches!(stopped, BlockProductionError::DriverStopped));

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn retained_commit_rejection_stops_manual_producer() {
    let (_mode_guard, handle, client) =
        start_fake_with_block_driver("automine-commit-rejected", BlockDriverConfig::manual()).await;
    let producer = handle
        .manual_block_producer()
        .expect("bug: manual mode should expose a producer");
    client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: submit should reach fake scheduler");

    let error = producer
        .produce_block()
        .await
        .expect_err("bug: retained commit rejection should fail");
    assert!(matches!(
        error,
        BlockProductionError::CommitRejected {
            block_id: 1,
            height: 1,
            ..
        }
    ));
    let stopped = producer
        .produce_block()
        .await
        .expect_err("bug: retained commit candidate should stop manual producer");
    assert!(matches!(stopped, BlockProductionError::DriverStopped));

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn manual_block_request_times_out_instead_of_hanging() {
    let (_mode_guard, handle, client) = start_fake_with_block_driver(
        "automine-hanging-commit",
        BlockDriverConfig::manual().with_manual_request_timeout(Duration::from_millis(100)),
    )
    .await;
    let producer = handle
        .manual_block_producer()
        .expect("bug: manual mode should expose a producer");
    client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: submit should reach fake scheduler");

    let error = tokio::time::timeout(Duration::from_secs(2), producer.produce_block())
        .await
        .expect("bug: configured block-production deadline should fire")
        .expect_err("bug: hanging commit should hit the configured deadline");
    assert!(matches!(error, BlockProductionError::TimedOut { .. }));
    let stopped = producer
        .produce_block()
        .await
        .expect_err("bug: timeout should make the producer terminal");
    assert!(matches!(stopped, BlockProductionError::DriverStopped));

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn shutdown_resolves_an_in_flight_manual_request() {
    let marker_dir = TempDir::new().expect("bug: tempdir creates");
    let marker_path = marker_dir.path().join("commit-entered");
    let _marker_guard = EnvGuard::set(
        FAKE_COMMIT_ENTERED_FILE_ENV,
        marker_path.to_str().expect("bug: tempdir path is utf-8"),
    );
    let (_mode_guard, handle, client) = start_fake_with_block_driver(
        "automine-hanging-commit",
        BlockDriverConfig::manual().with_manual_request_timeout(Duration::from_secs(30)),
    )
    .await;
    let producer = handle
        .manual_block_producer()
        .expect("bug: manual mode should expose a producer");
    client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: submit should reach fake scheduler");
    let request = tokio::spawn(async move { producer.produce_block().await });
    assert!(
        wait_path_exists(&marker_path, Instant::now() + Duration::from_secs(2)).await,
        "manual request never entered the fake commit RPC"
    );

    tokio::time::timeout(Duration::from_secs(2), handle.stop())
        .await
        .expect("bug: shutdown should not wait for the hanging commit")
        .expect("bug: clean shutdown");
    let error = tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .expect("bug: pending request should resolve during shutdown")
        .expect("bug: request task should join")
        .expect_err("bug: interrupted request should not report success");
    assert!(matches!(error, BlockProductionError::OutcomeUnknown));
}

#[tokio::test]
#[serial]
async fn commit_rpc_error_is_reconciled_against_the_scheduler_tip() {
    let (_mode_guard, handle, client) =
        start_fake_with_block_driver("automine-commit-response-lost", BlockDriverConfig::manual())
            .await;
    let producer = handle
        .manual_block_producer()
        .expect("bug: manual mode should expose a producer");
    client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: submit should reach fake scheduler");

    let produced = producer
        .produce_block()
        .await
        .expect("bug: reconciled tip should confirm the committed block");
    assert_eq!(produced.height, 1);

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn commit_rpc_error_without_tip_confirmation_is_indeterminate() {
    let (_mode_guard, handle, client) =
        start_fake_with_block_driver("automine-commit-error", BlockDriverConfig::manual()).await;
    let producer = handle
        .manual_block_producer()
        .expect("bug: manual mode should expose a producer");
    client
        .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
        .await
        .expect("bug: submit should reach fake scheduler");

    let error = producer
        .produce_block()
        .await
        .expect_err("bug: unchanged tip cannot confirm commit");
    assert!(matches!(
        error,
        BlockProductionError::CommitOutcomeUnknown { .. }
    ));
    let stopped = producer
        .produce_block()
        .await
        .expect_err("bug: indeterminate commit should make the producer terminal");
    assert!(matches!(stopped, BlockProductionError::DriverStopped));

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn runtime_scheduler_rpc_failures_are_typed() {
    for mode in ["automine-runtime-get-tip-error", "automine-nominate-error"] {
        let (_mode_guard, handle, client) =
            start_fake_with_block_driver(mode, BlockDriverConfig::manual()).await;
        let producer = handle
            .manual_block_producer()
            .expect("bug: manual mode should expose a producer");
        client
            .request::<out::SendTransactionResult, _>("sendTransaction", NamedParams(serde_json::json!({"signedTransaction": format!("0x{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}")})))
            .await
            .expect("bug: submit should reach fake scheduler");

        let error = producer
            .produce_block()
            .await
            .expect_err("bug: fake runtime RPC failure should reach the caller");
        if mode == "automine-runtime-get-tip-error" {
            assert!(matches!(
                error,
                BlockProductionError::SchedulerRpc {
                    op: "schedcoord_get_tip",
                    ..
                }
            ));
        } else {
            assert!(matches!(
                error,
                BlockProductionError::ProposalOutcomeUnknown { .. }
            ));
            let stopped = producer
                .produce_block()
                .await
                .expect_err("bug: unknown nomination should make producer terminal");
            assert!(matches!(stopped, BlockProductionError::DriverStopped));
        }

        handle.stop().await.expect("bug: clean shutdown");
    }
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_scheduler_connect_fails() {
    let err = start_fake_supervisor_expect_error("dead-scheduler").await;

    assert!(
        matches!(err, NodeSupervisorError::SchedulerConnect { .. }),
        "expected SchedulerConnect, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_scheduler_gateway_connect_fails() {
    let err = start_fake_supervisor_expect_error("dead-scheduler-gateway").await;

    assert!(
        matches!(err, NodeSupervisorError::SchedulerGatewayConnect { .. }),
        "expected SchedulerGatewayConnect, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_scheduler_gateway_intro_is_rejected() {
    let err = start_fake_supervisor_expect_error("scheduler-gateway-intro-rejected").await;

    assert!(
        matches!(
            err,
            NodeSupervisorError::ProtocolIntroRejected {
                service: "SchedGateway",
                is_protocol_mismatch: true,
                ..
            }
        ),
        "expected SchedGateway protocol rejection, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rejects_scheduler_gateway_from_a_different_network() {
    let err = start_fake_supervisor_expect_error("scheduler-gateway-network-hash-mismatch").await;

    assert!(
        matches!(
            err,
            NodeSupervisorError::SchedulerNetworkHashMismatch { .. }
        ),
        "expected scheduler gateway network mismatch, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn custom_network_preserves_independent_scheduler_and_gateway_checks() {
    for mode in [
        "scheduler-network-hash-mismatch",
        "scheduler-gateway-network-hash-mismatch",
    ] {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, mode);
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(
                HogdDevConfig::default()
                    .with_binary_path(fake_hogd_dev_binary())
                    .with_init_network_hash([0xab; 32].into())
                    .with_startup_timeout(Duration::from_secs(5)),
            );
        let error = NodeSupervisorHandle::start(config).await.unwrap_err();
        assert!(
            matches!(error, NodeSupervisorError::SchedulerNetworkHashMismatch { expected, .. }
            if expected == thruput_json_rpc_types::NetworkHashText::from([0xab; 32])),
            "{mode}"
        );
    }
}

#[tokio::test]
#[serial]
async fn start_rejects_incoherent_scheduler_plane_tips() {
    let err = start_fake_supervisor_expect_error("scheduler-gateway-tip-mismatch").await;

    assert!(
        matches!(err, NodeSupervisorError::SchedulerPlaneTipMismatch { .. }),
        "expected scheduler plane tip mismatch, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_scheduler_get_tip_rpc_fails() {
    let err = start_fake_supervisor_expect_error("scheduler-get-tip-error").await;

    assert!(
        matches!(
            err,
            NodeSupervisorError::SchedulerRpc {
                op: "schedcoord_get_tip",
                ..
            }
        ),
        "expected SchedulerRpc for get_tip, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_scheduler_network_hash_mismatches_manifest() {
    let err = start_fake_supervisor_expect_error("scheduler-network-hash-mismatch").await;

    assert!(
        matches!(
            &err,
            NodeSupervisorError::SchedulerNetworkHashMismatch {
                expected,
                observed,
            } if expected == &thruput_json_rpc_types::NetworkHashText::from([0; 32])
                && observed == &thruput_json_rpc_types::NetworkHashText::from([0xff; 32])
        ),
        "expected SchedulerNetworkHashMismatch, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_readiness_precedes_genesis() {
    let err = start_fake_supervisor_expect_error("scheduler-no-genesis").await;

    assert!(
        matches!(
            err,
            NodeSupervisorError::SchedulerRejected {
                op: "schedcoord_get_tip",
                ..
            }
        ),
        "expected SchedulerRejected for readiness without genesis, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn run_until_stop_signal_error_stops_hogd_dev() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
    let pid_dir = TempDir::new().expect("bug: tempdir creates");
    let pid_path = pid_dir.path().join("hogd.pid");
    let _pid_guard = EnvGuard::set(
        FAKE_HOGD_DEV_PID_FILE_ENV,
        pid_path.to_str().expect("bug: tempdir path is utf-8"),
    );

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");
    let pid = read_pid_file(&pid_path).expect("bug: fake should have written pid");

    let err = handle
        .run_until(async {
            Err(NodeSupervisorError::StopSignal {
                source: std::io::Error::other("ctrl-c registration failed"),
            })
        })
        .await
        .expect_err("bug: stop signal error should be returned");
    assert!(
        matches!(err, NodeSupervisorError::StopSignal { .. }),
        "expected StopSignal error, got {err:?}",
    );
    assert!(
        wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
        "fake hogd-dev pid {pid} still alive after stop signal error",
    );
}

#[tokio::test]
#[serial]
async fn stopped_observes_hogd_dev_exit_and_tears_down_rpc() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-then-exit");

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev");

    // Capture the bound RPC address before consuming the handle.
    let rpc_addr = handle.rpc_addr();

    // The fake reports readiness then immediately exits with code 0.
    // `stopped()` should observe the child exit, tear down the RPC
    // server, and return HogdExited.
    let outcome = tokio::time::timeout(Duration::from_secs(5), handle.stopped())
        .await
        .expect("bug: stopped() should observe hogd-dev exit promptly")
        .expect("bug: stopped() should not error on clean child exit");

    match outcome {
        SupervisorStop::HogdExited(status) => {
            assert!(
                status.success(),
                "fake should exit cleanly with code 0, got {status:?}",
            );
        }
        SupervisorStop::RpcStopped(_) => {
            panic!("expected HogdExited, got RpcStopped — race lost the wrong way");
        }
        SupervisorStop::StopRequested(_) => {
            panic!("expected HogdExited, got StopRequested");
        }
    }

    // RPC facade should be torn down: a fresh connect attempt must fail.
    // (HttpClientBuilder::build is lazy; the call is what hits the wire.)
    let url = format!("http://{rpc_addr}");
    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");
    let err = client
        .request::<out::ChainTip, _>("getChainTip", NamedParams(serde_json::json!({})))
        .await
        .expect_err("bug: RPC facade should be down after hogd-dev exit");
    // Any client error here is correct (transport closed, refused, etc.).
    // We only assert it's not a successful call mapped to NotImplemented.
    assert!(
        !matches!(err, ClientError::Call(_)),
        "expected transport-level error after RPC shutdown, got JSON-RPC call error: {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_rpc_bind_fails() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
    let pid_dir = TempDir::new().expect("bug: tempdir creates");
    let pid_path = pid_dir.path().join("hogd.pid");
    let _pid_guard = EnvGuard::set(
        FAKE_HOGD_DEV_PID_FILE_ENV,
        pid_path.to_str().expect("bug: tempdir path is utf-8"),
    );

    // Occupy a loopback port so the supervisor's RPC bind hits EADDRINUSE.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bug: ephemeral bind succeeds");
    let occupied = listener
        .local_addr()
        .expect("bug: bound listener has local addr");

    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address(occupied.to_string())
        .with_hogd_dev(hogd);

    let result = tokio::time::timeout(Duration::from_secs(10), NodeSupervisorHandle::start(config))
        .await
        .expect("bug: start should roll back and return promptly on rpc bind failure");

    let err = result.expect_err("bug: occupied port should fail rpc bind");
    assert!(
        matches!(err, NodeSupervisorError::RpcBind { .. }),
        "expected RpcBind, got {err:?}",
    );

    // The fake wrote its PID before the readiness line; if the rollback
    // path didn't actually reap the child, the OS would still know
    // about this pid.
    let pid = read_pid_file(&pid_path).expect("bug: fake should have written pid before readiness");
    assert!(
        wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
        "rollback leaked fake hogd-dev pid {pid}",
    );

    drop(listener);
}

#[test]
fn plane_status_scheduler_serializes_to_scheduler_string() {
    let status = PlaneStatus::Scheduler;
    let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
    assert_eq!(json, "scheduler");
}

#[test]
fn plane_status_indexer_serializes_to_indexer_string() {
    let status = PlaneStatus::Indexer;
    let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
    assert_eq!(json, "indexer");
}

#[test]
fn plane_status_db_gateway_serializes_to_db_gateway_string() {
    let status = PlaneStatus::DbGateway;
    let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
    assert_eq!(json, "db_gateway");
}

#[test]
fn plane_status_stub_serializes_to_stub_string() {
    let status = PlaneStatus::Stub;
    let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
    assert_eq!(json, "stub");
}

#[test]
fn plane_status_view_gateway_serializes_to_view_gateway_string() {
    let status = PlaneStatus::ViewGateway;
    let json = serde_json::to_value(status).expect("bug: PlaneStatus serializes");
    assert_eq!(json, "view_gateway");
}

#[test]
fn plane_status_read_predicates_match_known_planes() {
    assert!(!PlaneStatus::Scheduler.is_indexer_backed());
    assert!(PlaneStatus::Indexer.is_indexer_backed());
    assert!(!PlaneStatus::DbGateway.is_indexer_backed());
    assert!(!PlaneStatus::ViewGateway.is_indexer_backed());
    assert!(!PlaneStatus::Stub.is_indexer_backed());
}

#[test]
fn backend_planes_hogd_dev_full_constructor() {
    let planes = BackendPlanes::hogd_dev_full(PlaneStatus::ViewGateway);
    assert_eq!(planes.submit, PlaneStatus::Scheduler);
    assert_eq!(planes.chain, PlaneStatus::DbGateway);
    assert_eq!(planes.read, PlaneStatus::Indexer);
    assert_eq!(planes.view, PlaneStatus::ViewGateway);
}

#[test]
fn backend_planes_hogd_dev_full_serializes_correctly() {
    let planes = BackendPlanes::hogd_dev_full(PlaneStatus::ViewGateway);
    let json = serde_json::to_value(planes).expect("bug: BackendPlanes serializes");
    let expected = serde_json::json!({
        "submit": "scheduler",
        "chain": "db_gateway",
        "read": "indexer",
        "view": "view_gateway"
    });
    assert_eq!(json, expected);
}

#[test]
fn backend_planes_hogd_dev_storage_only_constructor() {
    let planes = BackendPlanes::hogd_dev_storage_only(PlaneStatus::Stub);
    assert_eq!(planes.submit, PlaneStatus::Scheduler);
    assert_eq!(planes.chain, PlaneStatus::DbGateway);
    assert_eq!(planes.read, PlaneStatus::DbGateway);
    assert_eq!(planes.view, PlaneStatus::Stub);
}

#[test]
fn desired_rpc_nofile_limit_uses_floor_and_headroom() {
    assert_eq!(desired_rpc_nofile_limit(10), RPC_NOFILE_LIMIT_FLOOR);
    assert_eq!(
        desired_rpc_nofile_limit(8_192),
        8_192 + RPC_NOFILE_LIMIT_HEADROOM
    );
}

#[cfg(unix)]
#[test]
fn rpc_nofile_soft_limit_update_fails_when_hard_limit_is_too_low() {
    let err = rpc_nofile_soft_limit_update(
        8_704,
        libc::rlimit {
            rlim_cur: 4_096,
            rlim_max: 8_192,
        },
    )
    .expect_err("hard limit below target should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::RpcNofileHardLimit {
            target: 8_704,
            hard: 8_192
        }
    ));
}

#[cfg(unix)]
#[test]
fn rpc_nofile_soft_limit_update_returns_needed_soft_limit() {
    let update = rpc_nofile_soft_limit_update(
        8_704,
        libc::rlimit {
            rlim_cur: 4_096,
            rlim_max: 16_384,
        },
    )
    .expect("hard limit permits target");

    assert_eq!(update, Some(8_704));
}

#[cfg(unix)]
#[test]
fn rpc_nofile_soft_limit_update_skips_when_soft_limit_is_high_enough() {
    let update = rpc_nofile_soft_limit_update(
        8_704,
        libc::rlimit {
            rlim_cur: 16_384,
            rlim_max: 16_384,
        },
    )
    .expect("existing soft limit should pass");

    assert_eq!(update, None);
}

#[test]
fn backend_planes_hogd_dev_storage_only_serializes_correctly() {
    let planes = BackendPlanes::hogd_dev_storage_only(PlaneStatus::Stub);
    let json = serde_json::to_value(planes).expect("bug: BackendPlanes serializes");
    let expected = serde_json::json!({
        "submit": "scheduler",
        "chain": "db_gateway",
        "read": "db_gateway",
        "view": "stub"
    });
    assert_eq!(json, expected);
}

// -----------------------------------------------------------------------
// Full read-plane integration tests (requires `ready-with-indexer` mode)
// -----------------------------------------------------------------------

/// Canned tx hashes from the fake indexer's committed set.
/// Mirrors the constants in `tests/fixtures/fake_hogd_dev.rs`.
const CANNED_HASH_A: &str = "CVDFLCAjXhVWiPXH9nTCTpCgVzmDVoiPzNJYuccr1dqB";
const CANNED_HASH_UNKNOWN: &str = "JEKNVnkbo3jma5nREBBJCDoXFVeKkD56V3xKrvRmWxFG";
const CANNED_HEIGHT_A: i64 = 5;
const CANNED_GAS_USED_A: u64 = 1_500;
const CANNED_STORAGE_KEY_HEX: &str =
    "0x4242424242424242424242424242424242424242424242424242424242424242";
const CANNED_STORAGE_MISSING_KEY_HEX: &str =
    "0x4343434343434343434343434343434343434343434343434343434343434343";
const CANNED_STORAGE_VALUE_HEX: &str = "0x6c6f63616c2d6e6f64652d73746f72616765";
const CANNED_STORAGE_HEIGHT: i64 = 12;

#[tokio::test]
#[serial]
async fn start_with_indexer_reports_hogd_dev_full_backend_planes() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev with indexer");

    assert_eq!(
        handle.backend_planes(),
        BackendPlanes::hogd_dev_full(PlaneStatus::Stub),
        "supervisor with indexer should report hogd_dev_full backend planes"
    );
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", handle.rpc_addr()))
        .expect("bug: jsonrpsee http client builds");

    let response = client
        .request::<out::AirdropAttempt, _>(
            "faucet_requestAirdrop",
            NamedParams(AirdropRequest {
                recipient: Base58(thruput_xdr::types::ContractAddress::from([0x88; 32])),
                client_nonce: ClientNonce::new(vec![0x99])
                    .expect("bug: test client nonce should validate"),
            }),
        )
        .await
        .expect("bug: full local runtime should expose airdrop");

    assert!(matches!(response, out::AirdropAttempt::Pending { .. }));

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn start_with_gateway_without_indexer_reports_storage_only_backend_planes() {
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-gateway");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start against fake hogd-dev with gateway");

    assert_eq!(
        handle.backend_planes(),
        BackendPlanes::hogd_dev_storage_only(PlaneStatus::Stub),
        "supervisor with only DbGateway should report storage-only backend planes"
    );
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", handle.rpc_addr()))
        .expect("bug: jsonrpsee http client builds");
    let error = client
        .request::<out::AirdropAttempt, _>(
            "faucet_requestAirdrop",
            NamedParams(AirdropRequest {
                recipient: Base58(thruput_xdr::types::ContractAddress::from([0x88; 32])),
                client_nonce: ClientNonce::new(vec![0x99])
                    .expect("bug: test client nonce should validate"),
            }),
        )
        .await
        .expect_err("bug: storage-only runtime must not expose airdrop");
    assert!(
        matches!(&error, ClientError::Call(object) if object.code() == jsonrpsee::types::error::METHOD_NOT_FOUND_CODE),
        "storage-only runtime should omit airdrop without a receipt backend, got {error:?}",
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn transaction_status_committed_returns_committed_with_correct_height() {
    // Arrange: start supervisor against fake that has both scheduler + indexer.
    // The fake scheduler returns empty (no pending), the fake indexer has
    // CANNED_TX_HASH_A committed at CANNED_HEIGHT_A.
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start");
    let url = format!("http://{}", handle.rpc_addr());

    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");

    // Act: query status for a canned committed hash.
    let status: serde_json::Value = client
        .request(
            "getTransactionStatus",
            NamedParams(serde_json::json!({"transactionHash": CANNED_HASH_A})),
        )
        .await
        .expect("bug: getTransactionStatus should succeed");

    // Assert: status field is "committed", height matches.
    assert_eq!(
        status["status"], "committed",
        "expected committed status for known tx, got {status}"
    );
    assert_eq!(
        status["blockHeight"],
        CANNED_HEIGHT_A.to_string(),
        "committed status height should be CANNED_HEIGHT_A"
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn transaction_status_unknown_hash_returns_unknown() {
    // Arrange
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start");
    let url = format!("http://{}", handle.rpc_addr());

    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");

    // Act: query status for an unknown hash.
    let status: serde_json::Value = client
        .request(
            "getTransactionStatus",
            NamedParams(serde_json::json!({"transactionHash": CANNED_HASH_UNKNOWN})),
        )
        .await
        .expect("bug: getTransactionStatus should succeed for unknown hash");

    // Assert: status field is "unknown".
    assert_eq!(
        status["status"], "unknown",
        "expected unknown status for non-canned hash, got {status}"
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn transaction_receipt_committed_returns_some() {
    // Arrange
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start");
    let url = format!("http://{}", handle.rpc_addr());

    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");

    // Act: request receipt for a canned committed hash.
    let receipt: serde_json::Value = client
        .request(
            "getTransactionReceipt",
            NamedParams(serde_json::json!({"transactionHash": CANNED_HASH_A})),
        )
        .await
        .expect("bug: getTransactionReceipt should succeed");

    // Assert: receipt is non-null, tx_hash and height round-trip correctly.
    assert!(
        !receipt.is_null(),
        "receipt should be non-null for committed tx"
    );
    assert_eq!(
        receipt["blockHeight"],
        CANNED_HEIGHT_A.to_string(),
        "receipt height should be CANNED_HEIGHT_A"
    );
    assert_eq!(
        receipt["transactionHash"].as_str().unwrap_or(""),
        CANNED_HASH_A,
        "receipt txHash should round-trip"
    );
    assert_eq!(
        receipt["gasUsed"],
        CANNED_GAS_USED_A.to_string(),
        "receipt should carry the gas the fake indexer's event log records"
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn contract_events_first_page_returns_two_events() {
    // Arrange
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start");
    let url = format!("http://{}", handle.rpc_addr());

    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");

    // Build a minimal EventQuery (first page, one named contract).
    // Serialization shapes:
    //   - `IterationOrder` serializes as "ascending" / "descending"
    //   - `BlockAnchor` serializes as a tagged object
    //   - `PaginationCursor` serializes as `0x`-prefixed hex; `0x` = empty
    let contract = thruput_json_rpc_types::ContractAddress::from(
        thruput_xdr::types::ContractAddress::from([1; 32]),
    )
    .to_string();
    let query = serde_json::json!({
        "filters": [{
            "addresses": [contract],
            "topics": {"topic0": [], "topic1": [], "topic2": []},
        }],
        "start": {"order": "ascending", "anchor": { "kind": "atBeginning" }},
        "limit": 10
    });

    // Act
    let page: serde_json::Value = client
        .request("getEvents", NamedParams(query))
        .await
        .expect("bug: getEvents should succeed");

    // Assert: first page has 2 events and a non-empty cursor.
    let events = page["events"]
        .as_array()
        .expect("bug: events should be array");
    assert_eq!(events.len(), 2, "first event page should contain 2 events");
    assert!(
        events
            .iter()
            .all(|event| event["event"]["address"] == contract),
        "events should come from the filtered contract"
    );
    let cursor = page["nextCursor"].as_str().unwrap_or("");
    assert!(
        cursor.len() > 2,
        "first page cursor should be non-empty (got {cursor:?})"
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn get_storage_at_existing_key_returns_item_from_hogd_dev_gateway() {
    // Arrange
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start");
    let url = format!("http://{}", handle.rpc_addr());

    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");
    let address = "1".repeat(32);

    // Act
    let read: serde_json::Value = client
        .request(
            "getStorage",
            NamedParams(serde_json::json!({"address": address, "key": CANNED_STORAGE_KEY_HEX})),
        )
        .await
        .expect("bug: getStorageAt should succeed");

    // Assert
    assert_eq!(read["stateBlockHeight"], CANNED_STORAGE_HEIGHT.to_string());
    assert_eq!(read["item"]["kind"], "bytestring");
    assert_eq!(read["item"]["bytes"], CANNED_STORAGE_VALUE_HEX);

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn get_storage_at_works_when_gateway_is_advertised_without_indexer() {
    // Arrange
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-gateway");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start");
    let url = format!("http://{}", handle.rpc_addr());

    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");
    let address = "1".repeat(32);

    // Act
    let read: serde_json::Value = client
        .request(
            "getStorage",
            NamedParams(serde_json::json!({"address": address, "key": CANNED_STORAGE_KEY_HEX})),
        )
        .await
        .expect("bug: getStorageAt should succeed with DbGateway only");

    // Assert
    assert_eq!(read["stateBlockHeight"], CANNED_STORAGE_HEIGHT.to_string());
    assert_eq!(read["item"]["kind"], "bytestring");
    assert_eq!(read["item"]["bytes"], CANNED_STORAGE_VALUE_HEX);

    let err = client
        .request::<serde_json::Value, _>(
            "getTransactionReceipt",
            NamedParams(serde_json::json!({"transactionHash": CANNED_HASH_A})),
        )
        .await
        .expect_err("bug: receipt reads should remain unavailable without indexer");
    assert!(
        matches!(&err, ClientError::Call(object) if object.code() == jsonrpsee::types::error::METHOD_NOT_FOUND_CODE),
        "expected method not found JSON-RPC error, got {err:?}",
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn get_storage_at_missing_key_returns_null_item_with_snapshot_height() {
    // Arrange
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start");
    let url = format!("http://{}", handle.rpc_addr());

    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");
    let address = "1".repeat(32);

    // Act
    let read: serde_json::Value = client
        .request(
            "getStorage",
            NamedParams(
                serde_json::json!({"address": address, "key": CANNED_STORAGE_MISSING_KEY_HEX}),
            ),
        )
        .await
        .expect("bug: getStorageAt miss should succeed");

    // Assert
    assert_eq!(read["stateBlockHeight"], CANNED_STORAGE_HEIGHT.to_string());
    assert!(
        read["item"].is_null(),
        "missing storage item should serialize as null"
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn get_storage_without_gateway_endpoint_is_not_registered() {
    // Arrange
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-with-indexer-no-gateway");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start");
    let url = format!("http://{}", handle.rpc_addr());

    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");
    let address = "1".repeat(32);

    // Act
    let err = client
        .request::<serde_json::Value, _>(
            "getStorage",
            NamedParams(serde_json::json!({"address": address, "key": CANNED_STORAGE_KEY_HEX})),
        )
        .await
        .expect_err("bug: missing DbGateway should fail");

    // Assert
    assert!(
        matches!(&err, ClientError::Call(object) if object.code() == jsonrpsee::types::error::METHOD_NOT_FOUND_CODE),
        "expected method not found JSON-RPC error, got {err:?}",
    );

    handle.stop().await.expect("bug: clean shutdown");
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_indexer_connect_fails() {
    let err = start_fake_supervisor_expect_error("indexer-connect-fail").await;

    assert!(
        matches!(err, NodeSupervisorError::IndexerConnect { .. }),
        "expected IndexerConnect error, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_indexer_rpc_fail_at_startup() {
    // The `indexer-rpc-fail` fake mode returns a ProgramError even for the
    // intro handshake. With the intro probe in HogdDevReadBackend::connect,
    // this now fails at startup rather than at the first read call.
    let err = start_fake_supervisor_expect_error("indexer-rpc-fail").await;

    assert!(
        matches!(
            err,
            NodeSupervisorError::IndexerRpc {
                op: "indexer_block_explorer_intro",
                ..
            }
        ),
        "expected IndexerRpc error for intro, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_db_gateway_connect_fails() {
    let err = start_fake_supervisor_expect_error("db-gateway-connect-fail").await;

    assert!(
        matches!(err, NodeSupervisorError::DbGatewayConnect { .. }),
        "expected DbGatewayConnect error, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_db_gateway_rpc_fail_at_startup() {
    let err = start_fake_supervisor_expect_error("db-gateway-rpc-fail").await;

    assert!(
        matches!(
            err,
            NodeSupervisorError::DbGatewayRpc {
                op: "dbgateway_intro",
                ..
            }
        ),
        "expected DbGatewayRpc error for intro, got {err:?}",
    );
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_db_gateway_intro_rejected() {
    let err = start_fake_supervisor_expect_error("db-gateway-intro-rejected").await;

    let message = err.to_string();
    match err {
        NodeSupervisorError::ProtocolIntroRejected {
            service,
            outcome,
            outcome_name,
            is_protocol_mismatch,
            protocol,
            expected_protocol,
        } => {
            assert_eq!(service, "DbGateway");
            assert_eq!(outcome, 1);
            assert_eq!(outcome_name, "DbGatewayIntroMismatchedProtocol");
            assert!(is_protocol_mismatch);
            assert_eq!(protocol, protocol_text(&PROTOCOL_VERSION));
            assert_eq!(
                expected_protocol.as_deref(),
                Some(protocol_text(&PROTOCOL_VERSION).as_str())
            );
            assert!(message.contains("DbGateway intro mismatch"));
            assert!(message.contains(&protocol_text(&PROTOCOL_VERSION)));
            assert!(message.contains("DbGatewayIntroMismatchedProtocol"));
        }
        other => panic!("expected ProtocolIntroRejected error, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn start_rolls_back_hogd_dev_when_indexer_intro_rejected() {
    let err = start_fake_supervisor_expect_error("indexer-intro-rejected").await;

    let message = err.to_string();
    match err {
        NodeSupervisorError::ProtocolIntroRejected {
            service,
            outcome,
            outcome_name,
            is_protocol_mismatch,
            protocol,
            expected_protocol,
        } => {
            assert_eq!(service, "IndexerBlockExplorer");
            assert_eq!(outcome, 1);
            assert_eq!(outcome_name, "IntroMismatchedProtocol");
            assert!(is_protocol_mismatch);
            assert_eq!(protocol, protocol_text(&PROTOCOL_VERSION));
            assert_eq!(
                expected_protocol.as_deref(),
                Some(protocol_text(&PROTOCOL_VERSION).as_str())
            );
            assert!(message.contains("IndexerBlockExplorer intro mismatch"));
            assert!(message.contains(&protocol_text(&PROTOCOL_VERSION)));
            assert!(message.contains("IntroMismatchedProtocol"));
        }
        other => panic!("expected ProtocolIntroRejected error, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn transaction_status_indexer_runtime_rpc_fail_surfaces_backend_unavailable() {
    // The `indexer-runtime-rpc-fail` fake mode lets intro succeed but
    // returns errors for all data-plane RPCs (get_transaction_info, etc).
    // Startup should succeed; JSON-RPC calls should return a backend error.
    let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "indexer-runtime-rpc-fail");
    let hogd = HogdDevConfig::default()
        .with_binary_path(fake_hogd_dev_binary())
        .with_startup_timeout(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_secs(2));
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);

    let handle = NodeSupervisorHandle::start(config)
        .await
        .expect("bug: supervisor should start when indexer intro succeeds");

    let url = format!("http://{}", handle.rpc_addr());
    let client = HttpClientBuilder::default()
        .build(&url)
        .expect("bug: jsonrpsee http client builds");

    // Act: query a tx status; the fake indexer will fail the data-plane call.
    let err = client
        .request::<serde_json::Value, _>(
            "getTransactionStatus",
            NamedParams(serde_json::json!({"transactionHash": CANNED_HASH_A})),
        )
        .await
        .expect_err("bug: getTransactionStatus should error when indexer data-plane fails");

    // Assert: error is a JSON-RPC call error (backend unavailable or
    // internal), not a transport-level failure.
    assert!(
        matches!(&err, ClientError::Call(_)),
        "expected a JSON-RPC call error from backend failure, got {err:?}",
    );

    handle.stop().await.expect("bug: clean shutdown");
}
