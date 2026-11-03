//! A `#[contract]` interface's generated `<Iface>Remote` proxy, bound to a
//! testkit `Caller`, against a recording RPC: each method must prepare the
//! expected invocation kind and send its selector, arguments and caller.

#![allow(unexpected_cfgs)]

use std::sync::{Arc, Mutex};

use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use thruput_json_rpc_types::requests::SimulateInvocationRequest;
use thruput_json_rpc_types::responses::{InvocationOutput, SimulationResult};
use thruput_sdk::ContractAddress;
use thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT;
use thruput_signing::transaction_hash;
use thruput_testkit::rpc_types::{
    base58::Base58,
    hex::HexBytes,
    responses::{Admission, ReceiptExecution, SendTransactionResult, TransactionReceipt},
    transaction::TxHash,
};
use thruput_testkit::xdr::{contract::ContractEntrypointID, transaction::SignedTransaction};
use thruput_testkit::{Agent, ContractCall, ContractView, NetworkConfig, Scenario, entrypoint_id};

mod common;
use common::{native_runtime_document, serve_manifest_network_info};

// The generated guest client's `try_*` methods pass `|exn| exn.to_owned()`,
// spanned to each method, which this workspace's pedantic lints reject.
// (The next SDK release should remove the need for this.)
#[allow(clippy::redundant_closure_for_method_calls)]
pub mod ledger {
    use thruput_sdk::{ContractAddress, contract};

    #[contract(version = "1.0.0")]
    pub trait Ledger {
        #[contract(effect = "readOnly")]
        fn balance(&self, owner: &ContractAddress) -> u64;

        #[contract(effect = "stateChanging")]
        fn transfer(&mut self, to: &ContractAddress, amount: u64) -> u32;

        // Declared `&self`, but an undeclared effect is never treated as a read.
        fn request(&self, amount: u64) -> u32;
    }
}
use ledger::LedgerRemote;

const TARGET: [u8; 32] = [7; 32];
const BALANCE: u64 = 42;
const TRANSFER_ID: u32 = 7;
const REQUEST_ID: u32 = 9;

fn selector(method: &str) -> ContractEntrypointID {
    entrypoint_id("Ledger", "1.0.0", method)
}

/// The value the target returns for `entrypoint`, encoded as the contract would.
fn returned(entrypoint: ContractEntrypointID) -> Vec<u8> {
    if entrypoint == selector("balance") {
        xdrs::cxs::encode(&BALANCE).unwrap()
    } else if entrypoint == selector("transfer") {
        xdrs::cxs::encode(&TRANSFER_ID).unwrap()
    } else {
        xdrs::cxs::encode(&REQUEST_ID).unwrap()
    }
}

/// One `DefaultWallet.execute` child call: target, entrypoint, gas, calldata.
type WalletCall = (ContractAddress, ContractEntrypointID, u64, Vec<u8>);

#[derive(Default)]
struct Recorded {
    submitted: Mutex<Vec<SignedTransaction>>,
    simulated: Mutex<Vec<SimulateInvocationRequest>>,
}

