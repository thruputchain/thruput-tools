//! Native native-token-balance traffic, submitted directly to `SchedGateway`.
use thruput_scheduler::{
    ParallelBatch, ParallelSignedTransactionBatch, sched_gateway::SchedGateway_SchedGateway1_Client,
};
use thruput_signing::{
    PubkeyAccount, SignTransactionParams, derive_dev_account_signer, sign_transaction,
};
use thruput_xdr::{PROTOCOL_VERSION, contract::ContractEntrypointID};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .ok_or("usage: submit_traffic RUNTIME_MANIFEST COUNT NONCE_START [FIXTURE_SEED]")?;
    let count: u32 = args.get(2).ok_or("missing count")?.parse()?;
    let nonce: u64 = args.get(3).ok_or("missing nonce")?.parse()?;
    let document = std::fs::read(path)?;
    let parsed = thruput_json_rpc_types::runtime_manifest::parse_runtime_manifest(&document)?;
    let endpoint = parsed
        .endpoints
        .scheduler_gateway
        .ok_or("missing schedulerGateway")?;
    let signer = if let Some(seed) = args.get(4) {
        if seed.is_empty() {
            return Err("fixture seed must be nonempty".into());
        }
        derive_dev_account_signer(&format!("thruput-firehose:{seed}"), 0)
    } else {
        if parsed.kind
            != thruput_json_rpc_types::runtime_manifest::HOSTED_DEVNET_RUNTIME_MANIFEST_KIND
        {
            return Err("local manifests contain public allocations only; supply a fixture seed and matching public initial allocation".into());
        }
        let derivation = parsed
            .bootstrap_account_derivation
            .as_ref()
            .ok_or("missing hosted derivation")?;
        if derivation.scheme != "thruput-bootstrap-account-v1" {
            return Err("unsupported hosted derivation".into());
        }
        let account = parsed
            .bootstrap_accounts
            .iter()
            .find(|a| a.role == "hosted_dev_account")
            .ok_or("missing hosted user account")?;
        derive_dev_account_signer(
            &derivation.seed,
            usize::try_from(account.index.ok_or("missing hosted index")?)?,
        )
    };
    let native = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
    let wallet = native.address();
    if !parsed.bootstrap_accounts.iter().any(|a| {
        (a.role == "local_user" || a.role == "hosted_dev_account")
            && a.wallet_address.0 == wallet
            && a.public_key.as_ref() == signer.verifying_key_bytes()
    }) {
        return Err("fixture account has no matching public initial allocation".into());
    }
    let network = parsed.network_hash.to_network_hash();
    let calldata = xdrs::cxs::encode(&(wallet,))?;
    let mut bytes = Vec::new();
    for i in 0..count {
        let tx = thruput_signing::default_wallet::build_default_wallet_call_transaction(
            thruput_signing::default_wallet::BuildDefaultWalletCallTransaction {
                account: &native,
                target_address: thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN,
                target_entrypoint: ContractEntrypointID::new(
                    0x41ac_95d6_e06e_2620_u64.to_be_bytes(),
                ),
                target_calldata: &calldata,
                gas_limit: 20_000_000,
                target_gas_limit: 10_000_000,
                block_expiry_height: 10_000_000,
                nonce: nonce.checked_add(u64::from(i)).ok_or("nonce overflow")?,
            },
        )?;
        let authenticated =
            sign_transaction(tx, &signer, SignTransactionParams::default(), &network)?;
        bytes.extend_from_slice(&xdrs::txs::encode(&authenticated)?);
    }
    let batch = ParallelSignedTransactionBatch::new(ParallelBatch {
        segment_tokens: xdrs::VecM(vec![count]),
        segment_bytes: xdrs::VecM(vec![u32::try_from(bytes.len())?]),
        xdr: xdrs::BytesM(bytes),
    });
    let client = thruput_x23_transport::connect(
        "local_test_traffic",
        &endpoint.to_string(),
        1,
        SchedGateway_SchedGateway1_Client::new,
    )
    .await?;
    let intro = client.schedgateway_intro(&PROTOCOL_VERSION).await?;
    if intro != thruput_scheduler::SchedGatewayIntroOutcome::SchedGatewayIntroSuccess {
        return Err("intro rejected".into());
    }
    println!("{:?}", client.schedgateway_submit_tx_batch(&batch).await?);
    Ok(())
}
