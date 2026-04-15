//! Groundhog JSON-RPC benchmark harness.
//!
//! Pre-generates signed transactions, then fires them at a configurable rate
//! (open-loop) against a running JSON-RPC server. Reports latency percentiles
//! and throughput.
//!
//! Usage:
//!   # Against dev-node (smoke):
//!   groundhog-bench --url http://127.0.0.1:9545 --rate 100 --duration 5
//!
//!   # Against real gateway (load):
//!   groundhog-bench --url http://127.0.0.1:8545 --rate 10000 --duration 60

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use groundhog_signing::{
    build_call_transaction, encode_signed_transaction_xdr, sign_transaction, BuildCallTransaction,
    LocalSigner, PubkeyAccount, SignTransactionParams, Signer, DEFAULT_PUBKEY_ACCOUNT_NONCE,
};
use groundhog_xdr::contract::ContractEntrypointID;
use hdrhistogram::Histogram;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

/// Groundhog JSON-RPC benchmark harness.
#[derive(Parser)]
#[command(name = "groundhog-bench")]
struct Cli {
    /// JSON-RPC server URL.
    #[arg(long, default_value = "http://127.0.0.1:9545")]
    url: String,

    /// Target requests per second (open-loop).
    #[arg(long, default_value = "100")]
    rate: u64,

    /// Duration in seconds.
    #[arg(long, default_value = "10")]
    duration: u64,

    /// Number of accounts to cycle through.
    #[arg(long, default_value = "10")]
    accounts: usize,

    /// Method to benchmark.
    #[arg(long, default_value = "sendRawTransaction")]
    method: String,

    /// Warmup seconds (results discarded).
    #[arg(long, default_value = "2")]
    warmup: u64,
}

/// Derive a dev-node account signer (matches dev-node's derivation).
fn dev_signer(index: usize) -> LocalSigner {
    let mut hasher = Sha256::new();
    hasher.update(b"groundhog-dev-accounts:");
    hasher.update(format!("account-{index}").as_bytes());
    let secret: [u8; 32] = hasher.finalize().into();
    LocalSigner::from_secret_key(secret)
}

/// Pre-generate a batch of hex-encoded signed transactions.
fn generate_tx_batch(count: usize, num_accounts: usize) -> Vec<String> {
    // Pre-compute signers so we do key derivation once per account, not once per tx.
    let signers: Vec<_> = (0..num_accounts).map(dev_signer).collect();
    let accounts: Vec<_> = signers
        .iter()
        .map(|s| PubkeyAccount::new(s.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE))
        .collect();
    let addresses: Vec<_> = accounts
        .iter()
        .map(|a| a.address().expect("bug: address derivation"))
        .collect();

    (0..count)
        .map(|i| {
            let idx = i % num_accounts;
            let tx = build_call_transaction(BuildCallTransaction {
                invoked_address: addresses[idx],
                entrypoint: ContractEntrypointID::from([0u8; 8]),
                calldata: vec![],
                gas_limit: 10_000_000,
                block_expiry_height: 10_000_000,
                nonce: i as u64,
                auth_claims: vec![accounts[idx].auth_claim()]
                    .try_into()
                    .expect("bug: auth claims"),
            });

            let signed = sign_transaction(tx, &signers[idx], SignTransactionParams::default())
                .expect("bug: sign");
            let xdr = encode_signed_transaction_xdr(&signed).expect("bug: encode");
            format!("0x{}", hex::encode(&xdr))
        })
        .collect()
}

/// Build a JSON-RPC request body.
fn make_request(method: &str, tx_hex: &str, id: u64) -> serde_json::Value {
    match method {
        "sendRawTransaction" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "groundhog_sendRawTransaction",
            "params": [tx_hex]
        }),
        "chainTip" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "groundhog_chainTip",
            "params": []
        }),
        "getTransactionStatus" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "groundhog_getTransactionStatus",
            "params": ["0x0000000000000000000000000000000000000000000000000000000000000000"]
        }),
        _ => panic!("unknown method: {method}"),
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let total_duration = Duration::from_secs(cli.duration);
    let warmup_duration = Duration::from_secs(cli.warmup);
    let interval = Duration::from_secs_f64(1.0 / cli.rate as f64);

    let tx_batch = if cli.method == "sendRawTransaction" {
        eprintln!("Generating transactions...");
        let batch = generate_tx_batch(
            (cli.rate as usize) * (cli.duration as usize + cli.warmup as usize + 1),
            cli.accounts,
        );
        eprintln!("Generated {} transactions", batch.len());
        batch
    } else {
        vec![]
    };

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(128)
        .build()
        .expect("bug: reqwest client");

    let histogram = Arc::new(Mutex::new(
        Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).expect("bug: histogram"),
    ));
    let errors = Arc::new(AtomicU64::new(0));
    let total_sent = Arc::new(AtomicU64::new(0));

    eprintln!(
        "Benchmarking {} at {} req/s for {}s ({}s warmup)...",
        cli.method, cli.rate, cli.duration, cli.warmup
    );

    let start = Instant::now();
    let mut tx_idx = 0u64;
    let mut next_fire = start;

    loop {
        let elapsed = start.elapsed();
        if elapsed > total_duration + warmup_duration {
            break;
        }

        // Wait until next fire time (open-loop: fire at fixed rate).
        let now = Instant::now();
        if now < next_fire {
            tokio::time::sleep(next_fire - now).await;
        }
        next_fire += interval;

        let in_warmup = elapsed < warmup_duration;
        let tx_hex = if tx_batch.is_empty() {
            ""
        } else {
            &tx_batch[tx_idx as usize % tx_batch.len()]
        };
        let body = make_request(&cli.method, tx_hex, tx_idx);
        tx_idx += 1;

        let client = client.clone();
        let url = cli.url.clone();
        let histogram = histogram.clone();
        let errors = errors.clone();
        let total_sent = total_sent.clone();

        tokio::spawn(async move {
            let req_start = Instant::now();
            let result = client.post(&url).json(&body).send().await;

            let latency_us = req_start.elapsed().as_micros() as u64;

            match result {
                Ok(resp) if resp.status().is_success() => {
                    if !in_warmup {
                        histogram.lock().await.record(latency_us).ok();
                        total_sent.fetch_add(1, Ordering::Relaxed);
                    }
                }
                _ => {
                    if !in_warmup {
                        errors.fetch_add(1, Ordering::Relaxed);
                        total_sent.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
    }

    // Wait for in-flight requests to finish.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let hist = histogram.lock().await;
    let sent = total_sent.load(Ordering::Relaxed);
    let errs = errors.load(Ordering::Relaxed);
    let actual_duration = total_duration.as_secs_f64();

    println!("\n=== Groundhog RPC Benchmark Results ===");
    println!("Method:     {}", cli.method);
    println!("Target:     {} req/s", cli.rate);
    println!("Duration:   {}s ({}s warmup)", cli.duration, cli.warmup);
    println!("Requests:   {} ({} errors)", sent, errs);
    println!(
        "Throughput: {:.1} req/s",
        (sent - errs) as f64 / actual_duration
    );
    println!();
    println!("Latency (microseconds):");
    println!("  p50:  {}", hist.value_at_percentile(50.0));
    println!("  p90:  {}", hist.value_at_percentile(90.0));
    println!("  p95:  {}", hist.value_at_percentile(95.0));
    println!("  p99:  {}", hist.value_at_percentile(99.0));
    println!("  p999: {}", hist.value_at_percentile(99.9));
    println!("  max:  {}", hist.max());
    println!("  mean: {:.0}", hist.mean());
}
