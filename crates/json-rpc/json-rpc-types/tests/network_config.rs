use serde_json::json;
use thruput_xdr::network_config::{ConfigParam, NetworkConfig, NetworkConfigV0};
use xdr_common::VecM;

use thruput_json_rpc_types::network_config::NetworkConfigParams;

fn network_config(params: Vec<ConfigParam>) -> NetworkConfig {
    NetworkConfig::_0 {
        v0: NetworkConfigV0 {
            params: VecM(params),
        },
    }
}

#[test]
fn stated_params_serialize_camel_case_and_unstated_ones_are_omitted() {
    let params = NetworkConfigParams {
        max_contract_size_bytes: Some(1_048_576),
        max_new_contracts_per_tx: Some(4),
        ..NetworkConfigParams::default()
    };
    let expected = json!({
        "maxContractSizeBytes": 1_048_576,
        "maxNewContractsPerTx": 4,
    });

    let encoded = serde_json::to_value(params).expect("bug: params serialize");
    assert_eq!(encoded, expected);

    let decoded: NetworkConfigParams =
        serde_json::from_value(encoded).expect("bug: params deserialize");
    assert_eq!(decoded, params);
    assert_eq!(
        serde_json::to_value(NetworkConfigParams::default()).expect("bug: params serialize"),
        json!({}),
        "nothing is filled in for an empty config"
    );
}

#[test]
fn retired_sha256_buffer_bytes_is_dropped_when_a_chain_still_states_it() {
    let config = network_config(vec![
        ConfigParam::kConfigParamMaxSHA256BufferBytes {
            max_sha256_buffer_bytes: 8192,
        },
        ConfigParam::kConfigParamMaxSavepointDepth {
            max_savepoint_depth: 9,
        },
    ]);

    let params = NetworkConfigParams::from_network_config(&config).unwrap();

    assert_eq!(
        params,
        NetworkConfigParams {
            max_savepoint_depth: Some(9),
            ..NetworkConfigParams::default()
        }
    );
    assert_eq!(
        serde_json::to_value(params).unwrap(),
        json!({ "maxSavepointDepth": 9 })
    );
}
