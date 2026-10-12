use thruput_json_rpc_types::network_descriptor::{
    NETWORK_DESCRIPTOR_SCHEMA_VERSION, NetworkDescriptor,
};

#[test]
fn native_descriptor_requires_no_legacy_wallet_metadata() {
    let value = serde_json::json!({
        "schemaVersion": NETWORK_DESCRIPTOR_SCHEMA_VERSION,
        "kind": "groundhog-network", "networkId": "devnet", "protocol": "x23",
        "networkHash": format!("0x{}", "00".repeat(32)),
        "genesisHash": format!("0x{}", "00".repeat(32)),
        "endpoints": { "rpcUrl": "https://rpc.example" },
        "nativeHog": { "tokenAddress": format!("0x{}", "00".repeat(32)), "decimals": 7, "transferEntrypoint": "0x63285b3523f9dc3f" },
        "faucet": { "walletAddress": format!("0x{}", "00".repeat(32)) }
    });
    let descriptor: NetworkDescriptor = serde_json::from_value(value).unwrap();
    let value = serde_json::to_value(descriptor).unwrap();
    assert_eq!(value["schemaVersion"], 5);
    assert!(value.get("walletCreate").is_none());
    assert!(value.get("devWallet").is_none());
}
