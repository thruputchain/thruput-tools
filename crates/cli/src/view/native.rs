//! Explicit native invocation simulation for qualification builds.

use jsonrpsee::core::client::ClientT;
use schemars::JsonSchema;
use serde::Serialize;
use thruput_json_rpc_client::native::NativeClient;
use thruput_json_rpc_types::{
    requests::SimulateInvocationRequest,
    responses::{DecodedResponse, InvocationOutput, SimulationResult},
};

use super::{
    ArtifactOptions, CliError, CommandResult, ContractAddress, HexBytes, Path, PreparedView, Print,
    ProfileOverrides, decode_abort, decode_return, format_contract_address, prefixed_hex,
    prepare_view, selector_json,
};
use crate::abort_json::AbortReport;
use crate::result::CommandError;

/// Resolve the same local inputs as `view`, then simulate through an explicitly
/// selected native endpoint. No signing key is needed and no transaction is sent.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all)]
pub fn run(
    contract: &str,
    method: &str,
    args: &[String],
    calldata: Option<&str>,
    from: Option<&str>,
    package: Option<&str>,
    artifacts: &ArtifactOptions,
    project_start: &Path,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let task = crate::print::progress::Task::start(print, "Preparing simulation", true);
    let prepared = prepare_view(
        contract,
        method,
        args,
        calldata,
        from,
        package,
        artifacts,
        project_start,
        profile,
        overrides,
    )?;
    let network_hash = expected_network(&prepared.native_profile)?;
    let client = NativeClient::new(crate::http_client::rpc_client(&prepared.rpc_url)?);
    task.print()
        .phase(format_args!("Simulating `{}`", prepared.method));
    let result = rt.block_on(fetch_simulation(&client, &prepared, network_hash.as_ref()));
    let warnings = task.finish();
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            if !print.is_json() {
                print.warn(crate::terminal::terminal_safe(&error.to_string()));
                for warning in warnings {
                    print.warn(warning);
                }
            }
            let report = NativeViewFailure {
                common: NativeViewCommon::from(&prepared),
                evidence: error.details_json().unwrap_or(serde_json::Value::Null),
            };
            print.json(&CommandResult::failure_with_error(
                cmd,
                report,
                CommandError::from_cli_error(&error),
            ))?;
            return Err(CliError::CommandReportedFailure);
        }
    };
    let report = NativeViewReport::new(&prepared, response);
    let failure = execution_failure(&report.simulation.output);
    if !print.is_json() {
        print_response(&report, print);
        for warning in warnings {
            print.warn(warning);
        }
    }
    if let Some(error) = failure {
        print.json(&CommandResult::failure_with_error(cmd, report, error))?;
        return Err(CliError::CommandReportedFailure);
    }
    print.json(&CommandResult::new(cmd, report))?;
    Ok(())
}

// Match transaction preparation: a configured runtime manifest supplies the
// authoritative pin, even when the profile carries an older network hash.
// Loading the manifest does not load a signing key or write configuration.
fn expected_network(
    profile: &crate::config::ResolvedProfile,
) -> Result<Option<thruput_xdr::types::NetworkHash>, CliError> {
    if let Some(manifest) =
        crate::runtime_manifest::load_configured_runtime_manifest(profile, "view")?
    {
        return Ok(Some(manifest.network_hash.to_network_hash()));
    }
    profile
        .network_hash
        .as_deref()
        .map(crate::config::parse_network_hash)
        .transpose()
        .map(|hash| hash.map(|hash| hash.to_network_hash()))
}

