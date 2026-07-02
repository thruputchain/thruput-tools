use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;

use crate::CliError;
use crate::config::ResolvedProfile;

/// Network metadata resolved from a profile's direct RPC URL or faucet descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedNetwork {
    /// Effective JSON-RPC URL for chain reads/writes.
    pub(crate) rpc_url: String,
    /// Configured faucet URL, when present.
    pub(crate) faucet_url: Option<String>,
    /// Fetched public descriptor, when resolution used a faucet URL.
    pub(crate) descriptor: Option<NetworkDescriptor>,
}

/// Resolve a profile into usable network metadata.
pub(crate) fn resolve_network(profile: &ResolvedProfile) -> Result<ResolvedNetwork, CliError> {
    if let Some(rpc_url) = profile.rpc_url.as_ref() {
        return Ok(ResolvedNetwork {
            rpc_url: rpc_url.clone(),
            faucet_url: profile.faucet_url.clone(),
            descriptor: None,
        });
    }
    let Some(faucet_url) = profile.faucet_url.as_deref() else {
        return Err(CliError::MissingRpcUrl);
    };
    let descriptor = fetch_network_descriptor(faucet_url)?;
    Ok(ResolvedNetwork {
        rpc_url: descriptor.endpoints.rpc_url.clone(),
        faucet_url: Some(faucet_url.trim_end_matches('/').to_owned()),
        descriptor: Some(descriptor),
    })
}

/// Fetch the public network descriptor served by a faucet service.
pub(crate) fn fetch_network_descriptor(faucet_url: &str) -> Result<NetworkDescriptor, CliError> {
    let faucet_url = faucet_url.trim_end_matches('/');
    let url = format!("{faucet_url}/v1/network");
    reqwest::blocking::get(&url)
        .map_err(|error| CliError::Other(format!("failed to call `{url}`: {error}")))?
        .error_for_status()
        .map_err(|error| CliError::Other(format!("faucet request `{url}` failed: {error}")))?
        .json()
        .map_err(|error| CliError::Other(format!("invalid JSON from `{url}`: {error}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn resolve_network_prefers_profile_rpc_url() {
        let profile = test_profile(Some("http://127.0.0.1:9545"), Some("http://127.0.0.1:8081"));

        let network = resolve_network(&profile).expect("bug: rpc_url should resolve");

        assert_eq!(network.rpc_url, "http://127.0.0.1:9545");
        assert_eq!(network.faucet_url.as_deref(), Some("http://127.0.0.1:8081"));
        assert!(network.descriptor.is_none());
    }

    #[test]
    fn resolve_network_requires_rpc_or_faucet_url() {
        let profile = test_profile(None, None);

        assert!(matches!(
            resolve_network(&profile),
            Err(CliError::MissingRpcUrl)
        ));
    }

    fn test_profile(rpc_url: Option<&str>, faucet_url: Option<&str>) -> ResolvedProfile {
        ResolvedProfile {
            name: "devnet".into(),
            rpc_url: rpc_url.map(str::to_owned),
            rpc_url_cli_override: false,
            faucet_url: faucet_url.map(str::to_owned),
            default_key: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::new(),
            wallets: BTreeMap::new(),
            source: None,
        }
    }
}
