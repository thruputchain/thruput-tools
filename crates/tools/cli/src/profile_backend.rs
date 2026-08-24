use crate::{
    CliError, config::ResolvedProfile, runtime_manifest::configured_runtime_manifest_path,
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
