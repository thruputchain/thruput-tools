use groundhog_json_rpc_types::hex::HexBytes;

use groundhog_json_rpc_types::network_descriptor::*;

#[test]
fn dev_wallet_entrypoints_parse_rotated_ids() {
    let descriptor = DevWalletDescriptor::new(
        HexBytes(vec![0xf9, 0x69, 0x54, 0x25, 0x72, 0xbd, 0x68, 0x60]),
        HexBytes(vec![0xa0, 0xff, 0xf9, 0x4c, 0x64, 0xfb, 0xaf, 0x3c]),
    );

    assert_eq!(
        descriptor.deploy_entrypoint_id().unwrap().as_ref(),
        &[0xf9, 0x69, 0x54, 0x25, 0x72, 0xbd, 0x68, 0x60]
    );
    assert_eq!(
        descriptor.do_action_entrypoint_id().unwrap().as_ref(),
        &[0xa0, 0xff, 0xf9, 0x4c, 0x64, 0xfb, 0xaf, 0x3c]
    );
}

#[test]
fn dev_wallet_entrypoints_reject_wrong_lengths() {
    let descriptor = DevWalletDescriptor::new(HexBytes(vec![0xf9; 7]), HexBytes(vec![0xa0; 9]));

    assert!(
        descriptor
            .deploy_entrypoint_id()
            .unwrap_err()
            .to_string()
            .contains("got 7")
    );
    assert!(
        descriptor
            .do_action_entrypoint_id()
            .unwrap_err()
            .to_string()
            .contains("got 9")
    );
}
