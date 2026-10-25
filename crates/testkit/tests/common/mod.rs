//! Fixtures shared by the testkit integration tests.

use jsonrpsee::{RpcModule, types::ErrorObjectOwned};

/// Serve the network identity named by the runtime manifest fixture (a zero `networkHash`).
pub fn serve_manifest_network_info<T: Send + Sync + 'static>(module: &mut RpcModule<T>) {
    module.register_method("getVersionInfo", |_, _, _| Ok::<_, ErrorObjectOwned>(serde_json::json!({
        "wireFormat": "14.0.0", "softwareVersion": "test", "implementation": "test",
        "supportedMethods": ["getNetworkInfo", "sendTransaction", "getTransactionStatus", "getTransactionReceipt", "dev_fundAccount"]
    }))).unwrap();

    module
        .register_method("getNetworkInfo", |_, _, _| {
            Ok::<_, ErrorObjectOwned>(thruput_json_rpc_types::responses::NetworkInfo {
                network_hash: thruput_json_rpc_types::NetworkHash::from(
                    thruput_xdr::types::SHA256Hash::from([0; 32]),
                ),
            })
        })
        .unwrap();
}
