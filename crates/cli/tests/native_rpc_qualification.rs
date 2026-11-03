//! Native CLI flow against the production endpoint of a real local runtime.
#![cfg(unix)]

#[path = "support/isolation.rs"]
mod isolation;

use std::{error::Error, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

use jsonrpsee::http_client::HttpClientBuilder;
use serde_json::Value;
use tempfile::TempDir;
use thruput_json_rpc_client::native::NativeClient;
use thruput_json_rpc_types::{
    ContractAddress,
    base58::Base58,
    bootstrap_manifest::BootstrapManifestAllocation,
    events::IterationOrder,
    requests::{self as input, AddressQuery, BlockQuery, EmptyRequest},
    responses as output,
};
use thruput_node_supervisor::{HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle};
use thruput_signing::{
    NATIVE_TOKEN_TRANSFER_ENTRYPOINT, PubkeyAccount, native_token_transfer_calldata,
};
use thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN;

const AMOUNT: u128 = 50_000_000;
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn require(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::other(message).into())
    }
}

fn text<'a>(value: &'a Value, name: &str) -> TestResult<&'a str> {
    value[name]
        .as_str()
        .ok_or_else(|| std::io::Error::other(format!("missing string {name}")).into())
}

struct Fixture {
    dir: TempDir,
    owner: [u8; 32],
}

impl Fixture {
    async fn new() -> TestResult<Self> {
        let mut fixture = Self {
            dir: TempDir::new()?,
            owner: [0; 32],
        };
        let key = fixture.command(&["keys", "generate", "owner"]).await?;
        fixture.owner =
            thruput_encoding::decode_fixed::<32>(text(&key["data"]["key"], "publicKey")?)?;
        let key_path = fixture.dir.path().join("config/thruput/keys/owner.json");
        require(
            std::fs::metadata(key_path)?.permissions().mode() & 0o777 == 0o600,
            "CLI key file permissions are not 0600",
        )?;
        Ok(fixture)
    }

