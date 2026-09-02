//! `rv32-lower` — CLI front end for the rv32 contract lowerer.
//!
//! ```text
//!   rv32-lower --elf target/.../release/<contract>-rv32 --out <contract>.rv32a
//! ```

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use groundhog_rv32_lower::{
    FORMAT_FINGERPRINT, LowerError, MetadataInput, lower, render_metadata, summary_json,
};
use tempfile::NamedTempFile;

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
    /// Replace existing output files without prompting.
    #[arg(long)]
    force: bool,
    /// Print the pinned artifact-format fingerprint and exit.
    #[arg(long)]
    print_format_fingerprint: bool,
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    Lower(#[from] LowerError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("confirmation required to replace existing output paths; pass --force\n{details}")]
    ConfirmationRequired { details: String, paths: Vec<String> },
    #[error("output replacement declined")]
    ConfirmationDeclined,
}

#[derive(Clone)]
struct OutputPath {
    path: PathBuf,
    state: OutputState,
    replacement_permissions: Option<fs::Permissions>,
}

#[derive(Clone, Copy)]
enum OutputState {
    Absent,
    Existing(FileIdentity),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn run(cli: &Cli) -> Result<(), CliError> {
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
    let metadata = cli.metadata.as_ref().map(|_| {
        render_metadata(&MetadataInput {
            elf: &elf_bytes,
            artifact: &lowered.artifact,
            code: &lowered.code,
            initial_memory: &lowered.initial_memory,
            initial_memory_start: lowered.initial_memory_start,
            memory_bytes: lowered.memory_bytes,
            rustc_version: cli.rustc_version.clone(),
            target_triple: cli.target_triple.clone(),
            build_flags: cli.build_flags.clone(),
        })
    });

    let out_path = inspect_output_path(out)?;
    let metadata_path = cli
        .metadata
        .as_deref()
        .map(inspect_output_path)
        .transpose()?;
    reject_output_aliases(&out_path, metadata_path.as_ref())?;
    confirm_overwrite(cli, &out_path, metadata_path.as_ref())?;
    write_outputs(
        &out_path,
        &lowered.artifact,
        metadata_path.as_ref().zip(metadata.as_deref()),
    )?;

    if cli.json {
        let out = out.to_string_lossy();
        println!("{}", summary_json(&out, &lowered));
    }
    Ok(())
}

fn confirm_overwrite(
    cli: &Cli,
    out: &OutputPath,
    metadata: Option<&OutputPath>,
) -> Result<(), CliError> {
    if cli.force {
        return Ok(());
    }

    let mut affected = Vec::with_capacity(2);
    if !matches!(out.state, OutputState::Absent) {
        affected.push(out.path.as_path());
    }
    if let Some(metadata) = metadata
        && !matches!(metadata.state, OutputState::Absent)
    {
        affected.push(metadata.path.as_path());
    }
    if affected.is_empty() {
        return Ok(());
    }

    let affected_paths = affected
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let details = affected_paths
        .iter()
        .map(|path| format!("  {}", terminal_safe(path)))
        .collect::<Vec<_>>()
        .join("\n");
    if cli.json || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(CliError::ConfirmationRequired {
            details,
            paths: affected_paths,
        });
    }

    let mut stderr = io::stderr().lock();
    write!(
        stderr,
        "The following output paths already exist:\n{details}\nReplace them? [y/N] "
    )?;
    stderr.flush()?;

    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    if matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::ConfirmationDeclined)
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

fn inspect_output_path(path: &Path) -> Result<OutputPath, CliError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let file_name = absolute.file_name().ok_or_else(|| {
        LowerError::Invalid(format!("output path {} has no file name", path.display()))
    })?;
    let parent = absolute.parent().ok_or_else(|| {
        LowerError::Invalid(format!("output path {} has no parent", path.display()))
    })?;
    let resolved = parent.canonicalize()?.join(file_name);
    let (state, replacement_permissions) = match fs::symlink_metadata(&resolved) {
        Ok(metadata) if metadata.is_file() => (
            OutputState::Existing(file_identity(&metadata)),
            Some(metadata.permissions()),
        ),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            (OutputState::Existing(file_identity(&metadata)), None)
        }
        Ok(_) => {
            return Err(LowerError::Invalid(format!(
                "output path {} is not a regular file",
                resolved.display()
            ))
            .into());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (OutputState::Absent, None),
        Err(error) => return Err(error.into()),
    };
    Ok(OutputPath {
        path: resolved,
        state,
        replacement_permissions,
    })
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {}
}

fn reject_output_aliases(out: &OutputPath, metadata: Option<&OutputPath>) -> Result<(), CliError> {
    let Some(metadata) = metadata else {
        return Ok(());
    };
    let same_identity = same_file_identity(&out.state, &metadata.state);
    if out.path == metadata.path || same_identity {
        return Err(LowerError::Invalid(
            "--out and --metadata must refer to different files".to_string(),
        )
        .into());
    }
    Ok(())
}

#[cfg(unix)]
fn same_file_identity(left: &OutputState, right: &OutputState) -> bool {
    matches!(
        (left, right),
        (OutputState::Existing(left), OutputState::Existing(right)) if left == right
    )
}

#[cfg(not(unix))]
fn same_file_identity(_left: &OutputState, _right: &OutputState) -> bool {
    false
}

