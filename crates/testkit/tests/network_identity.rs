//! Connecting to an existing network requires the endpoint to serve the manifest's network.

use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use thruput_json_rpc_types::responses::NetworkInfo;
use thruput_testkit::{NetworkConfig, Scenario, TestkitError};

#[tokio::test]
async fn endpoint_serving_another_network_is_rejected() {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(());
    module.register_method("getVersionInfo", |_, (), _| Ok::<_, ErrorObjectOwned>(serde_json::json!({
        "wireFormat": "15.0.0", "softwareVersion": "test", "implementation": "test",
        "supportedMethods": ["getNetworkInfo", "sendTransaction", "getTransactionStatus", "getTransactionReceipt", "dev_fundAccount"]
    }))).unwrap();

    module
        .register_method("getNetworkInfo", |_, (), _| {
            Ok::<_, ErrorObjectOwned>(NetworkInfo {
                network_hash: thruput_json_rpc_types::NetworkHash::from(
                    thruput_xdr::types::SHA256Hash::from([7; 32]),
                ),
            })
        })
        .unwrap();
    let handle = server.start(module);
    let manifest = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        manifest.path(),
        include_str!("fixtures/hosted-runtime.json"),
    )
    .unwrap();

    let result = Scenario::builder("network-identity")
        .accounts(1)
        .network(NetworkConfig::existing(&url, manifest.path()))
        .build()
        .await;

    assert!(matches!(
        result,
        Err(TestkitError::Manifest {
            field: "networkHash",
            ..
        })
    ));
    handle.stop().unwrap();
}
