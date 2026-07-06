use std::path::Path;

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_types::ContractAddress;
use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::state::StorageItemRead;
use serde_json::json;

use crate::codec::parse_contract_address;
use crate::config::{ProfileOverrides, resolve_default_key, resolve_profile_with_overrides};
use crate::result::CommandResult;
use crate::runtime_manifest::apply_local_node_state_for_dev_key;
use crate::{CliError, Print};

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
    let mut resolved_profile = resolve_profile_with_overrides(project_start, profile, overrides)?;
    if let Some(key_name) = resolve_default_key(project_start, profile)? {
        apply_local_node_state_for_dev_key(&mut resolved_profile, &key_name);
    }
    let rpc_url = resolved_profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?;

    let resolved_address = resolved_profile.resolve_contract(address);
    let address = ContractAddress::from(parse_contract_address(&resolved_address)?);
    let key = parse_storage_key(key)?;
    let key_hex = format_storage_key(&key);
    let address_hex = format_contract_address(&address);

    let task = print.start_task("Reading storage");

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let read = rt.block_on(fetch_storage_at(rpc_url, address, key))?;

    task.success("Storage read complete");
    print_storage_read(&address_hex, &key_hex, &read, print)?;
    print.json(&CommandResult::new(
        cmd,
        json!({
            "rpc_url": rpc_url,
            "address": address_hex,
            "key": key_hex,
            "block_height": read.block_height,
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
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|e| CliError::RpcConnect(format!("{rpc_url}: {e}")))?;
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
    let stripped = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    let bytes = hex::decode(stripped).map_err(|e| CliError::InvalidStorageKey(format!("{e}")))?;
    if bytes.len() != 32 {
        return Err(CliError::InvalidStorageKey(format!(
            "storage key must be exactly 32 bytes / 64 hex characters; got {} bytes",
            bytes.len()
        )));
    }
    Ok(HexBytes(bytes))
}

fn format_contract_address(address: &ContractAddress) -> String {
    format!("0x{}", hex::encode(address.0.as_ref()))
}

fn format_storage_key(key: &HexBytes) -> String {
    format!("0x{}", hex::encode(&key.0))
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
