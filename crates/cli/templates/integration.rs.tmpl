//! RPC integration tests run by `thruput test --integration`.
//!
//! The command builds the contract, starts an ephemeral local node, deploys
//! the contract with its own temporary stored key, and then runs this
//! test target. It passes the node and deployment details through the
//! environment variables below.
//! `THRUPUT_PROFILE` and `THRUPUT_DEFAULT_KEY` describe that deployment;
//! connect directly to `THRUPUT_RPC_URL` from these tests.

#[cfg(not(feature = "thruput-integration-test"))]
#[test]
#[ignore = "requires a local node; run via `thruput test --integration`"]
fn integration_tests_run_with_thruput() {}

#[cfg(feature = "thruput-integration-test")]
mod live {
    use thruput_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
    use std::{env, path::Path};

    // Ordinary Cargo leaves this ignored. `thruput test --integration`
    // activates it through the scaffold's build script.
    #[tokio::test]
    #[cfg_attr(
        not(thruput_integration_runner),
        ignore = "requires a local node; run via `thruput test --integration`"
    )]
    async fn integration_runner_provides_a_live_deployment() {
        if env::var("THRUPUT_INTEGRATION_TEST").as_deref() != Ok("1") {
            eprintln!("skipped: run with `thruput test --integration`");
            return;
        }

        let rpc_url = required_env("THRUPUT_RPC_URL");
        let contract_address = required_env("THRUPUT_CONTRACT_ADDRESS");
        let profile = required_env("THRUPUT_PROFILE");
        let default_key = required_env("THRUPUT_DEFAULT_KEY");

        assert!(
            rpc_url.starts_with("http://"),
            "unexpected RPC URL: {rpc_url}"
        );
        assert_contract_address(&contract_address);
        assert_eq!(profile, "integration");
        assert_eq!(default_key, "integration");

        let client = HttpClientBuilder::default()
            .build(&rpc_url)
            .expect("RPC URL should be valid");
        let tip = client
            .chain_tip()
            .await
            .expect("ephemeral node should answer chainTip");
        assert!(
            *tip.height > 0,
            "ephemeral chain should advance before integration tests run"
        );

        if let Some(runtime_manifest) = env::var_os("THRUPUT_RUNTIME_MANIFEST") {
            assert!(
                Path::new(&runtime_manifest).is_file(),
                "runtime manifest does not exist: {}",
                Path::new(&runtime_manifest).display()
            );
        }

        // Add contract-specific RPC assertions here using `contract_address`.
    }

    fn required_env(name: &str) -> String {
        env::var(name).unwrap_or_else(|_| panic!("{name} is set by `thruput test --integration`"))
    }

    fn assert_contract_address(address: &str) {
        let hex = address
            .strip_prefix("0x")
            .unwrap_or_else(|| panic!("contract address is missing its 0x prefix: {address}"));
        assert_eq!(hex.len(), 64, "contract address must contain 32 bytes");
        assert!(
            hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "contract address contains non-hex characters: {address}"
        );
    }
}
