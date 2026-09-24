//! `rv32-lower` — CLI front end for the rv32 contract lowerer.
//!
//! ```text
//!   rv32-lower --elf target/.../release/<contract>-rv32 --out <contract>.rv32a
//! ```

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use same_file::Handle;

use groundhog_rv32_lower::{
    FORMAT_FINGERPRINT, LowerError, LowerOptions, MetadataInput, lower_with, render_metadata,
    summary_json,
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
    /// Guest data address space to stamp into the artifact, in bytes; every
    /// data section must end within it. Defaults to the protocol's 64 KiB. A
    /// chain admits the artifact only up to its configured
    /// `max_rv32_memory_bytes`.
    #[arg(long, value_name = "BYTES")]
    memory_bytes: Option<usize>,
}

// Keep the checked handles through the writes: resolving paths and then
// reopening them would miss aliases introduced between the check and write.
fn open_output(path: &Path) -> Result<Handle, LowerError> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    Ok(Handle::from_file(file)?)
}

fn write_output(handle: &mut Handle, bytes: &[u8]) -> Result<(), LowerError> {
    let file = handle.as_file_mut();
    if file.metadata()?.is_file() {
        file.set_len(0)?;
    }
    file.write_all(bytes)?;
    Ok(())
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

    let mut input = Handle::from_path(elf)?;
    let mut elf_bytes = Vec::new();
    input.as_file_mut().read_to_end(&mut elf_bytes)?;
    let options = LowerOptions {
        memory_bytes: cli
            .memory_bytes
            .unwrap_or(LowerOptions::default().memory_bytes),
    };
    let lowered = lower_with(&elf_bytes, &options)?;
    // Open every destination without truncating before checking identities.
    // This also catches initially absent paths and symlink/hard-link aliases.
    let mut output = open_output(out)?;
    if output == input {
        return Err(LowerError::Invalid(
            "--out and --elf refer to the same file".to_string(),
        ));
    }
    let mut metadata_output = cli.metadata.as_deref().map(open_output).transpose()?;
    if let Some(handle) = &metadata_output {
        if *handle == input {
            return Err(LowerError::Invalid(
                "--metadata and --elf refer to the same file".to_string(),
            ));
        }
        if *handle == output {
            return Err(LowerError::Invalid(
                "--metadata and --out refer to the same file".to_string(),
            ));
        }
    }
    write_output(&mut output, &lowered.artifact)?;
    // The JSON summary carries `warnings`; plain mode prints them where a
    // human building a contract will see them.
    if !cli.json {
        for warning in &lowered.warnings {
            eprintln!("warning: {warning}");
        }
    }

    if let Some(handle) = &mut metadata_output {
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
        write_output(handle, document.as_bytes())?;
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
