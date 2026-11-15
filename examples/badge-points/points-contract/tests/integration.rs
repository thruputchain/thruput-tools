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
    use thruput_json_rpc_client::{HttpClientBuilder, native::NativeClient};
    use thruput_json_rpc_client::types::{requests::EmptyRequest, responses::DecodedResponse};
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

        let client = NativeClient::new(HttpClientBuilder::default()
            .build(&rpc_url)
            .expect("RPC URL should be valid"));
        let DecodedResponse::Known(tip) = client
            .get_chain_tip(EmptyRequest {})
            .await
            .expect("ephemeral node should answer getChainTip") else {
            panic!("ephemeral node returned an unsupported chain tip");
        };
        assert!(
            *tip.block_height > 0,
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
        serde_json::from_value::<thruput_json_rpc_client::types::ContractAddress>(
            serde_json::Value::String(address.to_owned()),
        )
        .expect("contract address must be base58 encoding of 32 bytes");
    }
}
