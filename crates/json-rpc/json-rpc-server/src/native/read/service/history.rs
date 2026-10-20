//! Validate native history at the shared service boundary without reading cursors.

use super::{NativeReadService, input, invalid, out};
use crate::{RpcServerConfig, RpcServerError, native::read::NativeHistoryPlane};
use jsonrpsee::core::RpcResult;
use std::cmp::Ordering;
use thruput_json_rpc_types::{BlockHeight, events, hex::HexBytes};

fn parameter(message: &str) -> RpcServerError {
    RpcServerError::InvalidParam(message.into())
}

fn prepare<S>(
    start: Option<&S>,
    cursor: Option<&HexBytes>,
    limit: &mut Option<u32>,
    cap: u32,
    max_cursor_bytes: usize,
) -> Result<(), RpcServerError> {
    if start.is_some() == cursor.is_some() {
        return Err(parameter("require exactly one of start or cursor"));
    }
    if cursor.is_some_and(|c| c.0.is_empty() || c.0.len() > max_cursor_bytes) {
        return Err(parameter("cursor length outside configured bounds"));
    }
    let effective = limit.unwrap_or(50.min(cap));
    if effective == 0 || effective > cap {
        return Err(parameter("limit outside configured bounds"));
    }
    *limit = Some(effective);
    Ok(())
}

fn direction(start: &input::HistoryStart) -> Option<events::IterationOrder> {
    match start {
        input::HistoryStart::Ascending { .. } => Some(events::IterationOrder::Ascending),
        input::HistoryStart::Descending { .. } => Some(events::IterationOrder::Descending),
        _ => None,
    }
}

fn anchor(start: &input::HistoryStart) -> Result<Option<&BlockHeight>, RpcServerError> {
    let value = match start {
        input::HistoryStart::Ascending { anchor } => match anchor {
            input::AscendingAnchor::AtBeginning {} => None,
            input::AscendingAnchor::AtHeight { block_height } => Some(block_height),
            _ => return Err(parameter("unsupported history anchor")),
        },
        input::HistoryStart::Descending { anchor } => match anchor {
            input::DescendingAnchor::AtEnd {} => None,
            input::DescendingAnchor::AtHeight { block_height } => Some(block_height),
            _ => return Err(parameter("unsupported history anchor")),
        },
        _ => return Err(parameter("unsupported history direction")),
    };
    if value.is_some_and(|height| **height < 0) {
        return Err(parameter("blockHeight must be nonnegative"));
    }
    Ok(value)
}

fn validate_filters(filters: &[input::EventFilter]) -> Result<(), RpcServerError> {
    if filters.is_empty() || filters.len() > events::MAX_EVENT_FILTERS_PER_QUERY as usize {
        return Err(parameter("filters outside event-query bounds"));
    }
    for filter in filters {
        if filter.addresses.is_empty()
            || filter.addresses.len() > events::MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER as usize
        {
            return Err(parameter("addresses outside event-filter bounds"));
        }
        if let Some(topics) = &filter.topics {
            for position in [&topics.topic0, &topics.topic1, &topics.topic2]
                .into_iter()
                .flatten()
            {
                if position.len() > events::MAX_TOPIC_VALUES_PER_POSITION as usize {
                    return Err(parameter("topics outside event-filter bounds"));
                }
            }
        }
    }
    Ok(())
}

fn page(
    count: usize,
    next: Option<&HexBytes>,
    oldest: &BlockHeight,
    latest: &BlockHeight,
    cursor: Option<&HexBytes>,
    limit: Option<u32>,
    config: &RpcServerConfig,
) -> Result<(), RpcServerError> {
    if **oldest < 0 || latest < oldest || count > limit.unwrap_or(0) as usize {
        return Err(invalid("history page has invalid bounds or cardinality"));
    }
    if next.is_some_and(|next| {
        next.0.is_empty() || next.0.len() > config.max_cursor_bytes || Some(next) == cursor
    }) {
        return Err(invalid("history page has an invalid or repeated cursor"));
    }
    Ok(())
}

