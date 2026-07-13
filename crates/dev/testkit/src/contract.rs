//! Contract method helpers.

pub use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_contract_util::{EntrypointSignature, NormalizedMethodSignature};

/// Compute entrypoint id for a contract method.
///
/// ```
/// let bump_entrypoint_id = groundhog_testkit::entrypoint_id("Counter", "0.1.0", "bump");
/// ```
#[must_use]
pub fn entrypoint_id(interface: &str, version: &str, method: &str) -> ContractEntrypointID {
    let sig = EntrypointSignature {
        interface_name: Some(interface.to_string()),
        version_str: version.to_string(),
        method_sig: NormalizedMethodSignature::new(method),
    };

    sig.id()
}

#[cfg(test)]
mod tests {
    use super::entrypoint_id;

    #[test]
    fn matches_known_erc20_selector() {
        // Same known-answer vector the CLI pins for `method_entrypoint_id`.
        assert_eq!(
            entrypoint_id("ERC20", "0.1.0", "transfer").into_inner(),
            [0x63, 0x28, 0x5b, 0x35, 0x23, 0xf9, 0xdc, 0x3f]
        );
    }
}
