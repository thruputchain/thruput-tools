//! Native event transport feeding the shared CLI decoding and rendering path.

use serde_json::json;
use thruput_json_rpc_client::native::NativeClient;
use thruput_json_rpc_types::{
    events::{EmittedEvent, EventPage, EventQuery, IdentifiedEvent, PaginationCursor},
    hex::HexBytes,
    requests::{self as input, DescendingAnchor, HistoryStart},
    responses::DecodedResponse,
};

use crate::{CliError, native_rpc};

pub(super) async fn fetch_event_page(
    url: &str,
    query: EventQuery,
    continuation: bool,
) -> Result<EventPage, CliError> {
    let client = NativeClient::new(crate::http_client::rpc_client(url)?);
    thruput_client::check_endpoint(&client, &["getEvents"])
        .await
        .map_err(|error| native_rpc::rpc_error(&error))?;
    let response = client
        .get_events(input::EventQuery {
            filters: query
                .filters
                .0
                .0
                .into_iter()
                .map(|filter| input::EventFilter {
                    addresses: filter.contract_addresses.0.0,
                    topics: Some(input::TopicFilter {
                        topic0: Some(filter.topics.topic0.0.0),
                        topic1: Some(filter.topics.topic1.0.0),
                        topic2: Some(filter.topics.topic2.0.0),
                    }),
                })
                .collect(),
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
                "unsupported native event response",
                json!({"method":"getEvents", "result":raw}),
            ));
        }
        _ => {
            return Err(native_rpc::failure(
                "unsupported native response",
                json!({"method":"getEvents"}),
            ));
        }
    };
    let page = EventPage {
        events: page
            .events
            .into_iter()
            .map(|identified| IdentifiedEvent {
                tx_hash: identified.transaction_hash,
                block_height: identified.block_height,
                sequence_number: identified.sequence_number,
                event: EmittedEvent {
                    contract_address: identified.event.address,
                    topics: identified.event.topics,
                    data: identified.event.data,
                },
            })
            .collect(),
        cursor: PaginationCursor(page.next_cursor.map_or_else(Vec::new, |cursor| cursor.0)),
        latest_block: page.latest_indexed_block_height,
        oldest_block: page.oldest_indexed_block_height,
    };
    super::validate_event_page(&page, query.limit).map_err(|error| {
        native_rpc::failure(
            error.to_string(),
            json!({"method":"getEvents", "requestedLimit":query.limit,
                "cause":{"code":error.code(), "message":error.to_string(), "details":error.details_json()}}),
        )
    })?;
    Ok(page)
}
