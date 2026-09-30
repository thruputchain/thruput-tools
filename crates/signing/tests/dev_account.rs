use groundhog_signing::dev_account::*;

#[test]
fn default_dev_account_derivation_is_stable() {
    let public_keys: Vec<_> = (0..DEFAULT_DEV_ACCOUNT_COUNT)
        .map(|index| hex::encode(derive_default_dev_account_signer(index).verifying_key_bytes()))
        .collect();

    assert_eq!(
        public_keys,
        [
            "591ff2bdaaf5a0cbfd31f7b0f88c1ddee39fd9e106ba46303bde718459960633",
            "f5495b73debe0193757220005043944a3f03ff20cea7c4559db158eb1389e93f",
            "b8c09ac34f6c3ff58ddb549b19751d4e8b52f217fe6138f47322989c55a279a5",
            "8c8cb82ec23c847434193b00949c37d50313ef4e73e6dc4904028072a236dfbc",
            "45e85a804e73df05e59cdbcabf2252938b3b8b87a274ef60b2eff0b79cdd76dc",
            "178f7e68819b387699d2b5acfc8cda87de03ff92db1944f83a114a1946945d37",
            "7ea77f21cf1e56066f95d01a5bdd42d34087bd8a02261a0cbf67b2eed93393d0",
            "ee21e4725d8b38a54e42a0eb83782cdcfcaae89afb665b53c7a941a3835bccf0",
            "e4788cde106e2144b3d111af4adf56170a6e25d2636616f891f304af38397174",
            "404b21c4c455673bb0c841a55cf5f154aa646a83e0ea9722f9a87c64f1864564",
        ]
    );
}
