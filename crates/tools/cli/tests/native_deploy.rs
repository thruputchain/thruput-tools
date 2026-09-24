//! Process-level native deployment contract and failure-recovery tests.
use groundhog_signing::{
    PubkeyAccount, derive_deploy_address, transaction_hash, wallet_contract_hash,
};
use groundhog_xdr::{transaction::SignedTransaction, types::SHA256Hash};
use jsonrpsee::{
    RpcModule,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use serde_json::{Value, json};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const NETWORK: &str = "0707070707070707070707070707070707070707070707070707070707070707";

struct Fixture {
    dir: TempDir,
}
impl Fixture {
    fn new(rpc: &str) -> Self {
        let fixture = Self {
            dir: TempDir::new().unwrap(),
        };
        std::fs::write(fixture.dir.path().join("groundhog.toml"), format!("default_profile = 'default'\n[profile.default]\ndefault_key = 'owner'\nrpc_url = '{rpc}'\nnetwork_hash = '{NETWORK}'\n")).unwrap();
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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_groundhog"));
        cmd.arg("--json")
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("GROUNDHOG_PROFILE")
            .env_remove("GROUNDHOG_RUNTIME_MANIFEST");
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
        let parsed: toml::Value = toml::from_str(
            &std::fs::read_to_string(self.dir.path().join("groundhog.toml")).unwrap(),
        )
        .unwrap();
        serde_json::to_value(parsed).unwrap()
    }
}

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Success,
    SendError,
    SendHashMismatch,
    LookupError,
    ReceiptHashMismatch,
    MissingReturn,
    ShortReturn,
    WrongAddress,
    Reverted,
}
struct State {
    outcome: Outcome,
    submissions: Mutex<Vec<String>>,
}
fn signed(hex_value: &str) -> SignedTransaction {
    xdrs::txs::decode(&hex::decode(hex_value.trim_start_matches("0x")).unwrap()).unwrap()
}
fn identity(signed: &SignedTransaction) -> (String, String) {
    let hash = format!(
        "0x{}",
        hex::encode(transaction_hash(&signed.tx).unwrap().as_ref())
    );
    let (nonce, code_hash, _amount): (SHA256Hash, SHA256Hash, u128) =
        xdrs::cxs::decode(&signed.tx.invocation.calldata.0).unwrap();
    let address =
        derive_deploy_address(&signed.tx.invocation.invoked_address, &nonce, &code_hash).unwrap();
    (hash, format!("0x{}", hex::encode(address.as_ref())))
}
async fn server(outcome: Outcome) -> (String, ServerHandle, Arc<State>) {
    let state = Arc::new(State {
        outcome,
        submissions: Mutex::new(vec![]),
    });
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(state.clone());
    module.register_method("groundhog_networkInfo", |_, _, _| json!({"networkHash":format!("0x{NETWORK}"), "genesisHash":format!("0x{}", "42".repeat(32))})).unwrap();
    module
        .register_method(
            "groundhog_chainTip",
            |_, _, _| json!({"height":42,"blockHash":format!("0x{}","00".repeat(32))}),
        )
        .unwrap();
    module
        .register_method("groundhog_sendRawTransaction", |params, state, _| {
            let (bytes,): (String,) = params.parse()?;
            let (hash, _) = identity(&signed(&bytes));
            state.submissions.lock().unwrap().push(bytes);
            match state.outcome {
                Outcome::SendError => Err(ErrorObjectOwned::owned(
                    -32000,
                    "post-admission response failure",
                    None::<()>,
                )),
                Outcome::SendHashMismatch => Ok(format!("0x{}", "bb".repeat(32))),
                _ => Ok(hash),
            }
        })
        .unwrap();
    module.register_method("groundhog_getTransactionReceipt", |_, state, _| {
        if matches!(state.outcome, Outcome::LookupError) { return Err(ErrorObjectOwned::owned(-32000, "receipt unavailable", None::<()>)); }
        let signed = signed(state.submissions.lock().unwrap().first().unwrap());
        let (hash, address) = identity(&signed);
        let mut receipt = json!({"txHash":hash,"height":43,"status":{"result":"ok"},"events":[],"gasUsed":99,"returnData":address});
        match state.outcome {
            Outcome::ReceiptHashMismatch => receipt["txHash"] = json!(format!("0x{}", "bb".repeat(32))),
            Outcome::MissingReturn => { receipt.as_object_mut().unwrap().remove("returnData"); },
            Outcome::ShortReturn => receipt["returnData"] = json!("0x01"),
            Outcome::WrongAddress => receipt["returnData"] = json!(format!("0x{}", "bb".repeat(32))),
            Outcome::Reverted => { receipt["status"] = json!({"result":"reverted"}); receipt.as_object_mut().unwrap().remove("returnData"); },
            _ => {}
        }
        Ok::<_,ErrorObjectOwned>(receipt)
    }).unwrap();
    (url, server.start(module), state)
}

