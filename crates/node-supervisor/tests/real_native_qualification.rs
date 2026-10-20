//! Opt-in real runtime coverage for the candidate native connector.
#![cfg(feature = "native-rpc-qualification")]

use std::{error::Error, path::PathBuf, time::Duration};

use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use thruput_json_rpc_api::GroundhogApiClient;
use thruput_json_rpc_client::native::{
    NativeClient,
    receipt::{ReceiptPollConfig, poll_receipt},
};
use thruput_json_rpc_server::RpcServerConfig;
use thruput_json_rpc_types::{
    ContractAddress, base58::Base58, bootstrap_manifest::BootstrapManifestAllocation,
    hex::HexBytes, requests as input, responses as output, transaction::TxHash,
};
use thruput_node_supervisor::{
    BlockDriverConfig, HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle,
};
use thruput_signing::{
    LocalSigner, NATIVE_HOG_ADDRESS, NATIVE_HOG_TRANSFER_ENTRYPOINT, PubkeyAccount,
    SignTransactionParams,
    default_wallet::{BuildDefaultWalletCallTransaction, build_default_wallet_call_transaction},
    encode_signed_transaction_xdr, native_hog_transfer_calldata, sign_transaction,
    transaction_hash,
};

const AMOUNT: u128 = 50_000_000;
const GAS: u64 = 25_000_000;
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn require(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::other(message).into())
    }
}

fn known<T>(result: output::DecodedResponse<T>) -> TestResult<T> {
    match result {
        output::DecodedResponse::Known(value) => Ok(value),
        other => {
            let raw = match other {
                output::DecodedResponse::Unsupported { raw } => raw.to_string(),
                _ => "unrecognized result envelope".into(),
            };
            Err(std::io::Error::other(format!("unsupported native result: {raw}")).into())
        }
    }
}

fn random_signer() -> TestResult<LocalSigner> {
    let mut secret = zeroize::Zeroizing::new([0; 32]);
    getrandom::fill(secret.as_mut()).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(LocalSigner::from_secret_key(*secret))
}

fn transport(address: std::net::SocketAddr) -> TestResult<HttpClient> {
    Ok(HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(30))
        .build(format!("http://{address}"))?)
}

#[tokio::test]
#[ignore = "requires an absolute THRUPUT_DEVNODE_BINARY for a compatible real runtime"]
async fn real_native_submit_receipt_balance_and_simulation() {
    let binary = PathBuf::from(
        std::env::var_os("THRUPUT_DEVNODE_BINARY")
            .expect("THRUPUT_DEVNODE_BINARY must name a compatible runtime"),
    );
    assert!(binary.is_absolute() && binary.is_file());
    let artifacts = binary.parent().unwrap().join("contracts");
    let sender = random_signer().unwrap();
    let node = NodeSupervisorHandle::start(
        NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_block_driver(BlockDriverConfig::default())
            .with_hogd_dev(
                HogdDevConfig::default()
                    .with_binary_path(binary)
                    .with_contract_artifact_directories([artifacts])
                    .with_bootstrap_accounts(vec![BootstrapManifestAllocation::new(
                        Base58(sender.verifying_key_bytes().into()),
                        "1000000000000",
                    )]),
            ),
    )
    .await
    .unwrap();
    // The helper returns errors instead of panicking, so both listeners and the
    // runtime are stopped before the test reports a failed assertion.
    let outcome = qualify(&node, &sender).await;
    node.stop().await.unwrap();
    outcome.unwrap();
}

async fn qualify(node: &NodeSupervisorHandle, sender: &LocalSigner) -> TestResult {
    let candidate = node
        .start_native_qualification(RpcServerConfig::default().with_view_gas_limit(GAS))
        .await?;
    let outcome = exercise(node, candidate.rpc_addr(), sender).await;
    candidate.stop().await;
    outcome
}

async fn exercise(
    node: &NodeSupervisorHandle,
    candidate_addr: std::net::SocketAddr,
    sender: &LocalSigner,
) -> TestResult {
    let legacy = transport(node.rpc_addr())?;
    require(
        legacy.version().await?.wire_format == "13.0.0",
        "legacy endpoint changed",
    )?;
    let native = NativeClient::new(transport(candidate_addr)?);
    check_discovery(&native).await?;
    let network = known(native.get_network_info(input::EmptyRequest {}).await?)?.network_hash;
    let signing_network = node
        .hogd_readiness()
        .runtime_manifest
        .as_ref()
        .ok_or_else(|| std::io::Error::other("validated runtime manifest missing"))?
        .network_hash
        .to_network_hash();
    require(
        network.0 == signing_network.clone().into_inner(),
        "candidate network differs from validated manifest",
    )?;
    let recipient = random_signer()?;
    let recipient =
        Base58(PubkeyAccount::from_public_key_bytes(recipient.verifying_key_bytes(), 0).address());
    let account = PubkeyAccount::from_public_key_bytes(sender.verifying_key_bytes(), 0);
    let calldata = native_hog_transfer_calldata(&recipient.0, AMOUNT)?;
    let costs = known(native.get_chain_config(input::EmptyRequest {}).await?)?
        .execution
        .ok_or_else(|| std::io::Error::other("authoritative execution config missing"))?
        .storage_costs;
    let expected_free_balance = i128::try_from(AMOUNT)? - i128::from(costs.balance_cost);
    let receipt = send_transfer(&native, sender, recipient, signing_network).await?;
    require(
        matches!(receipt.execution, output::ReceiptExecution::Return { .. }),
        "transfer execution did not return successfully",
    )?;
    require(
        receipt
            .events
            .iter()
            .any(|event| event.address == Base58(NATIVE_HOG_ADDRESS) && event.topics.len() == 3),
        "transfer receipt omitted its token event",
    )?;
    require(receipt.gas_used > 0, "receipt omitted metering")?;
    let funded = known(
        native
            .get_balance(input::AddressQuery { address: recipient })
            .await?,
    )?;
    require(
        funded.address == recipient && funded.raw_balance == expected_free_balance,
        &format!(
            "recipient balance does not reflect exactly one transfer: receipt={receipt:?}; balance={funded:?}; expected_address={recipient:?}; expected_free_balance={expected_free_balance}"
        ),
    )?;
    require(
        funded.state_block_height == receipt.block_height,
        "balance snapshot differs from the only submitted transaction",
    )?;
    check_simulation(
        &native,
        Base58(account.address()),
        recipient,
        calldata,
        funded,
    )
    .await?;
    require(
        legacy.version().await?.wire_format == "13.0.0",
        "wire 13 stopped serving",
    )
}

