//! Public bootstrap allocation for the account used by `submit_traffic`.
use thruput_json_rpc_types::{base58::Base58, bootstrap_manifest::BootstrapManifestAllocation};
use thruput_signing::{PubkeyAccount, derive_dev_account_signer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 3 {
        return Err("usage: fixture_account [FIXTURE_SEED] [INITIAL_STORAGE_TOKENS]".into());
    }
    let seed = args.get(1).map_or("fixture", String::as_str);
    if seed.is_empty() {
        return Err("fixture seed must be nonempty".into());
    }
    // Raw native token units, as consumed by the CLI's bootstrap allocations.
    // A quadrillion leaves ample headroom for repeated 100-call captures at
    // 20,000,000 gas per call (2,000,000,000 gas per batch).
    let amount = args.get(2).map_or("1000000000000000", String::as_str);
    let parsed: i64 = amount.parse()?;
    if parsed <= 0 || parsed.to_string() != amount {
        return Err("initialStorageTokens must be a positive canonical decimal amount".into());
    }
    let signer = derive_dev_account_signer(&format!("thruput-firehose:{seed}"), 0);
    let public_key = signer.verifying_key_bytes();
    let wallet = PubkeyAccount::from_public_key_bytes(public_key, 0).address();
    let allocation = BootstrapManifestAllocation::new(Base58(public_key.into()), amount);
    println!("{}", serde_json::to_string(&[allocation])?);
    eprintln!("wallet address: {}", Base58(wallet));
    Ok(())
}
