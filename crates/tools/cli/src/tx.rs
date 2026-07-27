use std::path::Path;

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_types::error::codes;
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};

use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::result::CommandResult;
use crate::tx_json::{TX_SHOW_JSON_SCHEMA_VERSION, TxShowReport};
use crate::tx_utils::{format_tx_hash, summarize_exec_status};
use crate::{CliError, Print};

/// Inspect a transaction's public status and committed receipt without waiting.
pub fn show(
    tx_hash_input: &str,
    project_start: &Path,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let tx_hash = parse_tx_hash(tx_hash_input)?;
    let profile = resolve_profile_with_overrides(project_start, profile_name, overrides)?;
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();

    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let (status, receipt) = rt.block_on(fetch_transaction(&rpc_url, tx_hash))?;

    print_show(&tx_hash, &status, receipt.as_ref(), print);
    print.json(&CommandResult::new(
        cmd,
        TxShowReport {
            schema_version: TX_SHOW_JSON_SCHEMA_VERSION,
            profile: profile.name,
            rpc_url,
            tx_hash,
            status,
            receipt,
        },
    ))
}

fn parse_tx_hash(input: &str) -> Result<TxHash, CliError> {
    serde_json::from_value(serde_json::Value::String(input.to_owned())).map_err(|error| {
        CliError::InvalidTransactionHash {
            input: input.to_owned(),
            display_input: human_safe(input),
            message: error.to_string(),
        }
    })
}

async fn fetch_transaction(
    rpc_url: &str,
    tx_hash: TxHash,
) -> Result<(TransactionStatus, Option<TransactionReceipt>), CliError> {
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|error| CliError::RpcConnect(format!("{rpc_url}: {error}")))?;
    let status = client
        .get_transaction_status(tx_hash)
        .await
        .map_err(|error| map_rpc_error("getTransactionStatus", &error))?;
    let committed_height = match &status {
        TransactionStatus::Committed { height } => Some(height.clone()),
        _ => None,
    };
    let Some(committed_height) = committed_height else {
        return Ok((status, None));
    };

    let receipt = client
        .get_transaction_receipt(tx_hash)
        .await
        .map_err(|error| map_rpc_error("getTransactionReceipt", &error))?;
    if let Some(receipt) = &receipt {
        if receipt.tx_hash != tx_hash {
            return Err(CliError::RpcMethodFailed {
                method: "getTransactionReceipt",
                message: format!(
                    "invalid response: receipt hash {} does not match requested hash {}",
                    format_tx_hash(&receipt.tx_hash),
                    format_tx_hash(&tx_hash),
                ),
            });
        }
        if receipt.height != committed_height {
            return Err(CliError::RpcMethodFailed {
                method: "getTransactionReceipt",
                message: format!(
                    "invalid response: receipt height {} does not match committed status height {}",
                    *receipt.height, *committed_height,
                ),
            });
        }
    }
    Ok((status, receipt))
}

fn map_rpc_error(method: &'static str, error: &jsonrpsee::core::client::Error) -> CliError {
    const METHOD_NOT_FOUND: i32 = -32601;
    let message = safe_rpc_error(error);
    if matches!(
        error,
        jsonrpsee::core::client::Error::Call(object)
            if matches!(object.code(), METHOD_NOT_FOUND | codes::NOT_IMPLEMENTED)
    ) {
        return CliError::RpcCapabilityUnsupported { method, message };
    }
    CliError::RpcMethodFailed { method, message }
}

fn safe_rpc_error(error: &jsonrpsee::core::client::Error) -> String {
    match error {
        jsonrpsee::core::client::Error::Call(object) => {
            format!("{} (code {})", human_safe(object.message()), object.code())
        }
        _ => human_safe(&error.to_string()),
    }
}

fn print_show(
    tx_hash: &TxHash,
    status: &TransactionStatus,
    receipt: Option<&TransactionReceipt>,
    print: &Print,
) {
    print.detail("hash", format_tx_hash(tx_hash));
    match status {
        TransactionStatus::Pending => print.detail("status", "pending"),
        TransactionStatus::Committed { height } => {
            print.detail("status", "committed");
            print.detail("height", **height);
        }
        TransactionStatus::Invalid { reason } => {
            print.detail("status", "invalid");
            print.detail("reason", human_safe(reason));
        }
        TransactionStatus::RecentFailure {
            attempt_height,
            conflict,
        } => {
            print.detail("status", "recent failure (advisory)");
            print.detail("attempt height", **attempt_height);
            if let Ok(conflict) = serde_json::to_string(conflict) {
                print.detail("conflict", human_safe(&conflict));
            }
        }
        TransactionStatus::Unknown => {
            print.detail("status", "unknown (not found in the mempool or indexer)");
        }
        TransactionStatus::Other => print.detail("status", "other (update the CLI)"),
        _ => print.detail("status", "unsupported (update the CLI)"),
    }

    if matches!(status, TransactionStatus::Committed { .. }) {
        let Some(receipt) = receipt else {
            print.detail("receipt", "unavailable");
            return;
        };
        print.detail("receipt", "available");
        print.detail("receipt hash", format_tx_hash(&receipt.tx_hash));
        print.detail("receipt height", *receipt.height);
        print.detail(
            "execution",
            human_safe(&summarize_exec_status(&receipt.status)),
        );
        for event in &receipt.events {
            if let Ok(event) = serde_json::to_string(event) {
                print.detail("event", human_safe(&event));
            }
        }
        if let Some(data) = &receipt.return_data {
            print.detail("return data", format_args!("0x{}", hex::encode(&data.0)));
        }
        if let Some(data) = &receipt.abort_data {
            print.detail("abort data", format_args!("0x{}", hex::encode(&data.0)));
        }
    }
}

fn human_safe(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::parse_tx_hash;
    use crate::tx_utils::format_tx_hash;

    #[test]
    fn transaction_hash_parser_accepts_shared_hex_forms() {
        for input in [
            format!("0x{}", "ab".repeat(32)),
            format!("0X{}", "AB".repeat(32)),
            "aB".repeat(32),
        ] {
            let parsed =
                parse_tx_hash(&input).expect("bug: shared transaction hash form should parse");
            assert_eq!(format_tx_hash(&parsed), format!("0x{}", "ab".repeat(32)));
        }
    }

    #[test]
    fn transaction_hash_parser_rejects_malformed_or_wrong_length_input() {
        for input in [
            "0xdeadbeef".to_owned(),
            format!("0x{}", "ab".repeat(33)),
            "not-hex".to_owned(),
        ] {
            let error =
                parse_tx_hash(&input).expect_err("bug: invalid transaction hash should fail");
            assert_eq!(error.code(), "invalid_transaction_hash");
        }
    }
}
