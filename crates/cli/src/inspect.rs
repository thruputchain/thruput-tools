use std::path::Path;

use serde_json::{Value, json};

use crate::config::ProfileOverrides;
use crate::contract_spec::onchain;
use crate::result::CommandResult;
use crate::{CliError, Print};

mod summary;

/// Read deployment metadata and the embedded spec from the selected node.
pub fn run(
    address: &str,
    output: Option<&Path>,
    project_start: &Path,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let read = onchain::read(address, project_start, profile, overrides)?;
    let spec = if let Some(spec) = &read.spec {
        spec_json(spec)?
    } else {
        Value::Null
    };
    let data = json!({
        "schemaVersion": 1,
        "address": read.address.to_string(),
        "stateBlockHeight": read.state_block_height,
        "deployment": read.deployment,
        "specStatus": read.status,
        "spec": spec,
    });
    if let Some(path) = output {
        if data["spec"].is_null() {
            return Err(CliError::SpecFailed(format!(
                "no embedded spec to export ({})",
                read.status
            )));
        }
        let mut contents = serde_json::to_string_pretty(&data["spec"])
            .map_err(|e| CliError::SpecFailed(e.to_string()))?;
        contents.push('\n');
        std::fs::write(path, contents)?;
    }
    if !print.is_json() {
        summary::render(&data, print);
    }
    if let Some(path) = output {
        print.detail("Spec written", path.display());
    }
    print.json(&CommandResult::new(cmd, data))?;
    Ok(())
}

fn spec_json(spec: &thruput_contract_spec::ContractSpec) -> Result<Value, CliError> {
    // Preflight bounds type depth to 64. The human JSON shape adds
    // object/array levels and can exceed serde_json's default 128.
    let json = thruput_spec_tool::render_spec_json(spec);
    let mut reader = serde_json::Deserializer::from_str(&json);
    reader.disable_recursion_limit();
    <Value as serde::Deserialize>::deserialize(&mut reader)
        .map_err(|e| CliError::SpecFailed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_contract_spec::ContractTypeKind;

    #[test]
    fn renders_the_maximum_supported_tuple_depth() {
        let mut bytes = Vec::new();
        for word in [1_u32, 0, 0, 0, 1, 0, 0] {
            bytes.extend(word.to_be_bytes());
        }
        for _ in 1..64 {
            bytes.extend(ContractTypeKind::Tuple.0.to_be_bytes());
            bytes.extend(1_u32.to_be_bytes());
        }
        bytes.extend(ContractTypeKind::U8.0.to_be_bytes());
        bytes.extend([0; 12]);
        let spec = crate::contract_spec::load_contract_spec_from_xdr(&bytes).unwrap();
        let value = spec_json(&spec).unwrap();
        assert!(!value["interfaces"].as_array().unwrap().is_empty());
    }
}
