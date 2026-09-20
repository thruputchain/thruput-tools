//! Focused, bounded indexer-miss and scheduler-lookup diagnostic.
#[path = "lookup_bench/runner.rs"]
mod runner;
#[path = "lookup_bench/transport.rs"]
mod transport;

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, ValueEnum, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    ClosedLoop,
    Paced,
    Burst,
}

#[derive(Clone, Debug, Parser, Serialize)]
#[command(about = "Exercise indexer misses and scheduler lookups without submitting transactions")]
struct Args {
    /// Target gateway. Remote traffic requires explicitly supplying its URL.
    #[arg(long, default_value = "http://127.0.0.1:8545")]
    url: String,
    #[arg(long, value_enum, default_value = "paced")]
    mode: Mode,
    /// Maximum simultaneous HTTP requests, including drain.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=4096))]
    concurrency: u32,
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=1000000))]
    rate: u32,
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=1000000))]
    burst_size: u32,
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..))]
    burst_interval_ms: u32,
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=3600))]
    duration_secs: u32,
    #[arg(long, default_value_t = 5000, value_parser = clap::value_parser!(u32).range(1..=60000))]
    timeout_ms: u32,
    #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(1..=1000000))]
    corpus_size: u32,
    #[arg(long, default_value = "lookup-miss-v1")]
    seed: String,
    /// Refuses to overwrite an existing result.
    #[arg(long, default_value = "lookup-result.json")]
    output: PathBuf,
    /// Optional Prometheus endpoint; observations cover all traffic on that server.
    #[arg(long)]
    metrics_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    runner::run(Args::parse()).await
}