fn stage_output(output: &OutputPath, bytes: &[u8]) -> Result<NamedTempFile, CliError> {
    let parent = output.path.parent().ok_or_else(|| {
        LowerError::Invalid(format!(
            "output path {} has no parent",
            output.path.display()
        ))
    })?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    if let Some(permissions) = &output.replacement_permissions {
        temporary
            .as_file_mut()
            .set_permissions(permissions.clone())?;
    }
    temporary.as_file_mut().sync_all()?;
    Ok(temporary)
}

fn revalidate_output(output: &OutputPath) -> Result<(), CliError> {
    match (output.state, fs::symlink_metadata(&output.path)) {
        (OutputState::Absent, Err(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        (OutputState::Existing(expected), Ok(metadata)) if file_identity(&metadata) == expected => {
            Ok(())
        }
        (_, Err(error)) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Err(LowerError::Invalid(format!(
            "output path changed after confirmation: {}",
            output.path.display()
        ))
        .into()),
    }
}

fn publish_output(temporary: NamedTempFile, output: &OutputPath) -> Result<(), CliError> {
    revalidate_output(output)?;
    match output.state {
        OutputState::Absent => temporary
            .persist_noclobber(&output.path)
            .map(|_| ())
            .map_err(|error| {
                if error.error.kind() == io::ErrorKind::AlreadyExists {
                    LowerError::Invalid(format!(
                        "output path changed after confirmation: {}",
                        output.path.display()
                    ))
                    .into()
                } else {
                    error.error.into()
                }
            }),
        OutputState::Existing(_) => temporary
            .persist(&output.path)
            .map(|_| ())
            .map_err(|error| error.error.into()),
    }
}

fn write_outputs(
    out: &OutputPath,
    artifact: &[u8],
    metadata: Option<(&OutputPath, &str)>,
) -> Result<(), CliError> {
    let out_temporary = stage_output(out, artifact)?;
    let metadata_temporary = metadata
        .map(|(path, document)| stage_output(path, document.as_bytes()))
        .transpose()?;

    revalidate_output(out)?;
    if let Some((path, _)) = metadata {
        revalidate_output(path)?;
    }

    publish_output(out_temporary, out)?;
    if let (Some((path, _)), Some(temporary)) = (metadata, metadata_temporary) {
        publish_output(temporary, path)?;
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
                let result = match &error {
                    CliError::ConfirmationRequired { paths, .. } => serde_json::json!({
                        "status": "confirmation_required",
                        "action": "replace existing rv32-lower output files",
                        "requiredFlag": "--force",
                        "paths": paths,
                    }),
                    _ => serde_json::json!({ "status": "error", "message": error.to_string() }),
                };
                println!("{result}");
            } else {
                eprintln!("rv32-lower: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_output_does_not_clobber_file_created_after_preflight() {
        let directory = tempfile::tempdir().expect("bug: temp directory should create");
        let path = directory.path().join("contract.rv32a");
        let output = inspect_output_path(&path).expect("bug: output should inspect");
        fs::write(&path, b"other writer").expect("bug: competing output should write");

        let error = write_outputs(&output, b"artifact", None)
            .expect_err("output created after preflight must not be replaced");

        assert!(error.to_string().contains("changed after confirmation"));
        assert_eq!(fs::read(path).unwrap(), b"other writer");
    }

    #[test]
    fn approved_output_does_not_replace_changed_file() {
        let directory = tempfile::tempdir().expect("bug: temp directory should create");
        let path = directory.path().join("contract.rv32a");
        fs::write(&path, b"approved").expect("bug: initial output should write");
        let original = fs::File::open(&path).expect("bug: initial output should open");
        let output = inspect_output_path(&path).expect("bug: output should inspect");
        fs::remove_file(&path).expect("bug: initial output should remove");
        fs::write(&path, b"replacement").expect("bug: replacement should write");

        let error = write_outputs(&output, b"artifact", None)
            .expect_err("changed output must not be replaced");

        drop(original);
        assert!(error.to_string().contains("changed after confirmation"));
        assert_eq!(fs::read(path).unwrap(), b"replacement");
    }

    #[test]
    fn changed_metadata_preflight_leaves_artifact_untouched() {
        let directory = tempfile::tempdir().expect("bug: temp directory should create");
        let out_path = directory.path().join("contract.rv32a");
        let metadata_path = directory.path().join("contract.json");
        fs::write(&out_path, b"approved artifact").expect("bug: artifact should write");
        fs::write(&metadata_path, b"approved metadata").expect("bug: metadata should write");
        let original_metadata = fs::File::open(&metadata_path)
            .expect("bug: original metadata should remain identifiable");
        let out = inspect_output_path(&out_path).expect("bug: artifact should inspect");
        let metadata = inspect_output_path(&metadata_path).expect("bug: metadata should inspect");
        fs::remove_file(&metadata_path).expect("bug: original metadata should remove");
        fs::write(&metadata_path, b"other writer").expect("bug: competing metadata should write");

        let error = write_outputs(&out, b"new artifact", Some((&metadata, "new metadata")))
            .expect_err("changed metadata must fail before artifact publication");

        drop(original_metadata);
        assert!(error.to_string().contains("changed after confirmation"));
        assert_eq!(fs::read(out_path).unwrap(), b"approved artifact");
        assert_eq!(fs::read(metadata_path).unwrap(), b"other writer");
    }
}
