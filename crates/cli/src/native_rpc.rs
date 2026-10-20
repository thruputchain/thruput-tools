//! Native RPC response and error evidence. No recovery action is inferred.

use serde_json::{Value, json};
use std::{future::Future, pin::Pin, task::Poll};
use thruput_client::{NativeRpcError, SubmitCallError};
use thruput_json_rpc_client::native::NativeClientError;
use thruput_json_rpc_client::native::receipt::{ReceiptPollCause, ReceiptPollError};

use crate::CliError;

pub(crate) fn failure(message: impl Into<String>, evidence: Value) -> CliError {
    CliError::NativeRpcFailure {
        message: message.into(),
        evidence,
    }
}

pub(crate) fn client_evidence(error: &NativeClientError) -> Value {
    match error {
        NativeClientError::Application { source, .. } => json!({"rpcError": source}),
        NativeClientError::Transport(source) => json!({"transportError": source.to_string()}),
        NativeClientError::InvalidResponse {
            method,
            raw,
            source,
        } => json!({
            "method": method, "result": raw, "decodeError": source.to_string(),
        }),
        _ => json!({"unsupportedClientError": error.to_string()}),
    }
}

pub(crate) fn client_error(error: &NativeClientError) -> CliError {
    failure(error.to_string(), client_evidence(error))
}

pub(crate) fn rpc_error(error: &NativeRpcError) -> CliError {
    let evidence = match error {
        NativeRpcError::Call { method, source } => {
            json!({"method": method, "cause": client_evidence(source)})
        }
        NativeRpcError::InvalidDiscovery { source } => {
            json!({"method": "getVersionInfo", "cause": client_evidence(source)})
        }
        NativeRpcError::Unsupported { method, raw } => json!({"method": method, "result": raw}),
        NativeRpcError::NetworkMismatch { expected, actual } => json!({
            "preflightReason": "networkMismatch",
            "expectedNetworkHash": thruput_json_rpc_types::NetworkHash::from(expected.clone().into_inner()),
            "actualNetworkHash": thruput_json_rpc_types::NetworkHash::from(actual.clone().into_inner()),
        }),
        NativeRpcError::MissingMethods { methods } => json!({
            "preflightReason": "missingMethods", "missingMethods": methods,
        }),
        NativeRpcError::WireFormat { actual } => json!({
            "preflightReason": "wireFormat", "actualWireFormat": actual, "expectedMajor": 14,
        }),
        NativeRpcError::InvalidExpiry { tip, offset } => json!({
            "preflightReason": "invalidExpiry", "blockHeight": tip.to_string(), "offset": offset.to_string(),
        }),
        NativeRpcError::ResponseRepresentation { method } => json!({
            "preflightReason": "unsupportedRepresentation", "method": method,
        }),
        _ => json!({"preflightError": error.to_string()}),
    };
    failure(error.to_string(), evidence)
}

/// Retain unsupported response evidence instead of inventing a known result.
pub(crate) fn known<T>(
    method: &'static str,
    result: Result<thruput_json_rpc_types::responses::DecodedResponse<T>, NativeClientError>,
) -> Result<T, CliError> {
    use thruput_json_rpc_types::responses::DecodedResponse;
    match result {
        Ok(DecodedResponse::Known(value)) => Ok(value),
        Ok(DecodedResponse::Unsupported { raw }) => Err(failure(
            format!("unsupported {method} result"),
            json!({"method":method,"result":raw}),
        )),
        Ok(_) => Err(failure(
            format!("unsupported {method} response representation"),
            json!({"method":method}),
        )),
        Err(error) => Err(client_error(&error)),
    }
}

pub(crate) fn submit_error(error: &SubmitCallError) -> CliError {
    let evidence = match error {
        SubmitCallError::Rpc { tx_hash, source } => {
            json!({"transactionHash":tx_hash,"cause":client_evidence(source)})
        }
        SubmitCallError::Unsupported { tx_hash, raw } => {
            json!({"transactionHash":tx_hash,"result":raw})
        }
        SubmitCallError::HashMismatch { tx_hash, returned } => {
            json!({"transactionHash":tx_hash,"returnedTransactionHash":returned})
        }
        SubmitCallError::Encoding { tx_hash, .. }
        | SubmitCallError::ResponseRepresentation { tx_hash } => {
            json!({"transactionHash":tx_hash})
        }
        _ => json!({"unsupportedSubmissionError":error.to_string()}),
    };
    failure(error.to_string(), evidence)
}

