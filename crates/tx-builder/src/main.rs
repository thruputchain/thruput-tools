//! Minimal transaction builder for end-to-end testing.
//!
//! Constructs a signed Groundhog transaction and prints the 0x-prefixed
//! hex-encoded XDR to stdout. Adapted from the wallet crate in sdk_rs.
//!
//! Usage:
//!   tx-builder --seed "my secret" --account "alice"
//!
//! Prints to stdout: 0x-prefixed hex XDR of a SignedTransaction
//! Prints to stderr: tx_hash and account address (for use in queries)

use clap::Parser;
use groundhog_signing::{
    build_call_transaction, encode_signed_transaction_xdr, sign_transaction, transaction_hash,
    BuildCallTransaction, LocalSigner, PubkeyAccount, SignTransactionParams, Signer,
};
use groundhog_xdr::contract::ContractEntrypointID;
use sha2::{Digest, Sha256};

/// Build a signed test transaction and print hex-encoded XDR to stdout.
#[derive(Parser)]
#[command(name = "tx-builder")]
struct Cli {
    /// Wallet seed phrase (hashed to derive keys).
    #[arg(long, default_value = "test-wallet-seed")]
    seed: String,

    /// Account name (hashed with seed to derive keypair).
    #[arg(long, default_value = "test-account")]
    account: String,

    /// Block expiry height for the transaction.
    #[arg(long, default_value = "1000000")]
    expiry: i64,

    /// Transaction nonce.
    #[arg(long, default_value = "1")]
    nonce: u64,

    /// Gas limit.
    #[arg(long, default_value = "1000000")]
    gas_limit: u64,
}

/// Derive a local signer from seed + account name.
fn derive_signer(seed: &str, account: &str) -> LocalSigner {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update(b":");
    hasher.update(account.as_bytes());
    let seed_bytes: [u8; 32] = hasher.finalize().into();
    LocalSigner::from_secret_key(seed_bytes)
}

fn main() {
    let cli = Cli::parse();

    let signer = derive_signer(&cli.seed, &cli.account);
    let account = PubkeyAccount::new(signer.public_key(), 0);
    let address = account.address().expect("bug: address should derive");

    let tx = build_call_transaction(BuildCallTransaction {
        invoked_address: address,
        entrypoint: ContractEntrypointID::from([0u8; 8]),
        gas_limit: cli.gas_limit,
        calldata: vec![],
        block_expiry_height: cli.expiry,
        nonce: cli.nonce,
        auth_claims: vec![account.auth_claim()]
            .try_into()
            .expect("bug: test auth claims are within bounds"),
    });

    let tx_hash = transaction_hash(&tx).expect("bug: tx should hash");
    let signed_tx = sign_transaction(tx, &signer, SignTransactionParams::default())
        .expect("bug: tx should sign");
    let stx_bytes =
        encode_signed_transaction_xdr(&signed_tx).expect("bug: signed tx should encode");
    println!("0x{}", hex::encode(&stx_bytes));

    // Print metadata to stderr for the test script
    eprintln!("tx_hash=0x{}", hex::encode(tx_hash.as_ref()));
    eprintln!("address=0x{}", hex::encode(address.as_ref()));
}
