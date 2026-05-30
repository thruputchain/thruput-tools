//! `rv32-lower` — CLI front end for the rv32 contract lowerer.
//!
//! ```text
//!   rv32-lower --elf target/.../release/<contract>-rv32 --out <contract>.rv32a
//! ```

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use groundhog_rv32_lower::{LowerError, MetadataInput, lower, render_metadata};

/// Lower a RISC-V rv32im contract ELF to a Groundhog v3 RV32 artifact.
#[derive(Parser)]
#[command(name = "rv32-lower", version, about)]
struct Cli {
    /// Input rv32 ELF (the `<contract>-rv32` cargo build output).
    #[arg(long)]
    elf: PathBuf,
    /// Output artifact path (`.rv32a`).
    #[arg(long)]
    out: PathBuf,
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
}

fn run(cli: &Cli) -> Result<(), LowerError> {
    let elf_bytes = fs::read(&cli.elf)?;
    let lowered = lower(&elf_bytes)?;
    fs::write(&cli.out, &lowered.artifact)?;

    if let Some(metadata_path) = &cli.metadata {
        let document = render_metadata(&MetadataInput {
            elf: &elf_bytes,
            artifact: &lowered.artifact,
            code: &lowered.code,
            initial_memory: &lowered.initial_memory,
            entries: &lowered.entries,
            rustc_version: cli.rustc_version.clone(),
            target_triple: cli.target_triple.clone(),
            build_flags: cli.build_flags.clone(),
        });
        fs::write(metadata_path, document)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rv32-lower: {error}");
            ExitCode::FAILURE
        }
    }
}
