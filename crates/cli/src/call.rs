use groundhog_rpc_api_types::receipt::ExecStatus;
use groundhog_rpc_api_types::transaction::TxHash;
use groundhog_signing::{
    BuildCallTransaction, SignTransactionParams, Signer, build_call_transaction, sign_transaction,
};
use groundhog_xdr::auth_claim::AuthClaim;
use serde_json::json;

use crate::config::{resolve_default_key, resolve_profile};
use crate::keys::load_local_signer;
use crate::result::CommandResult;
use crate::{CliError, Print};

/// Wallet `do_action` entrypoint ID (from dev-wallet WASM exports).
const WALLET_DO_ACTION_ENTRYPOINT: [u8; 8] = 0x1984_6D75_0FAD_81D6_u64.to_be_bytes();

/// Call a method on a deployed contract.
#[allow(clippy::too_many_lines)]
pub fn run(
    contract: &str,
    method: &str,
    args: &[String],
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

    // Resolve contract address (alias or literal)
    let contract_address_hex = resolved.resolve_contract(contract);
    let contract_address = parse_contract_address(&contract_address_hex)?;

    // Resolve signing key
    let key_name = resolve_default_key(&cwd, Some(profile))?.ok_or_else(|| {
        CliError::Other(
            "no signing key configured — set default_key in \
             groundhog.toml or run `groundhog keys generate`"
                .into(),
        )
    })?;
    let signer = load_local_signer(&key_name)?;

    // Build entrypoint ID from method name
    let entrypoint = method_name_to_entrypoint(method);

    // Encode calldata
    let calldata = encode_calldata(args)?;

    print.status(format_args!(
        "Calling {method} on {contract_address_hex} via {rpc_url}..."
    ));

    // Build and sign — invokes the sender's wallet contract with do_action.
    let pubkey = signer.public_key();
    let account = groundhog_signing::PubkeyAccount::new(
        pubkey,
        groundhog_signing::DEFAULT_PUBKEY_ACCOUNT_NONCE,
    );
    let wallet_address = account
        .address()
        .map_err(|e| CliError::Other(e.to_string()))?;

    // Encode the inner invocation (TXS format, matching wallet's txs::decode):
    // (contract_address, method_entrypoint, gas_limit, raw_calldata)
    let inner_gas_limit: u64 = 10_000_000;
    let inner_invocation = xdrs::txs::encode(&(
        contract_address,
        entrypoint,
        inner_gas_limit,
        calldata.as_slice(),
    ))
    .map_err(|e| CliError::Other(format!("failed to encode inner invocation: {e}")))?;

    // Outer calldata is CXS-encoded — the #[contract] macro deserializes
    // do_action's &[u8] parameter with xdrs::cxs::decode.
    let do_action_calldata = xdrs::cxs::encode(&inner_invocation.as_slice())
        .map_err(|e| CliError::Other(format!("failed to CXS-encode do_action calldata: {e}")))?;

    // Auth claim: kInvokedAddressSignature at key slot 0.
    let auth_claims: groundhog_xdr::auth_claim::AuthClaimData =
        vec![AuthClaim::kInvokedAddressSignature {
            invoked_address_signature_id: 0,
        }]
        .try_into()
        .map_err(|e: xdr_common::Error| CliError::Other(e.to_string()))?;

    let wallet_entrypoint =
        groundhog_xdr::contract::ContractEntrypointID::from(WALLET_DO_ACTION_ENTRYPOINT);

    let tx = build_call_transaction(BuildCallTransaction {
        invoked_address: wallet_address,
        entrypoint: wallet_entrypoint,
        calldata: do_action_calldata,
        gas_limit: 10_000_000,
        block_expiry_height: 1_000_000,
        nonce: nonce_from_timestamp(),
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

    // Print result
    match &receipt.status {
        ExecStatus::Ok => {
            print.status(format_args!("Call succeeded at height {}", *receipt.height));
            if let Some(ref data) = receipt.return_data {
                print.status(format_args!("  return: 0x{}", hex::encode(&data.0)));
            }
        }
        ExecStatus::Reverted { message } => {
            let msg = message.as_deref().unwrap_or("(no message)");
            print.warn(format_args!("Call reverted: {msg}"));
        }
        other => {
            print.warn(format_args!("Call failed: {other:?}"));
        }
    }

    print.json(&CommandResult::new(
        cmd,
        json!({
            "tx_hash": format_tx_hash(&tx_hash),
            "contract": contract_address_hex,
            "method": method,
            "height": receipt.height,
            "status": receipt.status,
            "return_data": receipt.return_data,
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

    // Poll for receipt — same pattern as deploy
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

fn parse_contract_address(
    hex_str: &str,
) -> Result<groundhog_xdr::types::ContractAddress, CliError> {
    let stripped = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes = hex::decode(stripped)
        .map_err(|e| CliError::Other(format!("invalid contract address hex: {e}")))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| CliError::Other("contract address must be 32 bytes".into()))?;
    Ok(groundhog_xdr::types::ContractAddress::from(arr))
}

fn method_name_to_entrypoint(method: &str) -> groundhog_xdr::contract::ContractEntrypointID {
    // Hash the method name to produce an 8-byte entrypoint ID.
    // This must match the SDK's entrypoint derivation.
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(method.as_bytes());
    let mut id = [0u8; 8];
    id.copy_from_slice(&hash[..8]);
    groundhog_xdr::contract::ContractEntrypointID::from(id)
}

fn encode_calldata(args: &[String]) -> Result<Vec<u8>, CliError> {
    if args.is_empty() {
        return Ok(vec![]);
    }
    // For v1, accept a single hex-encoded calldata argument.
    if args.len() == 1 {
        let stripped = args[0].strip_prefix("0x").unwrap_or(&args[0]);
        return hex::decode(stripped)
            .map_err(|e| CliError::Other(format!("invalid calldata hex: {e}")));
    }
    Err(CliError::Other(
        "multiple call arguments require contract spec support \
         (not yet implemented) — pass a single hex-encoded calldata \
         argument for now"
            .into(),
    ))
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
