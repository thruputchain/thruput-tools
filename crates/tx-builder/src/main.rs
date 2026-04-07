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
use ed25519_compact::{KeyPair, Seed as EdSeed};
use groundhog_xdr::auth_claim::*;
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::transaction::*;
use groundhog_xdr::types::*;
use sha2::{Digest, Sha256};
use xdr_common::{BytesM, VecM};

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

fn pubkey_account_tag() -> SHA256Hash {
    let bytes: [u8; 32] = Sha256::digest(b"PUBKEY_ACCOUNT_TAG").into();
    SHA256Hash::from(bytes)
}

/// Derive a keypair from seed + account name.
fn derive_keypair(seed: &str, account: &str) -> KeyPair {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update(b":");
    hasher.update(account.as_bytes());
    let seed_bytes: [u8; 32] = hasher.finalize().into();
    let ed_seed = EdSeed::new(seed_bytes);
    KeyPair::from_seed(ed_seed)
}

/// Derive the contract address for a pubkey account.
fn derive_address(pubkey: &Ed25519PublicKey, nonce: u64) -> ContractAddress {
    let preimage = PubkeyAccountPreimage {
        pubkey: PublicKey::Ed25519 { ed25519: *pubkey },
        nonce,
    };
    let preimage_bytes = xdrs::txs::encode(&preimage).expect("bug: encode preimage");
    let mut digest: [u8; 32] = Sha256::digest(&preimage_bytes).into();

    // XOR with the pubkey account tag
    let tag = pubkey_account_tag();
    for (d, t) in digest.iter_mut().zip(tag.as_ref().iter()) {
        *d ^= t;
    }

    ContractAddress::from(digest)
}

fn main() {
    let cli = Cli::parse();

    // Derive keypair
    let kp = derive_keypair(&cli.seed, &cli.account);
    let pk_bytes: [u8; 32] = kp
        .pk
        .as_ref()
        .try_into()
        .expect("bug: pubkey must be 32 bytes");
    let pubkey = Ed25519PublicKey::from(pk_bytes);
    let address = derive_address(&pubkey, 0);

    // Build the invocation — a no-op call to the account's own address.
    let invocation = Invocation {
        invoked_address: address,
        entrypoint: ContractEntrypointID::from([0u8; 8]),
        gas_limit: cli.gas_limit,
        calldata: BytesM(vec![]),
    };

    let uniquifier = Uniquifier {
        block_expiry_height: BlockHeight::from(cli.expiry),
        nonce: cli.nonce,
    };

    let auth_claim = AuthClaim::kPubkeySignature {
        pubkey_preimage: PubkeyAccountPreimage {
            pubkey: PublicKey::Ed25519 { ed25519: pubkey },
            nonce: 0,
        },
    };

    let tx = Transaction {
        invocation,
        uniquifier,
        contracts_to_deploy: VecM(vec![]),
        auth_claims: VecM(vec![auth_claim]),
        gas_claims: VecM(vec![]),
    };

    // Sign the transaction
    let tx_bytes = xdrs::txs::encode(&tx).expect("bug: encode tx");
    let sig = kp.sk.sign(&tx_bytes, None);
    let sig_bytes: [u8; 64] = sig.as_ref().try_into().expect("bug: sig must be 64 bytes");

    let decorated_sig = DecoratedSignature {
        auth_claim_idx: 0,
        public_key_id: 0,
        signature: Signature::Ed25519 {
            ed25519: Ed25519Signature::from(sig_bytes),
        },
    };

    let stx = SignedTransaction {
        tx,
        signatures: VecM(vec![decorated_sig]),
    };

    // Encode and print
    let stx_bytes = xdrs::txs::encode(&stx).expect("bug: encode signed tx");
    println!("0x{}", hex::encode(&stx_bytes));

    // Print metadata to stderr for the test script
    let tx_hash: [u8; 32] = Sha256::digest(&tx_bytes).into();
    eprintln!("tx_hash=0x{}", hex::encode(tx_hash));
    eprintln!("address=0x{}", hex::encode(address.as_ref()));
}
