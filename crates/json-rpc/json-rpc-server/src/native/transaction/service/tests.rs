//! Submission pre-checks and the shared db-tip cache behind them, driven
//! through the native `sendTransaction` path.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::*;
use crate::native::transaction::{NativeSubmitPlane, SubmissionAttempt};
use crate::tx::test_fixtures::{canonical_signed_transaction, signed_tx_bytes_with_contracts};
use crate::{RpcServerError, TipReading, ValidatedTransaction};
use jsonrpsee::types::ErrorObjectOwned;
use thruput_json_rpc_types::base58::Base58;
use thruput_json_rpc_types::block::ChainTip;
use thruput_json_rpc_types::error::native::codes;
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::transaction::TxHash;
use thruput_json_rpc_types::{BlockHeight, requests};
use xdr_common::VecM;

type Server<S, C> = NativeTransactionService<S, C, ()>;

fn server<S, C: ChainPlane>(submit: S, chain: C, config: RpcServerConfig) -> Server<S, C> {
    NativeTransactionService::new(submit, chain, (), config)
}

async fn submit<S: NativeSubmitPlane, C: ChainPlane>(
    server: &Server<S, C>,
    tx: Vec<u8>,
) -> Result<TxHash, ErrorObjectOwned> {
    server
        .send_transaction(requests::SendTransactionRequest {
            signed_transaction: HexBytes(tx),
        })
        .await
        .map(|result| result.transaction_hash)
}

fn canonical_tx() -> Vec<u8> {
    xdrs::txs::encode(&canonical_signed_transaction()).expect("bug: fixture should encode")
}

fn data(error: &ErrorObjectOwned) -> serde_json::Value {
    serde_json::from_str(error.data().expect("error should carry data").get()).unwrap()
}

/// A tip reading whose network config states `params`.
fn tip_reading_with_params(
    height: i64,
    params: Vec<thruput_xdr::network_config::ConfigParam>,
) -> TipReading {
    TipReading::new(
        ChainTip {
            height: BlockHeight::from(height),
            block_hash: Base58(thruput_xdr::types::SHA256Hash::from([0u8; 32])),
        },
        thruput_xdr::network_config::NetworkConfig::_0 {
            v0: thruput_xdr::network_config::NetworkConfigV0 {
                params: VecM(params),
            },
        },
    )
}

fn tip_reading(height: i64) -> TipReading {
    tip_reading_with_params(height, vec![])
}

/// A config stating only `maxNewContractsPerTx`.
fn max_contracts_params(
    max_new_contracts_per_tx: u32,
) -> Vec<thruput_xdr::network_config::ConfigParam> {
    vec![
        thruput_xdr::network_config::ConfigParam::kConfigParamMaxNewContractsPerTx {
            max_new_contracts_per_tx,
        },
    ]
}

fn rv32_contract(size: usize, fill: u8) -> thruput_xdr::contract::Contract {
    thruput_xdr::contract::Contract::kRv32 {
        rv32_contract: thruput_xdr::contract::ContractBlob {
            data: xdr_common::BytesM(vec![fill; size]),
        },
    }
}

#[derive(Clone)]
struct SubmitOnly {
    submitted: Arc<Mutex<Vec<ValidatedTransaction>>>,
}