fn ordered(
    comparisons: impl Iterator<Item = Ordering>,
    order: Option<events::IterationOrder>,
) -> bool {
    let mut ascending = true;
    let mut descending = true;
    for comparison in comparisons {
        ascending &= comparison.is_lt();
        descending &= comparison.is_gt();
    }
    match order {
        Some(events::IterationOrder::Ascending) => ascending,
        Some(events::IterationOrder::Descending) => descending,
        None => ascending || descending,
    }
}

fn in_range(
    height: &BlockHeight,
    oldest: &BlockHeight,
    latest: &BlockHeight,
    start: Option<&input::HistoryStart>,
) -> bool {
    if height < oldest || height > latest {
        return false;
    }
    match start {
        Some(input::HistoryStart::Ascending {
            anchor: input::AscendingAnchor::AtHeight { block_height },
        }) => height >= block_height,
        Some(input::HistoryStart::Descending {
            anchor: input::DescendingAnchor::AtHeight { block_height },
        }) => height <= block_height,
        _ => true,
    }
}

fn retained_anchor(
    start: Option<&input::HistoryStart>,
    oldest: &BlockHeight,
) -> Result<(), RpcServerError> {
    if let Some(start) = start
        && anchor(start)?.is_some_and(|height| height < oldest)
    {
        return Err(invalid(
            "successful history page excludes the requested anchor",
        ));
    }
    Ok(())
}

fn matches_filter(event: &out::EmittedEvent, filter: &input::EventFilter) -> bool {
    if !filter.addresses.contains(&event.address) {
        return false;
    }
    let Some(topics) = &filter.topics else {
        return true;
    };
    [&topics.topic0, &topics.topic1, &topics.topic2]
        .iter()
        .enumerate()
        .all(|(index, values)| {
            values.as_ref().is_none_or(|values| {
                values.is_empty()
                    || event
                        .topics
                        .get(index)
                        .is_some_and(|topic| values.contains(topic))
            })
        })
}

fn validate_events(
    request: &input::EventQuery,
    result: &out::EventPage,
    config: &RpcServerConfig,
) -> Result<(), RpcServerError> {
    page(
        result.events.len(),
        result.next_cursor.as_ref(),
        &result.oldest_indexed_block_height,
        &result.latest_indexed_block_height,
        request.cursor.as_ref(),
        request.limit,
        config,
    )?;
    retained_anchor(request.start.as_ref(), &result.oldest_indexed_block_height)?;
    let mut identities = std::collections::BTreeSet::new();
    for event in &result.events {
        let hash: &[u8] = event.transaction_hash.0.0.as_ref();
        if !identities.insert((hash, event.sequence_number)) {
            return Err(invalid("history page repeats an event identity"));
        }
        if !in_range(
            &event.block_height,
            &result.oldest_indexed_block_height,
            &result.latest_indexed_block_height,
            request.start.as_ref(),
        ) || event.event.topics.len() > 3
            || !request
                .filters
                .iter()
                .any(|filter| matches_filter(&event.event, filter))
        {
            return Err(invalid("event does not satisfy its history query"));
        }
    }
    let comparisons = result.events.windows(2).map(|pair| {
        let left: &[u8] = pair[0].transaction_hash.0.0.as_ref();
        let right: &[u8] = pair[1].transaction_hash.0.0.as_ref();
        (&pair[0].block_height, left, pair[0].sequence_number).cmp(&(
            &pair[1].block_height,
            right,
            pair[1].sequence_number,
        ))
    });
    if !ordered(comparisons, request.start.as_ref().and_then(direction)) {
        return Err(invalid("history events are not strictly ordered"));
    }
    Ok(())
}

fn validate_refs(
    result: &out::TransactionRefPage,
    order: Option<events::IterationOrder>,
) -> Result<(), RpcServerError> {
    let mut hashes = std::collections::BTreeSet::new();
    for item in &result.transactions {
        let hash: &[u8] = item.transaction_hash.0.0.as_ref();
        if !hashes.insert(hash) {
            return Err(invalid("history page repeats a transaction"));
        }
    }
    let comparisons = result.transactions.windows(2).map(|pair| {
        let left: &[u8] = pair[0].transaction_hash.0.0.as_ref();
        let right: &[u8] = pair[1].transaction_hash.0.0.as_ref();
        (&pair[0].block_height, left).cmp(&(&pair[1].block_height, right))
    });
    if !ordered(comparisons, order) {
        return Err(invalid("history references are not strictly ordered"));
    }
    Ok(())
}