pub(crate) fn poll_error(error: &ReceiptPollError) -> CliError {
    let cause = match &error.cause {
        ReceiptPollCause::Timeout => json!({"kind":"timeout"}),
        ReceiptPollCause::Cancelled => json!({"kind":"cancelled"}),
        ReceiptPollCause::Lookup(source) => {
            json!({"kind":"lookup","error":client_evidence(source)})
        }
        ReceiptPollCause::Unsupported { method, raw } => {
            json!({"kind":"unsupported","method":method,"result":raw})
        }
        ReceiptPollCause::Invalid { message, raw } => {
            json!({"kind":"invalid","message":message,"result":raw})
        }
        _ => json!({"kind":"unsupported","message":error.cause.to_string()}),
    };
    let commitment = error.evidence.commitment.as_ref().map(|known| {
        json!({
            "networkHash":known.network_hash,
            "transactionHash":known.transaction_hash,
            "blockHeight":known.block_height.to_string(),
        })
    });
    let evidence = json!({
        "transactionHash":error.evidence.transaction_hash,
        "networkHash":error.evidence.network_hash,
        "commitment":commitment,
        "lastStatus":error.evidence.last_status,
        "lastLookupError":error.evidence.last_lookup_error.as_ref().map(client_evidence),
        "cause":cause,
    });
    failure(error.to_string(), evidence)
}

pub(crate) async fn signal_cancellation(signal: impl Future<Output = std::io::Result<()>>) {
    if let Err(error) = signal.await {
        tracing::warn!(%error, "unable to install interrupt handler");
        // Signal registration failure is not a user cancellation.
        std::future::pending::<()>().await;
    }
}

// Tokio keeps its SIGINT handler after its future and runtime are dropped.
// Keep one fallback registered, and suppress the default action only while a
// CLI cancellation scope is alive. A mutex makes overlapping scope drops atomic.
#[cfg(unix)]
struct InterruptScope(&'static InterruptState);

#[cfg(unix)]
struct InterruptState {
    active: std::sync::Mutex<usize>,
    default_enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl InterruptScope {
    fn enter() -> Option<Self> {
        use std::sync::{
            Arc, OnceLock,
            atomic::{AtomicBool, Ordering},
        };
        static STATE: OnceLock<Result<Option<InterruptState>, std::io::Error>> = OnceLock::new();
        let state = STATE.get_or_init(|| {
            // Read the inherited disposition without changing it. Shell background
            // jobs may ignore SIGINT; neither our fallback nor Tokio may replace it.
            let mut inherited = std::mem::MaybeUninit::<libc::sigaction>::uninit();
            if unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), inherited.as_mut_ptr()) }
                != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if unsafe { inherited.assume_init() }.sa_sigaction == libc::SIG_IGN {
                return Ok(None);
            }
            let default_enabled = Arc::new(AtomicBool::new(true));
            signal_hook::flag::register_conditional_default(
                signal_hook::consts::SIGINT,
                Arc::clone(&default_enabled),
            )?;
            Ok(Some(InterruptState {
                active: std::sync::Mutex::new(0),
                default_enabled,
            }))
        });
        let state = match state {
            Ok(Some(state)) => state,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(%error, "unable to install interrupt fallback");
                return None;
            }
        };
        let mut active = state
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active += 1;
        state.default_enabled.store(false, Ordering::SeqCst);
        Some(Self(state))
    }
}

