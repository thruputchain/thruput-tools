use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::codegen::generate_ts_bindings;
use crate::contract_spec::load_contract_spec_from_file;
use crate::result::CommandResult;
use crate::{CliError, Print};

/// Run the `codegen ts` command.
pub fn run(
    source: &Path,
    output: Option<&Path>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    run_with_force(source, output, false, cmd, print)
}

/// Run the `codegen ts` command, optionally overwriting confirmed output.
pub fn run_with_force(
    source: &Path,
    output: Option<&Path>,
    force: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let spec = load_contract_spec_from_file(source)?.ok_or_else(|| {
        CliError::CodegenFailed(format!("no contract spec found in {}", source.display()))
    })?;
    let source_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let ts = generate_ts_bindings(&spec, source_name)?;

    if let Some(output_path) = output {
        let output_snapshot = output_snapshot(output_path)?;
        let output_exists = output_snapshot != OutputSnapshot::Missing;
        if output_exists
            && !print.confirm(
                force,
                "overwrite existing generated bindings",
                format_args!("Overwrite existing output `{}`?", output_path.display()),
            )?
        {
            print.note("Cancelled");
            return Ok(());
        }
        write_output(output_path, ts.as_bytes(), &output_snapshot)?;
        print.success("Wrote TypeScript bindings");
        print.detail("output", output_path.display());
        print.json(&CommandResult::new(
            cmd,
            json!({
                "source": source,
                "output": output_path,
            }),
        ))?;
    } else if print.is_json() {
        print.json(&CommandResult::new(
            cmd,
            json!({
                "source": source,
                "typescript": ts,
            }),
        ))?;
    } else {
        print!("{ts}");
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum OutputSnapshot {
    Missing,
    File(Vec<u8>),
    Symlink(PathBuf),
    Other,
}

fn output_snapshot(path: &Path) -> Result<OutputSnapshot, CliError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OutputSnapshot::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Ok(OutputSnapshot::Symlink(std::fs::read_link(path)?));
    }
    if file_type.is_file() {
        return Ok(OutputSnapshot::File(std::fs::read(path)?));
    }
    Ok(OutputSnapshot::Other)
}

fn write_output(path: &Path, contents: &[u8], expected: &OutputSnapshot) -> Result<(), CliError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".groundhog-codegen-")
        .tempfile_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    if &output_snapshot(path)? != expected {
        if *expected == OutputSnapshot::Missing {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("output `{}` was created concurrently", path.display()),
            )
            .into());
        }
        return Err(CliError::Other(format!(
            "output `{}` changed while awaiting confirmation",
            path.display()
        )));
    }
    if *expected == OutputSnapshot::Missing {
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)?;
    } else {
        temporary.persist(path).map_err(|error| error.error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_contract_spec::ContractSpec;

    fn write_empty_spec(path: &Path) {
        let spec = ContractSpec {
            interfaces: vec![].try_into().unwrap(),
            type_defs: vec![].try_into().unwrap(),
            events: vec![].try_into().unwrap(),
        };
        std::fs::write(path, xdrs::txs::encode(&spec).unwrap()).unwrap();
    }

    #[test]
    fn fresh_output_does_not_require_confirmation() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("empty.spec.xdr");
        let output = temp.path().join("bindings.ts");
        write_empty_spec(&source);

        run(&source, Some(&output), "codegen.ts", &Print::quiet()).unwrap();

        assert!(output.is_file());
    }

    #[test]
    fn existing_output_requires_force_when_non_interactive() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("empty.spec.xdr");
        let output = temp.path().join("bindings.ts");
        write_empty_spec(&source);
        std::fs::write(&output, "keep me").unwrap();

        let error = run(&source, Some(&output), "codegen.ts", &Print::quiet()).unwrap_err();

        assert!(matches!(error, CliError::ConfirmationRequired { .. }));
        assert_eq!(std::fs::read_to_string(output).unwrap(), "keep me");
    }

    #[cfg(unix)]
    #[test]
    fn output_symlink_counts_as_existing_and_is_not_followed() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("empty.spec.xdr");
        let target = temp.path().join("target.ts");
        let output = temp.path().join("bindings.ts");
        write_empty_spec(&source);
        std::fs::write(&target, "keep me").unwrap();
        std::os::unix::fs::symlink(&target, &output).unwrap();

        let error = run(&source, Some(&output), "codegen.ts", &Print::quiet()).unwrap_err();

        assert!(matches!(error, CliError::ConfirmationRequired { .. }));
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep me");
    }

    #[test]
    fn fresh_output_install_does_not_clobber_a_concurrent_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let output = temp.path().join("bindings.ts");
        std::fs::write(&output, "concurrent").unwrap();

        let error = write_output(&output, b"generated", &OutputSnapshot::Missing).unwrap_err();

        assert_eq!(error.code(), "io_error");
        assert_eq!(std::fs::read_to_string(output).unwrap(), "concurrent");
    }

    #[test]
    fn confirmed_overwrite_does_not_clobber_changed_output() {
        let temp = tempfile::TempDir::new().unwrap();
        let output = temp.path().join("bindings.ts");
        std::fs::write(&output, "reviewed").unwrap();
        let snapshot = output_snapshot(&output).unwrap();
        std::fs::write(&output, "changed concurrently").unwrap();

        let error = write_output(&output, b"generated", &snapshot).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("changed while awaiting confirmation")
        );
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "changed concurrently"
        );
    }
}
