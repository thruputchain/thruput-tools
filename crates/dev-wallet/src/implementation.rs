use super::interface::*;
use sdk::sys::raw_ChangePublicKeyRegistration;
use sdk::*;

#[derive(Default)]
pub struct DevWalletContract;

#[contract(exports = "guest_exports", storage = "false")]
impl DevWallet for DevWalletContract {
    fn initialize(&mut self, owner: &Ed25519PublicKey) {
        let scheme = CryptographicScheme::Ed25519.0;
        let weight = 1u32;
        raw_ChangePublicKeyRegistration(0, Some(&scheme), owner.as_ref(), Some(&weight)).unwrap();

        sdk::set_nonce_config(NonceConfig {
            accepted_keys: 0x01,
            required_weight: 1,
        })
        .unwrap();

        sdk::set_gas_permission_config(GasPermissionConfig {
            limit: 1_000_000_000,
            accepted_keys: 0x01,
            required_weight: 1,
        })
        .unwrap();
    }

    fn do_action(&self, invocation_bytes: &[u8]) {
        let auth_claim = sdk::get_auth_claim(0).unwrap();

        match auth_claim {
            AuthClaim::kInvokedAddressSignature {
                invoked_address_signature_id,
            } => {
                assert_eq!(invoked_address_signature_id, 0);
            }
            _ => {
                trap();
            }
        }

        let (addr, entrypoint, gas_limit, calldata): (
            ContractAddress,
            ContractEntrypointID,
            u64,
            &[u8],
        ) = xdrs::txs::decode(invocation_bytes).unwrap();

        match sdk::call::contract_call(&addr, entrypoint, gas_limit, calldata)
        {
            Ok(Ok(return_buf)) => sdk::contract_return(return_buf.as_ref()),
            Ok(Err(sdk::call::ContractError::Abort(abort_buf))) => {
                sdk::contract_abort(abort_buf.as_ref())
            }
            Ok(Err(sdk::call::ContractError::Fault(_))) | Err(_) => {
                trap();
            }
        }
    }

    fn deploy(&self, nonce: &SHA256Hash, contract_hash: &SHA256Hash) -> ContractAddress {
        let auth_claim = sdk::get_auth_claim(0).unwrap();

        match auth_claim {
            AuthClaim::kInvokedAddressSignature {
                invoked_address_signature_id,
            } => {
                assert_eq!(invoked_address_signature_id, 0);
            }
            _ => {
                trap();
            }
        }

        sdk::deploy_contract(nonce, contract_hash)
    }
}
