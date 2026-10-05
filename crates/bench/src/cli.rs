//! Command-line options and submission confirmation.

use std::io::{BufRead, Write};

use clap::Parser;

use crate::BenchError;
use crate::results::BENCH_RESULT_SCHEMA_VERSION;

const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const SUBMIT_CONFIRMATION_ACTION: &str = "submit signed state-changing benchmark transactions";

#[derive(clap::Subcommand)]
pub(super) enum Command {
    /// Compare repeated trials after validating experiment compatibility.
    Compare(crate::compare::CompareArgs),
}

/// Groundhog JSON-RPC benchmark harness.
#[derive(Parser)]
#[command(name = "groundhog-bench", version)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) command: Option<Command>,
    /// JSON-RPC server URL.
    #[arg(long, default_value = "http://127.0.0.1:9545")]
    pub(super) url: String,

    /// Runtime manifest used to derive benchmark account signers. Required for sendRawTransaction.
    #[arg(long)]
    pub(super) runtime_manifest: Option<std::path::PathBuf>,

    /// Target requests per second (open-loop, default 100).
    #[arg(long, conflicts_with_all = ["outstanding", "unthrottled"])]
    pub(super) rate: Option<u64>,

    /// Dispatch as fast as possible instead of using the open-loop rate schedule.
    #[arg(long)]
    pub(super) unthrottled: bool,

    /// Global maximum outstanding requests (closed-loop), independent of worker count.
    #[arg(long, conflicts_with = "unthrottled")]
    pub(super) outstanding: Option<usize>,

    /// Versioned confirmed-hash corpus to validate against the target before timing.
    #[arg(long, conflicts_with = "seed_count")]
    pub(super) hashes_file: Option<std::path::PathBuf>,

    /// Submit this many transactions and confirm them before timing read workloads.
    #[arg(long, requires_all = ["seed_output", "runtime_manifest"])]
    pub(super) seed_count: Option<usize>,

    /// Persist the fully confirmed seed corpus here; refuses to overwrite an existing file.
    #[arg(long, requires = "seed_count")]
    pub(super) seed_output: Option<std::path::PathBuf>,

    /// First seed nonce (defaults to the current Unix time in nanoseconds).
    #[arg(long, requires = "seed_count")]
    pub(super) seed_nonce_start: Option<u64>,

    /// Total seed/import validation deadline in seconds, outside the measured window.
    #[arg(long, default_value = "120")]
    pub(super) setup_timeout_secs: u64,

    /// Create a new marker file after setup, immediately before warmup load starts.
    #[arg(long)]
    pub(super) load_start_file: Option<std::path::PathBuf>,

    /// Runner-supplied gateway/backend build, host and effective configuration metadata.
    #[arg(long)]
    pub(super) target_info: Option<std::path::PathBuf>,

    /// Private gateway Prometheus endpoint, sampled during measurement.
    #[arg(long)]
    pub(super) metrics_url: Option<String>,

    /// Local gateway PID (Linux /proc only; excludes child processes).
    #[arg(long)]
    pub(super) gateway_pid: Option<u32>,

    /// Server observation interval in milliseconds.
    #[arg(long, default_value = "250", value_parser = clap::value_parser!(u64).range(10..=60000))]
    pub(super) sample_interval_ms: u64,

    /// Duration in seconds.
    #[arg(long, default_value = "10")]
    pub(super) duration: u64,

    /// Number of accounts to cycle through.
    #[arg(long, default_value = "10")]
    pub(super) accounts: usize,

    /// Method to benchmark.
    #[arg(long, default_value = "sendRawTransaction")]
    pub(super) method: String,

    /// Warmup seconds (results discarded).
    #[arg(long, default_value = "2")]
    pub(super) warmup: u64,

    /// Number of worker tasks (each with its own connection pool).
    #[arg(long, default_value = "1")]
    pub(super) workers: usize,

    /// Max idle connections per worker. Auto-sized from calibration if 0.
    #[arg(long, default_value = "0")]
    pub(super) connections_per_worker: usize,

    /// Pre-generated transaction batch size for sendRawTransaction.
    ///
    /// Defaults to the scheduled request count in rate-limited mode and 100000
    /// in unthrottled mode. Increase this if an unthrottled submit run exhausts
    /// the batch and starts reusing signed transactions.
    #[arg(long)]
    pub(super) tx_batch_size: Option<usize>,

    /// Max accumulated scheduling skew before aborting (seconds).
    #[arg(long, default_value = "1.0")]
    pub(super) max_skew: f64,

    /// Timeout for each complete HTTP request, including its response body.
    #[arg(long, default_value_t = DEFAULT_REQUEST_TIMEOUT_MS)]
    pub(super) request_timeout_ms: u64,

    /// Maximum accepted JSON-RPC response body size.
    #[arg(long, default_value_t = DEFAULT_MAX_RESPONSE_BYTES)]
    pub(super) max_response_bytes: usize,

    /// Stable name for the enclosing scenario, when run by an orchestrator.
    #[arg(long)]
    pub(super) scenario: Option<String>,

    /// Human-readable identity for the endpoint under test.
    #[arg(long)]
    pub(super) target_label: Option<String>,

    /// Emit a versioned machine-readable result on stdout.
    #[arg(long)]
    pub(super) json: bool,

    /// Skip confirmation for sendRawTransaction.
    #[arg(long)]
    pub(super) force: bool,
}

