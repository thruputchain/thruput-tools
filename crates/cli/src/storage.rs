use groundhog_json_rpc_types::hex::prefixed_hex;
use std::path::Path;

use groundhog_json_rpc_client::GroundhogApiClient;
use groundhog_json_rpc_types::ContractAddress;
use groundhog_json_rpc_types::hex::{FixedHexError, HexBytes, decode_fixed_hex};
use groundhog_json_rpc_types::state::StorageItemRead;
use serde_json::json;

use crate::account_resolution::resolve_contract_target;
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::result::CommandResult;
use crate::{CliError, Print};

/// Schema version of the `storage --json` data payload.
pub const STORAGE_JSON_SCHEMA_VERSION: u32 = 1;

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
    let rpc_url = resolved_profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?;

    let address = ContractAddress::from(resolve_contract_target(&resolved_profile, address)?);
    let key = parse_storage_key(key)?;
    let key_hex = prefixed_hex(&key);
    let address_hex = prefixed_hex(address.0);

    let task = print.start_task("Reading storage");

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let read = rt.block_on(fetch_storage_at(rpc_url, address, key))?;

    task.success("Storage read complete");
    print_storage_read(&address_hex, &key_hex, &read, print)?;
    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": STORAGE_JSON_SCHEMA_VERSION,
            "rpcUrl": rpc_url,
            "address": address_hex,
            "key": key_hex,
            "blockHeight": read.block_height,
            "item": read.item,
        }),
    ))?;

    Ok(())
}

async fn fetch_storage_at(
    rpc_url: &str,
    address: ContractAddress,
    key: HexBytes,
) -> Result<StorageItemRead, CliError> {
    let client = crate::http_client::rpc_client(rpc_url)?;
    client
        .get_storage_at(address, key)
        .await
        .map_err(|e| CliError::RpcMethodFailed {
            method: "getStorageAt",
            message: e.to_string(),
        })
}

fn print_storage_read(
    address: &str,
    key: &str,
    read: &StorageItemRead,
    print: &Print,
) -> Result<(), CliError> {
    print.detail("address", address);
    print.detail("key", key);
    print.detail("height", *read.block_height);
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
