//! Print the public identities `thruput-bench --account-seed <seed>` signs with.
//!
//! usage: fixture_accounts <seed> <count>
//!
//! Output is one JSON array of runtime-manifest bootstrap account entries:
//! `{index, role: "local_user", publicKey, walletAddress, initialStorageTokens: "0"}`
//! (base58). Replace a hosted runtime manifest's `bootstrapAccounts` with it and
//! pass that file as `--runtime-manifest` together with the same
//! `--account-seed` and `--accounts <count>`. The entries carry no allocation:
//! fund each `walletAddress` (for example with `faucet_requestAirdrop`) before
//! benchmarking. These are public test keys for disposable chains.
use thruput_json_rpc_types::base58::Base58;
use thruput_signing::{PubkeyAccount, derive_dev_account_signer};

/// Matches the bound `thruput-bench` applies to `--account-seed` fixtures.
const MAX_ACCOUNTS: usize = 1000;

#[derive(Debug, thiserror::Error)]
enum FixtureError {
    #[error("usage: fixture_accounts <seed> <count>")]
    Usage,
    #[error("seed must be nonempty")]
    EmptySeed,
    #[error("count must be an integer from 1 to {MAX_ACCOUNTS}, got {0:?}")]
    InvalidCount(String),
}

fn run(mut args: impl Iterator<Item = String>) -> Result<serde_json::Value, FixtureError> {
    let (Some(seed), Some(count), None) = (args.next(), args.next(), args.next()) else {
        return Err(FixtureError::Usage);
    };
    if seed.is_empty() {
        return Err(FixtureError::EmptySeed);
    }
    let count = match count.parse::<usize>() {
        Ok(n) if (1..=MAX_ACCOUNTS).contains(&n) => n,
        _ => return Err(FixtureError::InvalidCount(count)),
    };
    let accounts = (0..count)
        .map(|index| {
            let signer = derive_dev_account_signer(&format!("thruput-bench:{seed}"), index);
            let key = signer.verifying_key_bytes();
            let address = PubkeyAccount::from_public_key_bytes(key, 0).address();
            serde_json::json!({
                "index": index,
                "role": "local_user",
                "publicKey": Base58(thruput_xdr::types::Ed25519PublicKey::from(key)),
                "walletAddress": Base58(address),
                "initialStorageTokens": "0",
            })
        })
        .collect();
    Ok(serde_json::Value::Array(accounts))
}

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(accounts) => {
            println!("{accounts}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::from(2)
        }
    }
}