async fn fetch_simulation<C: ClientT + Sync>(
    client: &NativeClient<C>,
    prepared: &PreparedView,
    expected_network: Option<&thruput_xdr::types::NetworkHash>,
) -> Result<SimulationResult, CliError> {
    let required = if expected_network.is_some() {
        &["simulateInvocation", "getNetworkInfo"][..]
    } else {
        &["simulateInvocation"][..]
    };
    thruput_client::native::check_endpoint(client, required)
        .await
        .map_err(|error| crate::native_rpc::rpc_error(&error))?;
    if let Some(expected) = expected_network {
        thruput_client::native::validate_network(client, expected)
            .await
            .map_err(|error| crate::native_rpc::rpc_error(&error))?;
    }
    let request = SimulateInvocationRequest {
        address: prepared.request.contract,
        entrypoint: prepared.request.entrypoint.clone(),
        calldata: prepared.request.calldata.clone(),
        caller: prepared.request.caller,
        assumed_auth: None,
        gas_limit: None,
        min_state_block_height: None,
    };
    match client
        .simulate_invocation(request)
        .await
        .map_err(|error| crate::native_rpc::client_error(&error))?
    {
        DecodedResponse::Known(result) => Ok(result),
        DecodedResponse::Unsupported { raw } => Err(crate::native_rpc::failure(
            "unsupported simulateInvocation result",
            raw,
        )),
        _ => Err(crate::native_rpc::failure(
            "unsupported simulateInvocation response representation",
            serde_json::Value::Null,
        )),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeViewCommon {
    schema_version: u32,
    rpc_url: String,
    address: ContractAddress,
    method: String,
    entrypoint: HexBytes,
    calldata: HexBytes,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller: Option<ContractAddress>,
    selector: serde_json::Value,
}

impl From<&PreparedView> for NativeViewCommon {
    fn from(prepared: &PreparedView) -> Self {
        Self {
            schema_version: 4,
            rpc_url: prepared.rpc_url.clone(),
            address: prepared.request.contract,
            method: prepared.method.clone(),
            entrypoint: prepared.request.entrypoint.clone(),
            calldata: prepared.request.calldata.clone(),
            caller: prepared.request.caller,
            selector: selector_json(&prepared.resolved),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeViewReport {
    #[serde(flatten)]
    common: NativeViewCommon,
    #[serde(flatten)]
    simulation: SimulationResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    return_value: Option<serde_json::Value>,
    #[serde(flatten)]
    abort: AbortReport,
}

impl NativeViewReport {
    fn new(prepared: &PreparedView, simulation: SimulationResult) -> Self {
        let metadata = prepared.resolved.metadata.as_ref();
        let return_value = match &simulation.output {
            InvocationOutput::Return { return_data } => decode_return(&return_data.0, metadata),
            _ => None,
        };
        let decoded_abort = match &simulation.output {
            InvocationOutput::Abort { abort_data } => Some(decode_abort(&abort_data.0, metadata)),
            _ => None,
        };
        Self {
            common: NativeViewCommon::from(prepared),
            simulation,
            return_value,
            abort: AbortReport::from_decode(decoded_abort.as_ref()),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeViewFailure {
    #[serde(flatten)]
    common: NativeViewCommon,
    evidence: serde_json::Value,
}

fn execution_failure(output: &InvocationOutput) -> Option<CommandError> {
    let (code, message) = match output {
        InvocationOutput::Return { .. } => return None,
        InvocationOutput::Abort { .. } => ("simulation_aborted", "Simulation aborted"),
        InvocationOutput::Fault {} => ("simulation_faulted", "Simulation faulted"),
        _ => ("native_rpc_failure", "Unsupported simulation output"),
    };
    Some(CommandError::new(
        code,
        message,
        serde_json::json!({ "output": output }),
    ))
}

fn print_response(report: &NativeViewReport, print: &Print) {
    let (outcome, title) = match &report.simulation.output {
        InvocationOutput::Return { .. } => (crate::print::Outcome::Success, "Simulation succeeded"),
        InvocationOutput::Abort { .. } => (crate::print::Outcome::Warning, "Simulation aborted"),
        InvocationOutput::Fault {} => (crate::print::Outcome::Error, "Simulation faulted"),
        _ => (
            crate::print::Outcome::Error,
            "Unsupported simulation output",
        ),
    };
    let mut fields = vec![
        (
            "Address".into(),
            format_contract_address(&report.common.address),
        ),
        ("Method".into(), report.common.method.clone()),
        (
            "State block height".into(),
            report.simulation.state_block_height.to_string(),
        ),
        ("Gas used".into(), report.simulation.gas_used.to_string()),
    ];
    match &report.simulation.output {
        InvocationOutput::Return { return_data } => fields.push((
            "Return".into(),
            report
                .return_value
                .as_ref()
                .map_or_else(|| prefixed_hex(&return_data.0), ToString::to_string),
        )),
        InvocationOutput::Abort { abort_data } => fields.push((
            "Abort".into(),
            report
                .abort
                .abort_value
                .as_ref()
                .map_or_else(|| prefixed_hex(&abort_data.0), ToString::to_string),
        )),
        _ => {}
    }
    for (index, event) in report.simulation.events.iter().enumerate() {
        let mut parts = vec![format_contract_address(&event.address)];
        parts.extend(
            event
                .topics
                .iter()
                .enumerate()
                .map(|(index, topic)| format!("topic{index}={}", prefixed_hex(topic.0))),
        );
        parts.push(format!("data={}", prefixed_hex(&event.data.0)));
        fields.push((format!("Event {index}"), parts.join(" ")));
    }
    let warnings: Vec<_> = report.abort.abort_decode_error.iter().cloned().collect();
    print.outcome_report(outcome, title, &fields, &warnings);
}

#[cfg(test)]
mod tests;
