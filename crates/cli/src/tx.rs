use std::borrow::Cow;
use std::path::Path;

use groundhog_json_rpc_client::GroundhogApiClient;
use groundhog_json_rpc_types::ContractAddress;
use groundhog_json_rpc_types::events::{BlockAnchor, IterationOrder, PaginationCursor};
use groundhog_json_rpc_types::hex::decode_fixed_hex;
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::{
    AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash,
};

use crate::account_resolution::resolve_address_target_or_default_wallet;
use crate::checks::{Finding, FindingLevel};
use crate::codec::format_contract_address;
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::pagination::{parse_cursor, parse_limit};
use crate::result::CommandResult;
use crate::terminal::terminal_safe;
use crate::tx_json::{
    TX_LIST_JSON_SCHEMA_VERSION, TX_SHOW_JSON_SCHEMA_VERSION, TxListReport, TxShowReport,
};
use crate::tx_utils::{
    format_tx_hash, is_unsupported_rpc_method_error, print_receipt_data, rpc_error_message,
    summarize_exec_status,
};
use crate::{CliError, Print};

const ROOT_HISTORY_WARNING: &str = "history includes root invocations only; incoming wallet-wrapped transfers may not appear for recipients";

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
    let decoded_abort = receipt
        .as_ref()
        .and_then(|receipt| crate::tx_utils::decode_receipt_abort(receipt, None));

    if !print.is_json() && !print.is_quiet() {
        print_show(
            &tx_hash,
            &status,
            receipt.as_ref(),
            decoded_abort.as_ref(),
            print,
        );
    }
    print.json(&CommandResult::new(
        cmd,
        TxShowReport {
            schema_version: TX_SHOW_JSON_SCHEMA_VERSION,
            profile: profile.name,
            rpc_url,
            tx_hash,
            status,
            receipt,
            abort: crate::abort_json::AbortReport::from_decode(decoded_abort.as_ref()),
        },
    ))
}

/// List one page of root-invocation transaction history for an address.
#[expect(clippy::too_many_arguments)]
pub fn list(
    target_input: Option<&str>,
    limit_input: Option<&str>,
    cursor_input: Option<&str>,
    project_start: &Path,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let limit = parse_history_limit(limit_input)?;
    let cursor = parse_transaction_cursor(cursor_input)?;
    let profile = resolve_profile_with_overrides(project_start, profile_name, overrides)?;
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let (target_input, address) = resolve_address_target_or_default_wallet(&profile, target_input)?;
    let address_hex = format_contract_address(&address);

    let query = AddressTransactionQuery {
        address: ContractAddress::from(address),
        order: IterationOrder::Descending,
        anchor: BlockAnchor::AtEnd,
        cursor,
        limit,
    };
    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let page = rt.block_on(fetch_transaction_page(&rpc_url, query))?;

    if !print.is_json() {
        print_list(&target_input, &address_hex, &page, print);
    }
    print.json(&CommandResult::with_warnings(
        cmd,
        vec![Finding {
            level: FindingLevel::Warn,
            code: "root_invocation_history_only",
            message: ROOT_HISTORY_WARNING.into(),
            hint: Some(Cow::Borrowed(
                "do not treat this page as a complete incoming or outgoing asset ledger",
            )),
        }],
        TxListReport {
            schema_version: TX_LIST_JSON_SCHEMA_VERSION,
            profile: profile.name,
            rpc_url,
            target_input,
            address: ContractAddress::from(address),
            refs: page.refs,
            cursor: page.cursor,
            latest_block: page.latest_block,
            oldest_block: page.oldest_block,
        },
    ))
}

fn parse_history_limit(input: Option<&str>) -> Result<u32, CliError> {
    parse_limit(input).map_err(|error| CliError::InvalidTransactionHistoryLimit {
        display_input: terminal_safe(&error.input),
        input: error.input,
        message: error.message,
    })
}

fn parse_transaction_cursor(input: Option<&str>) -> Result<PaginationCursor, CliError> {
    parse_cursor(input).map_err(|error| CliError::InvalidTransactionCursor {
        display_input: terminal_safe(&error.input),
        input: error.input,
        message: error.message,
    })
}

