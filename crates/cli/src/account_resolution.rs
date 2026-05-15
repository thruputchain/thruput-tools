use groundhog_rpc_api_types::{ContractAddress, dev::DevAccount};
use groundhog_rpc_client::{GroundhogDevApiClient, HttpClientBuilder};
use groundhog_signing::{DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount, Signer};
use jsonrpsee::core::ClientError;
use jsonrpsee::types::error::METHOD_NOT_FOUND_CODE;

use crate::{CliError, dev_keys};

/// Resolve the wallet address used as a transaction's invoked address.
///
/// Stored keys keep the legacy pubkey-derived account path. Dev keys (`dev-N`)
/// must use wallet metadata advertised by the local dev backend and must not
/// fall back to local address derivation when that metadata is unavailable.
pub(crate) fn resolve_wallet_address(
    rpc_url: &str,
    key_name: &str,
    signer: &LocalSigner,
    rt: &tokio::runtime::Runtime,
) -> Result<groundhog_xdr::types::ContractAddress, CliError> {
    if let Some(index) = dev_keys::dev_key_index(key_name) {
        rt.block_on(async { resolve_dev_wallet_address(rpc_url, key_name, index, signer).await })
    } else {
        let account = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
        account
            .address()
            .map_err(|error| CliError::WalletAddressDerivationFailed {
                key_name: key_name.to_owned(),
                message: error.to_string(),
            })
    }
}

async fn resolve_dev_wallet_address(
    rpc_url: &str,
    key_name: &str,
    index: usize,
    signer: &LocalSigner,
) -> Result<groundhog_xdr::types::ContractAddress, CliError> {
    let account = lookup_dev_account(rpc_url, key_name, index).await?;

    let expected_public_key = signer.verifying_key_bytes();
    if account.public_key.0.as_slice() != expected_public_key.as_slice() {
        return Err(dev_account_error(
            key_name,
            format!(
                "groundhog_dev_getAccounts returned public key {} for dev-{index}, expected 0x{}",
                format_hex(&account.public_key.0),
                hex::encode(expected_public_key),
            ),
        ));
    }

    Ok(account.address.0)
}

/// Look up a local dev account address through `groundhog_dev_getAccounts`.
pub(crate) async fn lookup_dev_account_address(
    rpc_url: &str,
    key_name: &str,
    index: usize,
) -> Result<ContractAddress, CliError> {
    Ok(lookup_dev_account(rpc_url, key_name, index).await?.address)
}

async fn lookup_dev_account(
    rpc_url: &str,
    key_name: &str,
    index: usize,
) -> Result<DevAccount, CliError> {
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|error| {
            dev_account_error(key_name, format!("could not connect to {rpc_url}: {error}"))
        })?;

    let accounts = match client.get_accounts().await {
        Ok(accounts) => accounts,
        Err(error) if is_method_not_found(&error) => {
            return Err(dev_account_error(
                key_name,
                "RPC endpoint does not expose groundhog_dev_getAccounts",
            ));
        }
        Err(error) => {
            return Err(dev_account_error(
                key_name,
                format!("groundhog_dev_getAccounts failed: {error}"),
            ));
        }
    };

    accounts.get(index).cloned().ok_or_else(|| {
        dev_account_error(
            key_name,
            format!(
                "groundhog_dev_getAccounts returned {} accounts, missing dev-{index}",
                accounts.len()
            ),
        )
    })
}

fn dev_account_error(key_name: &str, message: impl Into<String>) -> CliError {
    CliError::LocalDevAccountUnavailable {
        key_name: key_name.to_string(),
        message: message.into(),
    }
}

fn is_method_not_found(error: &ClientError) -> bool {
    matches!(error, ClientError::Call(object) if object.code() == METHOD_NOT_FOUND_CODE)
}

fn format_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_dev_key_uses_pubkey_derived_wallet_address() {
        let signer = groundhog_signing::derive_default_dev_account_signer(0);
        let rt = tokio::runtime::Runtime::new().expect("bug: runtime should start");
        let resolved = resolve_wallet_address("http://127.0.0.1:1", "alice", &signer, &rt)
            .expect("bug: non-dev key should not call RPC");
        let expected = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE)
            .address()
            .expect("bug: pubkey account address should derive");

        assert_eq!(resolved, expected);
    }
}
