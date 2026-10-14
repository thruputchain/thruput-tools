use std::path::{Path, PathBuf};
use thruput_json_rpc_types::hex::decode_fixed_hex;

use serde_json::json;
use sha2::{Digest, Sha256};
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::receipt::TransactionReceipt;
use thruput_json_rpc_types::transaction::TxHash;
use thruput_signing::{
    BuildDeployTransaction, DEFAULT_TX_GAS_LIMIT, SchemeSigner, SignTransactionParams,
    build_deploy_transaction, derive_deploy_address, sign_and_encode_transaction_hex,
    source_address_auth_claim,
};
use thruput_xdr::transaction::Transaction;
use thruput_xdr::types::ContractAddress;
use thruput_xdr::types::{NetworkHash, SHA256Hash};
use xdr_common::VecM;

use crate::account_resolution::resolve_transaction_sender;
use crate::artifacts::{ArtifactKind, ArtifactOptions, ResolvedArtifact};
use crate::config::{ProfileOverrides, ResolvedProfile, resolve_profile_with_overrides};
use crate::dev_detail::{fetch_transaction_detail_blocking, print_transaction_detail};

use crate::result::{CommandError, CommandResult};
use crate::terminal::render_shell_arg;
use crate::tx_utils::{
    command_error_for_exec_status, format_tx_hash, nonce_from_timestamp, print_failed_receipt_data,
    resolve_block_expiry_height, submit_and_poll, summarize_exec_status,
};
use crate::{CliError, Print};

mod funding;

/// Current schema version for `deploy --json` data payloads.
pub const DEPLOY_JSON_SCHEMA_VERSION: u32 = 7;

/// HOG that `thruput test --integration` transfers to the contract it deploys.
///
/// Matches the `100_000_000` storage tokens the removed local dev wallet contract
/// moved to every address it deployed. hogd charges storage to the contract's
/// own address, so an unfunded contract's initializer would never commit.
pub(crate) const INTEGRATION_DEPLOY_FUND_HOG: &str = "10";

/// Explicit native deployment funding and initialization.
#[derive(Debug, Default)]
pub struct DeploymentOptions<'a> {
    /// Inspect deployment collateral without loading a signer or submitting.
    pub estimate: bool,
    /// Exact HOG transfer, including zero to skip funding.
    pub fund: Option<&'a str>,
    /// Method name from the artifact spec, or an explicit eight-byte selector.
    pub init_method: Option<&'a str>,
    /// Typed method arguments, or one raw hex argument with an explicit selector.
    pub init_args: &'a [String],
    /// Initializer gas budget. Defaults to the ordinary call budget.
    pub init_gas_limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct InitializerReport {
    method: String,
    entrypoint_id: String,
    #[serde(with = "thruput_json_rpc_types::decimal")]
    #[schemars(with = "thruput_json_rpc_types::decimal::Decimal<u64>")]
    gas_limit: u64,
    calldata: HexBytes,
}

/// Exact effects and identity of a native deployment.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeDeployment {
    sender: String,
    network_hash: String,
    contract_hash: String,
    deployment_nonce: String,
    #[serde(with = "thruput_json_rpc_types::decimal")]
    #[schemars(with = "thruput_json_rpc_types::decimal::Decimal<i64>")]
    expiry_height: i64,
    #[serde(with = "thruput_json_rpc_types::decimal")]
    #[schemars(with = "thruput_json_rpc_types::decimal::Decimal<u64>")]
    gas_limit: u64,
    funding_hog: String,
    funding_raw_amount: String,
    initializer: Option<InitializerReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeployAddressSource {
    Receipt,
    Derived,
}

/// Fully prepared deploy submission inputs shared by live execution and dry-run flows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedDeploy {
    initializer_metadata: Option<crate::codec::CallMetadata>,
    tx_hash: TxHash,
    native: Option<NativeDeployment>,
    pub(crate) rpc_url: String,
    pub(crate) profile_name: String,
    pub(crate) wallet_name: Option<String>,
    pub(crate) wallet_address: String,
    pub(crate) artifact: ResolvedArtifact,
    pub(crate) artifact_bytes_len: usize,
    pub(crate) predicted_address: String,
    pub(crate) wallet_entrypoint_id: String,
    pub(crate) wallet_entrypoint_source: &'static str,
    pub(crate) tx_hex: String,
    pub(crate) package_name: Option<String>,
}

#[derive(Debug)]
struct DeployPlan {
    initializer_metadata: Option<crate::codec::CallMetadata>,
    tx_hash: TxHash,
    native: Option<NativeDeployment>,
    rpc_url: String,
    profile_name: String,
    wallet_name: Option<String>,
    wallet_address: String,
    artifact: ResolvedArtifact,
    artifact_bytes_len: usize,
    predicted_address: String,
    wallet_entrypoint_id: String,
    wallet_entrypoint_source: &'static str,
    package_name: Option<String>,
    transaction: Transaction,
    signer: SchemeSigner,
    network_hash: NetworkHash,
}

/// Submitted deploy metadata after a live deploy transaction is accepted.
#[derive(Debug, Clone)]
pub(crate) struct SubmittedDeploy {
    pub(crate) tx_hash: TxHash,
    pub(crate) receipt: TransactionReceipt,
    pub(crate) contract_address: Option<String>,
    pub(crate) address_source: Option<DeployAddressSource>,
}