async fn scenario(recorded: Arc<Recorded>) -> (Scenario, jsonrpsee::server::ServerHandle) {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(recorded);
    serve_manifest_network_info(&mut module);
    module
        .register_method("simulateInvocation", |params, recorded, _| {
            let request: SimulateInvocationRequest = params.parse()?;
            let entrypoint = ContractEntrypointID::from(
                <[u8; 8]>::try_from(request.entrypoint.0.as_slice()).unwrap(),
            );
            recorded.simulated.lock().unwrap().push(request);
            Ok::<_, ErrorObjectOwned>(SimulationResult {
                state_block_height: 3.into(),
                output: InvocationOutput::Return {
                    return_data: HexBytes(returned(entrypoint)),
                },
                events: vec![],
                gas_used: 1,
            })
        })
        .unwrap();
    module
        .register_method("sendTransaction", |params, recorded, _| {
            let request: thruput_json_rpc_types::requests::SendTransactionRequest =
                params.parse()?;
            let signed: SignedTransaction =
                xdrs::txs::decode(&request.signed_transaction.0).unwrap();
            let hash = TxHash(Base58(transaction_hash(&signed.tx).unwrap()));
            recorded.submitted.lock().unwrap().push(signed);
            Ok::<_, ErrorObjectOwned>(SendTransactionResult {
                transaction_hash: hash,
                admission: Admission::Accepted,
            })
        })
        .unwrap();
    module
        .register_method("getTransactionReceipt", |params, recorded, _| {
            let hash = params
                .parse::<thruput_json_rpc_types::requests::TransactionQuery>()?
                .transaction_hash;
            let submitted = recorded.submitted.lock().unwrap();
            let signed = submitted
                .iter()
                .find(|signed| transaction_hash(&signed.tx).unwrap() == hash.0.0)
                .unwrap();
            let calls: Vec<WalletCall> = xdrs::cxs::decode(&signed.tx.invocation.calldata).unwrap();
            // The wallet wraps the child's return, one buffer per child call.
            let wrapped = xdrs::cxs::encode(&vec![returned(calls[0].1)]).unwrap();
            Ok::<_, ErrorObjectOwned>(Some(TransactionReceipt {
                invoked_addresses: vec![],
                transaction_hash: hash,
                block_height: 3.into(),
                execution: ReceiptExecution::Return {
                    return_data: HexBytes(wrapped),
                },
                events: vec![],
                gas_used: 1,
            }))
        })
        .unwrap();
    let server = server.start(module);

    let manifest = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        manifest.path(),
        serde_json::to_vec(&native_runtime_document()).unwrap(),
    )
    .unwrap();
    let scenario = Scenario::builder("remote-proxy")
        .network(NetworkConfig::existing(&url, manifest.path()))
        .build()
        .await
        .unwrap();
    (scenario, server)
}

#[tokio::test]
async fn generated_proxy_methods_simulate_reads_and_submit_calls() {
    let recorded = Arc::new(Recorded::default());
    let (scenario, server) = scenario(recorded.clone()).await;
    let owner = ContractAddress::from([9; 32]);
    let alice = Arc::new(Mutex::new(None));
    let seen = alice.clone();

    scenario
        .run_agents([Agent::new("alice", move |ctx| async move {
            *seen.lock().unwrap() = Some(ctx.address());
            let ledger = LedgerRemote::new(ctx.caller().clone(), &ContractAddress::from(TARGET));

            // The declared effect picks the prepared kind; these annotations
            // fail to compile if it changes.
            let balance: ContractView<u64> = ledger.balance(&owner);
            let transfer: ContractCall<u32> = ledger.transfer(&owner, 5);
            let request: ContractCall<u32> = ledger.request(3);

            assert_eq!(balance.await?, BALANCE);
            assert_eq!(transfer.await?, TRANSFER_ID);
            assert_eq!(request.simulate().await?, REQUEST_ID);
            Ok(())
        })])
        .await
        .unwrap();
    let _ = server.stop();
    let alice = alice.lock().unwrap().expect("bug: agent ran");

    // Reads are simulated against the target with the agent as `msg.sender`.
    let simulated = recorded.simulated.lock().unwrap();
    let simulated: Vec<_> = simulated
        .iter()
        .map(|request| {
            (
                request.address.0,
                request.entrypoint.0.clone(),
                request.calldata.0.clone(),
                request.caller.map(|caller| caller.0),
            )
        })
        .collect();
    assert_eq!(
        simulated,
        [
            (
                ContractAddress::from(TARGET),
                selector("balance").into_inner().to_vec(),
                xdrs::cxs::encode(&owner).unwrap(),
                Some(alice),
            ),
            (
                ContractAddress::from(TARGET),
                selector("request").into_inner().to_vec(),
                xdrs::cxs::encode(&3u64).unwrap(),
                Some(alice),
            ),
        ]
    );

    // Only the state-changing call is submitted, through the agent's wallet.
    let submitted = recorded.submitted.lock().unwrap();
    let [signed] = submitted.as_slice() else {
        panic!("expected exactly one submission, got {}", submitted.len());
    };
    assert_eq!(signed.tx.invocation.invoked_address, alice);
    assert_eq!(
        signed.tx.invocation.entrypoint,
        DEFAULT_WALLET_EXECUTE_ENTRYPOINT
    );
    let calls: Vec<WalletCall> = xdrs::cxs::decode(&signed.tx.invocation.calldata).unwrap();
    let [(target, entrypoint, _, calldata)] = calls.as_slice() else {
        panic!("expected one wallet child call, got {}", calls.len());
    };
    assert_eq!(*target, ContractAddress::from(TARGET));
    assert_eq!(*entrypoint, selector("transfer"));
    assert_eq!(*calldata, xdrs::cxs::encode(&(&owner, 5u64)).unwrap());
}
