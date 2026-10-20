mod native;

use std::borrow::Cow;
use std::path::Path;

use thruput_json_rpc_client::native::NativeClient;
use thruput_json_rpc_types::ContractAddress;
use thruput_json_rpc_types::base58::FixedIdentifier;
use thruput_json_rpc_types::events::{BlockAnchor, IterationOrder, PaginationCursor};
use thruput_json_rpc_types::requests::TransactionQuery;
use thruput_json_rpc_types::responses::{ReceiptExecution, TransactionReceipt, TransactionStatus};
use thruput_json_rpc_types::transaction::{AddressTransactionQuery, TransactionRefPage, TxHash};

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
use crate::tx_utils::format_tx_hash;
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
    let rpc_url = profile.rpc_endpoint()?.to_owned();

    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let task = crate::print::progress::Task::start(print, "Reading transaction", true);
    let (status, receipt) = rt.block_on(fetch_transaction(&rpc_url, tx_hash))?;
    let _ = task.finish();
    let decoded_abort = receipt
        .as_ref()
        .and_then(|receipt| match &receipt.execution {
            ReceiptExecution::Abort { abort_data } => {
                Some(crate::codec::decode_abort(&abort_data.0, None))
            }
            _ => None,
        });

    if !print.is_json() && !print.is_quiet() {
        print.group(crate::print::Outcome::Information, "Transaction", |print| {
            print_show(
                &tx_hash,
                &status,
                receipt.as_ref(),
                decoded_abort.as_ref(),
                print,
            );
        });
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
    if let Some(input) = cursor_input
        && cursor.0.is_empty()
    {
        return Err(CliError::InvalidTransactionCursor {
            input: input.to_owned(),
            display_input: terminal_safe(input),
            message: "an empty cursor marks the end of history; omit --cursor to start a new walk"
                .into(),
        });
    }
    let profile = resolve_profile_with_overrides(project_start, profile_name, overrides)?;
    let rpc_url = profile.rpc_endpoint()?.to_owned();
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
    let task = crate::print::progress::Task::start(print, "Reading transaction history", true);
    let page = rt.block_on(native::fetch_transaction_page(
        &rpc_url,
        query,
        cursor_input.is_some(),
    ))?;
    let _ = task.finish();

    if !print.is_json() {
        print.group(
            crate::print::Outcome::Information,
            "Transaction history",
            |print| print_list(&target_input, &address_hex, &page, print),
        );
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

fn parse_tx_hash(input: &str) -> Result<TxHash, CliError> {
    let bytes = thruput_xdr::types::SHA256Hash::decode_base58(input).map_err(|error| {
        CliError::InvalidTransactionHash {
            input: input.to_owned(),
            display_input: terminal_safe(input),
            message: error.to_string(),
        }
    })?;
    Ok(TxHash(bytes.into()))
}

async fn fetch_transaction(
    rpc_url: &str,
    tx_hash: TxHash,
) -> Result<(TransactionStatus, Option<TransactionReceipt>), CliError> {
    let client = NativeClient::new(crate::http_client::rpc_client(rpc_url)?);
    thruput_client::check_endpoint(&client, &["getTransactionStatus", "getTransactionReceipt"])
        .await
        .map_err(|error| crate::native_rpc::rpc_error(&error))?;
    let status = crate::native_rpc::known(
        "getTransactionStatus",
        client
            .get_transaction_status(TransactionQuery {
                transaction_hash: tx_hash,
            })
            .await,
    )?;
    let receipt = crate::native_rpc::known(
        "getTransactionReceipt",
        client
            .get_transaction_receipt(TransactionQuery {
                transaction_hash: tx_hash,
            })
            .await,
    )?;
    if let Some(receipt) = &receipt {
        let inconsistent_height = matches!(&status, TransactionStatus::Committed { block_height } if block_height != &receipt.block_height);
        if receipt.transaction_hash != tx_hash || inconsistent_height {
            return Err(crate::native_rpc::failure(
                "inconsistent transaction receipt",
                serde_json::json!({"transactionHash":tx_hash,"status":status,"receipt":receipt}),
            ));
        }
    }
    Ok((status, receipt))
}

fn print_show(
    tx_hash: &TxHash,
    status: &TransactionStatus,
    receipt: Option<&TransactionReceipt>,
    decoded_abort: Option<&Result<crate::codec::DecodedAbort, crate::codec::AbortDecodeError>>,
    print: &Print,
) {
    print.detail("hash", format_tx_hash(tx_hash));
    if let Ok(value) = serde_json::to_string(status) {
        print.detail("status", terminal_safe(&value));
    }
    if let Some(receipt) = receipt {
        print.detail("receipt block height", *receipt.block_height);
        if let Ok(value) = serde_json::to_string(&receipt.execution) {
            print.detail("execution", terminal_safe(&value));
        }
        crate::tx_utils::print_receipt_data(receipt, decoded_abort, print);
    } else {
        print.detail("receipt", "unavailable");
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
    use thruput_json_rpc_types::events::PaginationCursor;

    #[test]
    fn transaction_hash_parser_accepts_canonical_base58() {
        for input in [
            thruput_json_rpc_types::NetworkHashText::from([0xab; 32]).to_string(),
            "1".repeat(32),
        ] {
            let parsed =
                parse_tx_hash(&input).expect("bug: shared transaction hash form should parse");
            assert_eq!(format_tx_hash(&parsed), input);
        }
    }

    #[test]
    fn transaction_hash_parser_rejects_malformed_or_wrong_length_input() {
        for input in [
            "0xdeadbeef".to_owned(),
            format!("0x{}", "ab".repeat(33)),
            "not-hex".to_owned(),
            format!("0x{}", "ab".repeat(32)),
            "ab".repeat(32),
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