/// Deploy a contract to the network.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn run(
    artifact_path: Option<&Path>,
    options: &DeploymentOptions<'_>,
    package: Option<&str>,
    alias: Option<&str>,
    dry_run: bool,
    force: bool,
    wallet: Option<&str>,
    artifacts: &ArtifactOptions,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let resolved = resolve_profile_with_overrides(&cwd, profile, overrides)?;
    if options.estimate {
        let artifact = if let Some(path) = artifact_path {
            ResolvedArtifact {
                kind: crate::artifacts::infer_artifact_kind(path)?,
                path: path.to_path_buf(),
            }
        } else {
            crate::artifacts::resolve_thruput_artifact(&cwd, artifacts, package)?.artifact
        };
        let bytes = std::fs::read(&artifact.path)?;
        let report = funding::read(
            resolved.rpc_endpoint()?,
            &bytes,
            options.init_method.is_some(),
        )?;
        print.group(crate::print::Outcome::DryRun, "Deployment funding", |print| {
            print.detail("artifact bytes", report.artifact_bytes);
            print.detail("balance deposit HOG", crate::hog::format_amount(report.balance_deposit()));
            print.detail("deployment collateral HOG", crate::hog::format_amount(report.deployment_cost()));
            print.detail("minimum funding HOG", &report.minimum_funding_hog);
            print.detail("observed height", *report.execution.observed_tip.height);
            print.note("The minimum leaves no free balance for future storage. Sender storage and gas capacity are separate.");
            if options.init_method.is_some() {
                print.note("Initializer storage funding is unknown; choose an explicit total --fund amount.");
            }
        });
        print.json(&CommandResult::new(cmd, report))?;
        return Ok(());
    }
    if !dry_run {
        print.require_confirmation_available(force, "deploy a contract")?;
    }
    let plan = prepare_deploy_plan(
        &cwd,
        &resolved,
        artifact_path,
        options,
        package,
        artifacts,
        wallet,
        print,
    )?;
    if !dry_run
        && !print.confirm_review(
            "Review deployment",
            force,
            "deploy a contract",
            "Deploy this contract?",
            |print| {
                print.detail("artifact", plan.artifact.path.display());
                if let Some(package) = package {
                    print.detail("package", package);
                }
                print.detail(
                    "wallet",
                    plan.wallet_name.as_deref().unwrap_or(&plan.wallet_address),
                );
                print.detail("profile", &plan.profile_name);
                if let Some(native) = &plan.native {
                    print.detail("RPC", &plan.rpc_url);
                    print.detail("sender", &native.sender);
                    print.detail("network hash", &native.network_hash);
                    print.detail("contract hash", &native.contract_hash);
                    print.detail("deployment nonce", &native.deployment_nonce);
                    print.detail("predicted address", &plan.predicted_address);
                    print.detail("funding HOG", &native.funding_hog);
                    print_initializer(native.initializer.as_ref(), print);
                    print.detail("expiry height", native.expiry_height);
                    print.detail("gas budget", native.gas_limit);
                    print.detail("tx", format_tx_hash(&plan.tx_hash));
                    print.warn("Funding, deployment and optional initialization commit atomically; retained balance depends on storage collateral.");
                }
            },
        )?
    {
        print.cancelled();
        return Ok(());
    }
    let prepared = sign_deploy_plan(plan)?;
    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    crate::tx_utils::print_prepared_transaction(&prepared.tx_hex, print);
    let task = crate::print::progress::Task::start(print, "Submitting deployment", true);
    task.print().phase(format_args!(
        "Deploying {} artifact",
        prepared.artifact.kind.as_str()
    ));

    let submitted = match submit_prepared_deploy_and_resolve(&prepared) {
        Ok(submitted) => submitted,
        Err(error @ CliError::NativeDeploymentUnverified { .. }) if prepared.native.is_some() => {
            let _ = task.finish();
            return report_unverified_native_deploy(&prepared, &error, cmd, print);
        }
        Err(error) => return Err(error),
    };
    let tx_hash = submitted.tx_hash;
    let receipt = submitted.receipt;
    let transaction_detail =
        fetch_transaction_detail_blocking(&prepared.rpc_url, tx_hash, task.print());
    let warnings = task.finish();
    let contract_addr_hex = submitted.contract_address;
    let address_source = submitted.address_source;

    // Print result using the invocation directory retained before submission.
    let tx_hash_formatted = format_tx_hash(&tx_hash);
    let decoded_abort =
        crate::tx_utils::decode_receipt_abort(&receipt, prepared.initializer_metadata.as_ref());
    let insufficient_funding =
        is_insufficient_deploy_funding(&receipt, decoded_abort.as_ref(), Some(&prepared));
    print.group(
        if matches!(
            receipt.status,
            thruput_json_rpc_types::receipt::ExecStatus::Ok
        ) {
            crate::print::Outcome::Success
        } else {
            crate::print::Outcome::Error
        },
        "Deployment",
        |print| {
            match &receipt.status {
                thruput_json_rpc_types::receipt::ExecStatus::Ok => {
                    let contract_addr_hex = contract_addr_hex
                        .as_deref()
                        .expect("bug: successful deploy must resolve a contract address");
                    print.success("Deployed contract");
                    print.detail("address", contract_addr_hex);
                    if matches!(address_source, Some(DeployAddressSource::Derived)) {
                        print.warn(
                            "receipt omitted deploy return_data; address was derived locally from \
                     deployer, nonce, and contract hash",
                        );
                    }
                    print.detail("tx", &tx_hash_formatted);
                    print.detail("height", *receipt.height);
                    print.detail("gas used", receipt.gas_used);
                }
                other => {
                    if insufficient_funding {
                        print_insufficient_deploy_funding(&prepared, print);
                    } else {
                        print.warn(format_args!("Deploy {}", summarize_exec_status(other)));
                    }
                    print.detail("tx", &tx_hash_formatted);
                    print.detail("height", *receipt.height);
                    print_failed_receipt_data(&receipt, decoded_abort.as_ref(), print);
                }
            }
            for warning in &warnings {
                print.warn(warning);
            }
            if let Some(detail) = transaction_detail.as_ref() {
                print_transaction_detail(detail, print);
            }
        },
    );

    // Save contract alias
    let mut alias_warnings: Vec<crate::checks::Finding> = Vec::new();
    if matches!(
        &receipt.status,
        thruput_json_rpc_types::receipt::ExecStatus::Ok
    ) {
        // `--alias` wins; otherwise the alias is the package name, which is what
        // an author reads back with `thruput call <name>`. There is no third
        // source. `--artifact` names a bare binary and carries no package, so
        // with no `--alias` nothing is recorded: this used to fall back to the
        // project directory name, which agrees with the package name only by
        // luck in a single-package project and disagrees in any workspace.
        //
        // `cwd` is a search start, not a project root: profile_write_path
        // resolves it to the nearest thruput.toml, or the global config when
        // there is none, so an alias deployed outside a project lands in the
        // global profile rather than being dropped. With no profile anywhere,
        // the save reports NoActiveProfile and that surfaces as the warning
        // below.
        if let Some(contract_addr_hex) = contract_addr_hex.as_deref() {
            match alias.or(prepared.package_name.as_deref()) {
                Some(name) => match crate::config::save_profile_contract_alias(
                    &cwd,
                    Some(&prepared.profile_name),
                    name,
                    contract_addr_hex,
                ) {
                    Ok(_) => print.detail("alias", format_args!("{name} -> {contract_addr_hex}")),
                    // The contract is deployed; only the local record failed.
                    // Say that rather than confirming an alias that was never
                    // written, and do not fail a command whose on-chain work
                    // succeeded. `print.warn` is a no-op under --json, so the
                    // same fact also goes into the envelope's warnings, where a
                    // script can see it.
                    Err(error) => {
                        let message = format!(
                            "deployed, but recording the alias `{name}` in thruput.toml \
                             failed: {error}"
                        );
                        print.warn(&message);
                        alias_warnings.push(crate::checks::Finding {
                            level: crate::checks::FindingLevel::Warn,
                            code: "deploy.alias_not_saved",
                            message,
                            hint: Some(
                                "the contract is deployed; re-record the alias by hand in \
                                 thruput.toml"
                                    .into(),
                            ),
                        });
                    }
                },
                None => print.detail(
                    "alias",
                    "not recorded (no package name); pass --alias to name it",
                ),
            }
        }
    }

    let command_error =
        command_error_for_deploy_receipt(&receipt, decoded_abort.as_ref(), Some(&prepared));
    let mut data = json!({
        "schemaVersion": DEPLOY_JSON_SCHEMA_VERSION,
        "txHash": tx_hash_formatted,
        "artifactKind": prepared.artifact.kind.as_str(),
        "artifactPath": &prepared.artifact.path,
        "artifactBytes": prepared.artifact_bytes_len.to_string(),
        "address": contract_addr_hex,
        "addressSource": match address_source {
            Some(DeployAddressSource::Receipt) => Some("receipt"),
            Some(DeployAddressSource::Derived) => Some("derived"),
            None => None,
        },
        "height": receipt.height.to_string(),
        "status": receipt.status,
        "gasUsed": receipt.gas_used.to_string(),
        "transactionDetail": transaction_detail,
        "walletEntrypointId": prepared.wallet_entrypoint_id,
        "walletEntrypointSource": prepared.wallet_entrypoint_source,
    });
    if let Some(native) = &prepared.native {
        data["nativeDeployment"] =
            serde_json::to_value(native).map_err(|e| CliError::Other(e.to_string()))?;
        data["predictedAddress"] = json!(prepared.predicted_address);
        data["rpcUrl"] = json!(prepared.rpc_url);
    }
    if command_error.is_some() {
        data["returnData"] = json!(receipt.return_data);
        data["abortData"] = json!(receipt.abort_data);
        crate::tx_utils::insert_abort_json(&mut data, decoded_abort.as_ref());
    }
    if let Some(error) = command_error {
        print.json(&CommandResult::failure_with_error(cmd, data, error))?;
        return Err(CliError::CommandReportedFailure);
    }
    print.json(&CommandResult::with_warnings(cmd, alias_warnings, data))?;

    Ok(())
}

