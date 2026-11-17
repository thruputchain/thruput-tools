use crate::{
    CliError,
    config::{Network, ResolvedProfile},
    runtime_manifest::configured_runtime_manifest_path,
};

/// Where a command that funds a wallet sends its request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaucetBackend {
    /// The local dev wallet named by the profile's runtime manifest.
    LocalManifest,
    /// The hosted faucet behind the profile's RPC gateway, reached through
    /// `faucet_requestAirdrop`.
    RemoteFaucet,
}

/// Resolve the backend `feature` will use and check the profile carries
/// what that backend needs.
///
/// `feature` names the command in error text, for example `airdrop` or
/// `wallet create`.
pub(crate) fn resolve_faucet_backend(
    profile: &ResolvedProfile,
    feature: &str,
) -> Result<FaucetBackend, CliError> {
    let config_error = |message: String| CliError::ProfileConfigInvalid {
        profile: profile.name.clone(),
        message,
    };
    match profile.network {
        Some(Network::Local) => {
            if profile.rpc_url.is_none() {
                return Err(config_error(format!(
                    "targets local, but no local node endpoint is available for {feature}; start `thruput node` in another terminal or select another network with `--profile <name>`"
                )));
            }
            if configured_runtime_manifest_path(profile)?.is_none() {
                return Err(config_error(format!(
                    "targets local, but {feature} requires a local runtime manifest; run `thruput node` or configure `runtime_manifest`"
                )));
            }
            Ok(FaucetBackend::LocalManifest)
        }
        Some(network @ (Network::Futurenet | Network::Devnet)) => {
            let network = network.as_str();
            if configured_runtime_manifest_path(profile)?.is_some() {
                return Err(config_error(format!(
                    "targets {network}, but `--runtime-manifest` requires a local profile"
                )));
            }
            if profile.rpc_url.is_none() {
                return Err(config_error(format!(
                    "targets {network}, but {feature} requires `rpc_url`"
                )));
            }
            Ok(FaucetBackend::RemoteFaucet)
        }
        Some(Network::Mainnet) => Err(config_error(format!(
            "targets mainnet, but {feature} is not supported because mainnet does not exist yet"
        ))),
        // No declared network: use the runtime manifest if present, else the gateway.
        None => {
            if configured_runtime_manifest_path(profile)?.is_some() {
                Ok(FaucetBackend::LocalManifest)
            } else {
                Ok(FaucetBackend::RemoteFaucet)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn undeclared_network_prefers_a_runtime_manifest() {
        let mut profile = test_profile();
        profile.runtime_manifest = Some(PathBuf::from("runtime-manifest.json"));

        let backend = resolve_faucet_backend(&profile, "airdrop").expect("bug: should resolve");

        assert_eq!(backend, FaucetBackend::LocalManifest);
    }

    #[test]
    fn undeclared_network_without_a_manifest_asks_the_gateway() {
        let backend =
            resolve_faucet_backend(&test_profile(), "airdrop").expect("bug: should resolve");

        assert_eq!(backend, FaucetBackend::RemoteFaucet);
    }

    #[test]
    fn hosted_networks_use_the_gateway_faucet() {
        for network in [Network::Futurenet, Network::Devnet] {
            let mut profile = test_profile();
            profile.network = Some(network);

            let backend = resolve_faucet_backend(&profile, "airdrop").expect("bug: should resolve");

            assert_eq!(backend, FaucetBackend::RemoteFaucet);
        }
    }

    fn test_profile() -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: Some("https://rpc.example".into()),
            network_hash: None,
            rpc_url_cli_override: false,
            default_key: None,
            default_wallet: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::new(),
            wallets: BTreeMap::new(),
            source: None,
        }
    }
}
