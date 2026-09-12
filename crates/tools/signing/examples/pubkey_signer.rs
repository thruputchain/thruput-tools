//! JSON-lines signer for local demos using the native pubkey-account wallet.
//!
//! Uses the public deterministic dev-1 key. Each request supplies address,
//! entrypoint, calldata, networkHash, gasLimit, expiryHeight, and nonce.
//! Output is {"signedTx":"0x..."} or {"error":"..."}; no RPC submission.

use std::error::Error;
use std::io::{self, BufRead, Write};

use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};
use groundhog_signing::{
    BuildCallTransaction, DEFAULT_DEV_ACCOUNT_SEED, LocalSigner, PubkeyAccount,
    SignTransactionParams, Signer, build_call_transaction, derive_dev_account_signer,
    sign_and_encode_transaction_hex,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::{ContractAddress, NetworkHash, SHA256Hash};
use serde_json::{Value, json};
use xdr_common::BytesM;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn bytes(value: &Value, field: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let value = value[field]
        .as_str()
        .ok_or_else(|| invalid("expected a hex string"))?;
    Ok(hex::decode(value.strip_prefix("0x").unwrap_or(value))?)
}

fn fixed<const N: usize>(value: &Value, field: &str) -> Result<[u8; N], Box<dyn Error>> {
    bytes(value, field)?
        .try_into()
        .map_err(|_| invalid("wrong fixed-width hex length").into())
}

fn sign(value: &Value, signer: &LocalSigner) -> Result<String, Box<dyn Error>> {
    let account = PubkeyAccount::new(signer.public_key(), 0);
    let network = NetworkHash::new(SHA256Hash::new(fixed(value, "networkHash")?));
    let gas_limit = value["gasLimit"]
        .as_u64()
        .filter(|gas| *gas > 0)
        .ok_or_else(|| invalid("gasLimit must be positive"))?;
    // Pubkey authentication requires the root address to be the derived account.
    // Its native DefaultWallet.execute precompile forwards this one WalletCall.
    let calls = vec![(
        ContractAddress::from(fixed::<32>(value, "address")?),
        ContractEntrypointID::from(fixed::<8>(value, "entrypoint")?),
        gas_limit,
        BytesM(bytes(value, "calldata")?),
    )];
    let execute_id = contract_entrypoint_id(
        Some("DefaultWallet"),
        "0.1.0",
        &NormalizedMethodSignature::new("execute"),
    );
    let execute_bytes: [u8; 8] = execute_id.as_ref()[..8].try_into()?;
    let transaction = build_call_transaction(BuildCallTransaction {
        invoked_address: account.address()?,
        entrypoint: ContractEntrypointID::from(execute_bytes),
        calldata: xdrs::cxs::encode(&calls)?,
        gas_limit,
        block_expiry_height: value["expiryHeight"]
            .as_i64()
            .filter(|height| *height >= 0)
            .ok_or_else(|| invalid("expiryHeight must be nonnegative"))?,
        nonce: value["nonce"]
            .as_u64()
            .ok_or_else(|| invalid("nonce must be a uint64"))?,
        auth_claim: account.auth_claim(),
    });
    Ok(sign_and_encode_transaction_hex(
        transaction,
        signer,
        SignTransactionParams::default(),
        &network,
    )?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let signer = derive_dev_account_signer(DEFAULT_DEV_ACCOUNT_SEED, 1);
    let mut output = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let line = line?;
        let result = serde_json::from_str::<Value>(&line)
            .map_err(|error| -> Box<dyn Error> { error.into() })
            .and_then(|value| sign(&value, &signer));
        let response = match result {
            Ok(encoded) => json!({ "signedTx": encoded }),
            Err(error) => json!({ "error": error.to_string() }),
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
    Ok(())
}
