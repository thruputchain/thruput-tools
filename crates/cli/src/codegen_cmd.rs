use std::path::Path;

use serde_json::json;

use crate::codegen::generate_ts_bindings;
use crate::config::ProfileOverrides;
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
    let spec = load_contract_spec_from_file(source)?.ok_or_else(|| {
        CliError::CodegenFailed(format!("no contract spec found in {}", source.display()))
    })?;
    let source_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let ts = generate_ts_bindings(&spec, source_name)?;
    write_bindings(&ts, &json!(source), output, cmd, print)
}

/// Generate bindings from a verified embedded spec at an onchain address.
pub fn run_onchain(
    address: &str,
    output: Option<&Path>,
    project_start: &Path,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let read = crate::contract_spec::onchain::read(address, project_start, profile, overrides)?;
    let spec = read.spec.ok_or_else(|| {
        CliError::CodegenFailed(format!(
            "no embedded spec for {} ({})",
            read.address, read.status
        ))
    })?;
    let source = read.address.to_string();
    let ts = generate_ts_bindings(&spec, &source)?;
    write_bindings(&ts, &json!(source), output, cmd, print)
}

fn write_bindings(
    ts: &str,
    source: &serde_json::Value,
    output: Option<&Path>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if let Some(output_path) = output {
        std::fs::write(output_path, ts.as_bytes())?;
        print.group(
            crate::print::Outcome::Success,
            "Bindings generated",
            |print| {
                print.success("Wrote TypeScript bindings");
                print.detail("output", output_path.display());
            },
        );
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
