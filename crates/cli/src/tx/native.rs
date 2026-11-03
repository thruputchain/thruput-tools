//! Native address-history transport for the CLI report and renderer.

use serde_json::json;
use thruput_json_rpc_client::native::NativeClient;
use thruput_json_rpc_types::{
    events::PaginationCursor,
    hex::HexBytes,
    requests::{AddressHistoryQuery, DescendingAnchor, HistoryStart},
    responses::DecodedResponse,
    transaction::{AddressTransactionQuery, TransactionRef, TransactionRefPage},
};

use crate::{CliError, native_rpc};

pub(super) async fn fetch_transaction_page(
    url: &str,
    query: AddressTransactionQuery,
    continuation: bool,
) -> Result<TransactionRefPage, CliError> {
    let client = NativeClient::new(crate::http_client::rpc_client(url)?);
    thruput_client::check_endpoint(&client, &["getTransactionsByAddress"])
        .await
        .map_err(|error| native_rpc::rpc_error(&error))?;
    let response = client
        .get_transactions_by_address(AddressHistoryQuery {
            address: query.address,
            start: (!continuation).then_some(HistoryStart::Descending {
                anchor: DescendingAnchor::AtEnd {},
            }),
            cursor: continuation.then_some(HexBytes(query.cursor.0)),
            limit: Some(query.limit),
        })
        .await
        .map_err(|error| native_rpc::client_error(&error))?;
    let page = match response {
        DecodedResponse::Known(page) => page,
        DecodedResponse::Unsupported { raw } => {
            return Err(native_rpc::failure(
                "unsupported native address history response",
                json!({"method":"getTransactionsByAddress", "result":raw}),
            ));
        }
        _ => {
            return Err(native_rpc::failure(
                "unsupported native response",
                json!({"method":"getTransactionsByAddress"}),
            ));
        }
    };
    if u32::try_from(page.transactions.len()).map_or(true, |len| len > query.limit) {
        return Err(native_rpc::failure(
            "native address history exceeded the requested page limit",
            json!({"method":"getTransactionsByAddress", "requestedLimit":query.limit}),
        ));
    }
    // The CLI report remains v2: an empty cursor is its terminal marker.
    Ok(TransactionRefPage {
        refs: page
            .transactions
            .into_iter()
            .map(|reference| TransactionRef {
                tx_hash: reference.transaction_hash,
                height: reference.block_height,
            })
            .collect(),
        cursor: PaginationCursor(page.next_cursor.map_or_else(Vec::new, |cursor| cursor.0)),
        latest_block: page.coverage.through_block_height,
        oldest_block: page.coverage.from_block_height,
    })
}
