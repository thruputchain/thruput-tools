//! Process-level native deployment contract and failure-recovery tests.
use jsonrpsee::{
    RpcModule,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use serde_json::{Value, json};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use thruput_signing::{
    PubkeyAccount, derive_deploy_address, transaction_hash, wallet_contract_hash,
};
use thruput_xdr::{
    contract::ContractEntrypointID, transaction::SignedTransaction, types::SHA256Hash,
};

type InitCalldata = (ContractEntrypointID, u64, Vec<u8>);
type DeployCalldata = (SHA256Hash, SHA256Hash, u128, Option<InitCalldata>);

const NETWORK: &str = "US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx";

struct Fixture {
    dir: TempDir,
}
impl Fixture {
    fn new(rpc: &str) -> Self {
        let fixture = Self {
            dir: TempDir::new().unwrap(),
        };
        std::fs::write(fixture.dir.path().join("thruput.toml"), format!("default_profile = 'default'\n[profile.default]\ndefault_key = 'owner'\nrpc_url = '{rpc}'\nnetwork_hash = '{NETWORK}'\n")).unwrap();
        std::fs::write(fixture.dir.path().join("counter.rv32a"), b"test-artifact").unwrap();
        assert!(
            fixture
                .command()
                .args(["keys", "generate", "owner"])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(
            fixture
                .command()
                .args(["wallet", "create", "--force"])
                .output()
                .unwrap()
                .status
                .success()
        );
        fixture
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thruput"));
        cmd.arg("--json")
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("THRUPUT_PROFILE")
            .env_remove("THRUPUT_RUNTIME_MANIFEST");
        cmd
    }
    fn deploy(&self, args: &[&str]) -> Output {
        self.command()
            .args([
                "deploy",
                "--artifact",
                "counter.rv32a",
                "--alias",
                "counter",
                "--force",
            ])
            .args(args)
            .output()
            .unwrap()
    }
    fn config(&self) -> Value {
        let parsed: toml::Value =
            toml::from_str(&std::fs::read_to_string(self.dir.path().join("thruput.toml")).unwrap())
                .unwrap();
        serde_json::to_value(parsed).unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsaved_key_routes_call_transfer_and_deploy_through_native_wallet() {
    let (url, handle, state) = server(Outcome::Success).await;
    let fixture = Fixture::new(&url);
    let path = fixture.dir.path().join("thruput.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let profile = config["profile"]["default"].as_table_mut().unwrap();
    let owner = profile["wallets"]["owner"]["owner_public_key"]
        .as_str()
        .unwrap();
    let account = PubkeyAccount::from_public_key_bytes(
        thruput_signing::strkey::parse_public_key(owner).unwrap(),
        0,
    );
    profile.remove("wallets");
    profile.remove("default_wallet");
    std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let target = thruput_encoding::encode(&[0xab; 32]);
    let outputs = [
        (
            fixture
                .command()
                .args(["call", &target, "0x0102030405060708", "0x", "--dry-run"])
                .output()
                .unwrap(),
            "signedTransaction",
            thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
        ),
        (
            fixture
                .command()
                .args(["transfer", &target, "1", "--dry-run"])
                .output()
                .unwrap(),
            "signedTransaction",
            thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
        ),
        (
            fixture.deploy(&["--fund", "0", "--dry-run"]),
            "signedTx",
            thruput_signing::default_wallet::DEFAULT_WALLET_DEPLOY_ENTRYPOINT,
        ),
    ];
    for (output, field, entrypoint) in outputs {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        let tx = signed(report["data"][field].as_str().unwrap());
        assert_eq!(tx.tx.invocation.invoked_address, account.address());
        assert_eq!(tx.tx.auth_claim, account.auth_claim());
        assert_eq!(tx.tx.invocation.entrypoint, entrypoint);
    }
    assert!(state.submissions.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    handle.stop().unwrap();
}

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Success,
    SendError,
    ConnectionLost,
    Rejected(i32),
    SendHashMismatch,
    LookupError,
    ReceiptHashMismatch,
    MissingReturn,
    ShortReturn,
    WrongAddress,
    Reverted,
    FundingRejected,
    InitializerRejected,
}
struct State {
    outcome: Outcome,
    submissions: Mutex<Vec<String>>,
    receipt_requests: std::sync::atomic::AtomicUsize,
}
fn signed(hex_value: &str) -> SignedTransaction {
    xdrs::txs::decode(&hex::decode(hex_value.trim_start_matches("0x")).unwrap()).unwrap()
}
fn identity(signed: &SignedTransaction) -> (String, String) {
    let hash = thruput_encoding::encode(transaction_hash(&signed.tx).unwrap().as_ref());
    let (nonce, code_hash, _amount, _init): DeployCalldata =
        xdrs::cxs::decode(&signed.tx.invocation.calldata.0).unwrap();
    let address =
        derive_deploy_address(&signed.tx.invocation.invoked_address, &nonce, &code_hash).unwrap();
    (hash, thruput_encoding::encode(address.as_ref()))
}
async fn server(outcome: Outcome) -> (String, ServerHandle, Arc<State>) {
    let state = Arc::new(State {
        outcome,
        submissions: Mutex::new(vec![]),
        receipt_requests: std::sync::atomic::AtomicUsize::new(0),
    });
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(state.clone());
    module.register_method("getVersionInfo", |_, _, _| json!({"wireFormat":"14.0.0", "implementation":"test", "supportedMethods":["getVersionInfo","getBalance","getNetworkInfo","getChainTip","sendTransaction","getTransactionStatus","getTransactionReceipt"]})).unwrap();
    module
        .register_method(
            "getTransactionStatus",
            |_, _, _| json!({"status":"unknown"}),
        )
        .unwrap();

    module.register_method("getNetworkInfo", |_, _, _| json!({"networkHash":NETWORK, "genesisHash":thruput_encoding::encode(&[0x42; 32])})).unwrap();
    module
        .register_method(
            "getChainTip",
            |_, _, _| json!({"blockHeight":"42","blockHash":thruput_encoding::encode(&[0; 32])}),
        )
        .unwrap();
    module
        .register_method("sendTransaction", |params, state, _| {
            let request: thruput_json_rpc_types::requests::SendTransactionRequest = params.parse()?;
            let bytes = format!("0x{}", hex::encode(request.signed_transaction.0));
            let (hash, _) = identity(&signed(&bytes));
            state.submissions.lock().unwrap().push(bytes);
            match state.outcome {
                Outcome::Rejected(code) => Err(ErrorObjectOwned::owned(
                    code,
                    "definite pre-admission rejection",
                    None::<()>,
                )),
                Outcome::SendError => Err(ErrorObjectOwned::owned(
                    thruput_json_rpc_types::error::codes::BACKEND_TIMEOUT,
                    "post-admission response failure",
                    None::<()>,
                )),
                Outcome::ConnectionLost => Err(ErrorObjectOwned::owned(
                    thruput_json_rpc_types::error::codes::BACKEND_UNAVAILABLE,
                    "scheduler connection lost after dispatch",
                    None::<()>,
                )),
                Outcome::SendHashMismatch => Ok(json!({"transactionHash":thruput_encoding::encode(&[0xbb;32]),"admission":"accepted"})),
                _ => Ok(json!({"transactionHash":hash,"admission":"accepted"})),
            }
        })
        .unwrap();
    module.register_method("getTransactionReceipt", |_, state, _| {
        state.receipt_requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if matches!(state.outcome, Outcome::LookupError) { return Err(ErrorObjectOwned::owned(-32000, "receipt unavailable", None::<()>)); }
        let signed = signed(state.submissions.lock().unwrap().first().unwrap());
        let (hash, address) = identity(&signed);
        let returned = format!("0x{}", hex::encode(thruput_encoding::decode_fixed::<32>(&address).unwrap()));
        let mut receipt = json!({"transactionHash":hash,"blockHeight":"43","execution":{"result":"return","returnData":returned},"events":[],"gasUsed":"99"});
        match state.outcome {
            Outcome::ReceiptHashMismatch => receipt["transactionHash"] = json!(thruput_encoding::encode(&[0xbb; 32])),
            Outcome::MissingReturn => { receipt["execution"].as_object_mut().unwrap().remove("returnData"); },
            Outcome::ShortReturn => receipt["execution"]["returnData"] = json!("0x01"),
            Outcome::WrongAddress => receipt["execution"]["returnData"] = json!(format!("0x{}", "bb".repeat(32))),
            Outcome::Reverted => { receipt["execution"] = json!({"result":"abort","abortData":"0x"}); receipt["execution"].as_object_mut().unwrap().remove("returnData"); },
            Outcome::FundingRejected | Outcome::InitializerRejected => {
                let abort = if matches!(state.outcome, Outcome::FundingRejected) {
                    xdrs::cxs::encode(&(<thruput_erc20_if::ERC20Error as thruput_sdk::exn::Exn>::SELECTOR, thruput_erc20_if::ERC20Error::TooLarge)).unwrap()
                } else { xdrs::cxs::encode(&(42u64, 7u64)).unwrap() };
                receipt["execution"] = json!({"result":"abort","abortData":"0x"});
                receipt["execution"]["abortData"] = json!(format!("0x{}", hex::encode(abort)));
                receipt["execution"].as_object_mut().unwrap().remove("returnData");
            },
            _ => {}
        }
        Ok::<_,ErrorObjectOwned>(receipt)
    }).unwrap();
    (url, server.start(module), state)
}

#[test]
fn native_deploy_requires_explicit_funding_without_rpc() {
    let fixture = Fixture::new("http://127.0.0.1:1");
    for (args, code) in [
        (vec![], "native_deployment_funding_required"),
        (vec!["--fund", "0.00000001"], "invalid_native_token_amount"),
    ] {
        let output = fixture.deploy(&args);
        assert!(!output.status.success());
        let report: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(report["error"]["code"], code, "{report}");
    }
    assert!(
        fixture.config()["profile"]["default"]
            .get("contracts")
            .is_none()
    );
}

#[test]
fn legacy_wallet_rejects_native_funding_options_without_rpc() {
    let fixture = Fixture::new("http://127.0.0.1:1");
    let path = fixture.dir.path().join("thruput.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let wallet = config["profile"]["default"]["wallets"]["owner"]
        .as_table_mut()
        .unwrap();
    wallet.remove("account_nonce");
    wallet.remove("kind");
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    for args in [vec!["--fund", "5"], vec!["--init", "initialize"]] {
        let output = fixture.deploy(&args);
        assert!(!output.status.success());
        let report: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            report["error"]["code"],
            "native_deployment_options_for_legacy"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_deployment_rejects_network_mismatch_before_submission() {
    let (url, handle, state) = server(Outcome::Success).await;
    let fixture = Fixture::new(&url);
    let path = fixture.dir.path().join("thruput.toml");
    let config = std::fs::read_to_string(&path)
        .unwrap()
        .replace(NETWORK, &thruput_encoding::encode(&[0x08; 32]));
    std::fs::write(path, config).unwrap();
    let output = fixture.deploy(&["--fund", "5"]);
    assert!(!output.status.success());
    assert!(state.submissions.lock().unwrap().is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("network")
    );
    handle.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_deploy_dry_run_describes_canonical_effects_without_sending() {
    let (url, handle, state) = server(Outcome::Success).await;
    let fixture = Fixture::new(&url);
    let output = fixture.deploy(&["--fund", "5.0000001", "--dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &report["data"];
    let tx = signed(data["signedTx"].as_str().unwrap());
    let (hash, address) = identity(&tx);
    assert_eq!(data["txHash"], hash);
    assert_eq!(data["predictedAddress"], address);
    assert_eq!(
        tx.tx.invocation.entrypoint,
        thruput_signing::default_wallet::DEFAULT_WALLET_DEPLOY_ENTRYPOINT
    );
    let (nonce, code_hash, amount, initializer): DeployCalldata =
        xdrs::cxs::decode(&tx.tx.invocation.calldata.0).unwrap();
    assert_eq!(amount, 50_000_001);
    assert_eq!(initializer, None);
    assert_eq!(data["nativeDeployment"]["fundingRawAmount"], "50000001");
    assert_eq!(
        data["nativeDeployment"]["deploymentNonce"],
        format!("0x{}", hex::encode(nonce.as_ref()))
    );
    assert_eq!(data["nativeDeployment"]["networkHash"], NETWORK);
    assert_eq!(
        code_hash,
        wallet_contract_hash(&tx.tx.contracts_to_deploy.0[0]).unwrap()
    );
    let owner = fixture.config()["profile"]["default"]["wallets"]["owner"]["owner_public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let account = PubkeyAccount::from_public_key_bytes(
        thruput_signing::strkey::parse_public_key(&owner).unwrap(),
        0,
    );
    assert_eq!(tx.tx.invocation.invoked_address, account.address());
    assert_eq!(tx.tx.auth_claim, account.auth_claim());
    assert!(state.submissions.lock().unwrap().is_empty());
    assert!(
        fixture.config()["profile"]["default"]
            .get("contracts")
            .is_none()
    );
    handle.stop().unwrap();
}

#[test]
fn initializer_options_require_a_method_and_valid_arguments_before_rpc() {
    let fixture = Fixture::new("http://127.0.0.1:1");
    for args in [
        vec!["--init-arg", "42"],
        vec!["--init-gas-limit", "100"],
        vec!["--init", "0x000000000000003f", "--init-gas-limit", "0"],
        vec!["--init", "0x000000000000003f", "--init-arg", "not-hex"],
        vec!["--init", "initialize"],
        vec![
            "--init",
            "0x000000000000003f",
            "--init-gas-limit",
            "18446744073709551615",
        ],
    ] {
        let mut all = vec!["--fund", "5"];
        all.extend(args);
        let output = fixture.deploy(&all);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("connection"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_initializer_uses_the_deployed_artifact_spec_and_typed_arguments() {
    use thruput_contract_spec::{
        ContractField, ContractSpec, ContractSpecInterface, ContractSpecMethod, ContractType,
    };
    let (url, handle, state) = server(Outcome::Success).await;
    let fixture = Fixture::new(&url);
    let spec = ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: "Counter".try_into().unwrap(),
            vers: "0.1.0".try_into().unwrap(),
            throws: vec![].try_into().unwrap(),
            methods: vec![ContractSpecMethod {
                name: "initialize".try_into().unwrap(),
                args: vec![ContractField {
                    name: "value".try_into().unwrap(),
                    ty: ContractType::U64,
                }]
                .try_into()
                .unwrap(),
                return_type: ContractType::Void,
                effect: None,
            }]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };
    std::fs::write(
        fixture.dir.path().join("counter.spec.xdr"),
        xdrs::txs::encode(&spec).unwrap(),
    )
    .unwrap();
    let output = fixture.deploy(&[
        "--fund",
        "5",
        "--init",
        "initialize",
        "--init-arg",
        "42",
        "--dry-run",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let init = &report["data"]["nativeDeployment"]["initializer"];
    assert_eq!(init["entrypointId"], "0x2bfe21a62e5119c4");
    assert_eq!(init["calldata"], "0x2a");
    let tx = signed(report["data"]["signedTx"].as_str().unwrap());
    assert_eq!(
        tx.tx.invocation.gas_limit,
        thruput_signing::DEFAULT_TX_GAS_LIMIT + thruput_cli::call::DEFAULT_CALL_GAS_LIMIT
    );
    assert_eq!(
        init["gasLimit"],
        thruput_cli::call::DEFAULT_CALL_GAS_LIMIT.to_string()
    );
    for value in ["not-a-number", "18446744073709551616"] {
        let bad = fixture.deploy(&["--fund", "5", "--init", "initialize", "--init-arg", value]);
        assert!(!bad.status.success());
    }
    assert!(state.submissions.lock().unwrap().is_empty());
    handle.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_deploy_decodes_funding_errors_without_misattributing_initializer_errors() {
    for init in [false, true] {
        let (url, handle, _) = server(Outcome::FundingRejected).await;
        let fixture = Fixture::new(&url);
        let before = fixture.config();
        let mut args = vec!["--fund", "5"];
        if init {
            args.extend(["--init", "0x0102030405060708"]);
        }
        let output = fixture.deploy(&args);
        assert!(!output.status.success());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["error"]["code"],
            if init {
                "transaction_reverted"
            } else {
                "transaction_insufficient_funds"
            }
        );
        assert_eq!(report["data"]["abortException"], "ERC20Error");
        assert_eq!(report["data"]["abortValue"], "TooLarge");
        if init {
            assert!(report["error"]["hint"].is_null());
        }
        assert_eq!(fixture.config(), before);
        handle.stop().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_deploy_decodes_the_prepared_initializer_spec() {
    use thruput_contract_spec::{
        ContractExceptionType, ContractSpec, ContractSpecInterface, ContractSpecMethod,
        ContractType,
    };
    let (url, handle, _) = server(Outcome::InitializerRejected).await;
    let fixture = Fixture::new(&url);
    let spec = ContractSpec {
        interfaces: vec![ContractSpecInterface {
            name: "Counter".try_into().unwrap(),
            vers: "0.1.0".try_into().unwrap(),
            throws: vec![ContractExceptionType {
                selector: 42,
                ty: ContractType::U64,
            }]
            .try_into()
            .unwrap(),
            methods: vec![ContractSpecMethod {
                name: "initialize".try_into().unwrap(),
                args: vec![].try_into().unwrap(),
                return_type: ContractType::Void,
                effect: None,
            }]
            .try_into()
            .unwrap(),
        }]
        .try_into()
        .unwrap(),
        type_defs: vec![].try_into().unwrap(),
        events: vec![].try_into().unwrap(),
    };
    std::fs::write(
        fixture.dir.path().join("counter.spec.xdr"),
        xdrs::txs::encode(&spec).unwrap(),
    )
    .unwrap();
    let before = fixture.config();
    let output = fixture.deploy(&["--fund", "5", "--init", "initialize"]);
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["error"]["code"], "transaction_reverted");
    assert_eq!(report["data"]["abortValue"], "7");
    assert!(report["data"].get("abortDecodeError").is_none());
    assert_eq!(fixture.config(), before);
    handle.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_funding_and_initializer_are_preserved_in_dry_run_and_recovery() {
    for (outcome, dry_run) in [(Outcome::Success, true), (Outcome::SendError, false)] {
        let (url, handle, state) = server(outcome).await;
        let fixture = Fixture::new(&url);
        let mut args = vec![
            "--fund",
            "0.0000000",
            "--init",
            "0x000000000000003f",
            "--init-arg",
            "0xabcd",
            "--init-gas-limit",
            "12345",
        ];
        if dry_run {
            args.push("--dry-run");
        }
        let output = fixture.deploy(&args);
        assert_eq!(
            output.status.success(),
            dry_run,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        let data = &report["data"];
        let tx = signed(data["signedTx"].as_str().unwrap());
        let (_, _, amount, initializer): DeployCalldata =
            xdrs::cxs::decode(&tx.tx.invocation.calldata.0).unwrap();
        assert_eq!(amount, 0);
        assert_eq!(
            initializer,
            Some((
                ContractEntrypointID::from(63u64.to_be_bytes()),
                12345,
                vec![0xab, 0xcd]
            ))
        );
        assert_eq!(data["schemaVersion"], 9);
        assert_eq!(data["nativeDeployment"]["fundingRawAmount"], "0");
        assert_eq!(
            data["nativeDeployment"]["initializer"]["entrypointId"],
            "0x000000000000003f"
        );
        assert_eq!(
            data["nativeDeployment"]["initializer"]["calldata"],
            "0xabcd"
        );
        assert_eq!(data["nativeDeployment"]["initializer"]["gasLimit"], "12345");
        assert_eq!(
            state.submissions.lock().unwrap().len(),
            usize::from(!dry_run)
        );
        handle.stop().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_deploy_preserves_rpc_rejections_without_repeating_submission() {
    use thruput_json_rpc_types::error::codes;
    for code in [
        codes::INVALID_PARAMS,
        codes::TX_INVALID,
        codes::TX_REJECTED,
        codes::OVERLOADED,
        -32601,
    ] {
        let (url, handle, state) = server(Outcome::Rejected(code)).await;
        let fixture = Fixture::new(&url);
        let output = fixture.deploy(&["--fund", "5"]);
        assert!(!output.status.success());
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["error"]["code"], "native_rpc_failure");
        assert_eq!(
            report["error"]["details"]["cause"]["rpcError"]["code"],
            code
        );
        assert!(report["error"]["details"]["cause"]["rpcError"]["data"].is_null());
        assert_eq!(state.submissions.lock().unwrap().len(), 1);
        assert_eq!(
            state
                .receipt_requests
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(
            fixture.config()["profile"]["default"]
                .get("contracts")
                .is_none()
        );
        handle.stop().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_deploy_preserves_identity_and_never_saves_alias_for_unverified_results() {
    for outcome in [
        Outcome::SendError,
        Outcome::ConnectionLost,
        Outcome::SendHashMismatch,
        Outcome::LookupError,
        Outcome::ReceiptHashMismatch,
        Outcome::MissingReturn,
        Outcome::ShortReturn,
        Outcome::WrongAddress,
    ] {
        let (url, handle, state) = server(outcome).await;
        let fixture = Fixture::new(&url);
        let output = fixture.deploy(&["--fund", "5"]);
        assert!(!output.status.success(), "{outcome:?}");
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        let submitted = state.submissions.lock().unwrap();
        assert_eq!(submitted.len(), 1);
        let (hash, address) = identity(&signed(&submitted[0]));
        assert_eq!(report["data"]["txHash"], hash, "{outcome:?}: {report}");
        assert_eq!(report["data"]["predictedAddress"], address);
        assert_eq!(report["data"]["signedTx"], submitted[0]);
        if matches!(outcome, Outcome::ShortReturn | Outcome::WrongAddress) {
            assert_eq!(report["data"]["confirmation"], "committed");
            assert_eq!(report["data"]["receipt"]["execution"]["result"], "return");
            assert!(report["data"]["receipt"]["blockHeight"].is_string());
        }
        assert_eq!(
            report["data"]["nativeDeployment"]["fundingRawAmount"],
            "50000000"
        );
        assert!(
            report["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("rerunning deploy generates a new nonce")
        );
        assert!(
            fixture.config()["profile"]["default"]
                .get("contracts")
                .is_none()
        );
        handle.stop().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_verified_success_saves_the_native_alias() {
    for outcome in [Outcome::Success, Outcome::Reverted] {
        let (url, handle, state) = server(outcome).await;
        let fixture = Fixture::new(&url);
        let output = fixture.deploy(&["--fund", "5"]);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        let submissions = state.submissions.lock().unwrap();
        assert_eq!(submissions.len(), 1);
        let (_, address) = identity(&signed(&submissions[0]));
        if matches!(outcome, Outcome::Success) {
            assert!(output.status.success(), "{report}");
            assert_eq!(
                fixture.config()["profile"]["default"]["contracts"]["counter"],
                address
            );
        } else {
            assert!(!output.status.success());
            assert_eq!(report["error"]["code"], "transaction_reverted");
            assert!(
                fixture.config()["profile"]["default"]
                    .get("contracts")
                    .is_none()
            );
        }
        handle.stop().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_hint_preserves_explicit_profile_without_a_default() {
    let (url, handle, _) = server(Outcome::SendError).await;
    let fixture = Fixture::new(&url);
    let path = fixture.dir.path().join("thruput.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    let mut config: toml::Value = toml::from_str(&config).unwrap();
    config.as_table_mut().unwrap().remove("default_profile");
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    let output = fixture.deploy(&["--profile", "default", "--fund", "5"]);
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let hint = report["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("--profile=default"), "{hint}");
    assert!(hint.contains(&format!("--rpc-url={url}")), "{hint}");
    assert_eq!(report["data"]["profile"], "default");
    handle.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_alias_write_failure_preserves_verified_deployment() {
    let (url, handle, state) = server(Outcome::Success).await;
    let fixture = Fixture::new(&url);
    let path = fixture.dir.path().join("thruput.toml");
    let config = std::fs::read_to_string(&path)
        .unwrap()
        .replace("[profile.default]", "[profile.default]\ncontracts = {}");
    std::fs::write(path, config).unwrap();
    let output = fixture.deploy(&["--fund", "5"]);
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let submitted = state.submissions.lock().unwrap();
    assert_eq!(submitted.len(), 1);
    let (hash, address) = identity(&signed(&submitted[0]));
    assert_eq!(report["data"]["txHash"], hash);
    assert_eq!(report["data"]["address"], address);
    assert_eq!(report["data"]["addressSource"], "receipt");
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "deploy.alias_not_saved")
    );
    assert!(
        fixture.config()["profile"]["default"]["contracts"]
            .get("counter")
            .is_none()
    );
    handle.stop().unwrap();
}