pub(super) fn confirm_submission(
    cli: &Cli,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
    input: &mut impl BufRead,
    error: &mut impl Write,
) -> Result<(), BenchError> {
    if (cli.method != "sendRawTransaction" && cli.seed_count.is_none()) || cli.force {
        return Ok(());
    }
    if cli.json || !stdin_is_terminal || !stderr_is_terminal {
        return Err(BenchError::ConfirmationRequired);
    }

    let rate = if cli.unthrottled || cli.outstanding.is_some() {
        "unthrottled".to_string()
    } else {
        format!("{} requests/second", cli.rate.unwrap_or(100))
    };
    if let Some(count) = cli.seed_count {
        writeln!(
            error,
            "Setup will submit {count} seed transactions before the read benchmark."
        )
        .map_err(|source| BenchError::ConfirmationIo { source })?;
    }
    let url = terminal_safe(&cli.url);
    write!(
        error,
        "This benchmark will submit signed, native-wallet transactions via sendRawTransaction.\nURL: {}\nAccounts: {}\nRate: {rate}\nDuration: {}s plus {}s warmup\nContinue? [y/N] ",
        url, cli.accounts, cli.duration, cli.warmup
    )
    .and_then(|()| error.flush())
    .map_err(|source| BenchError::ConfirmationIo { source })?;

    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .map_err(|source| BenchError::ConfirmationIo { source })?;
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err(BenchError::ConfirmationDeclined)
    }
}