fn command_error_for_deploy_receipt(
    receipt: &TransactionReceipt,
    decoded_abort: Option<&Result<crate::codec::DecodedAbort, crate::codec::AbortDecodeError>>,
    prepared: Option<&PreparedDeploy>,
) -> Option<CommandError> {
    if is_insufficient_deploy_funding(receipt, decoded_abort, prepared) {
        let hint = prepared.map_or_else(
            || {
                "fund the signing wallet before retrying; when the selected profile has a faucet, run `thruput airdrop <WALLET>`".to_owned()
            },
            |prepared| {
                let [balance, airdrop] = deploy_funding_commands(prepared);
                format!(
                    "fund the signing wallet before retrying; inspect it with `{balance}`, then run `{airdrop}` when the selected profile has a faucet"
                )
            },
        );
        return Some(
            CommandError::new(
                "transaction_insufficient_funds",
                "the signing wallet has insufficient HOG to fund another contract deployment",
                json!({
                    "reason": "nativeHogTransferOverdraft",
                    "walletEntrypoint": "deploy",
                    "erc20Error": "tooLarge",
                }),
            )
            .with_hint(hint),
        );
    }
    command_error_for_exec_status(&receipt.status)
}

fn is_insufficient_deploy_funding(
    receipt: &TransactionReceipt,
    decoded_abort: Option<&Result<crate::codec::DecodedAbort, crate::codec::AbortDecodeError>>,
    prepared: Option<&PreparedDeploy>,
) -> bool {
    // Forwarded errors do not identify their stage. An initializer may also
    // transfer tokens, so its ERC20 error is not proof of a funding failure.
    if prepared.is_some_and(|prepared| {
        prepared
            .native
            .as_ref()
            .is_some_and(|native| native.initializer.is_some())
    }) {
        return false;
    }
    if !matches!(
        receipt.status,
        thruput_json_rpc_types::receipt::ExecStatus::Reverted { .. }
    ) {
        return false;
    }
    crate::codec::is_erc20_too_large(decoded_abort)
}

fn deploy_funding_commands(prepared: &PreparedDeploy) -> [String; 2] {
    let wallet = prepared
        .wallet_name
        .as_deref()
        .unwrap_or(&prepared.wallet_address);
    let wallet = render_shell_arg(wallet);
    let profile = render_shell_arg(&format!("--profile={}", prepared.profile_name));
    [
        format!("thruput balance {profile} -- {wallet}"),
        format!("thruput airdrop {profile} -- {wallet}"),
    ]
}

fn print_insufficient_deploy_funding(prepared: &PreparedDeploy, print: &Print) {
    let wallet = prepared
        .wallet_name
        .as_deref()
        .unwrap_or(&prepared.wallet_address);
    let wallet = crate::terminal::terminal_safe(wallet);
    print.warn(format_args!(
        "Deploy failed because wallet `{wallet}` has insufficient HOG for deployment"
    ));
    print.detail("wallet address", &prepared.wallet_address);
    print.detail(
        "profile",
        crate::terminal::terminal_safe(&prepared.profile_name),
    );
    print.status(
        "The project alias and prior deployment are not the cause; fund the selected wallet and retry the original deploy command.",
    );
    print.next_steps(&deploy_funding_commands(prepared));
}

fn print_dry_run(
    prepared: &PreparedDeploy,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.group(crate::print::Outcome::DryRun, "Deployment", |print| {
        print.status(format_args!(
            "Prepared dry-run deploy for {} artifact ({} bytes) to {}.",
            prepared.artifact.kind.as_str(),
            prepared.artifact_bytes_len,
            prepared.rpc_url
        ));
        print.status(format_args!(
            "  predicted address: {}",
            prepared.predicted_address
        ));
        if let Some(native) = &prepared.native {
            print.detail("sender", &native.sender);
            print.detail("network hash", &native.network_hash);
            print.detail("contract hash", &native.contract_hash);
            print.detail("deployment nonce", &native.deployment_nonce);
            print.detail("funding HOG", &native.funding_hog);
            print_initializer(native.initializer.as_ref(), print);
            print.detail("expiry height", native.expiry_height);
            print.detail("gas budget", native.gas_limit);
            print.warn(
                "Funding is the authorized transfer, not a minimum-cost estimate or final balance.",
            );
        }
        print.detail("tx", format_tx_hash(&prepared.tx_hash));
        print.status(format_args!("  signed tx: {}", prepared.tx_hex));
        print.status("  submitted: no (--dry-run)");
    });

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": DEPLOY_JSON_SCHEMA_VERSION,
            "dryRun": true,
            "submitted": false,
            "rpcUrl": prepared.rpc_url,
            "artifactKind": prepared.artifact.kind.as_str(),
            "artifactPath": &prepared.artifact.path,
            "artifactBytes": prepared.artifact_bytes_len.to_string(),
            "predictedAddress": prepared.predicted_address,
            "walletEntrypointId": prepared.wallet_entrypoint_id,
            "walletEntrypointSource": prepared.wallet_entrypoint_source,
            "signedTx": prepared.tx_hex,
            "txHash": format_tx_hash(&prepared.tx_hash),
            "nativeDeployment": prepared.native,
        }),
    ))?;

    Ok(())
}

