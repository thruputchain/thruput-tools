use groundhog_json_rpc_client::{
    PollTransactionReceiptError, poll_transaction_receipt_with_config,
};
use groundhog_json_rpc_types::{
    BlockHeight,
    hex::Hex0x,
    receipt::{ExecStatus, TransactionReceipt},
    transaction::{TransactionStatus, TxHash},
};
use jsonrpsee::{RpcModule, server::Server, types::ErrorObjectOwned};
use std::{collections::VecDeque, sync::Mutex, time::Duration};

async fn poll(
    statuses: Vec<Option<TransactionStatus>>,
    receipt_after: Option<usize>,
) -> Result<TransactionReceipt, PollTransactionReceiptError> {
    let tx_hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x33; 32])));
    let attempts = statuses.len();
    let mut module = RpcModule::new(Mutex::new((VecDeque::from(statuses), 0usize)));
    module
        .register_method("groundhog_getTransactionStatus", |_, state, _| {
            state
                .lock()
                .unwrap()
                .0
                .pop_front()
                .unwrap()
                .ok_or_else(|| ErrorObjectOwned::owned(-32601, "status unavailable", None::<()>))
        })
        .unwrap();
    module
        .register_method("groundhog_getTransactionReceipt", move |_, state, _| {
            let mut state = state.lock().unwrap();
            let receipt =
                receipt_after
                    .filter(|after| state.1 >= *after)
                    .map(|_| TransactionReceipt {
                        tx_hash,
                        height: BlockHeight::from(42),
                        status: ExecStatus::Ok,
                        events: vec![],
                        return_data: None,
                        abort_data: None,
                    });
            state.1 += 1;
            receipt
        })
        .unwrap();
    let server = Server::builder().build("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();
    let handle = server.start(module);
    let result = poll_transaction_receipt_with_config(
        &format!("http://{addr}"),
        tx_hash,
        attempts,
        Duration::from_millis(1),
    )
    .await;
    handle.stop().unwrap();
    handle.stopped().await;
    result
}

#[tokio::test]
async fn timeout_reports_latest_unknown_after_pending() {
    let error = poll(
        vec![
            Some(TransactionStatus::Pending),
            Some(TransactionStatus::Unknown),
        ],
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        PollTransactionReceiptError::ReceiptTimeout {
            attempts: 2,
            last_status: Some(TransactionStatus::Unknown),
            ..
        }
    ));
}

#[tokio::test]
async fn status_errors_retain_last_successful_observation() {
    let error = poll(vec![Some(TransactionStatus::Pending), None], None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        PollTransactionReceiptError::ReceiptTimeout {
            last_status: Some(TransactionStatus::Pending),
            ..
        }
    ));
}

#[tokio::test]
async fn unsupported_status_times_out_without_observation() {
    let error = poll(vec![None, None], None).await.unwrap_err();
    assert!(matches!(
        error,
        PollTransactionReceiptError::ReceiptTimeout {
            last_status: None,
            ..
        }
    ));
}

#[tokio::test]
async fn receipt_after_unknown_succeeds() {
    let receipt = poll(vec![Some(TransactionStatus::Unknown), None], Some(1))
        .await
        .unwrap();
    assert!(matches!(receipt.status, ExecStatus::Ok));
}

#[tokio::test]
async fn invalid_still_stops_polling() {
    let error = poll(
        vec![
            Some(TransactionStatus::Invalid {
                reason: "expired".into(),
            }),
            None,
        ],
        Some(1),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        PollTransactionReceiptError::TransactionStatus {
            status: TransactionStatus::Invalid { .. },
            ..
        }
    ));
}
