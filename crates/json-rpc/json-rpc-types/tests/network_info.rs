use groundhog_xdr::types::SHA256Hash;
use serde_json::json;

use groundhog_json_rpc_types::NetworkHash;

use groundhog_json_rpc_types::network_info::NetworkInfo;

#[test]
fn network_info_round_trips_with_camel_case_fields() {
    let network_info = NetworkInfo::new(NetworkHash::from(SHA256Hash::from([0x07; 32])));
    let expected = json!({
        "networkHash": format!("0x{}", "07".repeat(32)),
    });

    let encoded = serde_json::to_value(&network_info).expect("bug: NetworkInfo serializes");
    assert_eq!(encoded, expected);

    let decoded: NetworkInfo =
        serde_json::from_value(encoded).expect("bug: NetworkInfo deserializes");
    assert_eq!(decoded, network_info);
}
