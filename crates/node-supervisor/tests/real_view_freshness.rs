//! Read-your-writes against a real runtime; run explicitly with --ignored.
//!
//! hogd's view gateway can serve the previous snapshot for a moment after a
//! commit. A view floored at the receipt height must wait for the new
//! snapshot rather than fail, so a client needs no retry of its own.

#[path = "support/ramdb.rs"]
mod ramdb;

use std::{error::Error, path::PathBuf, time::Duration};

use jsonrpsee::http_client::HttpClientBuilder;
use thruput_json_rpc_client::native::{
    NativeClient, NativeClientError,
    receipt::{ReceiptPollConfig, poll_receipt},
};
use thruput_json_rpc_types::{
    base58::Base58, bootstrap_manifest::BootstrapManifestAllocation,
    error::native::codes::STATE_HEIGHT_UNAVAILABLE, hex::HexBytes, requests as input,
    responses as output, transaction::TxHash,
};
use thruput_node_supervisor::{
    BlockDriverConfig, HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle,
};
use thruput_signing::{
    LocalSigner, NATIVE_TOKEN_TRANSFER_ENTRYPOINT, PubkeyAccount, SignTransactionParams,
    default_wallet::{BuildDefaultWalletCallTransaction, build_default_wallet_call_transaction},
    encode_signed_transaction_xdr, native_token_transfer_calldata, sign_transaction,
    transaction_hash,
};
use thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN;

const AMOUNT: u128 = 1_000_000;
const GAS: u64 = 25_000_000;
/// Enough commits that at least one read lands before the view refreshes.
const ROUNDS: u64 = 5;
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn known<T>(result: output::DecodedResponse<T>) -> TestResult<T> {
    match result {
        output::DecodedResponse::Known(value) => Ok(value),
        output::DecodedResponse::Unsupported { raw } => {
            Err(std::io::Error::other(format!("unsupported native result: {raw}")).into())
        }
        _ => Err(std::io::Error::other("unrecognized result envelope").into()),
    }
}

/// A fresh key for each run, so no fixed secret is used on chain.
fn random_signer() -> TestResult<LocalSigner> {
    let mut secret = zeroize::Zeroizing::new([0; 32]);
    getrandom::fill(secret.as_mut()).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(LocalSigner::from_secret_key(*secret))
}

#[tokio::test]
#[ignore = "requires an absolute THRUPUT_DEVNODE_BINARY for a compatible real runtime"]
async fn instant_mode_view_at_the_receipt_height_needs_no_retry() {
    let binary = PathBuf::from(
        std::env::var_os("THRUPUT_DEVNODE_BINARY")
            .expect("THRUPUT_DEVNODE_BINARY must name a compatible runtime"),
    );
    let sender = random_signer().unwrap();
    let node = NodeSupervisorHandle::start(
        NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_view_gas_limit(GAS)
            .with_block_driver(BlockDriverConfig::instant())
            .with_hogd_dev(
                HogdDevConfig::default()
                    .with_binary_path(binary)
                    .with_startup_timeout(Duration::from_mins(1))
                    .with_ramdb_bytes(ramdb::ramdb_bytes(256 * ramdb::MIB))
                    .with_bootstrap_accounts(vec![BootstrapManifestAllocation::new(
                        Base58(sender.verifying_key_bytes().into()),
                        "1000000000000",
                    )]),
            ),
    )
    .await
    .unwrap();
    // Stop the runtime before reporting a failure.
    let outcome = read_after_each_commit(&node, &sender).await;
    node.stop().await.unwrap();
    outcome.unwrap();
}

async fn read_after_each_commit(node: &NodeSupervisorHandle, sender: &LocalSigner) -> TestResult {
    let native = NativeClient::new(
        HttpClientBuilder::default()
            .request_timeout(Duration::from_secs(30))
            .build(format!("http://{}", node.rpc_addr()))?,
    );
    let network = node
        .hogd_readiness()
        .runtime_manifest
        .as_ref()
        .ok_or("validated runtime manifest missing")?
        .network_hash
        .to_network_hash();
    let account = PubkeyAccount::from_public_key_bytes(sender.verifying_key_bytes(), 0);
    let recipient = random_signer()?;
    let recipient = PubkeyAccount::from_public_key_bytes(recipient.verifying_key_bytes(), 0);
    let calldata = native_token_transfer_calldata(&recipient.address(), AMOUNT)?;
    for nonce in 1..=ROUNDS {
        let tip = known(native.get_chain_tip(input::EmptyRequest {}).await?)?;
        let tx = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
            account: &account,
            target_address: ADDRESS_NATIVE_TOKEN,
            target_entrypoint: NATIVE_TOKEN_TRANSFER_ENTRYPOINT,
            target_calldata: &calldata,
            target_gas_limit: GAS,
            gas_limit: GAS,
            block_expiry_height: *tip.block_height + 100,
            nonce,
        })?;
        let hash = TxHash(Base58(transaction_hash(&tx)?));
        let signed = sign_transaction(tx, sender, SignTransactionParams::default(), &network)?;
        known(
            native
                .send_transaction(input::SendTransactionRequest {
                    signed_transaction: HexBytes(encode_signed_transaction_xdr(&signed)?),
                })
                .await?,
        )?;
        // Instant mode produces the block; no block is requested here.
        let receipt = poll_receipt(
            &native,
            Base58(network.clone().into_inner()),
            hash,
            None,
            ReceiptPollConfig {
                timeout: Duration::from_secs(30),
                interval: Duration::from_millis(5),
            },
            std::future::pending(),
        )
        .await?;
        let view = known(
            native
                .simulate_invocation(input::SimulateInvocationRequest {
                    address: Base58(ADDRESS_NATIVE_TOKEN),
                    entrypoint: HexBytes(NATIVE_TOKEN_TRANSFER_ENTRYPOINT.as_ref().to_vec()),
                    calldata: HexBytes(calldata.clone()),
                    caller: Some(Base58(account.address())),
                    assumed_auth: None,
                    gas_limit: Some(GAS),
                    min_state_block_height: Some(receipt.block_height.clone()),
                })
                .await?,
        )?;
        if view.state_block_height < receipt.block_height {
            return Err(format!(
                "view at {} is below receipt height {}",
                *view.state_block_height, *receipt.block_height
            )
            .into());
        }
    }
    // A floor beyond the committed tip is not waited for.
    let tip = known(native.get_chain_tip(input::EmptyRequest {}).await?)?;
    let started = std::time::Instant::now();
    let future = native
        .simulate_invocation(input::SimulateInvocationRequest {
            address: Base58(ADDRESS_NATIVE_TOKEN),
            entrypoint: HexBytes(NATIVE_TOKEN_TRANSFER_ENTRYPOINT.as_ref().to_vec()),
            calldata: HexBytes(calldata),
            caller: Some(Base58(account.address())),
            assumed_auth: None,
            gas_limit: Some(GAS),
            min_state_block_height: Some((*tip.block_height + 1000).into()),
        })
        .await;
    let fast = started.elapsed() < Duration::from_secs(1);
    match future {
        Err(NativeClientError::Application { source, .. })
            if source.code() == STATE_HEIGHT_UNAVAILABLE && fast => {}
        other => {
            return Err(format!(
                "a future floor should fail at once with -32012: {:?} after {:?}",
                other.map(|_| ()),
                started.elapsed()
            )
            .into());
        }
    }
    Ok(())
}
