use thruput_signing::dev_wallet::native_token_transfer_calldata;
use thruput_xdr::types::ContractAddress;

#[test]
fn native_token_transfer_calldata_encodes_recipient_and_raw_amount() {
    let recipient = ContractAddress::from([0x55; 32]);
    let calldata = native_token_transfer_calldata(&recipient, 123)
        .expect("bug: native token transfer calldata should encode");
    let decoded: (ContractAddress, u128) =
        xdrs::cxs::decode(&calldata).expect("bug: transfer calldata should decode");

    assert_eq!(decoded, (recipient, 123));
}
