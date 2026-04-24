//! Dedicated executor thread with channel-based command dispatch.
//!
//! `DevBackend` owns `Rc<TestEnvironment>` which is `!Send`. This module
//! keeps it on a single `std::thread` and bridges to async via channels.

use groundhog_rpc_api_types::block::ChainTip;
use groundhog_rpc_api_types::dev::{DevAccount, StorageItemInfo, TransactionDetail};
use groundhog_rpc_api_types::events::IdentifiedEvent;
use groundhog_rpc_api_types::receipt::TransactionReceipt;
use groundhog_rpc_api_types::transaction::{TransactionStatus, TxHash};
use groundhog_rpc_api_types::{BlockHeight, ContractAddress};
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
    GetTransactionDetail {
        tx_hash: TxHash,
        reply: oneshot::Sender<Option<TransactionDetail>>,
    },
    GetStorageAt {
        address: ContractAddress,
        key: [u8; 32],
        reply: oneshot::Sender<Option<StorageItemInfo>>,
    },
    GetTransactionStatus {
        tx_hash: TxHash,
        reply: oneshot::Sender<TransactionStatus>,
    },
    GetContractEvents {
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
        reply: oneshot::Sender<Vec<IdentifiedEvent>>,
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
    #[cfg(test)]
    SeedBytestring {
        address: ContractAddress,
        key: [u8; 32],
        value: Vec<u8>,
        reply: oneshot::Sender<()>,
    },
    #[cfg(test)]
    SeedDeployedBytestring {
        address: ContractAddress,
        key: [u8; 32],
        value: Vec<u8>,
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

    /// Full retained execution detail.
    pub async fn get_transaction_detail(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionDetail>, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::GetTransactionDetail { tx_hash, reply })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }

    /// Latest-state point read for a contract storage key.
    pub async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<Option<StorageItemInfo>, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::GetStorageAt {
                address,
                key,
                reply,
            })
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

    /// Contract events filtered by address and height range.
    pub async fn get_contract_events(
        &self,
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
    ) -> Result<Vec<IdentifiedEvent>, DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::GetContractEvents {
                address,
                from_height,
                to_height,
                reply,
            })
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

    #[cfg(test)]
    pub async fn seed_bytestring(
        &self,
        address: ContractAddress,
        key: [u8; 32],
        value: Vec<u8>,
    ) -> Result<(), DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::SeedBytestring {
                address,
                key,
                value,
                reply,
            })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }

    #[cfg(test)]
    pub async fn seed_deployed_bytestring(
        &self,
        address: ContractAddress,
        key: [u8; 32],
        value: Vec<u8>,
    ) -> Result<(), DevNodeError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DevCommand::SeedDeployedBytestring {
                address,
                key,
                value,
                reply,
            })
            .map_err(|_| DevNodeError::WorkerStopped)?;
        rx.await.map_err(|_| DevNodeError::WorkerStopped)
    }
}

/// Spawn the worker thread and return a handle for sending commands.
pub fn spawn_worker(
    account_count: usize,
    storage_tokens: u64,
    transaction_retention_limit: usize,
) -> Result<WorkerHandle, DevNodeError> {
    let (tx, rx) = mpsc::unbounded_channel::<DevCommand>();
    let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), DevNodeError>>(1);

    std::thread::Builder::new()
        .name("groundhog-dev-node-worker".into())
        .spawn(move || {
            run_worker_thread(
                account_count,
                storage_tokens,
                transaction_retention_limit,
                &init_tx,
                rx,
            );
        })
        .expect("bug: failed to spawn dev node worker thread");

    init_rx.recv().map_err(|_| DevNodeError::WorkerStopped)??;

    Ok(WorkerHandle { tx })
}

