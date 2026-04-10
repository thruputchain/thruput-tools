//! Dedicated executor thread with channel-based command dispatch.
//!
//! `DevBackend` owns `Rc<TestEnvironment>` which is `!Send`. This module
//! keeps it on a single `std::thread` and bridges to async via channels.

use groundhog_rpc_api_types::block::ChainTip;
use groundhog_rpc_api_types::dev::DevAccount;
use groundhog_rpc_api_types::receipt::TransactionReceipt;
use groundhog_rpc_api_types::transaction::{TransactionStatus, TxHash};
use tokio::sync::{mpsc, oneshot};

use crate::backend::DevBackend;
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
    Reset {
        reply: oneshot::Sender<()>,
    },
}

/// Handle for sending commands to the worker thread.
#[derive(Clone)]
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
pub fn spawn_worker(account_count: usize, storage_tokens: u64) -> WorkerHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<DevCommand>();

    std::thread::spawn(move || {
        let mut backend = DevBackend::new(account_count, storage_tokens);

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
                DevCommand::Reset { reply } => {
                    backend.reset();
                    let _ = reply.send(());
                }
            }
        }
    });

    WorkerHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::spawn_worker;
    use crate::accounts::DEFAULT_STORAGE_TOKENS;

    #[tokio::test]
    async fn worker_initializes_backend_on_dedicated_thread() {
        let worker = spawn_worker(1, DEFAULT_STORAGE_TOKENS);
        let accounts = worker
            .get_accounts()
            .await
            .expect("bug: worker should start");
        assert_eq!(accounts.len(), 1);
    }
}
