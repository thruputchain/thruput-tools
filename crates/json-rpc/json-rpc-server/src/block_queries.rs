//! Shared request and response checks for exact-height block reads.

use thruput_json_rpc_types::{
    BlockHeight,
    events::IterationOrder,
    transaction::{BlockTransactionQuery, TransactionRefPage},
};

use crate::{RpcServerConfig, RpcServerError};

pub(crate) fn validate_height(height: &BlockHeight) -> Result<(), RpcServerError> {
    if **height < 0 {
        return Err(RpcServerError::InvalidParam(
            "height must be nonnegative".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_query(
    query: &BlockTransactionQuery,
    config: &RpcServerConfig,
) -> Result<(), RpcServerError> {
    validate_height(&query.height)?;
    let cap = config.transaction_page_cap();
    if query.limit == 0 {
        return Err(RpcServerError::InvalidParam(
            "limit must be greater than zero".into(),
        ));
    }
    if query.limit > cap {
        return Err(RpcServerError::InvalidParam(format!(
            "limit exceeds maximum {cap}"
        )));
    }
    if query.cursor.0.len() > config.max_cursor_bytes {
        return Err(RpcServerError::InvalidParam(format!(
            "cursor length exceeds maximum {}",
            config.max_cursor_bytes
        )));
    }
    Ok(())
}

/// Reject inconsistent block pages before they are served or counted as backend successes.
pub fn validate_page(
    query: &BlockTransactionQuery,
    page: &TransactionRefPage,
    max_cursor_bytes: usize,
) -> Result<(), RpcServerError> {
    let invalid = || RpcServerError::BackendUnavailable("invalid block transaction page".into());
    if *page.oldest_block < 1
        || page.latest_block < page.oldest_block
        || page.refs.len() > query.limit as usize
        || page.cursor.0.len() > max_cursor_bytes
        || (!page.cursor.0.is_empty() && (page.refs.is_empty() || page.cursor == query.cursor))
    {
        return Err(invalid());
    }
    let outside = query.height < page.oldest_block || query.height > page.latest_block;
    if outside && (!page.refs.is_empty() || !page.cursor.0.is_empty()) {
        return Err(invalid());
    }
    if page.refs.iter().any(|item| item.height != query.height) {
        return Err(invalid());
    }
    // Continuation order is private cursor state. Check strict monotonicity
    // either way without decoding or changing that opaque state.
    let compare = |pair: &[thruput_json_rpc_types::transaction::TransactionRef]| {
        let left: &[u8] = pair[0].tx_hash.0.0.as_ref();
        let right: &[u8] = pair[1].tx_hash.0.0.as_ref();
        left.cmp(right)
    };
    let ascending = page.refs.windows(2).all(|pair| compare(pair).is_lt());
    let descending = page.refs.windows(2).all(|pair| compare(pair).is_gt());
    let ordered = if query.cursor.0.is_empty() {
        match query.order {
            IterationOrder::Ascending => ascending,
            IterationOrder::Descending => descending,
        }
    } else {
        ascending || descending
    };
    if !ordered {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_json_rpc_types::{
        base58::Base58,
        events::PaginationCursor,
        transaction::{TransactionRef, TxHash},
    };

    fn query() -> BlockTransactionQuery {
        BlockTransactionQuery {
            height: 9.into(),
            order: IterationOrder::Ascending,
            cursor: PaginationCursor(vec![]),
            limit: 2,
        }
    }

    fn page() -> TransactionRefPage {
        TransactionRefPage {
            refs: [0x7F, 0x80]
                .into_iter()
                .map(|byte| TransactionRef {
                    tx_hash: TxHash(Base58([byte; 32].into())),
                    height: 9.into(),
                })
                .collect(),
            cursor: PaginationCursor(vec![1]),
            oldest_block: 1.into(),
            latest_block: 10.into(),
        }
    }

    #[test]
    fn positive_limits_and_bounds_are_enforced_before_dispatch() {
        let config = RpcServerConfig::default().with_max_transactions_per_page(10);
        let mut q = query();
        q.limit = 0;
        assert!(validate_query(&q, &config).is_err());
        q.limit = 10;
        validate_query(&q, &config).unwrap();
        q.limit = 11;
        assert!(validate_query(&q, &config).is_err());
        q.limit = 1;
        q.height = (-1).into();
        assert!(validate_query(&q, &config).is_err());
        q.height = i64::MAX.into();
        validate_query(&q, &config).unwrap();
        q.cursor.0.resize(257, 0);
        assert!(validate_query(&q, &config).is_err());
        q.cursor.0.clear();
        q.limit = 201;
        assert!(
            validate_query(
                &q,
                &RpcServerConfig::default().with_max_transactions_per_page(500)
            )
            .is_err()
        );
    }

    #[test]
    fn pages_require_exact_height_bounds_and_strict_hash_order() {
        let config = RpcServerConfig::default();
        let q = query();
        validate_page(&q, &page(), config.max_cursor_bytes).unwrap();
        let mut wrong = page();
        wrong.refs[0].height = 8.into();
        assert!(validate_page(&q, &wrong, config.max_cursor_bytes).is_err());
        wrong = page();
        wrong.latest_block = 8.into();
        assert!(validate_page(&q, &wrong, config.max_cursor_bytes).is_err());
        wrong = page();
        wrong.oldest_block = 0.into();
        assert!(validate_page(&q, &wrong, config.max_cursor_bytes).is_err());
        wrong = page();
        wrong.refs.reverse();
        assert!(validate_page(&q, &wrong, config.max_cursor_bytes).is_err());
        let mut descending = q.clone();
        descending.order = IterationOrder::Descending;
        validate_page(&descending, &wrong, config.max_cursor_bytes).unwrap();
        descending.cursor = PaginationCursor(vec![2]);
        validate_page(&descending, &page(), config.max_cursor_bytes).unwrap();
        wrong.refs[1] = wrong.refs[0].clone();
        assert!(validate_page(&descending, &wrong, config.max_cursor_bytes).is_err());
    }

    #[test]
    fn empty_pages_are_terminal_and_continuations_make_progress() {
        let config = RpcServerConfig::default();
        let q = query();
        let mut empty = page();
        empty.refs.clear();
        assert!(validate_page(&q, &empty, config.max_cursor_bytes).is_err());
        empty.cursor.0.clear();
        validate_page(&q, &empty, config.max_cursor_bytes).unwrap();
        empty.latest_block = 8.into();
        validate_page(&q, &empty, config.max_cursor_bytes).unwrap();
        let mut resumed = q.clone();
        resumed.cursor = page().cursor;
        assert!(validate_page(&resumed, &page(), config.max_cursor_bytes).is_err());
        let mut oversized = page();
        oversized.refs.push(oversized.refs[1].clone());
        assert!(validate_page(&q, &oversized, config.max_cursor_bytes).is_err());
    }
}
