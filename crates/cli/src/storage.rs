use std::path::Path;
use thruput_json_rpc_types::hex::prefixed_hex;

use serde_json::json;
use thruput_json_rpc_client::native::NativeClient;
use thruput_json_rpc_types::ContractAddress;
use thruput_json_rpc_types::hex::{FixedHexError, HexBytes, decode_fixed_hex};
use thruput_json_rpc_types::requests::StorageQuery;
use thruput_json_rpc_types::responses::StorageResult;

use crate::account_resolution::resolve_contract_target;
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::result::CommandResult;
use crate::{CliError, Print};

/// Schema version of the `storage --json` data payload.
pub const STORAGE_JSON_SCHEMA_VERSION: u32 = 3;

/// Read the latest committed storage item at a contract address/key pair.
pub fn run(
    address: &str,
    key: &str,
    project_start: &Path,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let resolved_profile = resolve_profile_with_overrides(project_start, profile, overrides)?;
    let rpc_url = resolved_profile.rpc_endpoint()?;

    let address = ContractAddress::from(resolve_contract_target(&resolved_profile, address)?);
    let key = parse_storage_key(key)?;
    let key_hex = prefixed_hex(&key);
    let address_text = address.to_string();

    let task = crate::print::progress::Task::start(print, "Reading storage", true);

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let read = rt.block_on(fetch_storage_at(rpc_url, address, key))?;

    let _ = task.finish();
    if print.has_human_ui() {
        let item = read
            .item
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| CliError::Other(format!("failed to render storage item: {error}")))?;
        print.outcome_card(
            if item.is_some() {
                crate::print::Outcome::Information
            } else {
                crate::print::Outcome::Empty
            },
            "Storage",
            &[
                ("Address".into(), address_text.clone()),
                ("Key".into(), key_hex.clone()),
                ("Height".into(), read.state_block_height.to_string()),
                ("Item".into(), item.unwrap_or_else(|| "Absent".into())),
            ],
        );
    } else {
        print.success("Storage read complete");
        print_storage_read(&address_text, &key_hex, &read, print)?;
    }
    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": STORAGE_JSON_SCHEMA_VERSION,
            "rpcUrl": rpc_url,
            "address": address_text,
            "key": key_hex,
            "stateBlockHeight": read.state_block_height.to_string(),
            "item": read.item,
        }),
    ))?;

    Ok(())
}

async fn fetch_storage_at(
    rpc_url: &str,
    address: ContractAddress,
    key: HexBytes,
) -> Result<StorageResult, CliError> {
    let client = NativeClient::new(crate::http_client::rpc_client(rpc_url)?);
    thruput_client::check_endpoint(&client, &["getStorage"])
        .await
        .map_err(|error| crate::native_rpc::rpc_error(&error))?;
    crate::native_rpc::known(
        "getStorage",
        client.get_storage(StorageQuery { address, key }).await,
    )
}

fn print_storage_read(
    address: &str,
    key: &str,
    read: &StorageResult,
    print: &Print,
) -> Result<(), CliError> {
    print.detail("address", address);
    print.detail("key", key);
    print.detail("height", *read.state_block_height);
    match &read.item {
        Some(item) => {
            let value = serde_json::to_string(item)
                .map_err(|e| CliError::Other(format!("failed to render storage item: {e}")))?;
            print.detail("item", value);
        }
        None => {
            print.detail("item", "<absent>");
        }
    }
    Ok(())
}

fn parse_storage_key(raw: &str) -> Result<HexBytes, CliError> {
    let bytes = decode_fixed_hex::<32>(raw).map_err(|error| {
        CliError::InvalidStorageKey(match error {
            FixedHexError::InvalidHex(error) => error.to_string(),
            FixedHexError::InvalidLength { len, .. } => {
                format!("storage key must be exactly 32 bytes / 64 hex characters; got {len} bytes")
            }
        })
    })?;
    Ok(HexBytes(bytes.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_storage_key_accepts_32_byte_prefixed_hex() {
        let key = parse_storage_key(&format!("0x{}", "42".repeat(32))).unwrap();
        assert_eq!(key.0, vec![0x42; 32]);
    }

    #[test]
    fn parse_storage_key_accepts_32_byte_unprefixed_hex() {
        let key = parse_storage_key(&"42".repeat(32)).unwrap();
        assert_eq!(key.0, vec![0x42; 32]);
    }

    #[test]
    fn parse_storage_key_rejects_short_hex() {
        let err = parse_storage_key("0x1234").unwrap_err();
        assert!(err.to_string().contains("exactly 32 bytes"));
    }

    #[test]
    fn parse_storage_key_rejects_long_hex() {
        let err = parse_storage_key(&format!("0x{}", "42".repeat(33))).unwrap_err();
        assert!(err.to_string().contains("exactly 32 bytes"));
    }

    #[test]
    fn parse_storage_key_rejects_non_hex() {
        let err = parse_storage_key("0xnot-hex").unwrap_err();
        assert!(err.to_string().contains("invalid storage key"));
    }
}
