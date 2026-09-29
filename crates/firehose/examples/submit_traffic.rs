//! Local benchmark's self-call traffic, submitted directly to `SchedGateway`.
use groundhog_scheduler::{
    ParallelBatch, ParallelSignedTransactionBatch, sched_gateway::SchedGateway_SchedGateway1_Client,
};
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, SignTransactionParams,
    build_dev_wallet_do_action_transaction, derive_dev_account_signer, sign_transaction,
};
use groundhog_xdr::{
    PROTOCOL_VERSION,
    contract::ContractEntrypointID,
    types::{ContractAddress, NetworkHash},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .ok_or("usage: submit_traffic RUNTIME_MANIFEST COUNT NONCE_START")?;
    let count: u32 = args.get(2).ok_or("missing count")?.parse()?;
    let nonce: u64 = args.get(3).ok_or("missing nonce")?.parse()?;
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let endpoint = manifest["endpoints"]["schedulerGateway"]
        .as_str()
        .ok_or("missing schedulerGateway")?;
    let account = &manifest["bootstrapAccounts"][1];
    let wallet = account["walletAddress"].as_str().ok_or("missing wallet")?;
    let wallet = ContractAddress::from(
        <[u8; 32]>::try_from(hex::decode(wallet.trim_start_matches("0x"))?)
            .map_err(|_| "wallet width")?,
    );
    let selector = manifest["bootstrap"]["devWallet"]["doActionEntrypoint"]
        .as_str()
        .ok_or("missing selector")?;
    let selector = ContractEntrypointID::from(
        <[u8; 8]>::try_from(hex::decode(selector.trim_start_matches("0x"))?)
            .map_err(|_| "selector width")?,
    );
    let network = manifest["networkHash"]
        .as_str()
        .ok_or("missing network hash")?;
    let network = NetworkHash::new(
        <[u8; 32]>::try_from(hex::decode(network.trim_start_matches("0x"))?)
            .map_err(|_| "network width")?
            .into(),
    );
    let seed = manifest["bootstrapAccountDerivation"]["seed"]
        .as_str()
        .ok_or("missing seed")?;
    let signer = derive_dev_account_signer(seed, 1);
    let mut bytes = Vec::new();
    for i in 0..count {
        let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
            wallet_address: wallet,
            wallet_entrypoint: selector,
            target_address: wallet,
            target_entrypoint: selector,
            target_calldata: &[],
            gas_limit: 10_000_000,
            target_gas_limit: 10_000_000,
            block_expiry_height: 10_000_000,
            nonce: nonce + u64::from(i),
        })?;
        let authenticated =
            sign_transaction(tx, &signer, SignTransactionParams::default(), &network)?;
        bytes.extend_from_slice(&xdrs::txs::encode(&authenticated)?);
    }
    let batch = ParallelSignedTransactionBatch::new(ParallelBatch {
        segment_tokens: xdrs::VecM(vec![count]),
        segment_bytes: xdrs::VecM(vec![u32::try_from(bytes.len())?]),
        xdr: xdrs::BytesM(bytes),
    });
    let client = groundhog_x23_transport::connect(
        "local_test_traffic",
        endpoint,
        1,
        SchedGateway_SchedGateway1_Client::new,
    )
    .await?;
    let intro = client.schedgateway_intro(&PROTOCOL_VERSION).await?;
    if intro != groundhog_scheduler::SchedGatewayIntroOutcome::SchedGatewayIntroSuccess {
        return Err("intro rejected".into());
    }
    println!("{:?}", client.schedgateway_submit_tx_batch(&batch).await?);
    Ok(())
}
