//! `rv32-lower` — CLI front end for the rv32 contract lowerer.
//!
//! ```text
//!   rv32-lower --elf target/.../release/<contract>-rv32 --out <contract>.rv32a
//! ```

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use groundhog_rv32_lower::{
    FORMAT_FINGERPRINT, LowerError, MetadataInput, lower, render_metadata, summary_json,
};

const BUILD_VERSION: groundhog_build_info::BuildVersion = groundhog_build_info::build_version!();

/// Lower a RISC-V rv32im contract ELF to a Groundhog v5 RV32 artifact.
#[derive(Parser)]
#[command(name = "rv32-lower", version = BUILD_VERSION.to_string(), about)]
struct Cli {
    /// Input rv32 ELF (the `<contract>-rv32` cargo build output).
    #[arg(long)]
    elf: Option<PathBuf>,
    /// Output artifact path (`.rv32a`).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Optional JSON sidecar for build metadata.
    #[arg(long)]
    metadata: Option<PathBuf>,
    /// Recorded in metadata if `--metadata` is set.
    #[arg(long)]
    rustc_version: Option<String>,
    /// Recorded in metadata if `--metadata` is set.
    #[arg(long)]
    target_triple: Option<String>,
    /// Recorded in metadata if `--metadata` is set.
    #[arg(long)]
    build_flags: Option<String>,
    /// Emit a structured JSON result to stdout (for scripting / LLM callers).
    #[arg(long)]
    json: bool,
    /// Print the pinned artifact-format fingerprint and exit.
    #[arg(long)]
    print_format_fingerprint: bool,
}

fn run(cli: &Cli) -> Result<(), LowerError> {
    if cli.print_format_fingerprint {
        if cli.json {
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "artifact_format_fingerprint": format!("{FORMAT_FINGERPRINT:#018x}"),
                })
            );
        } else {
            println!("{FORMAT_FINGERPRINT:#018x}");
        }
        return Ok(());
    }

    let elf = cli
        .elf
        .as_ref()
        .ok_or_else(|| LowerError::Invalid("--elf is required".to_string()))?;
    let out = cli
        .out
        .as_ref()
        .ok_or_else(|| LowerError::Invalid("--out is required".to_string()))?;

    let elf_bytes = fs::read(elf)?;
    let lowered = lower(&elf_bytes)?;
    fs::write(out, &lowered.artifact)?;

    if let Some(metadata_path) = &cli.metadata {
        let document = render_metadata(&MetadataInput {
            elf: &elf_bytes,
            artifact: &lowered.artifact,
            code: &lowered.code,
            initial_memory: &lowered.initial_memory,
            initial_memory_start: lowered.initial_memory_start,
            memory_bytes: lowered.memory_bytes,
            rustc_version: cli.rustc_version.clone(),
            target_triple: cli.target_triple.clone(),
            build_flags: cli.build_flags.clone(),
        });
        fs::write(metadata_path, document)?;
    }

    if cli.json {
        let out = out.to_string_lossy();
        println!("{}", summary_json(&out, &lowered));
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if cli.json {
                // Keep stdout a single machine-readable stream under --json.
                println!(
                    "{}",
                    serde_json::json!({ "status": "error", "message": error.to_string() })
                );
            } else {
                eprintln!("rv32-lower: {error}");
            }
            ExitCode::FAILURE
        }
    }
}