fn run_worker_thread(
    account_count: usize,
    storage_tokens: u64,
    transaction_retention_limit: usize,
    init_tx: &std::sync::mpsc::SyncSender<Result<(), DevNodeError>>,
    mut rx: mpsc::UnboundedReceiver<DevCommand>,
) {
    tracing::info!(
        account_count,
        storage_tokens,
        transaction_retention_limit,
        "dev node worker started"
    );

    let backend_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let backend = DevBackend::new(account_count, storage_tokens, transaction_retention_limit);
        let _ = init_tx.send(Ok(()));
        backend
    }));

    let mut backend = match backend_result {
        Ok(backend) => backend,
        Err(payload) => {
            let err = startup_failure_error(&payload);
            tracing::error!(error = %err, "dev node worker panicked during startup");
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    let worker_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while let Some(cmd) = rx.blocking_recv() {
            handle_command(&mut backend, cmd);
        }
    }));

    match worker_result {
        Ok(()) => tracing::info!("dev node worker stopped"),
        Err(payload) => tracing::error!(
            panic = %panic_payload_message(&payload),
            "dev node worker panicked"
        ),
    }
}

fn handle_command(backend: &mut DevBackend, cmd: DevCommand) {
    match cmd {
        DevCommand::SendRawTransaction { tx_bytes, reply } => {
            let result = backend.send_raw_transaction(&tx_bytes);
            let _ = reply.send(result);
        }
        DevCommand::GetTransactionReceipt { tx_hash, reply } => {
            let _ = reply.send(backend.get_transaction_receipt(&tx_hash));
        }
        DevCommand::GetTransactionDetail { tx_hash, reply } => {
            let _ = reply.send(backend.get_transaction_detail(&tx_hash));
        }
        DevCommand::GetStorageAt {
            address,
            key,
            reply,
        } => {
            let _ = reply.send(backend.get_storage_at(&address.0, &key));
        }
        DevCommand::GetTransactionStatus { tx_hash, reply } => {
            let _ = reply.send(backend.get_transaction_status(&tx_hash));
        }
        DevCommand::GetContractEvents {
            address,
            from_height,
            to_height,
            reply,
        } => {
            let _ = reply.send(backend.get_contract_events(&address.0, &from_height, &to_height));
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
        #[cfg(test)]
        DevCommand::SeedBytestring {
            address,
            key,
            value,
            reply,
        } => {
            backend.seed_bytestring(&address.0, &key, &value);
            let _ = reply.send(());
        }
        #[cfg(test)]
        DevCommand::SeedDeployedBytestring {
            address,
            key,
            value,
            reply,
        } => {
            backend.seed_deployed_bytestring(&address.0, &key, &value);
            let _ = reply.send(());
        }
    }
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

fn startup_failure_error(payload: &Box<dyn std::any::Any + Send>) -> DevNodeError {
    let message = panic_payload_message(payload);
    if message.contains("failed to create db: 5") {
        DevNodeError::SandboxInit(
            "native_exec failed to initialize its embedded test database \
             (FFI error 5: unrecoverable). This commonly happens in \
             restricted sandbox or process-isolation environments; rerun \
             `groundhog node` from a normal local shell outside the sandbox."
                .into(),
        )
    } else {
        DevNodeError::WorkerInit(message)
    }
}

#[cfg(test)]
mod tests {
    use super::spawn_worker;
    use crate::DEFAULT_TRANSACTION_RETENTION_LIMIT;
    use crate::accounts::DEFAULT_STORAGE_TOKENS;

    #[tokio::test]
    async fn worker_initializes_backend_on_dedicated_thread() {
        let worker = spawn_worker(
            1,
            DEFAULT_STORAGE_TOKENS,
            DEFAULT_TRANSACTION_RETENTION_LIMIT,
        )
        .expect("bug: worker should start");
        let accounts = worker
            .get_accounts()
            .await
            .expect("bug: worker should start");
        assert_eq!(accounts.len(), 1);
    }

    #[test]
    fn startup_failure_classifies_db_init_failure_as_sandbox() {
        let payload: Box<dyn std::any::Any + Send> =
            Box::new(String::from("failed to create db: 5"));
        match super::startup_failure_error(&payload) {
            crate::error::DevNodeError::SandboxInit(msg) => assert!(msg.contains("FFI error 5")),
            other => panic!("expected SandboxInit, got {other:?}"),
        }
    }

    #[test]
    fn startup_failure_classifies_unknown_panic_as_worker_init() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("totally unrelated"));
        match super::startup_failure_error(&payload) {
            crate::error::DevNodeError::WorkerInit(msg) => assert_eq!(msg, "totally unrelated"),
            other => panic!("expected WorkerInit, got {other:?}"),
        }
    }
}