impl SubmitOnly {
    fn new() -> Self {
        Self {
            submitted: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl NativeSubmitPlane for SubmitOnly {
    async fn native_submit(
        &self,
        _context: RpcRequestContext,
        tx: ValidatedTransaction,
        _attempt: SubmissionAttempt,
    ) -> Result<(), RpcServerError> {
        self.submitted.lock().await.push(tx);
        Ok(())
    }
}

#[derive(Clone)]
struct RecordingSubmit {
    contexts: Arc<Mutex<Vec<RpcRequestContext>>>,
    delay: Duration,
}

#[async_trait::async_trait]
impl NativeSubmitPlane for RecordingSubmit {
    async fn native_submit(
        &self,
        context: RpcRequestContext,
        _tx: ValidatedTransaction,
        _attempt: SubmissionAttempt,
    ) -> Result<(), RpcServerError> {
        self.contexts.lock().await.push(context);
        tokio::time::sleep(self.delay).await;
        Ok(())
    }
}

/// Implement `ChainPlane` from a `chain_tip` body; the submit path reads
/// nothing else from the chain plane.
macro_rules! chain_plane {
    ($ty:ty, |$self:ident, $context:ident| $body:expr) => {
        #[async_trait::async_trait]
        impl ChainPlane for $ty {
            async fn chain_tip(
                &$self,
                $context: RpcRequestContext,
            ) -> Result<TipReading, RpcServerError> {
                $body
            }

            async fn pending_transaction_count(
                &self,
                _context: RpcRequestContext,
            ) -> Result<u64, RpcServerError> {
                unreachable!("the submit path does not read the mempool")
            }
        }
    };
}

/// Tip at the canonical vector's `block_expiry_height` of 1,000,000.
#[derive(Clone, Copy)]
struct ExpiredTipChain;
chain_plane!(ExpiredTipChain, |self, _context| Ok(tip_reading(1_000_000)));

/// Chain plane whose `chain_tip` always fails.
#[derive(Clone, Copy)]
struct UnavailableChain;
chain_plane!(UnavailableChain, |self, _context| Err(
    RpcServerError::BackendUnavailable("scheduler down".into())
));

/// Chain plane whose `chain_tip` never completes in test time.
#[derive(Clone, Copy)]
struct StalledTipChain;
chain_plane!(StalledTipChain, |self, _context| {
    tokio::time::sleep(Duration::from_hours(1)).await;
    unreachable!("bug: stalled tip probe should have been abandoned")
});

/// Counts `chain_tip` calls and reports a fixed pre-expiry tip, optionally
/// taking `probe_duration` to answer.
#[derive(Clone)]
struct CountingTipChain {
    tip_calls: Arc<Mutex<u32>>,
    probe_duration: Duration,
}

impl CountingTipChain {
    fn new(tip_calls: Arc<Mutex<u32>>) -> Self {
        Self::slow(tip_calls, Duration::ZERO)
    }

    fn slow(tip_calls: Arc<Mutex<u32>>, probe_duration: Duration) -> Self {
        Self {
            tip_calls,
            probe_duration,
        }
    }
}

chain_plane!(CountingTipChain, |self, _context| {
    *self.tip_calls.lock().await += 1;
    if !self.probe_duration.is_zero() {
        tokio::time::sleep(self.probe_duration).await;
    }
    Ok(tip_reading(42))
});

#[derive(Clone)]
struct RecordingTipChain {
    contexts: Arc<Mutex<Vec<RpcRequestContext>>>,
    delay: Duration,
}

chain_plane!(RecordingTipChain, |self, context| {
    self.contexts.lock().await.push(context);
    tokio::time::sleep(self.delay).await;
    Ok(tip_reading(42))
});

/// Pre-expiry tip carrying chosen network config parameters; counts how
/// often it is asked.
#[derive(Clone)]
struct ConfigChain {
    params: Arc<Mutex<Vec<thruput_xdr::network_config::ConfigParam>>>,
    tip_calls: Arc<Mutex<u32>>,
}

impl ConfigChain {
    /// A tip whose config states only `maxNewContractsPerTx`.
    fn new(max_new_contracts_per_tx: u32) -> Self {
        Self::with_params(max_contracts_params(max_new_contracts_per_tx))
    }

    fn with_params(params: Vec<thruput_xdr::network_config::ConfigParam>) -> Self {
        Self {
            params: Arc::new(Mutex::new(params)),
            tip_calls: Arc::new(Mutex::new(0)),
        }
    }

    /// Replace the config the next tip reports, as an upgrade block does.
    async fn set_params(&self, params: Vec<thruput_xdr::network_config::ConfigParam>) {
        *self.params.lock().await = params;
    }
}

chain_plane!(ConfigChain, |self, _context| {
    *self.tip_calls.lock().await += 1;
    Ok(tip_reading_with_params(
        42,
        self.params.lock().await.clone(),
    ))
});

#[tokio::test]
async fn expiry_precheck_probes_tip_at_most_once_per_refresh_interval() {
    let tip_calls = Arc::new(Mutex::new(0));
    let submitter = SubmitOnly::new();
    let server = server(
        submitter.clone(),
        CountingTipChain::new(tip_calls.clone()),
        RpcServerConfig::default(),
    );

    for _ in 0..3 {
        submit(&server, canonical_tx())
            .await
            .expect("bug: unexpired transaction should submit");
    }

    assert_eq!(
        *tip_calls.lock().await,
        1,
        "tip should be probed once within the refresh interval"
    );
    assert_eq!(submitter.submitted.lock().await.len(), 3);
}

/// The reuse window must never be shorter than the interval that refreshes it.
#[tokio::test(start_paused = true)]
async fn submit_inside_the_refresh_interval_does_not_fetch_its_own_tip() {
    const INTERVAL_MS: u64 = 10_000;

    let tip_calls = Arc::new(Mutex::new(0));
    let server = server(
        SubmitOnly::new(),
        CountingTipChain::new(tip_calls.clone()),
        RpcServerConfig::default().with_tip_refresh_interval_ms(INTERVAL_MS),
    );

    submit(&server, canonical_tx())
        .await
        .expect("bug: unexpired transaction should submit");
    assert_eq!(
        *tip_calls.lock().await,
        1,
        "the first submit warms the reading"
    );

    // Last instant inside the interval: any shorter window has lapsed, and
    // the first refresh tick has not run.
    tokio::time::advance(Duration::from_millis(INTERVAL_MS - 1)).await;

    submit(&server, canonical_tx())
        .await
        .expect("bug: unexpired transaction should submit");
    assert_eq!(
        *tip_calls.lock().await,
        1,
        "a submit inside the refresh interval must reuse the reading \
         rather than fetch one on its own latency path"
    );
}

#[tokio::test]
async fn expiry_precheck_probes_tip_again_once_the_window_lapses() {
    let tip_calls = Arc::new(Mutex::new(0));
    let server = server(
        SubmitOnly::new(),
        CountingTipChain::new(tip_calls.clone()),
        RpcServerConfig::default().with_tip_refresh_interval_ms(0),
    );

    for _ in 0..2 {
        submit(&server, canonical_tx())
            .await
            .expect("bug: unexpired transaction should submit");
    }

    assert_eq!(
        *tip_calls.lock().await,
        2,
        "an expired cache entry should trigger a fresh probe"
    );
}

#[tokio::test]
async fn stalled_tip_probe_is_abandoned_and_submit_proceeds() {
    let submitter = SubmitOnly::new();
    let server = server(
        submitter.clone(),
        StalledTipChain,
        RpcServerConfig::default().with_tip_probe_timeout_ms(50),
    );

    let started = std::time::Instant::now();
    submit(&server, canonical_tx())
        .await
        .expect("bug: a stalled tip probe must not block submission");

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "submit should not ride out the stalled probe, took {:?}",
        started.elapsed()
    );
    assert_eq!(submitter.submitted.lock().await.len(), 1);
}

#[tokio::test]
async fn chain_tip_failure_fails_open_and_submits() {
    let submitter = SubmitOnly::new();
    let server = server(
        submitter.clone(),
        UnavailableChain,
        RpcServerConfig::default(),
    );

    submit(&server, canonical_tx())
        .await
        .expect("bug: tip unavailability should not block submission");

    assert_eq!(submitter.submitted.lock().await.len(), 1);
}

#[tokio::test]
async fn expired_transaction_is_rejected_before_submit() {
    let submitter = SubmitOnly::new();
    let server = server(
        submitter.clone(),
        ExpiredTipChain,
        RpcServerConfig::default(),
    );

    let error = submit(&server, canonical_tx())
        .await
        .expect_err("bug: expired transaction should be rejected");

    assert_eq!(error.code(), codes::TRANSACTION_REJECTED);
    assert_eq!(data(&error)["reason"], "expired");
    assert_eq!(data(&error)["admission"], "notAttempted");
    assert!(data(&error)["transactionHash"].is_string());
    assert!(submitter.submitted.lock().await.is_empty());
}

#[tokio::test]
async fn validation_expiry_probe_and_submit_share_one_request_deadline() {
    let submit_contexts = Arc::new(Mutex::new(Vec::new()));
    let tip_contexts = Arc::new(Mutex::new(Vec::new()));
    let server = server(
        RecordingSubmit {
            contexts: submit_contexts.clone(),
            delay: Duration::from_millis(40),
        },
        RecordingTipChain {
            contexts: tip_contexts.clone(),
            delay: Duration::from_millis(25),
        },
        RpcServerConfig::default()
            .with_request_timeout_ms(50)
            .with_tip_probe_timeout_ms(500),
    );

    let error = submit(&server, canonical_tx())
        .await
        .expect_err("the combined probe and submit work should exhaust one request budget");

    assert_eq!(error.code(), codes::BACKEND_TIMEOUT);
    let tip_context = tip_contexts.lock().await[0];
    let submit_context = submit_contexts.lock().await[0];
    assert_eq!(tip_context.started_at(), submit_context.started_at());
    assert_eq!(tip_context.deadline(), submit_context.deadline());
}

#[tokio::test]
async fn expiry_probe_uses_a_shorter_deadline_without_resetting_request_start() {
    let submit_contexts = Arc::new(Mutex::new(Vec::new()));
    let tip_contexts = Arc::new(Mutex::new(Vec::new()));
    let server = server(
        RecordingSubmit {
            contexts: submit_contexts.clone(),
            delay: Duration::ZERO,
        },
        RecordingTipChain {
            contexts: tip_contexts.clone(),
            delay: Duration::ZERO,
        },
        RpcServerConfig::default()
            .with_request_timeout_ms(500)
            .with_tip_probe_timeout_ms(50),
    );

    submit(&server, canonical_tx())
        .await
        .expect("the bounded expiry probe and submission should complete");

    let tip_context = tip_contexts.lock().await[0];
    let submit_context = submit_contexts.lock().await[0];
    assert_eq!(tip_context.started_at(), submit_context.started_at());
    assert!(
        tip_context.deadline().expect("probe deadline")
            < submit_context.deadline().expect("request deadline"),
        "the nested expiry probe should receive the shorter deadline"
    );
}

#[tokio::test]
async fn config_reading_rides_on_the_tip_probe() {
    let chain = ConfigChain::new(4);
    let submitter = SubmitOnly::new();
    let server = server(submitter.clone(), chain.clone(), RpcServerConfig::default());

    for _ in 0..3 {
        submit(
            &server,
            signed_tx_bytes_with_contracts(vec![rv32_contract(8, 1)]),
        )
        .await
        .expect("bug: one attached contract is within a cap of four");
    }

    assert_eq!(
        *chain.tip_calls.lock().await,
        1,
        "count check should use the tip reading, not a second backend call"
    );
    assert_eq!(submitter.submitted.lock().await.len(), 3);
}

#[tokio::test]
async fn too_many_contracts_is_rejected_with_structured_data_before_submit() {
    let submitter = SubmitOnly::new();
    let server = server(
        submitter.clone(),
        ConfigChain::new(1),
        RpcServerConfig::default(),
    );

    let error = submit(
        &server,
        signed_tx_bytes_with_contracts(vec![rv32_contract(8, 1), rv32_contract(8, 2)]),
    )
    .await
    .expect_err("bug: two contracts should exceed a cap of one");

    assert_eq!(error.code(), codes::TRANSACTION_INVALID);
    assert_eq!(
        error.message(),
        "invalid transaction: transaction attaches 2 contracts; chain permits 1"
    );
    assert_eq!(
        data(&error),
        serde_json::json!({
            "kind": "transactionInvalid",
            "reason": "executorBoundViolation",
            "admission": "notAttempted",
            "detail": {"reason": "tooManyContracts", "attached": 2, "max": 1}
        })
    );
    assert!(submitter.submitted.lock().await.is_empty());
}

#[tokio::test]
async fn oversized_contract_is_rejected_when_tip_is_unavailable() {
    // Without a tip reading the chain-configured count check is skipped,
    // but the size cap the protocol fixes for an unstated config is still in
    // force.
    let submitter = SubmitOnly::new();
    let server = server(
        submitter.clone(),
        UnavailableChain,
        RpcServerConfig::default(),
    );
    let oversized = thruput_xdr::contract::kDefaultMaxContractSizeBytes as usize + 1;

    let error = submit(
        &server,
        signed_tx_bytes_with_contracts(vec![rv32_contract(oversized, 1)]),
    )
    .await
    .expect_err("bug: oversized contract should be rejected");
    submit(
        &server,
        signed_tx_bytes_with_contracts((1..=8).map(|fill| rv32_contract(1, fill)).collect()),
    )
    .await
    .expect("bug: without a tip reading the count check is skipped");

    assert_eq!(error.code(), codes::TRANSACTION_INVALID);
    assert_eq!(data(&error)["detail"]["reason"], "contractTooLarge");
    assert_eq!(data(&error)["detail"]["index"], 0);
    assert_eq!(submitter.submitted.lock().await.len(), 1);
}

#[tokio::test]
async fn stated_contract_size_cap_from_the_tip_bounds_submissions() {
    // The tip's config states a cap far below the protocol default; the
    // submit path must apply the stated value, not the constant.
    let chain = ConfigChain::with_params(vec![
        thruput_xdr::network_config::ConfigParam::kConfigParamMaxContractSizeBytes {
            max_contract_size_bytes: 64,
        },
    ]);
    let submitter = SubmitOnly::new();
    let server = server(submitter.clone(), chain, RpcServerConfig::default());

    let error = submit(
        &server,
        signed_tx_bytes_with_contracts(vec![rv32_contract(65, 1)]),
    )
    .await
    .expect_err("bug: a 65-byte contract should exceed the stated 64-byte cap");
    submit(
        &server,
        signed_tx_bytes_with_contracts(vec![rv32_contract(64, 1)]),
    )
    .await
    .expect("bug: a contract at the stated cap should be admitted");

    assert_eq!(error.code(), codes::TRANSACTION_INVALID);
    assert_eq!(
        data(&error),
        serde_json::json!({
            "kind": "transactionInvalid",
            "reason": "executorBoundViolation",
            "admission": "notAttempted",
            "detail": {"reason": "contractTooLarge", "index": 0, "sizeBytes": 65, "maxBytes": 64}
        })
    );
    assert_eq!(submitter.submitted.lock().await.len(), 1);
}

#[tokio::test]
async fn bound_violation_is_rechecked_against_a_fresh_tip_before_rejecting() {
    // The cached config caps attachments at one; an upgrade then raises it to
    // two. A submit inside the window must not reject on the stale cap: it
    // reads the tip again and applies the raised one.
    let chain = ConfigChain::new(1);
    let submitter = SubmitOnly::new();
    let server = server(submitter.clone(), chain.clone(), RpcServerConfig::default());
    submit(
        &server,
        signed_tx_bytes_with_contracts(vec![rv32_contract(8, 1)]),
    )
    .await
    .expect("bug: one contract is within a cap of one");
    chain.set_params(max_contracts_params(2)).await;

    let two_contracts =
        signed_tx_bytes_with_contracts(vec![rv32_contract(8, 1), rv32_contract(8, 2)]);
    submit(&server, two_contracts.clone())
        .await
        .expect("bug: the raised cap should admit two contracts");

    assert_eq!(
        *chain.tip_calls.lock().await,
        2,
        "the violation should trigger exactly one fresh probe"
    );
    let sent = submitter.submitted.lock().await;
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1].bytes, two_contracts,
        "the recheck must validate and submit the same bytes"
    );
}

