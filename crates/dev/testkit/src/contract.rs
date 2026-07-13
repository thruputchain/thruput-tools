//! Contract method helpers.

use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};

/// Compute the 8-byte entrypoint selector for a contract method.
///
/// This is the value [`crate::AgentContext::call`] and
/// [`crate::Network::view_call`] expect. It is derived from the contract's
/// interface `name`, `version`, and `method` name exactly as the `groundhog`
/// CLI derives selectors, so tests can name methods instead of pasting raw hex
/// (which drifts whenever a fixture is rebuilt).
///
/// ```
/// let bump = groundhog_testkit::entrypoint_id("CallerCounter", "0.1.0", "bump");
/// ```
#[must_use]
pub fn entrypoint_id(interface: &str, version: &str, method: &str) -> [u8; 8] {
    let signature = NormalizedMethodSignature::new(method);
    let id = contract_entrypoint_id(Some(interface), version, &signature);
    id.as_ref()[..8]
        .try_into()
        .expect("bug: entrypoint id is at least 8 bytes")
}

#[cfg(test)]
mod tests {
    use super::entrypoint_id;

    #[test]
    fn matches_known_erc20_selector() {
        // Same known-answer vector the CLI pins for `method_entrypoint_id`.
        assert_eq!(
            entrypoint_id("ERC20", "0.1.0", "transfer"),
            [0x63, 0x28, 0x5b, 0x35, 0x23, 0xf9, 0xdc, 0x3f]
        );
    }
}