async fn send_transfer(
    native: &NativeClient<HttpClient>,
    sender: &LocalSigner,
    recipient: ContractAddress,
    signing_network: thruput_xdr::types::NetworkHash,
) -> TestResult<output::TransactionReceipt> {
    let account = PubkeyAccount::from_public_key_bytes(sender.verifying_key_bytes(), 0);
    let calldata = native_hog_transfer_calldata(&recipient.0, AMOUNT)?;
    let tip = known(native.get_chain_tip(input::EmptyRequest {}).await?)?;
    let tx = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
        account: &account,
        target_address: NATIVE_HOG_ADDRESS,
        target_entrypoint: NATIVE_HOG_TRANSFER_ENTRYPOINT,
        target_calldata: &calldata,
        target_gas_limit: GAS,
        gas_limit: GAS,
        block_expiry_height: *tip.block_height + 100,
        nonce: 1,
    })?;
    let expected_hash = TxHash(Base58(transaction_hash(&tx)?));
    let signed = sign_transaction(
        tx,
        sender,
        SignTransactionParams::default(),
        &signing_network,
    )?;
    // Exactly one send. The waiter below has only read methods available.
    let admitted = known(
        native
            .send_transaction(input::SendTransactionRequest {
                signed_transaction: HexBytes(encode_signed_transaction_xdr(&signed)?),
            })
            .await?,
    )?;
    require(
        admitted.transaction_hash == expected_hash,
        "admission hash mismatch",
    )?;
    require(
        matches!(admitted.admission, output::Admission::Accepted),
        "missing admission",
    )?;
    let receipt = poll_receipt(
        native,
        Base58(signing_network.clone().into_inner()),
        expected_hash,
        None,
        ReceiptPollConfig {
            timeout: Duration::from_secs(30),
            interval: Duration::from_millis(50),
        },
        std::future::pending(),
    )
    .await?;
    Ok(receipt)
}

async fn check_discovery(native: &NativeClient<HttpClient>) -> TestResult {
    let version = known(native.get_version_info(input::EmptyRequest {}).await?)?;
    require(
        version.wire_format == "14.0.0",
        "candidate wire version mismatch",
    )?;
    let mut expected_methods = [
        "getVersionInfo",
        "getNetworkInfo",
        "getServerLimits",
        "getChainTip",
        "getChainConfig",
        "getStorage",
        "getBalance",
        "getRegisteredAuth",
        "simulateInvocation",
        "simulateInvocations",
        "sendTransaction",
        "getTransactionStatus",
        "getTransactionReceipt",
    ];
    expected_methods.sort_unstable();
    require(
        version.supported_methods == expected_methods,
        "candidate method map mismatch",
    )?;
    require(
        version.implementation == env!("CARGO_PKG_VERSION"),
        "implementation version mismatch",
    )?;
    require(
        version.release_version.is_none() && version.bundle_manifest_digest.is_none(),
        "candidate claimed an installed release identity",
    )
}

async fn check_simulation(
    native: &NativeClient<HttpClient>,
    sender: ContractAddress,
    recipient: ContractAddress,
    calldata: Vec<u8>,
    before: output::BalanceResult,
) -> TestResult {
    // Native HOG transfer debits GetMsgSender. Explicit caller supplies that
    // frame context; this direct token invocation needs no assumed signature.
    let result = known(
        native
            .simulate_invocation(input::SimulateInvocationRequest {
                address: Base58(NATIVE_HOG_ADDRESS),
                entrypoint: HexBytes(NATIVE_HOG_TRANSFER_ENTRYPOINT.as_ref().to_vec()),
                calldata: HexBytes(calldata),
                caller: Some(sender),
                assumed_auth: None,
                gas_limit: Some(GAS),
                min_state_block_height: Some(before.state_block_height.clone()),
            })
            .await?,
    )?;
    require(
        matches!(result.output, output::InvocationOutput::Return { .. }),
        "token simulation did not return successfully",
    )?;
    require(
        !result.events.is_empty() && result.gas_used > 0,
        "simulation omitted events or gas",
    )?;
    require(
        result.state_block_height == before.state_block_height,
        "simulation snapshot mismatch",
    )?;
    let after = known(
        native
            .get_balance(input::AddressQuery { address: recipient })
            .await?,
    )?;
    require(
        after.raw_balance == before.raw_balance
            && after.state_block_height == before.state_block_height,
        "simulation mutated committed recipient balance",
    )
}
