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
