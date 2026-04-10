use std::path::Path;

use groundhog_rpc_api_types::hex::HexBytes;
use groundhog_signing::{
    BuildDeployTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams, Signer,
    WALLET_DEPLOY_ENTRYPOINT, build_deploy_transaction, derive_deploy_address, sign_transaction,
};
use groundhog_xdr::auth_claim::AuthClaim;
use groundhog_xdr::types::ContractAddress;
use groundhog_xdr::types::SHA256Hash;
use serde_json::json;
use sha2::{Digest, Sha256};
use xdr_common::VecM;

use crate::config::{resolve_default_key, resolve_profile};
use crate::keys::load_local_signer;
use crate::result::CommandResult;
use crate::tx_utils::{format_tx_hash, nonce_from_timestamp, submit_and_poll};
use crate::{CliError, Print};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployAddressSource {
    Receipt,
    Derived,
}

/// Deploy a contract to the network.
#[allow(clippy::too_many_lines)]
pub fn run(
    wasm_path: &Option<String>,
    profile: &str,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let resolved = resolve_profile(&cwd, Some(profile))?;

    let rpc_url = resolved.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;

    // Resolve signing key
    let key_name = resolve_default_key(&cwd, Some(profile))?.ok_or(CliError::MissingSigningKey)?;
    let signer = load_local_signer(&key_name)?;

    // Find WASM binary
    let wasm_bytes = match wasm_path {
        Some(path) => std::fs::read(path)?,
        None => find_and_read_wasm(&cwd)?,
    };

    print.status(format_args!(
        "Deploying contract ({} bytes) to {rpc_url}...",
        wasm_bytes.len()
    ));

    // Build the deploy transaction — invokes the sender's wallet contract.
    let pubkey = signer.public_key();
    let account = groundhog_signing::PubkeyAccount::new(
        pubkey,
        groundhog_signing::DEFAULT_PUBKEY_ACCOUNT_NONCE,
    );
    let wallet_address = account
        .address()
        .map_err(|e| CliError::Other(e.to_string()))?;

    let contract = groundhog_xdr::contract::Contract::kWasmi {
        wasmi_contract: groundhog_xdr::contract::ContractBlob {
            wasm: xdr_common::BytesM(wasm_bytes),
        },
    };

    // Compute the contract hash: sha256(xdr_encode(contract)).
    let contract_xdr = xdrs::txs::encode(&contract)
        .map_err(|e| CliError::Other(format!("failed to XDR-encode contract: {e}")))?;
    let contract_hash_bytes: [u8; 32] = Sha256::digest(&contract_xdr).into();
    let contract_hash = SHA256Hash::from(contract_hash_bytes);

    // Create a deploy nonce for address derivation.
    let deploy_nonce_val = nonce_from_timestamp();
    let deploy_nonce_bytes: [u8; 32] = Sha256::digest(deploy_nonce_val.to_le_bytes()).into();
    let deploy_nonce = SHA256Hash::from(deploy_nonce_bytes);
    let predicted_address = derive_deploy_address(&wallet_address, &deploy_nonce, &contract_hash)
        .map_err(|e| CliError::Other(e.to_string()))?;

    // Calldata = CXS-encoded (nonce, contract_hash) — matches wallet deploy signature.
    // Must use CXS encoding (not txs) because the #[contract] macro
    // deserializes calldata with xdrs::cxs::decode.
    let calldata = xdrs::cxs::encode(&(&deploy_nonce, &contract_hash))
        .map_err(|e| CliError::Other(format!("failed to encode deploy calldata: {e}")))?;

    // Auth claim: kInvokedAddressSignature at key slot 0.
    let auth_claims: groundhog_xdr::auth_claim::AuthClaimData =
        vec![AuthClaim::kInvokedAddressSignature {
            invoked_address_signature_id: 0,
        }]
        .try_into()
        .map_err(|e: xdr_common::Error| CliError::Other(e.to_string()))?;

    let entrypoint = groundhog_xdr::contract::ContractEntrypointID::from(WALLET_DEPLOY_ENTRYPOINT);

    let tx = build_deploy_transaction(BuildDeployTransaction {
        invoked_address: wallet_address,
        entrypoint,
        calldata,
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height: 1_000_000,
        nonce: deploy_nonce_val,
        contracts_to_deploy: VecM(vec![contract]),
        auth_claims,
    });

    let signed_tx = sign_transaction(tx, &signer, SignTransactionParams::default())
        .map_err(|e| CliError::Other(e.to_string()))?;
    let tx_xdr = groundhog_signing::encode_signed_transaction_xdr(&signed_tx)
        .map_err(|e| CliError::Other(e.to_string()))?;
    let tx_hex = format!("0x{}", hex::encode(&tx_xdr));

    // Submit via JSON-RPC
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let (tx_hash, receipt) = rt.block_on(async { submit_and_poll(rpc_url, &tx_hex).await })?;

    // Extract the deployed contract address from the receipt.
    let (contract_addr_hex, address_source): (String, DeployAddressSource) = match &receipt.status {
        groundhog_rpc_api_types::receipt::ExecStatus::Ok => {
            resolve_deployed_address(&receipt.return_data, &predicted_address)?
        }
        _ => (String::new(), DeployAddressSource::Derived),
    };

    // Print result
    match &receipt.status {
        groundhog_rpc_api_types::receipt::ExecStatus::Ok => {
            print.status(format_args!("Deployed at {contract_addr_hex}"));
            if matches!(address_source, DeployAddressSource::Derived) {
                print.warn(
                    "receipt omitted deploy return_data; address was derived locally from \
                     deployer, nonce, and contract hash",
                );
            }
            print.status(format_args!("  tx: {}", format_tx_hash(&tx_hash)));
            print.status(format_args!("  height: {}", *receipt.height));
        }
        other => {
            print.warn(format_args!("Deploy failed: {other:?}"));
            print.status(format_args!("  tx: {}", format_tx_hash(&tx_hash)));
        }
    }

    // Save contract alias
    if matches!(
        &receipt.status,
        groundhog_rpc_api_types::receipt::ExecStatus::Ok
    ) {
        let project_root = crate::config::find_project_config(&cwd)
            .and_then(|p| p.parent().map(Path::to_path_buf));
        if let Some(root) = project_root
            && let Some(name) = root.file_name().and_then(|n| n.to_str())
        {
            let _ = crate::config::save_profile_contract_alias(
                &root,
                Some(profile),
                name,
                &contract_addr_hex,
            );
            print.status(format_args!(
                "  alias: {name} -> {contract_addr_hex} (saved to groundhog.toml)"
            ));
        }
    }

    print.json(&CommandResult::new(
        cmd,
        json!({
            "tx_hash": format_tx_hash(&tx_hash),
            "address": contract_addr_hex,
            "address_source": match address_source {
                DeployAddressSource::Receipt => "receipt",
                DeployAddressSource::Derived => "derived",
            },
            "height": receipt.height,
            "status": receipt.status,
        }),
    ))?;

    Ok(())
}

