//! Canonical entrypoint ID computation for Groundhog contracts.
//!
//! The entrypoint ID is the first 8 bytes of
//! `SHA-256(interface_name || major_version || method_name)`.

use groundhog_xdr::contract::ContractEntrypointID;
use sha2::{Digest, Sha256};

/// Compute the canonical entrypoint ID for a contract method.
///
/// Returns the first 8 bytes of `SHA-256(interface_name || major_version(version) || method)`.
pub fn contract_entrypoint_id(
    interface_name: &str,
    version_str: &str,
    method: &str,
) -> ContractEntrypointID {
    let mut hasher = Sha256::new();
    hasher.update(interface_name.as_bytes());
    hasher.update(major_version(version_str).as_bytes());
    hasher.update(method.as_bytes());
    let hash = hasher.finalize();
    let mut id = [0u8; 8];
    id.copy_from_slice(&hash[..8]);
    ContractEntrypointID::from(id)
}

/// Normalize a semver string to its major version for entrypoint hashing.
///
/// Strips build metadata (`+…`) and pre-release tags (`-…`), then returns
/// the shortest prefix that uniquely identifies the breaking-change level:
/// - major alone if major ≥ 1 (e.g. `"1.2.3"` → `"1"`)
/// - `major.minor` if minor ≥ 1 (e.g. `"0.1.0"` → `"0.1"`)
/// - `major.minor.patch` otherwise (e.g. `"0.0.1"` → `"0.0.1"`)
pub fn major_version(version: &str) -> &str {
    let normalized = version.split_once('+').map_or(version, |(base, _)| base);
    let normalized = normalized
        .split_once('-')
        .map_or(normalized, |(base, _)| base);

    let mut parts = normalized.split('.');
    let major = parts.next().unwrap_or(normalized);
    let minor = parts.next().unwrap_or("0");
    let patch = parts.next().unwrap_or("0");

    if major != "0" {
        major
    } else if minor != "0" {
        &normalized[..major.len() + 1 + minor.len()]
    } else if patch != "0" {
        &normalized[..major.len() + 1 + minor.len() + 1 + patch.len()]
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_known_entrypoint_id() {
        use std::fmt::Write;

        let id = contract_entrypoint_id("ERC20", "0.1.0", "transfer");
        let bytes = id.into_inner();
        let hex = bytes.iter().fold(String::new(), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        });
        assert_eq!(hex, "3799fd1c3d9b098f");
    }

    #[test]
    fn major_version_extracts_correctly() {
        assert_eq!(major_version("1.2.3"), "1");
        assert_eq!(major_version("0.1.0"), "0.1");
        assert_eq!(major_version("0.0.1"), "0.0.1");
        assert_eq!(major_version("0.0.0"), "0.0.0");
        assert_eq!(major_version("2.0.0-beta+build"), "2");
    }
}
