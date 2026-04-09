use std::path::Path;

use groundhog_rpc_api_types::transaction::TxHash;
use groundhog_signing::{
    BuildDeployTransaction, SignTransactionParams, Signer, build_deploy_transaction,
    sign_transaction,
};
use serde_json::json;
use xdr_common::VecM;

use crate::config::{resolve_default_key, resolve_profile};
use crate::keys::load_local_signer;
use crate::result::CommandResult;
use crate::{CliError, Print};

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

    let rpc_url = resolved
        .rpc_url
        .as_deref()
        .ok_or_else(|| CliError::Other("no rpc_url in profile".into()))?;

    // Resolve signing key
    let key_name = resolve_default_key(&cwd, Some(profile))?.ok_or_else(|| {
        CliError::Other(
            "no signing key configured — set default_key in \
             groundhog.toml or run `groundhog keys generate`"
                .into(),
        )
    })?;
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

    // Build the deploy transaction
    let pubkey = signer.public_key();
    let account = groundhog_signing::PubkeyAccount::new(
        pubkey,
        groundhog_signing::DEFAULT_PUBKEY_ACCOUNT_NONCE,
    );
    let address = account
        .address()
        .map_err(|e| CliError::Other(e.to_string()))?;

    let contract = groundhog_xdr::contract::Contract::kWasmi {
        wasmi_contract: groundhog_xdr::contract::ContractBlob {
            wasm: xdr_common::BytesM(wasm_bytes),
        },
    };

    let auth_claims = vec![account.auth_claim()]
        .try_into()
        .map_err(|e: xdr_common::Error| CliError::Other(e.to_string()))?;

    let tx = build_deploy_transaction(BuildDeployTransaction {
        invoked_address: address,
        entrypoint: groundhog_xdr::contract::ContractEntrypointID::from([0u8; 8]),
        calldata: vec![],
        gas_limit: 10_000_000,
        block_expiry_height: 1_000_000,
        nonce: nonce_from_timestamp(),
        contracts_to_deploy: VecM(vec![contract]),
        auth_claims,
    });

    let signed_tx = sign_transaction(tx, &signer, SignTransactionParams::default())
        .map_err(|e| CliError::Other(e.to_string()))?;
    let tx_xdr = groundhog_signing::encode_signed_transaction_xdr(&signed_tx)
        .map_err(|e| CliError::Other(e.to_string()))?;
    let tx_hex = format!("0x{}", hex::encode(&tx_xdr));

    // Submit via JSON-RPC
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::Other(e.to_string()))?;
    let (tx_hash, receipt) = rt.block_on(async { submit_and_poll(rpc_url, &tx_hex).await })?;

    // Extract the deployed contract address from the receipt.
    let contract_addr_hex = match &receipt.status {
        groundhog_rpc_api_types::receipt::ExecStatus::Ok => {
            let return_data = receipt.return_data.as_ref().ok_or_else(|| {
                CliError::Other(
                    "deploy succeeded but receipt has no return_data \
                     (cannot determine contract address)"
                        .into(),
                )
            })?;
            format!("0x{}", hex::encode(&return_data.0))
        }
        _ => String::new(),
    };

    // Print result
    match &receipt.status {
        groundhog_rpc_api_types::receipt::ExecStatus::Ok => {
            print.status(format_args!("Deployed at {contract_addr_hex}"));
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
            "height": receipt.height,
            "status": receipt.status,
        }),
    ))?;

    Ok(())
}

async fn submit_and_poll(
    rpc_url: &str,
    tx_hex: &str,
) -> Result<(TxHash, groundhog_rpc_api_types::receipt::TransactionReceipt), CliError> {
    use groundhog_rpc_client::{GroundhogApiClient, HttpClientBuilder};

    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|e| CliError::RpcConnect(format!("{rpc_url}: {e}")))?;

    let tx_hash = client
        .send_raw_transaction(tx_hex.to_string())
        .await
        .map_err(|e| CliError::Other(format!("sendRawTransaction failed: {e}")))?;

    let max_attempts = 30;
    let poll_interval = std::time::Duration::from_secs(1);
    for attempt in 0..max_attempts {
        match client.get_transaction_receipt(tx_hash).await {
            Ok(Some(receipt)) => return Ok((tx_hash, receipt)),
            Ok(None) if attempt < max_attempts - 1 => {
                tokio::time::sleep(poll_interval).await;
            }
            Ok(None) => {
                return Err(CliError::Other(format!(
                    "transaction {} submitted but no receipt after {max_attempts}s — \
                     check the node logs",
                    format_tx_hash(&tx_hash),
                )));
            }
            Err(e) => {
                return Err(CliError::Other(format!(
                    "getTransactionReceipt failed: {e}"
                )));
            }
        }
    }
    unreachable!()
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

fn nonce_from_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(1)
}

fn format_tx_hash(tx_hash: &TxHash) -> String {
    serde_json::to_string(tx_hash)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}