    async fn command(&self, args: &[&str]) -> TestResult<Value> {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_thruput"));
        command
            .args(["--json", "--non-interactive"])
            .args(args)
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_CACHE_HOME", self.dir.path().join("cache"))
            .env_remove("THRUPUT_PROFILE")
            .env_remove("THRUPUT_RUNTIME_MANIFEST")
            .kill_on_drop(true);
        isolation::isolate_from_developer_env(command.as_std_mut());
        let output = tokio::time::timeout(Duration::from_mins(1), command.output()).await??;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "CLI failed: {}\n{}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            ))
            .into());
        }
        // One complete JSON envelope also rejects progress or text mixed into stdout.
        let report: Value = serde_json::from_slice(&output.stdout)?;
        require(report["ok"] == true, "CLI reported a failed command")?;
        Ok(report)
    }

    fn configure(&self, node: &NodeSupervisorHandle, rpc: std::net::SocketAddr) -> TestResult {
        std::fs::write(
            self.dir.path().join("thruput.toml"),
            format!(
                "default_profile = 'default'\n[profile.default]\ndefault_key = 'owner'\nrpc_url = 'http://{rpc}'\nnetwork_hash = '{}'\n",
                node.hogd_readiness().network_hash.as_str()
            ),
        )?;
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires absolute THRUPUT_DEVNODE_BINARY; runs a fresh real local runtime"]
async fn native_cli_call_receipt_view_and_history() {
    let binary = PathBuf::from(
        std::env::var_os("THRUPUT_DEVNODE_BINARY")
            .expect("THRUPUT_DEVNODE_BINARY must select a compatible runtime"),
    );
    assert!(binary.is_absolute() && binary.is_file());
    let artifacts = binary.parent().unwrap().join("contracts");
    let fixture = Fixture::new().await.unwrap();
    let node = NodeSupervisorHandle::start(
        NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_view_gas_limit(25_000_000)
            .with_hogd_dev(
                HogdDevConfig::default()
                    .with_ramdb_bytes(1024 * 1024 * 1024)
                    .with_binary_path(binary)
                    .with_contract_artifact_directories([artifacts])
                    .with_bootstrap_accounts(vec![BootstrapManifestAllocation::new(
                        Base58(fixture.owner.into()),
                        "1000000000000",
                    )]),
            ),
    )
    .await
    .unwrap();
    let result = exercise(&fixture, &node).await;
    node.stop().await.unwrap();
    result.unwrap();
}

async fn exercise(fixture: &Fixture, node: &NodeSupervisorHandle) -> TestResult {
    fixture.configure(node, node.rpc_addr())?;
    let native = NativeClient::new(
        HttpClientBuilder::default().build(format!("http://{}", node.rpc_addr()))?,
    );
    let output::DecodedResponse::Known(version) = native.get_version_info(EmptyRequest {}).await?
    else {
        return Err(std::io::Error::other("unsupported discovery response").into());
    };
    require(
        version.wire_format == thruput_json_rpc_api::WIRE_FORMAT_VERSION,
        "native wire version mismatch",
    )?;
    let expected_balance = expected_free_balance(&native).await?;
    let recipient = Base58(PubkeyAccount::from_public_key_bytes([0x63; 32], 0).address());
    let token = thruput_encoding::encode(ADDRESS_NATIVE_TOKEN.as_ref());
    let selector = format!(
        "0x{}",
        hex::encode(NATIVE_TOKEN_TRANSFER_ENTRYPOINT.as_ref())
    );
    let calldata = format!(
        "0x{}",
        hex::encode(native_token_transfer_calldata(&recipient.0, AMOUNT)?)
    );
    // Default commands must use the production native endpoint.
    let call = fixture
        .command(&["call", &token, &selector, &calldata, "--force"])
        .await?;
    require(
        call["command"] == "call" && call["data"]["schemaVersion"] == 4,
        "native call JSON envelope/schema mismatch",
    )?;
    let receipt: output::TransactionReceipt = serde_json::from_value(call["data"].clone())?;
    require(
        matches!(receipt.execution, output::ReceiptExecution::Return { .. }),
        "CLI transfer did not execute successfully",
    )?;
    require(
        receipt.gas_used > 0 && !receipt.events.is_empty(),
        "CLI receipt omitted metering or events",
    )?;
    check_block(&native, &receipt.block_height).await?;
    check_history(&native, fixture, &receipt).await?;
    require(
        call["data"]["blockHeight"].is_string() && call["data"]["gasUsed"].is_string(),
        "CLI receipt wide integers must be decimal strings",
    )?;
    let before = balance(&native, recipient).await?;
    require(
        before.raw_balance == expected_balance && before.state_block_height >= receipt.block_height,
        "CLI transfer balance does not match amount less balance storage collateral",
    )?;
    let sender = thruput_encoding::encode(
        PubkeyAccount::from_public_key_bytes(fixture.owner, 0)
            .address()
            .as_ref(),
    );
    let view = fixture
        .command(&["view", &token, &selector, &calldata, "--from", &sender])
        .await?;
    check_view(&view, &before)?;
    let after = balance(&native, recipient).await?;
    require(
        after.raw_balance == before.raw_balance
            && after.state_block_height == before.state_block_height,
        "CLI view changed committed state",
    )?;
    Ok(())
}

fn check_view(report: &Value, before: &output::BalanceResult) -> TestResult {
    require(
        report["command"] == "view" && report["data"]["schemaVersion"] == 4,
        "native view JSON envelope/schema mismatch",
    )?;
    let simulation: output::SimulationResult = serde_json::from_value(report["data"].clone())?;
    require(
        matches!(simulation.output, output::InvocationOutput::Return { .. }),
        "CLI token simulation did not execute successfully",
    )?;
    require(
        simulation.state_block_height == before.state_block_height
            && simulation.gas_used > 0
            && !simulation.events.is_empty(),
        "CLI simulation lost its snapshot/events/gas",
    )?;
    require(
        report["data"]["stateBlockHeight"].is_string() && report["data"]["gasUsed"].is_string(),
        "CLI simulation wide integers must be decimal strings",
    )
}

async fn balance(
    client: &NativeClient<jsonrpsee::http_client::HttpClient>,
    address: ContractAddress,
) -> TestResult<output::BalanceResult> {
    match client.get_balance(AddressQuery { address }).await? {
        output::DecodedResponse::Known(balance) => Ok(balance),
        _ => Err(std::io::Error::other("unsupported balance response").into()),
    }
}

async fn expected_free_balance(
    client: &NativeClient<jsonrpsee::http_client::HttpClient>,
) -> TestResult<i128> {
    let output::DecodedResponse::Known(config) = client.get_chain_config(EmptyRequest {}).await?
    else {
        return Err(std::io::Error::other("unsupported chain configuration").into());
    };
    let execution = config
        .execution
        .ok_or_else(|| std::io::Error::other("missing authoritative storage costs"))?;
    Ok(i128::try_from(AMOUNT)? - i128::from(execution.storage_costs.balance_cost))
}

async fn check_block(
    native: &NativeClient<jsonrpsee::http_client::HttpClient>,
    height: &thruput_json_rpc_types::BlockHeight,
) -> TestResult {
    let output::DecodedResponse::Known(Some(block)) = native
        .get_block_by_height(BlockQuery {
            block_height: height.clone(),
        })
        .await?
    else {
        return Err(std::io::Error::other("CLI receipt block is missing or invalid").into());
    };
    let output::DecodedResponse::Known(network) = native.get_network_info(EmptyRequest {}).await?
    else {
        return Err(std::io::Error::other("unsupported network response").into());
    };
    require(
        block.block_height == *height && block.network_hash == network.network_hash,
        "CLI receipt block has the wrong height or network",
    )?;
    Ok(())
}

async fn check_history(
    native: &NativeClient<jsonrpsee::http_client::HttpClient>,
    fixture: &Fixture,
    receipt: &output::TransactionReceipt,
) -> TestResult {
    check_history_commands(fixture, receipt).await?;
    let start = input::HistoryStart::Ascending {
        anchor: input::AscendingAnchor::AtHeight {
            block_height: receipt.block_height.clone(),
        },
    };
    let output::DecodedResponse::Known(root) = native
        .get_transactions_by_address(input::AddressHistoryQuery {
            address: Base58(PubkeyAccount::from_public_key_bytes(fixture.owner, 0).address()),
            start: Some(start.clone()),
            cursor: None,
            limit: Some(10),
        })
        .await?
    else {
        return Err(std::io::Error::other("unsupported root history response").into());
    };
    let output::DecodedResponse::Known(block) = native
        .get_transactions_by_block(input::BlockPageRequest {
            block_height: receipt.block_height.clone(),
            start: Some(input::BlockStart {
                order: IterationOrder::Descending,
            }),
            cursor: None,
            limit: Some(10),
        })
        .await?
    else {
        return Err(std::io::Error::other("unsupported block history response").into());
    };
    require(
        root.transactions.len() == 1
            && root.transactions[0].transaction_hash == receipt.transaction_hash
            && root.transactions[0].block_height == receipt.block_height
            && root.transactions[0].is_root_target
            && root.transactions[0].was_invoked
            && root.next_cursor.is_none(),
        "CLI receipt disagrees with native address history",
    )?;
    require(
        block.transactions.len() == 1
            && block.transactions[0].transaction_hash == receipt.transaction_hash
            && block.transactions[0].block_height == receipt.block_height
            && block.next_cursor.is_none(),
        "CLI receipt disagrees with native block history",
    )?;
    let output::DecodedResponse::Known(events) = native
        .get_events(input::EventQuery {
            filters: vec![input::EventFilter {
                addresses: vec![Base58(ADDRESS_NATIVE_TOKEN)],
                topics: None,
            }],
            start: Some(start),
            cursor: None,
            limit: Some(100),
        })
        .await?
    else {
        return Err(std::io::Error::other("unsupported event history response").into());
    };
    let expected = receipt
        .events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.address == Base58(ADDRESS_NATIVE_TOKEN))
        .collect::<Vec<_>>();
    require(
        events.events.len() == expected.len() && events.next_cursor.is_none(),
        "CLI receipt token events disagree with event history",
    )?;
    for (event, (sequence, emitted)) in events.events.iter().zip(expected) {
        require(
            event.transaction_hash == receipt.transaction_hash
                && event.block_height == receipt.block_height
                && event.sequence_number as usize == sequence
                && serde_json::to_value(&event.event)? == serde_json::to_value(emitted)?,
            "CLI receipt event identity or payload differs from history",
        )?;
    }
    Ok(())
}

