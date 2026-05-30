//! Deterministic dev account key derivation.
//!
//! Keys with the `dev-` prefix are derived from a well-known seed,
//! matching the keys created by `groundhog node`. No keystore
//! lookup, no import step.

use groundhog_signing::LocalSigner;

use crate::CliError;

/// Check if a key name refers to a dev account.
pub fn is_dev_key(name: &str) -> bool {
    dev_key_index(name).is_some()
}

/// Parse a `dev-N` key name into its dev-account index.
pub fn dev_key_index(name: &str) -> Option<usize> {
    name.strip_prefix("dev-").and_then(|s| s.parse().ok())
}

/// Derive a `LocalSigner` for a dev key name like "dev-0".
///
/// Returns `Err` if the name doesn't match the `dev-N` pattern.
pub fn load_dev_signer(name: &str) -> Result<LocalSigner, CliError> {
    let index = dev_key_index(name).ok_or_else(|| {
        CliError::Other(format!(
            "`{name}` is not a valid dev key name (expected dev-0, dev-1, ...)"
        ))
    })?;
    Ok(groundhog_signing::derive_default_dev_account_signer(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_key_detection() {
        assert!(is_dev_key("dev-0"));
        assert!(is_dev_key("dev-99"));
        assert!(!is_dev_key("default"));
        assert!(!is_dev_key("dev-"));
        assert!(!is_dev_key("dev-abc"));
        assert!(!is_dev_key("developer-0"));
    }

    #[test]
    fn dev_key_derivation_is_deterministic() {
        let signer_a = load_dev_signer("dev-0").unwrap();
        let signer_b = load_dev_signer("dev-0").unwrap();
        assert_eq!(
            signer_a.verifying_key_bytes(),
            signer_b.verifying_key_bytes(),
        );
    }

    #[test]
    fn dev_keys_are_distinct() {
        let signer_0 = load_dev_signer("dev-0").unwrap();
        let signer_1 = load_dev_signer("dev-1").unwrap();
        assert_ne!(
            signer_0.verifying_key_bytes(),
            signer_1.verifying_key_bytes(),
        );
    }
}
