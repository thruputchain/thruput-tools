use groundhog_rpc_api_types::transaction::TxHash;

use crate::CliError;

pub fn nonce_from_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(1)
}

pub fn format_tx_hash(tx_hash: &TxHash) -> String {
    serde_json::to_string(tx_hash)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

pub fn poll_exhausted_error() -> CliError {
    CliError::Other("internal error: receipt polling exhausted unexpectedly".into())
}