fn terminal_safe(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

pub(super) fn confirmation_required_json() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": BENCH_RESULT_SCHEMA_VERSION,
        "kind": "groundhog-bench-confirmation",
        "status": "confirmation_required",
        "action": SUBMIT_CONFIRMATION_ACTION,
        "requiredFlag": "--force"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_method_does_not_require_confirmation() {
        let cli = confirmation_cli("chainTip");
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        let mut error = Vec::new();

        confirm_submission(&cli, false, false, &mut input, &mut error)
            .expect("bug: read-only benchmarks should not require confirmation");

        assert!(error.is_empty());
    }

    #[test]
    fn force_is_a_benchmark_command_option() {
        let cli = Cli::try_parse_from(["groundhog-bench", "--force"])
            .expect("bug: groundhog-bench should accept --force");

        assert!(cli.force);
    }

    #[test]
    fn confirmation_required_json_is_actionable() {
        let value = confirmation_required_json();

        assert_eq!(value["status"], "confirmation_required");
        assert_eq!(value["action"], SUBMIT_CONFIRMATION_ACTION);
        assert_eq!(value["requiredFlag"], "--force");
    }

    #[test]
    fn force_skips_submit_confirmation_without_reading_input() {
        struct Unreadable;

        impl std::io::Read for Unreadable {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                panic!("bug: --force must not read stdin")
            }
        }

        impl BufRead for Unreadable {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                panic!("bug: --force must not read stdin")
            }

            fn consume(&mut self, _amount: usize) {}
        }

        let mut cli = confirmation_cli("sendRawTransaction");
        cli.force = true;
        let mut input = Unreadable;
        let mut error = Vec::new();

        confirm_submission(&cli, false, false, &mut input, &mut error)
            .expect("bug: --force should bypass confirmation");

        assert!(error.is_empty());
    }

    #[test]
    fn submit_confirmation_fails_closed_for_json_or_non_tty() {
        for (json, stdin_is_terminal, stderr_is_terminal) in [
            (true, true, true),
            (false, false, true),
            (false, true, false),
        ] {
            let mut cli = confirmation_cli("sendRawTransaction");
            cli.json = json;
            let mut input = std::io::Cursor::new(b"yes\n");
            let mut error = Vec::new();

            let result = confirm_submission(
                &cli,
                stdin_is_terminal,
                stderr_is_terminal,
                &mut input,
                &mut error,
            );

            assert!(matches!(result, Err(BenchError::ConfirmationRequired)));
            assert_eq!(input.position(), 0);
            assert!(error.is_empty());
        }
    }

    #[test]
    fn submit_confirmation_accepts_y_and_yes() {
        let cli = confirmation_cli("sendRawTransaction");
        for answer in ["y\n", "YES\n"] {
            let mut input = std::io::Cursor::new(answer.as_bytes());
            let mut error = Vec::new();

            confirm_submission(&cli, true, true, &mut input, &mut error)
                .expect("bug: y and yes should confirm submission");

            let prompt = String::from_utf8(error).expect("bug: prompt should be UTF-8");
            assert!(prompt.contains("plus 2s warmup"));
            assert!(prompt.ends_with("Continue? [y/N] "));
        }
    }

    #[test]
    fn submit_confirmation_defaults_to_no() {
        let cli = confirmation_cli("sendRawTransaction");
        for answer in ["\n", "no\n", "anything else\n"] {
            let mut input = std::io::Cursor::new(answer.as_bytes());
            let mut error = Vec::new();

            let result = confirm_submission(&cli, true, true, &mut input, &mut error);

            assert!(matches!(result, Err(BenchError::ConfirmationDeclined)));
        }
    }

    fn confirmation_cli(method: &str) -> Cli {
        let mut cli = Cli::parse_from(["groundhog-bench"]);
        cli.method = method.to_string();
        cli
    }
    #[test]
    fn scheduling_modes_are_mutually_exclusive() {
        for args in [
            vec!["groundhog-bench", "--outstanding", "3", "--rate", "100"],
            vec!["groundhog-bench", "--outstanding", "3", "--unthrottled"],
            vec!["groundhog-bench", "--rate", "100", "--unthrottled"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        assert!(Cli::try_parse_from(["groundhog-bench", "--outstanding", "3"]).is_ok());
    }

    #[test]
    fn read_workload_seeding_requires_submission_confirmation() {
        let mut cli = confirmation_cli("getTransactionReceipt");
        cli.seed_count = Some(3);
        cli.json = true;
        let mut input = std::io::Cursor::new(b"yes\n");
        let mut output = Vec::new();
        assert!(matches!(
            confirm_submission(&cli, false, false, &mut input, &mut output),
            Err(BenchError::ConfirmationRequired)
        ));
        assert_eq!(input.position(), 0);
        cli.force = true;
        assert!(confirm_submission(&cli, false, false, &mut input, &mut output).is_ok());
    }
}