async fn check_history_commands(
    fixture: &Fixture,
    receipt: &output::TransactionReceipt,
) -> TestResult {
    let owner = thruput_encoding::encode(
        PubkeyAccount::from_public_key_bytes(fixture.owner, 0)
            .address()
            .as_ref(),
    );
    let transactions = fixture
        .command(&["tx", "list", &owner, "--limit", "1"])
        .await?;
    let transactions: thruput_cli::tx_json::TxListReport =
        serde_json::from_value(transactions["data"].clone())?;
    require(
        transactions.schema_version == 2
            && transactions.refs.len() == 1
            && transactions.refs[0].tx_hash == receipt.transaction_hash
            && transactions.refs[0].height == receipt.block_height,
        "CLI native root-history command did not find its submitted transaction",
    )?;
    let token = thruput_encoding::encode(ADDRESS_NATIVE_TOKEN.as_ref());
    let events = fixture
        .command(&["events", "list", &token, "--limit", "100"])
        .await?;
    let events: thruput_cli::events_json::EventListReport =
        serde_json::from_value(events["data"].clone())?;
    require(
        events.schema_version == 2
            && events.events.iter().any(|event| {
                event.raw.tx_hash == receipt.transaction_hash
                    && event.raw.block_height == receipt.block_height
                    && event.raw.contract_address == Base58(ADDRESS_NATIVE_TOKEN)
            }),
        "CLI native event command did not find the submitted token transfer",
    )?;
    Ok(())
}
