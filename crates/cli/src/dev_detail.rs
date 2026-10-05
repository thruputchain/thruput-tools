use groundhog_json_rpc_types::dev::{StorageDeltaInfo, TransactionDetail, format_gas_summary};
use groundhog_json_rpc_types::events::EmittedEvent;
use groundhog_json_rpc_types::hex::prefixed_hex;
use groundhog_json_rpc_types::transaction::TxHash;
use jsonrpsee::core::ClientError;
use jsonrpsee::types::error::METHOD_NOT_FOUND_CODE;

use crate::{CliError, Print};

const GET_TRANSACTION_DETAIL_METHOD: &str = "groundhog_dev_getTransactionDetail";

pub fn fetch_transaction_detail_blocking(
    rpc_url: &str,
    tx_hash: TxHash,
    print: &Print,
) -> Option<TransactionDetail> {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(error) => {
            print.warn(format_args!(
                "transaction detail unavailable: failed to start runtime: {error}"
            ));
            return None;
        }
    };

    match rt.block_on(async { fetch_transaction_detail(rpc_url, tx_hash).await }) {
        Ok(detail) => detail,
        Err(error) => {
            print.warn(format_args!("transaction detail unavailable: {error}"));
            None
        }
    }
}

pub fn print_transaction_detail(detail: &TransactionDetail, print: &Print) {
    print.status("");

    if !detail.storage_deltas.is_empty() {
        print.status(format!(
            "  Storage deltas ({}):",
            detail.storage_deltas.len()
        ));
        for delta in &detail.storage_deltas {
            print.status(format!("    {}", format_storage_delta(delta)));
        }
    }

    if !detail.receipt.events.is_empty() {
        if !detail.storage_deltas.is_empty() {
            print.status("");
        }
        print.status(format!("  Events ({}):", detail.receipt.events.len()));
        for (index, event) in detail.receipt.events.iter().enumerate() {
            print.status(format!("    [{index}] {}", format_event(event)));
        }
    }

    if let Some(return_data) = detail.receipt.return_data.as_ref() {
        if !detail.storage_deltas.is_empty() || !detail.receipt.events.is_empty() {
            print.status("");
        }
        print.status(format!("  Return: {}", prefixed_hex(return_data)));
    }

    if let Some(abort_data) = detail.receipt.abort_data.as_ref() {
        if !detail.storage_deltas.is_empty()
            || !detail.receipt.events.is_empty()
            || detail.receipt.return_data.is_some()
        {
            print.status("");
        }
        print.status(format!("  Abort: {}", prefixed_hex(abort_data)));
    }

    print.status(format!(
        "  Gas: {}",
        format_gas_summary(detail.gas_consumed, detail.gas_limit)
    ));
}

async fn fetch_transaction_detail(
    rpc_url: &str,
    tx_hash: TxHash,
) -> Result<Option<TransactionDetail>, CliError> {
    use jsonrpsee::core::client::ClientT;

    let client = crate::http_client::rpc_client(rpc_url)?;

    match client
        .request(
            GET_TRANSACTION_DETAIL_METHOD,
            jsonrpsee::rpc_params![tx_hash],
        )
        .await
    {
        Ok(detail) => Ok(detail),
        Err(error) if is_method_not_found(&error) => Ok(None),
        Err(error) => Err(CliError::RpcMethodFailed {
            method: "getTransactionDetail",
            message: error.to_string(),
        }),
    }
}

fn is_method_not_found(error: &ClientError) -> bool {
    matches!(error, ClientError::Call(object) if object.code() == METHOD_NOT_FOUND_CODE)
}

fn format_storage_delta(delta: &StorageDeltaInfo) -> String {
    format!(
        "{}:{}  {}",
        prefixed_hex(delta.address.0),
        prefixed_hex(&delta.key),
        delta.summary,
    )
}

pub(crate) fn format_event(event: &EmittedEvent) -> String {
    let mut parts = vec![prefixed_hex(event.contract_address.0)];
    parts.extend(
        event
            .topics
            .iter()
            .enumerate()
            .map(|(index, topic)| format!("topic{index}=0x{}", hex::encode(topic.0))),
    );
    parts.push(format!("data={}", prefixed_hex(&event.data)));
    parts.join(" ")
}