async fn fetch_transaction_page(
    rpc_url: &str,
    query: AddressTransactionQuery,
) -> Result<TransactionRefPage, CliError> {
    let requested_limit = query.limit;
    let client = crate::http_client::rpc_client(rpc_url)?;
    let page = client
        .get_transactions_by_address(query)
        .await
        .map_err(|error| map_rpc_error("getTransactionsByAddress", &error))?;
    if u32::try_from(page.refs.len()).map_or(true, |len| len > requested_limit) {
        return Err(CliError::RpcMethodFailed {
            method: "getTransactionsByAddress",
            message: format!(
                "invalid response: returned {} references for requested limit {requested_limit}",
                page.refs.len(),
            ),
        });
    }
    Ok(page)
}

fn parse_tx_hash(input: &str) -> Result<TxHash, CliError> {
    let bytes =
        decode_fixed_hex::<32>(input).map_err(|error| CliError::InvalidTransactionHash {
            input: input.to_owned(),
            display_input: terminal_safe(input),
            message: error.to_string(),
        })?;
    Ok(TxHash(groundhog_xdr::types::SHA256Hash::from(bytes).into()))
}

async fn fetch_transaction(
    rpc_url: &str,
    tx_hash: TxHash,
) -> Result<(TransactionStatus, Option<TransactionReceipt>), CliError> {
    let client = crate::http_client::rpc_client(rpc_url)?;
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

pub(crate) fn map_rpc_error(
    method: &'static str,
    error: &jsonrpsee::core::client::Error,
) -> CliError {
    let message = rpc_error_message(error);
    if is_unsupported_rpc_method_error(error) {
        return CliError::RpcCapabilityUnsupported { method, message };
    }
    CliError::RpcMethodFailed { method, message }
}

fn print_show(
    tx_hash: &TxHash,
    status: &TransactionStatus,
    receipt: Option<&TransactionReceipt>,
    decoded_abort: Option<&Result<crate::codec::DecodedAbort, crate::codec::AbortDecodeError>>,
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
            print.detail("reason", terminal_safe(reason));
        }
        TransactionStatus::RecentFailure {
            attempt_height,
            cause,
        } => {
            print.detail("status", "recent failure (advisory)");
            print.detail("attempt height", **attempt_height);
            if let Ok(cause) = serde_json::to_string(cause) {
                print.detail("cause", terminal_safe(&cause));
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
            terminal_safe(&summarize_exec_status(&receipt.status)),
        );
        print_receipt_data(receipt, decoded_abort, print);
    }
}

fn print_list(target_input: &str, address: &str, page: &TransactionRefPage, print: &Print) {
    print.warn(ROOT_HISTORY_WARNING);
    if print.is_quiet() {
        return;
    }
    if !target_input.eq_ignore_ascii_case(address) {
        print.detail("target", terminal_safe(target_input));
    }
    print.detail("address", address);
    print.detail("latest indexed block", *page.latest_block);
    print.detail("oldest retained block", *page.oldest_block);
    if page.refs.is_empty() {
        if page.cursor.0.is_empty() {
            print.detail("transactions", "none (end of history)");
        } else {
            print.detail("transactions", "none in this page");
        }
    } else {
        for transaction in &page.refs {
            print.detail(
                "tx",
                format_args!(
                    "{} at height {}",
                    format_tx_hash(&transaction.tx_hash),
                    *transaction.height,
                ),
            );
        }
    }
    if !page.cursor.0.is_empty() {
        print.status("");
        print.detail("next cursor", format_cursor(&page.cursor));
    }
}

fn format_cursor(cursor: &PaginationCursor) -> String {
    format!("0x{}", hex::encode(&cursor.0))
}

#[cfg(test)]
mod tests {
    use super::{format_cursor, parse_tx_hash};
    use crate::tx_utils::format_tx_hash;
    use groundhog_json_rpc_types::events::PaginationCursor;

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

    #[test]
    fn cursor_formatter_preserves_opaque_bytes() {
        assert_eq!(
            format_cursor(&PaginationCursor(vec![0x00, 0xab, 0xff])),
            "0x00abff"
        );
    }
}