fn find_and_read_wasm(cwd: &Path) -> Result<Vec<u8>, CliError> {
    let wasm_dir = cwd.join("target/wasm32-unknown-unknown/release");
    if !wasm_dir.exists() {
        return Err(CliError::Other(
            "no WASM build output found — run `groundhog build` first".into(),
        ));
    }
    let mut wasm_files: Vec<_> = std::fs::read_dir(&wasm_dir)?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "wasm"))
        .collect();

    if wasm_files.is_empty() {
        return Err(CliError::Other(
            "no .wasm files in target/wasm32-unknown-unknown/release/ \
             — run `groundhog build` first"
                .into(),
        ));
    }

    wasm_files.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    let wasm_path = wasm_files
        .last()
        .expect("bug: wasm_files is non-empty")
        .path();
    std::fs::read(&wasm_path).map_err(CliError::from)
}

fn resolve_deployed_address(
    return_data: &Option<HexBytes>,
    predicted_address: &ContractAddress,
) -> Result<(String, DeployAddressSource), CliError> {
    if let Some(return_data) = return_data {
        if return_data.0.len() == 32 {
            return Ok((
                format!("0x{}", hex::encode(&return_data.0)),
                DeployAddressSource::Receipt,
            ));
        }
        return Err(CliError::Other(format!(
            "deploy receipt return_data had {} bytes, expected 32-byte contract address",
            return_data.0.len()
        )));
    }

    Ok((
        format!("0x{}", hex::encode(predicted_address.as_ref())),
        DeployAddressSource::Derived,
    ))
}

#[cfg(test)]
mod tests {
    use super::{DeployAddressSource, derive_deploy_address, resolve_deployed_address};
    use groundhog_rpc_api_types::hex::HexBytes;
    use groundhog_xdr::types::{ContractAddress, SHA256Hash};

    #[test]
    fn deploy_address_resolution_falls_back_to_prediction() {
        let predicted = ContractAddress::from([0x11; 32]);
        let resolved = resolve_deployed_address(&None, &predicted)
            .expect("bug: predicted address fallback should succeed");
        assert_eq!(resolved.0, format!("0x{}", "11".repeat(32)));
        assert_eq!(resolved.1, DeployAddressSource::Derived);
    }

    #[test]
    fn deploy_address_resolution_prefers_receipt_return_data() {
        let predicted = ContractAddress::from([0x11; 32]);
        let resolved = resolve_deployed_address(&Some(HexBytes(vec![0x22; 32])), &predicted)
            .expect("bug: receipt return_data should be accepted");
        assert_eq!(resolved.0, format!("0x{}", "22".repeat(32)));
        assert_eq!(resolved.1, DeployAddressSource::Receipt);
    }

    #[test]
    fn deploy_address_derivation_is_deterministic() {
        let deployer = ContractAddress::from([0xAB; 32]);
        let nonce = SHA256Hash::from([0xCD; 32]);
        let contract_hash = SHA256Hash::from([0xEF; 32]);

        let first = derive_deploy_address(&deployer, &nonce, &contract_hash)
            .expect("bug: derivation should succeed");
        let second = derive_deploy_address(&deployer, &nonce, &contract_hash)
            .expect("bug: derivation should succeed");

        assert_eq!(first, second);
        assert_ne!(first, deployer);
    }
}