impl<C, R: NativeHistoryPlane, V> NativeReadService<C, R, V> {
    /// Scan an indexed event range; an empty page can still have a continuation.
    #[tracing::instrument(skip_all)]
    pub async fn get_events(&self, mut request: input::EventQuery) -> RpcResult<out::EventPage> {
        let context = self.context();
        self.within(context, async {
            prepare(
                request.start.as_ref(),
                request.cursor.as_ref(),
                &mut request.limit,
                self.config.event_page_cap(),
                self.config.max_cursor_bytes,
            )?;
            if let Some(start) = &request.start {
                anchor(start)?;
            }
            validate_filters(&request.filters)?;
            let result = self.state.native_events(context, request.clone()).await?;
            validate_events(&request, &result, &self.config)?;
            Ok(result)
        })
        .await
    }

    /// Read root-address references without claiming internal-call or transfer history.
    #[tracing::instrument(skip_all)]
    pub async fn get_transactions_by_root_address(
        &self,
        mut request: input::RootAddressQuery,
    ) -> RpcResult<out::TransactionRefPage> {
        let context = self.context();
        self.within(context, async {
            prepare(
                request.start.as_ref(),
                request.cursor.as_ref(),
                &mut request.limit,
                self.config.transaction_page_cap(),
                self.config.max_cursor_bytes,
            )?;
            if let Some(start) = &request.start {
                anchor(start)?;
            }
            let result = self
                .state
                .native_root_transactions(context, request.clone())
                .await?;
            page(
                result.transactions.len(),
                result.next_cursor.as_ref(),
                &result.oldest_indexed_block_height,
                &result.latest_indexed_block_height,
                request.cursor.as_ref(),
                request.limit,
                &self.config,
            )?;
            retained_anchor(request.start.as_ref(), &result.oldest_indexed_block_height)?;
            if result.transactions.iter().any(|item| {
                !in_range(
                    &item.block_height,
                    &result.oldest_indexed_block_height,
                    &result.latest_indexed_block_height,
                    request.start.as_ref(),
                )
            }) {
                return Err(invalid("reference does not satisfy its history range"));
            }
            validate_refs(&result, request.start.as_ref().and_then(direction))?;
            Ok(result)
        })
        .await
    }

    /// Read exactly one block, with cursor-owned continuation order.
    #[tracing::instrument(skip_all)]
    pub async fn get_transactions_by_block(
        &self,
        mut request: input::BlockPageRequest,
    ) -> RpcResult<out::BlockTransactionPage> {
        let context = self.context();
        self.within(context, async {
            prepare(
                request.start.as_ref(),
                request.cursor.as_ref(),
                &mut request.limit,
                self.config.transaction_page_cap(),
                self.config.max_cursor_bytes,
            )?;
            if *request.block_height < 0 {
                return Err(parameter("blockHeight must be nonnegative"));
            }
            let result = self
                .state
                .native_block_transactions(context, request.clone())
                .await?;
            page(
                result.transactions.len(),
                result.next_cursor.as_ref(),
                &result.oldest_indexed_block_height,
                &result.latest_indexed_block_height,
                request.cursor.as_ref(),
                request.limit,
                &self.config,
            )?;
            if request.cursor.is_some()
                && (request.block_height < result.oldest_indexed_block_height
                    || request.block_height > result.latest_indexed_block_height)
            {
                return Err(invalid(
                    "successful block continuation is outside indexed coverage",
                ));
            }
            if *result.oldest_indexed_block_height < 1
                || (result.transactions.is_empty() && result.next_cursor.is_some())
                || result.transactions.iter().any(|item| {
                    item.block_height != request.block_height
                        || !in_range(
                            &item.block_height,
                            &result.oldest_indexed_block_height,
                            &result.latest_indexed_block_height,
                            None,
                        )
                })
            {
                return Err(invalid("invalid exact-block reference page"));
            }
            validate_refs(&result, request.start.as_ref().map(|start| start.order))?;
            Ok(result)
        })
        .await
    }
}

#[cfg(test)]
mod tests;