#[test]
fn native_deploy_requires_opt_in_and_exact_positive_funding_without_rpc() {
    let fixture = Fixture::new("http://127.0.0.1:1");
    for (args, code) in [
        (vec![], "native_wallet_deployment_unavailable"),
        (vec!["--fund", "5"], "native_wallet_deployment_unavailable"),
        (
            vec!["--experimental-native-deploy"],
            "native_deployment_funding_required",
        ),
        (
            vec!["--experimental-native-deploy", "--fund", "0"],
            "invalid_hog_amount",
        ),
        (
            vec!["--experimental-native-deploy", "--fund", "0.00000001"],
            "invalid_hog_amount",
        ),
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
    let path = fixture.dir.path().join("groundhog.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let wallet = config["profile"]["default"]["wallets"]["owner"]
        .as_table_mut()
        .unwrap();
    wallet.remove("account_nonce");
    wallet.remove("kind");
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    for args in [vec!["--fund", "5"], vec!["--experimental-native-deploy"]] {
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
    let path = fixture.dir.path().join("groundhog.toml");
    let config = std::fs::read_to_string(&path)
        .unwrap()
        .replace(NETWORK, &"08".repeat(32));
    std::fs::write(path, config).unwrap();
    let output = fixture.deploy(&["--experimental-native-deploy", "--fund", "5"]);
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
    let output = fixture.deploy(&[
        "--experimental-native-deploy",
        "--fund",
        "5.0000001",
        "--dry-run",
    ]);
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
        groundhog_signing::default_wallet::DEFAULT_WALLET_DEPLOY_FUNDED_ENTRYPOINT
    );
    let (nonce, code_hash, amount): (SHA256Hash, SHA256Hash, u128) =
        xdrs::cxs::decode(&tx.tx.invocation.calldata.0).unwrap();
    assert_eq!(amount, 50_000_001);
    assert_eq!(data["nativeDeployment"]["fundingRawAmount"], "50000001");
    assert_eq!(
        data["nativeDeployment"]["runtimeSupport"],
        "operator_asserted"
    );
    assert_eq!(
        data["nativeDeployment"]["deploymentNonce"],
        format!("0x{}", hex::encode(nonce.as_ref()))
    );
    assert_eq!(
        data["nativeDeployment"]["networkHash"],
        format!("0x{NETWORK}")
    );
    assert_eq!(
        code_hash,
        wallet_contract_hash(&tx.tx.contracts_to_deploy.0[0]).unwrap()
    );
    let owner = fixture.config()["profile"]["default"]["wallets"]["owner"]["owner_public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let account = PubkeyAccount::from_public_key_bytes(
        groundhog_signing::strkey::parse_public_key(&owner).unwrap(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_deploy_preserves_identity_and_never_saves_alias_for_unverified_results() {
    for outcome in [
        Outcome::SendError,
        Outcome::SendHashMismatch,
        Outcome::LookupError,
        Outcome::ReceiptHashMismatch,
        Outcome::MissingReturn,
        Outcome::ShortReturn,
        Outcome::WrongAddress,
    ] {
        let (url, handle, state) = server(outcome).await;
        let fixture = Fixture::new(&url);
        let output = fixture.deploy(&["--experimental-native-deploy", "--fund", "5"]);
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
        let output = fixture.deploy(&["--experimental-native-deploy", "--fund", "5"]);
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
    let path = fixture.dir.path().join("groundhog.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    let mut config: toml::Value = toml::from_str(&config).unwrap();
    config.as_table_mut().unwrap().remove("default_profile");
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    let output = fixture.deploy(&[
        "--profile",
        "default",
        "--experimental-native-deploy",
        "--fund",
        "5",
    ]);
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
    let path = fixture.dir.path().join("groundhog.toml");
    let config = std::fs::read_to_string(&path)
        .unwrap()
        .replace("[profile.default]", "[profile.default]\ncontracts = {}");
    std::fs::write(path, config).unwrap();
    let output = fixture.deploy(&["--experimental-native-deploy", "--fund", "5"]);
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
