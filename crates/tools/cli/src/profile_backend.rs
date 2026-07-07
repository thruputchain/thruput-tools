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
    fn backend_prefers_project_local_node_state_over_faucet() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let project = tmp.path();
        let config_path = project.join("groundhog.toml");
        std::fs::write(&config_path, "").expect("bug: config should be written");
        let manifest_path = project.join("runtime-manifest.json");
        crate::runtime_manifest::write_local_node_state(
            project,
            "http://127.0.0.1:9545",
            &manifest_path,
        )
        .expect("bug: local node state should be written");

        let mut profile = test_profile();
        profile.source = Some(config_path);
        profile.rpc_url = Some("http://127.0.0.1:9545".into());
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
            rpc_url: Some("https://rpc.example".into()),
            faucet_url: None,
            rpc_url_cli_override: false,
            default_key: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::new(),
            wallets: BTreeMap::new(),
            source: None,
        }
    }
}
