use std::path::Path;

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
    let spec = load_contract_spec_from_file(source)?.ok_or_else(|| {
        CliError::CodegenFailed(format!("no contract spec found in {}", source.display()))
    })?;
    let source_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let ts = generate_ts_bindings(&spec, source_name)?;

    if let Some(output_path) = output {
        std::fs::write(output_path, ts.as_bytes())?;
        print.status(format_args!("Wrote {}", output_path.display()));
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
