use thruput_signing::dev_account::*;

#[test]
fn default_dev_account_derivation_is_stable() {
    let public_keys: Vec<_> = (0..DEFAULT_DEV_ACCOUNT_COUNT)
        .map(|index| hex::encode(derive_default_dev_account_signer(index).verifying_key_bytes()))
        .collect();

    assert_eq!(
        public_keys,
        [
            "29eb00af4faa4461e8f4f8ad1c8f679d10570e94fee73f8c897713e7d56281e1",
            "11c0a466b89bb554a735ffad116151e48c5aa1c4debfc3080c8034ab0d4f0ab5",
            "2bf5c38a719bf77ced2ddf8cbdf1539ff24980d08ed2d277890db00739daad8f",
            "d31aba37e7542eee44e314a1569e8c7877fc67743360f056e2c33301bba32d63",
            "4a7d23a4bd695bceb8fabcec99d1690bfccaf32747fba78f736765f5a52b7266",
            "e5e95a72510b946e4ec9c4f921eac8149ceadca1304a619c2959df5d63900065",
            "25bed695348618b533ba205a2751a53e8cf5fbfc6a425e1f36864a38d3e02950",
            "78ae89c893402e6ef7acd7f85db30e95268b9b9e59f763bcf4dffb7d63aed2ad",
            "aba443eed2ec9ac7567e4570d133d95172e83390419223fffa947b8a5535bd9c",
            "ad2c62dff54ead3868d7729cebffebc770aa5fdb8bdb9e6e8c3e8acedb060f18",
        ]
    );
}
