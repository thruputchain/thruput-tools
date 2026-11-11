//! Print the public identities `thruput-bench --account-seed <seed>` signs with.
//!
//! usage: fixture_accounts <seed> <count>
//! Output: one JSON array of `{index, publicKey, walletAddress}` (base58), for a
//! runtime manifest's `local_user` bootstrap accounts.
use thruput_json_rpc_types::base58::Base58;
use thruput_signing::{PubkeyAccount, derive_dev_account_signer};

fn main() {
    let mut args = std::env::args().skip(1);
    let seed = args.next().expect("seed");
    let count: usize = args.next().expect("count").parse().expect("count");
    let accounts: Vec<_> = (0..count)
        .map(|index| {
            let signer = derive_dev_account_signer(&format!("thruput-bench:{seed}"), index);
            let key = signer.verifying_key_bytes();
            let address = PubkeyAccount::from_public_key_bytes(key, 0).address();
            serde_json::json!({
                "index": index,
                "publicKey": Base58(thruput_xdr::types::Ed25519PublicKey::from(key)),
                "walletAddress": Base58(address),
            })
        })
        .collect();
    println!("{}", serde_json::Value::Array(accounts));
}
