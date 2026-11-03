//! Fixtures shared by the testkit integration tests.

use jsonrpsee::{RpcModule, types::ErrorObjectOwned};

/// Serve the network identity named by the runtime manifest fixture (a zero `networkHash`).
pub fn serve_manifest_network_info<T: Send + Sync + 'static>(module: &mut RpcModule<T>) {
    module.register_method("getVersionInfo", |_, _, _| Ok::<_, ErrorObjectOwned>(serde_json::json!({
        "wireFormat": "15.0.0", "softwareVersion": "test", "implementation": "test",
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

/// A hosted runtime manifest whose bootstrap accounts match the dev-account seed.
#[allow(dead_code)] // Not every test target uses it.
pub fn native_runtime_document() -> serde_json::Value {
    let mut document: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/hosted-runtime.json")).unwrap();
    document["kind"] = "hosted-devnet-runtime".into();
    for index in 0..2 {
        let signer = thruput_signing::derive_dev_account_signer("thruput-dev-accounts", index);
        document["bootstrapAccounts"][index]["publicKey"] =
            serde_json::to_value(thruput_json_rpc_types::Ed25519PublicKey::from(
                thruput_xdr::types::Ed25519PublicKey::from(signer.verifying_key_bytes()),
            ))
            .unwrap();
        document["bootstrapAccounts"][index]["walletAddress"] =
            serde_json::to_value(thruput_json_rpc_types::ContractAddress::from(
                thruput_signing::PubkeyAccount::from_public_key_bytes(
                    signer.verifying_key_bytes(),
                    0,
                )
                .address(),
            ))
            .unwrap();
    }
    document
}
