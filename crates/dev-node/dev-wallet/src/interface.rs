use sdk::*;

#[contract(client = Client)]
pub trait DevWallet {
    /// Set up auth data for this wallet address.
    ///
    /// Registers the owner's Ed25519 public key at key slot 0 with weight 1,
    /// configures nonce and gas permissions to require that key.
    fn initialize(&mut self, owner: &Ed25519PublicKey);

    /// Forward an invocation to another contract.
    ///
    /// Verifies the caller's auth claim, then deserializes invocation_bytes
    /// into (address, entrypoint, gas_limit, calldata) and delegates via
    /// raw_Invoke.
    fn do_action(&self, invocation_bytes: &[u8]);

    /// Deploy a contract and return its address.
    ///
    /// The contract WASM must be included in the transaction's
    /// `contracts_to_deploy`. This method calls the `deploy_contract`
    /// syscall which creates the deployment at a derived address.
    fn deploy(&self, nonce: &SHA256Hash, contract_hash: &SHA256Hash) -> ContractAddress;
}
