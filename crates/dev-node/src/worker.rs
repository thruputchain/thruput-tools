//! Dedicated executor thread with channel-based command dispatch.
//!
//! `DevBackend` owns `Rc<TestEnvironment>` which is `!Send`. This module
//! keeps it on a single `std::thread` and bridges to async via channels.

use groundhog_rpc_api_types::block::ChainTip;
use groundhog_rpc_api_types::dev::DevAccount;
use groundhog_rpc_api_types::receipt::TransactionReceipt;
use groundhog_rpc_api_types::transaction::{TransactionStatus, TxHash};
use tokio::sync::{mpsc, oneshot};

use crate::backend::{BackendStats, DevBackend};
use crate::error::DevNodeError;

/// Commands sent from RPC handlers to the worker thread.
pub enum DevCommand {
    SendRawTransaction {
        tx_bytes: Vec<u8>,
        reply: oneshot::Sender<Result<TxHash, DevNodeError>>,
    },
    GetTransactionReceipt {
        tx_hash: TxHash,
        reply: oneshot::Sender<Option<TransactionReceipt>>,
    },
    GetTransactionStatus {
        tx_hash: TxHash,
        reply: oneshot::Sender<TransactionStatus>,
    },
    ChainTip {
        reply: oneshot::Sender<ChainTip>,
    },
    GetAccounts {
        reply: oneshot::Sender<Vec<DevAccount>>,
    },
    Stats {
        reply: oneshot::Sender<BackendStats>,
    },
    Reset {
        reply: oneshot::Sender<()>,
    },
}

/// Handle for sending commands to the worker thread.
#[derive(Debug, Clone)]
pub struct WorkerHandle {
    tx: mpsc::UnboundedSender<DevCommand>,
}

impl WorkerHandle {
    /// Submit a raw transaction for execution.
    pub async fn send_raw_transaction(&self, tx_bytes: Vec<u8>) -> Result<TxHash, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::SendRawTransaction { tx_bytes, reply })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)?
    }

    /// Fetch receipt for a committed transaction.
    pub async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::GetTransactionReceipt { tx_hash, reply })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }

    /// Lightweight status lookup.
    pub async fn get_transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::GetTransactionStatus { tx_hash, reply })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }

    /// Current chain tip.
    pub async fn chain_tip(&self) -> Result<ChainTip, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::ChainTip { reply })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }

    /// List pre-funded dev accounts.
    pub async fn get_accounts(&self) -> Result<Vec<DevAccount>, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::GetAccounts { reply })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }

    pub async fn stats(&self) -> Result<BackendStats, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::Stats { reply })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }

    /// Reset all state to initial conditions.
    pub async fn reset(&self) -> Result<(), DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::Reset { reply })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }
}

/// Spawn the worker thread and return a handle for sending commands.
pub fn spawn_worker(
    account_count: usize,
    storage_tokens: u64,
) -> Result<WorkerHandle, DevNodeError> {
    let (tx, mut rx) = mpsc::unbounded_channel::<DevCommand>();
    let (init_tx, init_rx) =
        std::sync::mpsc::sync_channel::<Result<Vec<DevAccount>, DevNodeError>>(1);

    std::thread::Builder::new()
        .name("groundhog-dev-node-worker".into())
        .spawn(move || {
            tracing::info!(account_count, storage_tokens, "dev node worker started");

            let backend_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                DevBackend::new(account_count, storage_tokens)
            }));

            let mut backend = match backend_result {
                Ok(backend) => {
                    let accounts = backend.accounts().to_vec();
                    let _ = init_tx.send(Ok(accounts));
                    backend
                }
                Err(payload) => {
                    let message = startup_failure_message(&payload);
                    let _ = init_tx.send(Err(DevNodeError::WorkerInit(message.clone())));
                    tracing::error!(panic = %message, "dev node worker panicked during startup");
                    return;
                }
            };

            let worker_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        DevCommand::SendRawTransaction { tx_bytes, reply } => {
                            let result = backend.send_raw_transaction(&tx_bytes);
                            let _ = reply.send(result);
                        }
                        DevCommand::GetTransactionReceipt { tx_hash, reply } => {
                            let _ = reply.send(backend.get_transaction_receipt(&tx_hash));
                        }
                        DevCommand::GetTransactionStatus { tx_hash, reply } => {
                            let _ = reply.send(backend.get_transaction_status(&tx_hash));
                        }
                        DevCommand::ChainTip { reply } => {
                            let _ = reply.send(backend.chain_tip());
                        }
                        DevCommand::GetAccounts { reply } => {
                            let _ = reply.send(backend.accounts().to_vec());
                        }
                        DevCommand::Stats { reply } => {
                            let _ = reply.send(backend.stats());
                        }
                        DevCommand::Reset { reply } => {
                            backend.reset();
                            let _ = reply.send(());
                        }
                    }
                }
            }));

            match worker_result {
                Ok(()) => tracing::info!("dev node worker stopped"),
                Err(payload) => tracing::error!(
                    panic = %panic_payload_message(&payload),
                    "dev node worker panicked"
                ),
            }
        })
        .expect("bug: failed to spawn dev node worker thread");

    init_rx.recv().map_err(|_| DevNodeError::WorkerStopped)??;

    Ok(WorkerHandle { tx })
}

fn panic_payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(msg) = payload.downcast_ref::<&str>() {
        (*msg).to_string()
    } else if let Some(msg) = payload.downcast_ref::<String>() {
        msg.clone()
    } else {
        "unknown panic payload".into()
    }
}

fn startup_failure_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    let message = panic_payload_message(payload);
    if message.contains("failed to create db: 5") {
        return "native_exec failed to initialize its embedded test database \
                (FFI error 5: unrecoverable). This commonly happens in \
                restricted sandbox or process-isolation environments; rerun \
                `groundhog node` from a normal local shell outside the sandbox."
            .into();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::spawn_worker;
    use crate::accounts::DEFAULT_STORAGE_TOKENS;

    #[tokio::test]
    async fn worker_initializes_backend_on_dedicated_thread() {
        let worker = spawn_worker(1, DEFAULT_STORAGE_TOKENS).expect("bug: worker should start");
        let accounts = worker
            .get_accounts()
            .await
            .expect("bug: worker should start");
        assert_eq!(accounts.len(), 1);
    }

    #[test]
    fn startup_failure_message_adds_sandbox_hint_for_db_init_failure() {
        let payload: Box<dyn std::any::Any + Send> =
            Box::new(String::from("failed to create db: 5"));
        let message = super::startup_failure_message(&payload);
        assert!(message.contains("sandbox"));
        assert!(message.contains("FFI error 5"));
    }
}