/// Submit a previously prepared deploy transaction and wait for its receipt.
pub(crate) fn submit_prepared_deploy(
    prepared: &PreparedDeploy,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    if prepared.native.is_some() {
        return rt.block_on(async {
            let client = crate::http_client::rpc_client(&prepared.rpc_url)?;
            submit_native_deploy(prepared, &client, thruput_client::PollOptions::default()).await
        });
    }
    rt.block_on(async { submit_and_poll(&prepared.rpc_url, &prepared.tx_hex).await })
}

/// Submit a deploy transaction and resolve the deployed contract address when successful.
pub(crate) fn submit_prepared_deploy_and_resolve(
    prepared: &PreparedDeploy,
) -> Result<SubmittedDeploy, CliError> {
    let (tx_hash, receipt) = submit_prepared_deploy(prepared)?;
    let predicted_address = ContractAddress::from(hex_to_32_bytes(&prepared.predicted_address)?);
    let (contract_address, address_source) = match &receipt.status {
        thruput_json_rpc_types::receipt::ExecStatus::Ok => {
            let (address, source) = if prepared.native.is_some() {
                resolve_native_deployed_address(receipt.return_data.as_ref(), &predicted_address)?
            } else {
                resolve_deployed_address(receipt.return_data.as_ref(), &predicted_address)?
            };
            (Some(address), Some(source))
        }
        _ => (None, None),
    };

    Ok(SubmittedDeploy {
        tx_hash,
        receipt,
        contract_address,
        address_source,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn prepare_deploy_plan(
    cwd: &Path,
    resolved: &ResolvedProfile,
    artifact_path: Option<&Path>,
    options: &DeploymentOptions<'_>,
    package: Option<&str>,
    artifacts: &ArtifactOptions,
    wallet: Option<&str>,
    print: &Print,
) -> Result<DeployPlan, CliError> {
    resolved.rpc_endpoint()?;

    let (artifact, package_name) = if let Some(path) = artifact_path {
        (
            ResolvedArtifact {
                kind: crate::artifacts::infer_artifact_kind(path)?,
                path: path.to_path_buf(),
            },
            None,
        )
    } else {
        let resolved_artifact =
            crate::artifacts::resolve_thruput_artifact(cwd, artifacts, package)?;
        warn_if_stale(
            &resolved_artifact.artifact,
            &resolved_artifact.package_root,
            &resolved_artifact.package_name,
            print,
        );
        (
            resolved_artifact.artifact,
            Some(resolved_artifact.package_name),
        )
    };
    let artifact_bytes = std::fs::read(&artifact.path)?;

    prepare_deploy_transaction_plan(
        artifact,
        artifact_bytes,
        package_name,
        resolved,
        wallet,
        options,
        None,
    )
}

/// Build and sign the deploy transaction `thruput test --integration` submits.
///
/// The local-user sender is a native account, so the deployment funds the new
/// contract with [`INTEGRATION_DEPLOY_FUND_HOG`].
pub(crate) fn prepare_deploy_for_rpc_with_manifest(
    artifact_path: &Path,
    rpc_url: &str,
    key_name: &str,
    signer: thruput_signing::LocalSigner,
    runtime_manifest: Option<PathBuf>,
) -> Result<PreparedDeploy, CliError> {
    let artifact_bytes = std::fs::read(artifact_path)?;
    let profile = ResolvedProfile {
        name: "integration".into(),
        network: None,
        rpc_url: Some(rpc_url.into()),
        network_hash: None,
        rpc_url_cli_override: false,
        faucet_url: None,
        default_key: Some(key_name.into()),
        default_wallet: None,
        runtime_manifest,
        runtime_manifest_cli_override: false,
        runtime_artifacts_dir: None,
        contracts: std::collections::BTreeMap::default(),
        wallets: std::collections::BTreeMap::default(),
        source: None,
    };
    let account =
        thruput_signing::PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
    let sender = crate::account_resolution::ResolvedTransactionSender {
        native_account: Some(account.clone()),
        key_name: key_name.into(),
        wallet_name: None,
        wallet_address: account.address(),
        signer: signer.into(),
        network_hash: crate::account_resolution::resolve_network_hash(&profile, key_name)?,
    };
    sign_deploy_plan(prepare_deploy_transaction_plan(
        ResolvedArtifact {
            kind: ArtifactKind::Rv32,
            path: artifact_path.to_path_buf(),
        },
        artifact_bytes,
        None,
        &profile,
        None,
        &DeploymentOptions {
            fund: Some(INTEGRATION_DEPLOY_FUND_HOG),
            ..DeploymentOptions::default()
        },
        Some(sender),
    )?)
}

#[allow(clippy::too_many_lines)]
fn prepare_deploy_transaction_plan(
    artifact: ResolvedArtifact,
    artifact_bytes: Vec<u8>,
    package_name: Option<String>,
    profile: &ResolvedProfile,
    selected_wallet: Option<&str>,
    options: &DeploymentOptions<'_>,
    sender: Option<crate::account_resolution::ResolvedTransactionSender>,
) -> Result<DeployPlan, CliError> {
    let rpc_url = profile.rpc_endpoint()?;
    if options.init_method.is_none()
        && (!options.init_args.is_empty() || options.init_gas_limit.is_some())
    {
        return Err(CliError::InvalidDeploymentInitializer {
            message: "initializer arguments and gas require --init <METHOD>".into(),
        });
    }
    if options.init_gas_limit == Some(0) {
        return Err(CliError::InvalidDeploymentInitializer {
            message: "initializer gas must be greater than zero".into(),
        });
    }

    // Build the deploy transaction — invokes the sender's wallet contract.
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let sender = match sender {
        Some(sender) => sender,
        None => resolve_transaction_sender(profile, selected_wallet)?,
    };
    let funding = if options.fund == Some("minimum") {
        if sender.native_account.is_none() {
            return Err(CliError::NativeDeploymentOptionsForLegacy);
        }
        if options.init_method.is_some() {
            return Err(CliError::DeploymentFunding { message: "--fund minimum cannot determine initializer storage; use --estimate and choose an explicit --fund amount".into() });
        }
        Some(funding::read(rpc_url, &artifact_bytes, false)?.minimum_funding())
    } else {
        resolve_native_funding(sender.native_account.is_some(), options.fund)?
    };

    if sender.native_account.is_none() && options.init_method.is_some() {
        return Err(CliError::NativeDeploymentOptionsForLegacy);
    }
    let init_resolved = options
        .init_method
        .map(|method| crate::call::resolve_artifact_call(&artifact.path, method))
        .transpose()?;
    let init_call = init_resolved
        .as_ref()
        .map(|resolved| {
            let calldata =
                crate::codec::encode_args(options.init_args, resolved.metadata.as_ref())?;
            Ok::<_, CliError>((resolved.entrypoint, calldata))
        })
        .transpose()?;
    let init_gas_limit = options
        .init_gas_limit
        .unwrap_or(crate::call::DEFAULT_CALL_GAS_LIMIT);
    let gas_limit = if init_call.is_some() {
        DEFAULT_TX_GAS_LIMIT
            .checked_add(init_gas_limit)
            .ok_or_else(|| CliError::InvalidDeploymentInitializer {
                message: "initializer gas plus deployment budget exceeds u64".into(),
            })?
    } else {
        DEFAULT_TX_GAS_LIMIT
    };

    let artifact_bytes_len = artifact_bytes.len();
    let wallet_address = sender.wallet_address;

    let blob = thruput_xdr::contract::ContractBlob {
        data: xdr_common::BytesM(artifact_bytes),
    };
    let contract = match artifact.kind {
        ArtifactKind::Rv32 => thruput_xdr::contract::Contract::kRv32 {
            rv32_contract: blob,
        },
    };

    // Compute the contract hash: sha256(xdr_encode(contract)).
    let contract_xdr = xdrs::txs::encode(&contract)
        .map_err(|e| CliError::Other(format!("failed to XDR-encode contract: {e}")))?;
    let contract_hash_bytes: [u8; 32] = Sha256::digest(&contract_xdr).into();
    let contract_hash = SHA256Hash::from(contract_hash_bytes);

    // Create a deploy nonce for address derivation.
    let deploy_nonce_val = nonce_from_timestamp();
    let deploy_nonce_bytes: [u8; 32] = Sha256::digest(deploy_nonce_val.to_le_bytes()).into();
    let deploy_nonce = SHA256Hash::from(deploy_nonce_bytes);
    let predicted_address = derive_deploy_address(&wallet_address, &deploy_nonce, &contract_hash)
        .map_err(|e| CliError::Other(e.to_string()))?;

    let wallet_context = sender.signing_context(profile, &rt)?;
    let block_expiry_height = rt.block_on(async { resolve_block_expiry_height(rpc_url).await })?;
    let tx = if let Some(account) = &sender.native_account {
        thruput_signing::default_wallet::build_default_wallet_deploy_transaction(
            thruput_signing::default_wallet::BuildDefaultWalletDeployTransaction {
                account,
                deployment_nonce: deploy_nonce,
                code: thruput_signing::default_wallet::DeploymentCode::Attached(contract),
                amount: funding.expect("bug: native funding validated above"),
                initializer: init_call.as_ref().map(|(entrypoint, calldata)| {
                    thruput_signing::default_wallet::WalletInitCall {
                        entrypoint: *entrypoint,
                        gas_limit: init_gas_limit,
                        calldata,
                    }
                }),
                gas_limit,
                block_expiry_height,
                nonce: deploy_nonce_val,
            },
        )
        .map_err(|e| CliError::Other(e.to_string()))?
    } else {
        let calldata = xdrs::cxs::encode(&(&deploy_nonce, &contract_hash))
            .map_err(|e| CliError::Other(format!("failed to encode deploy calldata: {e}")))?;
        build_deploy_transaction(BuildDeployTransaction {
            invoked_address: wallet_address,
            entrypoint: wallet_context.entrypoints.deploy,
            calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height,
            nonce: deploy_nonce_val,
            contracts_to_deploy: VecM(vec![contract]),
            auth_claim: source_address_auth_claim(),
        })
    };
    let tx_hash = TxHash(
        thruput_signing::transaction_hash(&tx)
            .map_err(|e| CliError::Other(e.to_string()))?
            .into(),
    );
    let native = funding.map(|amount| NativeDeployment {
        sender: crate::codec::format_contract_address(&wallet_address),
        network_hash: format!("0x{}", hex::encode(wallet_context.network_hash.as_ref())),
        contract_hash: format!("0x{}", hex::encode(contract_hash.as_ref())),
        deployment_nonce: format!("0x{}", hex::encode(deploy_nonce.as_ref())),
        expiry_height: block_expiry_height,
        gas_limit,
        funding_hog: crate::hog::format_amount(amount),
        funding_raw_amount: amount.to_string(),
        initializer: init_call.map(|(entrypoint, calldata)| InitializerReport {
            method: options
                .init_method
                .expect("bug: initializer was resolved")
                .to_owned(),
            entrypoint_id: crate::call::format_entrypoint_id(entrypoint),
            gas_limit: init_gas_limit,
            calldata: HexBytes(calldata),
        }),
    });

    Ok(DeployPlan {
        initializer_metadata: init_resolved.and_then(|resolved| resolved.metadata),
        tx_hash,
        native,
        rpc_url: rpc_url.to_owned(),
        profile_name: profile.name.clone(),
        wallet_name: sender.wallet_name,
        wallet_address: format!("0x{}", hex::encode(wallet_address.as_ref())),
        artifact,
        artifact_bytes_len,
        predicted_address: format!("0x{}", hex::encode(predicted_address.as_ref())),
        wallet_entrypoint_id: format!(
            "0x{}",
            hex::encode(wallet_context.entrypoints.deploy.into_inner())
        ),
        wallet_entrypoint_source: wallet_context.entrypoints.source,
        package_name,
        transaction: tx,
        signer: sender.signer,
        network_hash: wallet_context.network_hash,
    })
}

fn sign_deploy_plan(plan: DeployPlan) -> Result<PreparedDeploy, CliError> {
    let tx_hex = sign_and_encode_transaction_hex(
        plan.transaction,
        &plan.signer,
        SignTransactionParams::default(),
        &plan.network_hash,
    )
    .map_err(|error| CliError::Other(error.to_string()))?;
    Ok(PreparedDeploy {
        initializer_metadata: plan.initializer_metadata,
        tx_hash: plan.tx_hash,
        native: plan.native,
        rpc_url: plan.rpc_url,
        profile_name: plan.profile_name,
        wallet_name: plan.wallet_name,
        wallet_address: plan.wallet_address,
        artifact: plan.artifact,
        artifact_bytes_len: plan.artifact_bytes_len,
        predicted_address: plan.predicted_address,
        wallet_entrypoint_id: plan.wallet_entrypoint_id,
        wallet_entrypoint_source: plan.wallet_entrypoint_source,
        tx_hex,
        package_name: plan.package_name,
    })
}

fn print_initializer(initializer: Option<&InitializerReport>, print: &Print) {
    if let Some(initializer) = initializer {
        print.detail("initializer", &initializer.method);
        print.detail("initializer entrypoint", &initializer.entrypoint_id);
        print.detail("initializer gas", initializer.gas_limit);
        print.detail(
            "initializer calldata",
            format!("0x{}", hex::encode(&initializer.calldata.0)),
        );
    }
}

fn resolve_native_funding(native: bool, fund: Option<&str>) -> Result<Option<u128>, CliError> {
    if !native {
        if fund.is_some() {
            return Err(CliError::NativeDeploymentOptionsForLegacy);
        }
        return Ok(None);
    }
    let amount = fund.ok_or(CliError::NativeDeploymentFundingRequired)?;
    match thruput_signing::parse_hog_amount(amount) {
        Ok(amount) => Ok(Some(amount)),
        Err(thruput_signing::ParseHogAmountError::Zero) => Ok(Some(0)),
        Err(error) => Err(CliError::InvalidHogAmount {
            input: amount.trim().to_owned(),
            message: error.to_string(),
        }),
    }
}

async fn submit_native_deploy(
    prepared: &PreparedDeploy,
    client: &(impl jsonrpsee::core::client::ClientT + Sync),
    polling: thruput_client::PollOptions,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    let returned =
        thruput_json_rpc_client::submit_transaction_with_client(client, &prepared.tx_hex)
            .await
            .map_err(|error| native_submission_error(&error))?;
    if returned != prepared.tx_hash {
        return Err(CliError::NativeDeploymentUnverified {
            message: format!(
                "endpoint returned {}, expected {}",
                format_tx_hash(&returned),
                format_tx_hash(&prepared.tx_hash)
            ),
        });
    }
    let receipt = thruput_json_rpc_client::poll_transaction_receipt_with_client_config(
        client,
        prepared.tx_hash,
        polling.max_attempts,
        polling.interval,
    )
    .await
    .map_err(|error| {
        let error = crate::tx_utils::map_poll_receipt_error(error);
        if matches!(error, CliError::TransactionInvalid { .. }) {
            error
        } else {
            CliError::NativeDeploymentUnverified {
                message: error.to_string(),
            }
        }
    })?;
    if receipt.tx_hash != prepared.tx_hash {
        return Err(CliError::NativeDeploymentUnverified {
            message: "receipt hash does not match the submitted transaction".into(),
        });
    }
    Ok((prepared.tx_hash, receipt))
}

fn native_submission_error(error: &jsonrpsee::core::ClientError) -> CliError {
    let message = crate::tx_utils::rpc_error_message(error);
    if crate::tx_utils::is_unsupported_rpc_method_error(error) {
        CliError::RpcCapabilityUnsupported {
            method: "sendRawTransaction",
            message,
        }
    } else if crate::tx_utils::is_conclusive_send_error(error) {
        CliError::RpcMethodFailed {
            method: "sendRawTransaction",
            message,
        }
    } else {
        CliError::NativeDeploymentUnverified {
            message: format!("sendRawTransaction failed: {message}"),
        }
    }
}

fn resolve_native_deployed_address(
    return_data: Option<&HexBytes>,
    predicted: &ContractAddress,
) -> Result<(String, DeployAddressSource), CliError> {
    match return_data {
        Some(data) if data.0.as_slice() == predicted.as_ref() => Ok((crate::codec::format_contract_address(predicted), DeployAddressSource::Receipt)),
        _ => Err(CliError::NativeDeploymentUnverified { message: "successful receipt must return the exact predicted 32-byte contract address; alias was not saved".into() }),
    }
}

fn report_unverified_native_deploy(
    prepared: &PreparedDeploy,
    error: &CliError,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let tx_hash = format_tx_hash(&prepared.tx_hash);
    let inspect = format!(
        "thruput tx show {tx_hash} {} {}",
        render_shell_arg(&format!("--profile={}", prepared.profile_name)),
        render_shell_arg(&format!("--rpc-url={}", prepared.rpc_url))
    );
    let hint = format!(
        "reconcile the original transaction with `{inspect}` before another deployment; rerunning deploy generates a new nonce and address. A timeout or unknown status does not prove failure"
    );
    print.warn(error.to_string());
    print.detail("tx", &tx_hash);
    print.detail("predicted address", &prepared.predicted_address);
    print.detail("signed tx", &prepared.tx_hex);
    print.note(&hint);
    print.json(&CommandResult::failure_with_error(
        cmd,
        json!({
            "schemaVersion": DEPLOY_JSON_SCHEMA_VERSION, "confirmation": "unverified",
            "txHash": tx_hash, "rpcUrl": prepared.rpc_url, "profile": prepared.profile_name,
            "predictedAddress": prepared.predicted_address, "signedTx": prepared.tx_hex,
            "nativeDeployment": prepared.native,
        }),
        CommandError::from_cli_error(error).with_hint(hint),
    ))?;
    Err(CliError::CommandReportedFailure)
}

/// Parse a `0x`-prefixed hex string into a 32-byte array.
fn hex_to_32_bytes(hex_str: &str) -> Result<[u8; 32], CliError> {
    decode_fixed_hex(hex_str)
        .map_err(|error| CliError::Other(format!("invalid predicted address: {error}")))
}

fn warn_if_stale(
    artifact: &ResolvedArtifact,
    package_root: &Path,
    package_name: &str,
    print: &Print,
) {
    let Ok(artifact_mtime) = std::fs::metadata(&artifact.path).and_then(|m| m.modified()) else {
        return;
    };
    if newest_input_mtime(package_root).is_some_and(|t| t > artifact_mtime) {
        print.warn(stale_artifact_warning(artifact.kind, package_name));
    }
}

fn stale_artifact_warning(kind: ArtifactKind, package_name: &str) -> String {
    format!(
        "source files or Cargo.toml are newer than the {} artifact — run `thruput build -p {package_name}` to avoid deploying stale code",
        kind.as_str()
    )
}

/// When the package's build inputs last changed: its `src/` tree and its
/// manifest. The manifest counts because `[package.metadata.thruput]`
/// changes the artifact (its guest memory) without touching a source file.
/// `None` when the package has no `src/` directory to speak for.
fn newest_input_mtime(package_root: &Path) -> Option<std::time::SystemTime> {
    let src_dir = package_root.join("src");
    if !src_dir.is_dir() {
        return None;
    }
    let manifest = std::fs::metadata(package_root.join("Cargo.toml"))
        .and_then(|m| m.modified())
        .ok();
    newest_source_mtime(&src_dir)
        .into_iter()
        .chain(manifest)
        .max()
}

fn newest_source_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            if let Some(t) = newest_source_mtime(&path) {
                newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
            && let Ok(t) = std::fs::metadata(&path).and_then(|m| m.modified())
        {
            newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
        }
    }
    newest
}

fn resolve_deployed_address(
    return_data: Option<&HexBytes>,
    predicted_address: &ContractAddress,
) -> Result<(String, DeployAddressSource), CliError> {
    if let Some(return_data) = return_data {
        if return_data.0.len() == 32 {
            return Ok((
                format!("0x{}", hex::encode(&return_data.0)),
                DeployAddressSource::Receipt,
            ));
        }
        return Err(CliError::Other(format!(
            "deploy receipt return_data had {} bytes, expected 32-byte contract address",
            return_data.0.len()
        )));
    }

    Ok((
        format!("0x{}", hex::encode(predicted_address.as_ref())),
        DeployAddressSource::Derived,
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        DeployAddressSource, PreparedDeploy, command_error_for_deploy_receipt,
        deploy_funding_commands, derive_deploy_address, hex_to_32_bytes, newest_input_mtime,
        resolve_deployed_address, stale_artifact_warning,
    };
    use super::{INTEGRATION_DEPLOY_FUND_HOG, resolve_native_funding};
    use crate::artifacts::{ArtifactKind, ArtifactOptions, resolve_thruput_artifact};
    use serial_test::serial;
    use tempfile::TempDir;
    use thruput_json_rpc_types::{
        BlockHeight,
        hex::{Hex0x, HexBytes},
        receipt::{ExecStatus, TransactionReceipt},
        transaction::TxHash,
    };
    use thruput_xdr::types::{ContractAddress, SHA256Hash};

    #[test]
    fn integration_deploy_funds_the_contract_like_the_removed_dev_wallet() {
        // The dev wallet contract that `thruput test --integration` used to
        // deploy through transferred 100_000_000 storage tokens (10 HOG) to
        // every new contract before deploying it. A native deployment has to
        // keep doing so: hogd charges storage to the contract's own address,
        // so an unfunded contract cannot commit what its initializer writes.
        let funding = resolve_native_funding(true, Some(INTEGRATION_DEPLOY_FUND_HOG)).unwrap();
        assert_eq!(funding, Some(100_000_000));
    }

    #[tokio::test]
    async fn native_poll_timeout_does_not_resubmit() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let submissions = Arc::new(AtomicUsize::new(0));
        let server = jsonrpsee::server::ServerBuilder::default()
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let mut module = jsonrpsee::RpcModule::new(submissions.clone());
        let hash = TxHash(Hex0x(SHA256Hash::from([1; 32])));
        module
            .register_method("groundhog_sendRawTransaction", move |_, state, _| {
                state.fetch_add(1, Ordering::SeqCst);
                Ok::<_, jsonrpsee::types::ErrorObjectOwned>(hash)
            })
            .unwrap();
        module
            .register_method("groundhog_getTransactionReceipt", |_, _, _| {
                None::<TransactionReceipt>
            })
            .unwrap();
        let handle = server.start(module);
        let client = crate::http_client::rpc_client(&url).unwrap();
        let prepared = PreparedDeploy {
            initializer_metadata: None,
            tx_hash: hash,
            native: None,
            rpc_url: url,
            profile_name: "test".into(),
            wallet_name: None,
            wallet_address: String::new(),
            artifact: crate::artifacts::ResolvedArtifact {
                kind: ArtifactKind::Rv32,
                path: std::path::PathBuf::new(),
            },
            artifact_bytes_len: 0,
            predicted_address: String::new(),
            wallet_entrypoint_id: String::new(),
            wallet_entrypoint_source: "test",
            tx_hex: "0x00".into(),
            package_name: None,
        };
        let error = super::submit_native_deploy(
            &prepared,
            &client,
            thruput_client::PollOptions {
                max_attempts: 1,
                interval: std::time::Duration::ZERO,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(submissions.load(Ordering::SeqCst), 1);
        assert!(matches!(
            error,
            crate::CliError::NativeDeploymentUnverified { .. }
        ));
        assert!(
            error
                .to_string()
                .contains(&crate::tx_utils::format_tx_hash(&hash))
        );
        handle.stop().unwrap();
    }

    #[test]
    fn native_submit_transport_timeout_remains_unverified() {
        assert!(matches!(
            super::native_submission_error(&jsonrpsee::core::ClientError::RequestTimeout),
            crate::CliError::NativeDeploymentUnverified { .. }
        ));
    }

    #[test]
    fn hex_to_32_bytes_parses_prefixed_hex() {
        let input = format!("0x{}", "ab".repeat(32));
        let bytes = hex_to_32_bytes(&input).expect("bug: valid prefixed hex should parse");
        assert_eq!(bytes, [0xab; 32]);
    }

    #[test]
    fn hex_to_32_bytes_parses_unprefixed_hex() {
        let input = "cd".repeat(32);
        let bytes = hex_to_32_bytes(&input).expect("bug: valid unprefixed hex should parse");
        assert_eq!(bytes, [0xcd; 32]);
    }

    #[test]
    fn stale_artifact_warning_uses_current_build_command() {
        let warning = stale_artifact_warning(ArtifactKind::Rv32, "demo-contract");

        assert!(warning.contains("thruput build -p demo-contract"));
        assert!(!warning.contains("--target"));
    }

    #[test]
    fn manifest_counts_as_a_build_input_for_staleness() {
        use std::time::{Duration, SystemTime};

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let source = root.join("src/lib.rs");
        let manifest = root.join("Cargo.toml");
        std::fs::write(&source, "").unwrap();
        std::fs::write(&manifest, "[package]\n").unwrap();

        let older = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let newer = older + Duration::from_mins(1);
        std::fs::File::open(&source)
            .unwrap()
            .set_modified(older)
            .unwrap();
        std::fs::File::open(&manifest)
            .unwrap()
            .set_modified(newer)
            .unwrap();

        // A manifest edit alone (say `initial_memory_bytes`) makes the
        // artifact stale, even with every source file untouched.
        assert_eq!(newest_input_mtime(root), Some(newer));

        std::fs::File::open(&manifest)
            .unwrap()
            .set_modified(older - Duration::from_mins(1))
            .unwrap();
        assert_eq!(newest_input_mtime(root), Some(older));

        // No `src/` means nothing to compare against.
        std::fs::remove_dir_all(root.join("src")).unwrap();
        assert_eq!(newest_input_mtime(root), None);
    }

    #[test]
    fn hex_to_32_bytes_rejects_wrong_length() {
        let short = format!("0x{}", "ab".repeat(16));
        assert!(hex_to_32_bytes(&short).is_err());
    }

    #[test]
    fn hex_to_32_bytes_rejects_invalid_hex() {
        let bad = format!("0x{}", "zz".repeat(32));
        assert!(hex_to_32_bytes(&bad).is_err());
    }

    #[test]
    fn deploy_address_resolution_falls_back_to_prediction() {
        let predicted = ContractAddress::from([0x11; 32]);
        let resolved = resolve_deployed_address(None, &predicted)
            .expect("bug: predicted address fallback should succeed");
        assert_eq!(resolved.0, format!("0x{}", "11".repeat(32)));
        assert_eq!(resolved.1, DeployAddressSource::Derived);
    }

    #[test]
    fn deploy_address_resolution_prefers_receipt_return_data() {
        let predicted = ContractAddress::from([0x11; 32]);
        let resolved = resolve_deployed_address(Some(&HexBytes(vec![0x22; 32])), &predicted)
            .expect("bug: receipt return_data should be accepted");
        assert_eq!(resolved.0, format!("0x{}", "22".repeat(32)));
        assert_eq!(resolved.1, DeployAddressSource::Receipt);
    }

    #[test]
    fn deploy_address_derivation_is_deterministic() {
        let deployer = ContractAddress::from([0xAB; 32]);
        let nonce = SHA256Hash::from([0xCD; 32]);
        let contract_hash = SHA256Hash::from([0xEF; 32]);

        let first = derive_deploy_address(&deployer, &nonce, &contract_hash)
            .expect("bug: derivation should succeed");
        let second = derive_deploy_address(&deployer, &nonce, &contract_hash)
            .expect("bug: derivation should succeed");

        assert_eq!(first, second);
        assert_ne!(first, deployer);
    }

    #[test]
    fn native_hog_overdraft_is_actionable_deploy_failure() {
        let abort_data = xdrs::cxs::encode(&(
            <thruput_erc20_if::ERC20Error as sdk::exn::Exn>::SELECTOR,
            thruput_erc20_if::ERC20Error::TooLarge,
        ))
        .expect("bug: ERC20 exception should encode");
        let receipt = failed_receipt(Some(HexBytes(abort_data)));
        let decoded_abort = crate::tx_utils::decode_receipt_abort(&receipt, None);

        let error = command_error_for_deploy_receipt(&receipt, decoded_abort.as_ref(), None)
            .expect("native HOG overdraft should be classified");
        let json = serde_json::to_value(error).expect("bug: command error should serialize");

        assert_eq!(json["code"], "transaction_insufficient_funds");
        assert_eq!(json["details"]["reason"], "nativeHogTransferOverdraft");
        assert!(
            json["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("when the selected profile has a faucet"))
        );
    }

    #[test]
    fn unrelated_revert_remains_generic_deploy_failure() {
        let receipt = failed_receipt(Some(HexBytes(vec![0x01, 0x02])));
        let decoded_abort = crate::tx_utils::decode_receipt_abort(&receipt, None);

        let error = command_error_for_deploy_receipt(&receipt, decoded_abort.as_ref(), None)
            .expect("reverted receipt should remain a command failure");
        let json = serde_json::to_value(error).expect("bug: command error should serialize");

        assert_eq!(json["code"], "transaction_reverted");
        assert!(json["hint"].is_null());
    }

    #[test]
    fn funding_commands_support_option_like_wallet_and_profile_names() {
        let prepared = PreparedDeploy {
            initializer_metadata: None,
            tx_hash: TxHash(Hex0x(SHA256Hash::from([0; 32]))),
            native: None,
            rpc_url: String::new(),
            profile_name: "-preview".into(),
            wallet_name: Some("-alice".into()),
            wallet_address: format!("0x{}", "11".repeat(32)),
            artifact: crate::artifacts::ResolvedArtifact {
                kind: ArtifactKind::Rv32,
                path: std::path::PathBuf::new(),
            },
            artifact_bytes_len: 0,
            predicted_address: String::new(),
            wallet_entrypoint_id: String::new(),
            wallet_entrypoint_source: "test",
            tx_hex: String::new(),
            package_name: None,
        };

        assert_eq!(
            deploy_funding_commands(&prepared),
            [
                "thruput balance --profile=-preview -- -alice",
                "thruput airdrop --profile=-preview -- -alice",
            ]
        );
    }

    fn failed_receipt(abort_data: Option<HexBytes>) -> TransactionReceipt {
        TransactionReceipt {
            tx_hash: TxHash(Hex0x(SHA256Hash::from([0x11; 32]))),
            height: BlockHeight::from(1),
            status: ExecStatus::Reverted { message: None },
            events: Vec::new(),
            return_data: None,
            abort_data,
            gas_used: 1_500,
        }
    }

    #[test]
    #[serial]
    fn resolve_artifact_path_uses_explicit_target_dir() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let expected_rv32 = custom_target.join("thruput").join("demo_contract.rv32a");
        std::fs::create_dir_all(
            expected_rv32
                .parent()
                .expect("bug: rv32 parent should exist"),
        )
        .expect("bug: custom target dir should be created");
        std::fs::write(&expected_rv32, b"rv32").expect("bug: rv32 fixture should be written");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let resolved = resolve_thruput_artifact(&project_root, &artifacts, None)
            .expect("bug: rv32 should resolve from explicit target dir");

        assert_eq!(resolved.artifact.path, expected_rv32);
        assert_eq!(resolved.package_name, "demo-contract");
        assert_eq!(resolved.package_root, project_root);
    }

    #[test]
    #[serial]
    fn resolve_artifact_path_reports_selected_missing_artifact() {
        let project = create_temp_project("demo-contract");
        let project_root =
            std::fs::canonicalize(project.path()).expect("bug: project path should canonicalize");
        let custom_target = project_root.join("custom-target");
        let expected_rv32 = custom_target.join("thruput").join("demo_contract.rv32a");

        let artifacts = ArtifactOptions::new(Some(custom_target));
        let _cwd = CurrentDirGuard::enter(&project_root);
        let err = resolve_thruput_artifact(&project_root, &artifacts, None)
            .expect_err("bug: missing rv32 artifact should error");

        assert!(
            matches!(err, crate::CliError::ArtifactNotFound { kind: "rv32", path } if path == expected_rv32.display().to_string())
        );
    }

    fn create_temp_project(package_name: &str) -> TempDir {
        let project = tempfile::tempdir().expect("bug: temp dir should be created");
        write_project_manifest(project.path(), package_name);
        std::fs::create_dir_all(project.path().join("src"))
            .expect("bug: src dir should be created");
        std::fs::write(project.path().join("src/lib.rs"), "pub fn demo() {}\n")
            .expect("bug: lib.rs should be written");
        project
    }

    fn write_project_manifest(project_root: &Path, package_name: &str) {
        std::fs::write(
            project_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
            ),
        )
        .expect("bug: Cargo.toml should be written");
    }

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
    }

    impl CurrentDirGuard {
        fn enter(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("bug: current dir should resolve");
            std::env::set_current_dir(path).expect("bug: current dir should change");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).expect("bug: current dir should restore");
        }
    }
}
