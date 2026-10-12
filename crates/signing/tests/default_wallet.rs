use thruput_signing::default_wallet::*;
use thruput_signing::{PubkeyAccount, SigningError, derive_deploy_address, wallet_contract_hash};
use thruput_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
use thruput_xdr::types::{ContractAddress, SHA256Hash};
use xdr_common::BytesM;

#[test]
fn native_call_binds_address_auth_and_canonical_child_layout() {
    let account = PubkeyAccount::from_public_key_bytes([17; 32], 9);
    let target = ContractAddress::from([22; 32]);
    let tx = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
        account: &account,
        target_address: target,
        target_entrypoint: ContractEntrypointID::from(63_u64.to_be_bytes()),
        target_calldata: &[0xab, 0xcd],
        target_gas_limit: 64,
        gas_limit: 1000,
        block_expiry_height: 37,
        nonce: 11,
    })
    .unwrap();
    assert_eq!(tx.invocation.invoked_address, account.address());
    assert_eq!(tx.auth_claim, account.auth_claim());
    assert_eq!(
        hex::encode(tx.invocation.entrypoint.as_ref()),
        "c3edb11fdd6d5d58"
    );
    assert_eq!(tx.invocation.gas_limit, 1000);
    assert_eq!(*tx.uniquifier.block_expiry_height, 37);
    assert_eq!(tx.uniquifier.nonce, 11);
    assert!(tx.contracts_to_deploy.is_empty());
    // Canonical WalletCall has fixed eight-byte entrypoint data. A small
    // selector catches accidental compact-integer encoding of that field.
    let calls: Vec<(ContractAddress, ContractEntrypointID, u64, Vec<u8>)> =
        xdrs::cxs::decode(&tx.invocation.calldata).unwrap();
    assert_eq!(
        calls,
        vec![(
            target,
            ContractEntrypointID::from(63_u64.to_be_bytes()),
            64,
            vec![0xab, 0xcd]
        )]
    );
    // CXS: one element, 32-byte target, 8-byte selector, gas 64 (3f 40),
    // two calldata bytes. Derived from the canonical ABI, not its encoder.
    let mut expected = vec![1];
    expected.extend_from_slice(&[22; 32]);
    expected.extend_from_slice(&63_u64.to_be_bytes());
    expected.extend_from_slice(&[0x3f, 0x40, 2, 0xab, 0xcd]);
    assert_eq!(tx.invocation.calldata.0, expected);
    assert_ne!(
        account.address(),
        PubkeyAccount::from_public_key_bytes([17; 32], 0).address()
    );
}

#[test]
fn deployment_attaches_the_exact_code_it_hashes_and_keeps_salt_stable() {
    let account = PubkeyAccount::from_public_key_bytes([18; 32], 0);
    let contract = Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(vec![1, 2, 3]),
        },
    };
    let hash = wallet_contract_hash(&contract).unwrap();
    let salt = SHA256Hash::from([29; 32]);
    let build = |nonce, expiry| {
        build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
            account: &account,
            deployment_nonce: salt,
            code: thruput_signing::default_wallet::DeploymentCode::Attached(contract.clone()),
            amount: 0,
            initializer: None,
            gas_limit: 1000,
            block_expiry_height: expiry,
            nonce,
        })
        .unwrap()
    };
    let tx = build(10, 100);
    let later = build(11, 200);
    assert_eq!(tx.invocation.invoked_address, account.address());
    assert_eq!(tx.auth_claim, account.auth_claim());
    assert_eq!(
        hex::encode(tx.invocation.entrypoint.as_ref()),
        "6be0cb0f6de0917a"
    );
    assert_eq!(tx.contracts_to_deploy.as_slice(), &[contract]);
    assert_eq!(
        xdrs::cxs::decode::<(
            SHA256Hash,
            SHA256Hash,
            u128,
            Option<(ContractEntrypointID, u64, Vec<u8>)>
        )>(&tx.invocation.calldata)
        .unwrap(),
        (salt, hash, 0, None)
    );
    assert_eq!(tx.invocation, later.invocation);
    assert_ne!(tx.uniquifier, later.uniquifier);
    let address = derive_deploy_address(&account.address(), &salt, &hash).unwrap();
    assert_ne!(address, account.address());
}

#[test]
fn funded_deployment_encodes_exact_amount_and_attaches_matching_code() {
    let account = PubkeyAccount::from_public_key_bytes([18; 32], 9);
    let contract = Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(vec![1, 2, 3]),
        },
    };
    let hash = wallet_contract_hash(&contract).unwrap();
    let salt = SHA256Hash::from([29; 32]);
    for amount in [0, 64, (1_u128 << 64) + 63, u128::MAX] {
        let tx = build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
            account: &account,
            deployment_nonce: salt,
            code: thruput_signing::default_wallet::DeploymentCode::Attached(contract.clone()),
            amount,
            initializer: None,
            gas_limit: 1000,
            block_expiry_height: 100,
            nonce: 11,
        })
        .unwrap();
        assert_eq!(
            hex::encode(tx.invocation.entrypoint.as_ref()),
            "6be0cb0f6de0917a"
        );
        assert_eq!(tx.invocation.invoked_address, account.address());
        assert_eq!(tx.auth_claim, account.auth_claim());
        assert_eq!(
            tx.contracts_to_deploy.as_slice(),
            std::slice::from_ref(&contract)
        );
        assert_eq!(
            xdrs::cxs::decode::<(
                SHA256Hash,
                SHA256Hash,
                (u64, u64),
                Option<(ContractEntrypointID, u64, Vec<u8>)>
            )>(&tx.invocation.calldata)
            .unwrap(),
            (
                salt,
                hash,
                (
                    u64::try_from(amount >> 64).unwrap(),
                    u64::try_from(amount & u128::from(u64::MAX)).unwrap()
                ),
                None
            ),
        );
        if amount == 64 {
            let mut expected = Vec::from(salt.as_ref());
            expected.extend_from_slice(hash.as_ref());
            expected.extend_from_slice(&[0, 0x3f, 0x40, 0]);
            assert_eq!(tx.invocation.calldata.0, expected);
        }
    }
}