#[tokio::test]
async fn bound_violation_on_a_fresh_reading_is_rejected_without_a_second_probe() {
    let chain = ConfigChain::new(1);
    let submitter = SubmitOnly::new();
    let server = server(submitter.clone(), chain.clone(), RpcServerConfig::default());

    let error = submit(
        &server,
        signed_tx_bytes_with_contracts(vec![rv32_contract(8, 1), rv32_contract(8, 2)]),
    )
    .await
    .expect_err("bug: two contracts should exceed a cap of one");

    assert_eq!(error.code(), codes::TRANSACTION_INVALID);
    assert_eq!(
        *chain.tip_calls.lock().await,
        1,
        "the reading this request fetched is already current"
    );
    assert!(submitter.submitted.lock().await.is_empty());
}

#[tokio::test]
async fn background_refresh_fetches_the_tip_without_submissions() {
    let chain = ConfigChain::new(4);
    let _server = server(
        SubmitOnly::new(),
        chain.clone(),
        RpcServerConfig::default().with_tip_refresh_interval_ms(10),
    );

    let started = std::time::Instant::now();
    while *chain.tip_calls.lock().await == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "background refresh should have probed the tip by now"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Refreshes must land on a fixed schedule: scheduling from the end of each
/// probe pushes every later one out by the time that probe took.
#[tokio::test(start_paused = true)]
async fn background_refresh_does_not_drift_by_the_time_a_probe_takes() {
    const INTERVAL_MS: u64 = 10_000;
    const PROBE: Duration = Duration::from_secs(1);

    // Advancing a paused clock does not itself poll the spawned task;
    // yielding afterwards lets it observe the tick.
    async fn advance(by: Duration) {
        tokio::time::advance(by).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    let tip_calls = Arc::new(Mutex::new(0));
    let _server = server(
        SubmitOnly::new(),
        CountingTipChain::slow(tip_calls.clone(), PROBE),
        RpcServerConfig::default().with_tip_refresh_interval_ms(INTERVAL_MS),
    );
    // Anchor the task's schedule at zero before moving the clock.
    tokio::task::yield_now().await;

    // First tick at 10s. The probe counts on entry, then takes a second to
    // answer.
    advance(Duration::from_millis(INTERVAL_MS)).await;
    assert_eq!(
        *tip_calls.lock().await,
        1,
        "the first refresh is due at 10s"
    );
    advance(PROBE).await;

    // The second belongs at 20s. A schedule restarted when the first probe
    // answered would place it at 21s instead.
    advance(Duration::from_millis(8_900)).await;
    assert_eq!(*tip_calls.lock().await, 1, "not due before 20s");

    advance(Duration::from_millis(200)).await;
    assert_eq!(
        *tip_calls.lock().await,
        2,
        "the second refresh must land on the fixed schedule, not one probe later"
    );
}

#[tokio::test]
async fn background_refresh_stops_when_the_server_is_dropped() {
    let chain = ConfigChain::new(4);
    let server = server(
        SubmitOnly::new(),
        chain.clone(),
        RpcServerConfig::default().with_tip_refresh_interval_ms(5),
    );
    while *chain.tip_calls.lock().await == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    drop(server);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let after_drop = *chain.tip_calls.lock().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        *chain.tip_calls.lock().await,
        after_drop,
        "no probes should run after the last server clone is dropped"
    );
}