#[cfg(unix)]
impl Drop for InterruptScope {
    fn drop(&mut self) {
        let mut active = self
            .0
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active -= 1;
        self.0
            .default_enabled
            .store(*active == 0, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn wait_for_interrupt(
    signal: impl Future<Output = std::io::Result<()>>,
    #[cfg(unix)] scope: InterruptScope,
) {
    let result = signal.await;
    #[cfg(unix)]
    drop(scope);
    signal_cancellation(std::future::ready(result)).await;
}

pub(crate) async fn install_interrupt_listener() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    #[cfg(unix)]
    let Some(scope) = InterruptScope::enter() else {
        return Box::pin(std::future::pending());
    };
    let mut signal: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(wait_for_interrupt(
        tokio::signal::ctrl_c(),
        #[cfg(unix)]
        scope,
    ));
    // ctrl_c installs its process handler on first poll, before any RPC work.
    let ready = std::future::poll_fn(|cx| Poll::Ready(signal.as_mut().poll(cx).is_ready())).await;
    if ready {
        Box::pin(std::future::ready(()))
    } else {
        signal
    }
}

/// Local progress through a single submission, separate from admission evidence.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SubmissionPhase {
    NotSent,
    Uncertain,
    Acknowledged,
}

#[derive(Debug)]
pub(crate) struct SubmissionFailure {
    pub phase: SubmissionPhase,
    pub error: CliError,
}

/// Gate discovery and the one write on cancellation; every later query is read-only.
pub(crate) async fn submit_and_wait<C, F>(
    client: &thruput_client::NativeClient<C>,
    transaction_hash: thruput_json_rpc_types::transaction::TxHash,
    signed_transaction: &str,
    network_hash: &thruput_xdr::types::NetworkHash,
    config: thruput_client::ReceiptPollConfig,
    cancelled: F,
) -> Result<thruput_json_rpc_types::responses::TransactionReceipt, SubmissionFailure>
where
    C: jsonrpsee::core::client::ClientT + Sync,
    F: Future<Output = ()>,
{
    let with_phase = |phase, error: CliError| {
        let message = match &error {
            CliError::NativeRpcFailure { message, .. } => message.clone(),
            _ => error.to_string(),
        };
        let mut evidence = error.details_json().unwrap_or_else(|| json!({}));
        evidence["submission"] = json!(phase);
        evidence["transactionHash"] = json!(transaction_hash);
        evidence["networkHash"] = json!(thruput_json_rpc_types::NetworkHash::from(
            network_hash.clone().into_inner()
        ));
        SubmissionFailure {
            phase,
            error: failure(message, evidence),
        }
    };
    let cancelled_error = |phase| {
        with_phase(
            phase,
            failure(
                "transaction submission cancelled",
                json!({"cause":{"kind":"cancelled"}}),
            ),
        )
    };
    if config.interval.is_zero() {
        return Err(with_phase(
            SubmissionPhase::NotSent,
            failure(
                "receipt polling interval must be positive",
                json!({"cause":{"kind":"invalidPollingConfig"}}),
            ),
        ));
    }
    tokio::pin!(cancelled);
    let preflight = async {
        thruput_client::check_endpoint(
            client,
            &[
                "sendTransaction",
                "getTransactionStatus",
                "getTransactionReceipt",
                "getNetworkInfo",
            ],
        )
        .await
        .map_err(|error| rpc_error(&error))?;
        thruput_client::validate_network(client, network_hash)
            .await
            .map(|_| ())
            .map_err(|error| rpc_error(&error))
    };
    tokio::select! {
        biased;
        () = &mut cancelled => return Err(cancelled_error(SubmissionPhase::NotSent)),
        result = preflight => result.map_err(|error| with_phase(SubmissionPhase::NotSent, error))?,
    }
    let mut started = false;
    let submission = {
        let send = async {
            started = true;
            thruput_client::submit_signed_transaction(client, transaction_hash, signed_transaction)
                .await
        };
        tokio::select! {
            biased;
            () = &mut cancelled => None,
            result = send => Some(result),
        }
    };
    let submitted = match submission {
        Some(result) => result.map_err(|error| {
            with_phase(
                if matches!(error, SubmitCallError::Encoding { .. }) {
                    SubmissionPhase::NotSent
                } else {
                    SubmissionPhase::Uncertain
                },
                submit_error(&error),
            )
        })?,
        None => {
            return Err(cancelled_error(if started {
                SubmissionPhase::Uncertain
            } else {
                SubmissionPhase::NotSent
            }));
        }
    };
    submitted
        .wait(
            client,
            network_hash.clone().into_inner().into(),
            config,
            &mut cancelled,
        )
        .await
        .map_err(|error| with_phase(SubmissionPhase::Acknowledged, poll_error(&error)))
}

pub(crate) fn failure_observations(
    phase: SubmissionPhase,
    evidence: &Value,
) -> Vec<(&'static str, String)> {
    let phase = match phase {
        SubmissionPhase::NotSent => "not sent",
        SubmissionPhase::Uncertain => "outcome uncertain",
        SubmissionPhase::Acknowledged => "admission acknowledged",
    };
    let mut fields = vec![("Submission", phase.to_owned())];
    if let Some(height) = evidence
        .get("commitment")
        .and_then(|value| value.get("blockHeight"))
        .and_then(Value::as_str)
    {
        fields.push((
            "Committed block height",
            crate::terminal::terminal_safe(height),
        ));
    }
    if let Some(status) = evidence
        .get("lastStatus")
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
    {
        fields.push(("Last status", crate::terminal::terminal_safe(status)));
    }
    fields
}

/// Recover the local submission phase retained in a native failure.
pub(crate) fn submission_phase(error: &CliError) -> Option<SubmissionPhase> {
    let CliError::NativeRpcFailure { evidence, .. } = error else {
        return None;
    };
    serde_json::from_value(evidence.get("submission")?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_native_response_retains_method_result_and_decode_error() {
        let raw = json!({"admission":"accepted", "transactionHash":17, "extra":"retained"});
        let source = serde_json::from_value::<
            thruput_json_rpc_types::responses::SendTransactionResult,
        >(raw.clone())
        .unwrap_err();
        let error = NativeClientError::InvalidResponse {
            method: "sendTransaction",
            raw: Box::new(raw.clone()),
            source,
        };
        let evidence = client_error(&error).details_json().unwrap();
        assert_eq!(evidence["method"], "sendTransaction");
        assert_eq!(evidence["result"], raw);
        assert!(
            evidence["decodeError"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );
    }

    #[test]
    fn preflight_evidence_retains_typed_identity_and_decimal_heights() {
        let expected =
            thruput_xdr::types::NetworkHash::from(thruput_xdr::types::SHA256Hash::from([1; 32]));
        let actual =
            thruput_xdr::types::NetworkHash::from(thruput_xdr::types::SHA256Hash::from([2; 32]));
        let error = rpc_error(&NativeRpcError::NetworkMismatch {
            expected: expected.clone(),
            actual,
        });
        let details = error.details_json().unwrap();
        assert_eq!(details["preflightReason"], "networkMismatch");
        assert_eq!(
            details["expectedNetworkHash"],
            serde_json::to_value(thruput_json_rpc_types::NetworkHash::from(
                expected.into_inner()
            ))
            .unwrap()
        );
        let error = rpc_error(&NativeRpcError::InvalidExpiry {
            tip: i64::MAX,
            offset: 1,
        });
        let details = error.details_json().unwrap();
        assert_eq!(details["blockHeight"], i64::MAX.to_string());
        assert_eq!(details["offset"], "1");
    }

    #[cfg(unix)]
    #[test]
    fn interrupt_scope_process_probe() {
        let Ok(mode) = std::env::var("THRUPUT_INTERRUPT_SCOPE_PROBE") else {
            return;
        };
        if mode == "ignored" {
            // This dedicated child simulates the disposition inherited from a shell.
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_IGN);
            }
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        if mode == "after" || mode == "registration-error" {
            let listener = runtime.block_on(install_interrupt_listener());
            drop(listener);
            if mode == "registration-error" {
                runtime.block_on(async {
                    let scope = InterruptScope::enter().unwrap();
                    let mut failed = Box::pin(wait_for_interrupt(
                        std::future::ready(Err(std::io::Error::other("test registration failure"))),
                        scope,
                    ));
                    assert!(
                        !std::future::poll_fn(|cx| Poll::Ready(
                            failed.as_mut().poll(cx).is_ready()
                        ))
                        .await
                    );
                    // The failed listener stays alive, but its default-action guard is gone.
                    let probe = InterruptScope::enter().unwrap();
                    assert_eq!(*probe.0.active.lock().unwrap(), 1);
                    drop(probe);
                    println!("INTERRUPT_READY");
                    std::future::pending::<()>().await;
                });
            }
            drop(runtime);
            println!("INTERRUPT_READY");
            loop {
                std::thread::park();
            }
        }
        runtime.block_on(async {
            let first = install_interrupt_listener().await;
            let listener = if mode == "overlapping" {
                let second = install_interrupt_listener().await;
                drop(first);
                second
            } else if mode == "subsequent" {
                drop(first);
                install_interrupt_listener().await
            } else {
                first
            };
            println!("INTERRUPT_READY");
            if mode == "ignored" {
                let mut disposition = std::mem::MaybeUninit::<libc::sigaction>::uninit();
                assert_eq!(
                    unsafe {
                        libc::sigaction(libc::SIGINT, std::ptr::null(), disposition.as_mut_ptr())
                    },
                    0
                );
                assert_eq!(
                    unsafe { disposition.assume_init() }.sa_sigaction,
                    libc::SIG_IGN
                );
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(250), listener)
                        .await
                        .is_err()
                );
            } else {
                tokio::time::timeout(std::time::Duration::from_secs(10), listener)
                    .await
                    .unwrap();
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn interrupt_scopes_restore_default_after_wait_and_support_later_waits() {
        use std::{
            io::{BufRead, BufReader},
            os::unix::process::ExitStatusExt,
            process::{Command, Stdio},
        };
        for mode in [
            "during",
            "after",
            "subsequent",
            "overlapping",
            "ignored",
            "registration-error",
        ] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native_rpc::tests::interrupt_scope_process_probe",
                    "--nocapture",
                ])
                .env("THRUPUT_INTERRUPT_SCOPE_PROBE", mode)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if line.unwrap().contains("INTERRUPT_READY") {
                        let _ = ready_tx.send(());
                    }
                }
            });
            if let Err(error) = ready_rx.recv_timeout(std::time::Duration::from_mins(1)) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{mode}: child did not reach interrupt boundary: {error}");
            }
            // This is our own still-running child, paused at the boundary above.
            assert_eq!(
                unsafe { libc::kill(i32::try_from(child.id()).unwrap(), libc::SIGINT) },
                0
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{mode}: child did not finish after SIGINT");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            if mode == "after" || mode == "registration-error" {
                assert_eq!(status.signal(), Some(libc::SIGINT));
            } else {
                assert!(status.success(), "{mode}: {status}");
            }
        }
    }
}