#[test]
fn deployment_initializer_uses_fixed_selector_and_optional_cxs_layout() {
    let account = PubkeyAccount::from_public_key_bytes([18; 32], 9);
    let contract = Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(vec![1, 2, 3]),
        },
    };
    let hash = wallet_contract_hash(&contract).unwrap();
    let salt = SHA256Hash::from([29; 32]);
    let tx = build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
        account: &account,
        deployment_nonce: salt,
        code: thruput_signing::default_wallet::DeploymentCode::Attached(contract),
        amount: 64,
        initializer: Some(WalletInitCall {
            entrypoint: ContractEntrypointID::from(63_u64.to_be_bytes()),
            gas_limit: 64,
            calldata: &[0xab, 0xcd],
        }),
        gas_limit: 1000,
        block_expiry_height: 100,
        nonce: 11,
    })
    .unwrap();
    // nonce, hash, u128 amount (high then low), Some, fixed 8-byte selector,
    // gas and length-prefixed calldata. This is independent of the tuple encoder.
    let mut expected = Vec::from(salt.as_ref());
    expected.extend_from_slice(hash.as_ref());
    expected.extend_from_slice(&[0, 0x3f, 0x40, 1]);
    expected.extend_from_slice(&63_u64.to_be_bytes());
    expected.extend_from_slice(&[0x3f, 0x40, 2, 0xab, 0xcd]);
    assert_eq!(tx.invocation.calldata.0, expected);
}

#[test]
fn single_call_return_preserves_empty_and_binary_child_payloads() {
    for payload in [vec![], vec![0, 0xff, 63, 64], vec![1; 256]] {
        let encoded = xdrs::cxs::encode(&vec![payload.clone()]).unwrap();
        assert_eq!(
            decode_default_wallet_call_return(&encoded).unwrap(),
            payload
        );
    }
}

#[test]
fn single_call_return_rejects_missing_extra_truncated_and_trailing_data() {
    for count in [0, 2] {
        let encoded = xdrs::cxs::encode(&vec![vec![1_u8]; count]).unwrap();
        assert!(
            matches!(decode_default_wallet_call_return(&encoded), Err(SigningError::UnexpectedWalletReturnCount(n)) if n == count)
        );
    }
    let good = xdrs::cxs::encode(&vec![vec![1_u8, 2, 3]]).unwrap();
    for len in 0..good.len() {
        assert!(decode_default_wallet_call_return(&good[..len]).is_err());
    }
    let mut trailing = good;
    trailing.push(0);
    assert!(decode_default_wallet_call_return(&trailing).is_err());
}

#[test]
fn single_call_return_bounds_the_complete_encoded_buffer() {
    // One child plus its collection framing fits exactly in the root buffer.
    let payload = vec![1_u8; 1020];
    let boundary = xdrs::cxs::encode(&vec![payload.clone()]).unwrap();
    assert_eq!(boundary.len(), 1024);
    assert_eq!(
        decode_default_wallet_call_return(&boundary).unwrap(),
        payload
    );

    // The child alone is within the limit, but its framing crosses the boundary.
    for child_len in [1021, 1024] {
        let encoded = xdrs::cxs::encode(&vec![vec![1_u8; child_len]]).unwrap();
        assert_eq!(encoded.len(), child_len + 4);
        assert!(matches!(
            decode_default_wallet_call_return(&encoded),
            Err(SigningError::XdrDecode(_))
        ));
    }
}

#[test]
fn registered_code_keeps_deployment_effects_and_auth_without_attachment() {
    let account = PubkeyAccount::from_public_key_bytes([0x44; 32], 0);
    let contract = Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(vec![1, 2, 3]),
        },
    };
    let hash = wallet_contract_hash(&contract).unwrap();
    let build = |code| {
        build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
            account: &account,
            deployment_nonce: [0x55; 32].into(),
            code,
            amount: 123,
            initializer: Some(WalletInitCall {
                entrypoint: [0x66; 8].into(),
                gas_limit: 456,
                calldata: &[7, 8],
            }),
            gas_limit: 1000,
            block_expiry_height: 100,
            nonce: 1,
        })
        .unwrap()
    };
    let attached = build(DeploymentCode::Attached(contract));
    let registered = build(DeploymentCode::Registered(hash));
    assert_eq!(attached.invocation, registered.invocation);
    assert_eq!(attached.auth_claim, registered.auth_claim);
    assert_eq!(attached.uniquifier, registered.uniquifier);
    assert_eq!(attached.contracts_to_deploy.len(), 1);
    assert!(registered.contracts_to_deploy.is_empty());
}
