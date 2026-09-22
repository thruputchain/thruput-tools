use crate::{
    CliError,
    config::{Network, ResolvedProfile},
    runtime_manifest::configured_runtime_manifest_path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileBackend {
    LocalManifest,
    RemoteFaucet,
    RpcOnly,
}

pub(crate) fn resolve_profile_backend(
    profile: &ResolvedProfile,
) -> Result<ProfileBackend, CliError> {
    if configured_runtime_manifest_path(profile)?.is_some() {
        return Ok(ProfileBackend::LocalManifest);
    }
    if profile.faucet_url.is_some() {
        return Ok(ProfileBackend::RemoteFaucet);
    }
    Ok(ProfileBackend::RpcOnly)
}

/// Where a command that funds a wallet sends its request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaucetBackend {
    /// The local dev wallet named by the profile's runtime manifest.
    LocalManifest,
    /// The hosted faucet at the profile's `faucet_url`.
    RemoteFaucet,
}

/// Resolve the backend `feature` will use and check the profile carries
/// what that backend needs.
///
/// `feature` names the command in error text, for example `airdrop` or
/// `wallet create`. A profile that names a network is held to that
/// network's requirements; one that does not is classified by what it
/// configures.
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
                    "targets local, but {feature} requires `rpc_url`; run `groundhog node` or configure `rpc_url`"
                )));
            }
            if configured_runtime_manifest_path(profile)?.is_none() {
                return Err(config_error(format!(
                    "targets local, but {feature} requires a local runtime manifest; run `groundhog node` or configure `runtime_manifest`"
                )));
            }
            Ok(FaucetBackend::LocalManifest)
        }
        Some(Network::Devnet) => {
            if configured_runtime_manifest_path(profile)?.is_some() {
                return Err(config_error(
                    "targets devnet, but `--runtime-manifest` requires a local profile".into(),
                ));
            }
            if profile.rpc_url.is_none() {
                return Err(config_error(format!(
                    "targets devnet, but {feature} requires `rpc_url`"
                )));
            }
            if profile.faucet_url.is_none() {
                return Err(config_error(format!(
                    "targets devnet, but {feature} requires `faucet_url`"
                )));
            }
            Ok(FaucetBackend::RemoteFaucet)
        }
        Some(Network::Mainnet) => Err(config_error(format!(
            "targets mainnet, but {feature} is not supported because mainnet does not exist yet"
        ))),
        None => match resolve_profile_backend(profile)? {
            ProfileBackend::LocalManifest => Ok(FaucetBackend::LocalManifest),
            ProfileBackend::RemoteFaucet => Ok(FaucetBackend::RemoteFaucet),
            ProfileBackend::RpcOnly => Err(CliError::Other(format!(
                "profile `{}` is RPC-only; {feature} requires either `runtime_manifest` or `faucet_url`",
                profile.name
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn backend_prefers_explicit_runtime_manifest_over_faucet() {
        let mut profile = test_profile();
        profile.faucet_url = Some("https://faucet.example".into());
        profile.runtime_manifest = Some(PathBuf::from("runtime-manifest.json"));

        let backend = resolve_profile_backend(&profile).expect("bug: backend should resolve");

        assert_eq!(backend, ProfileBackend::LocalManifest);
    }

    #[test]
    fn backend_prefers_profile_runtime_manifest_over_faucet() {
        let manifest_path = PathBuf::from("runtime-manifest.json");
        let mut profile = test_profile();
        profile.rpc_url = Some("http://127.0.0.1:9545".into());
        profile.runtime_manifest = Some(manifest_path);
        profile.faucet_url = Some("https://faucet.example".into());

        let backend = resolve_profile_backend(&profile).expect("bug: backend should resolve");

        assert_eq!(backend, ProfileBackend::LocalManifest);
    }

    #[test]
    fn backend_uses_remote_faucet_without_runtime_manifest() {
        let mut profile = test_profile();
        profile.faucet_url = Some("https://faucet.example".into());

        let backend = resolve_profile_backend(&profile).expect("bug: backend should resolve");

        assert_eq!(backend, ProfileBackend::RemoteFaucet);
    }

    #[test]
    fn backend_is_rpc_only_without_runtime_manifest_or_faucet() {
        let backend =
            resolve_profile_backend(&test_profile()).expect("bug: backend should resolve");

        assert_eq!(backend, ProfileBackend::RpcOnly);
    }

    fn test_profile() -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: Some("https://rpc.example".into()),
            network_hash: None,
            faucet_url: None,
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
