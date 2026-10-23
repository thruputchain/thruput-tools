use thruput_indexer::indexer::TxInfoOutcome;
use thruput_indexer::indexer::TxInfoResult;
use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::{base58::Base58, transaction::TxHash};

use thruput_hogd_backend::indexer::*;

#[test]
fn only_the_not_found_outcome_reads_as_a_miss() {
    assert!(matches!(
        tx_lookup(
            "indexer",
            TxHash(Base58([0x33; 32].into())),
            TxInfoResult::Default {
                outcome: TxInfoOutcome::TxInfoNotFound.0,
            },
        ),
        Ok(TxLookup::NotFound)
    ));

    let error = tx_lookup(
        "indexer",
        TxHash(Base58([0x33; 32].into())),
        TxInfoResult::Default { outcome: 4242 },
    )
    .unwrap_err();
    assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
    assert!(error.to_string().contains("4242"), "{error}");
}

#[test]
fn a_miss_has_no_committed_status() {
    assert!(committed_status(TxLookup::NotFound).is_none());
}
