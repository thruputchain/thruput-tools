//! CLI runtime-manifest loading and profile/env path resolution.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use thruput_json_rpc_types::runtime_manifest::{RuntimeManifest, parse_runtime_manifest};

use crate::{CliError, config::ResolvedProfile};

pub(crate) const ENV_RUNTIME_MANIFEST: &str = "THRUPUT_RUNTIME_MANIFEST";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Load the runtime manifest configured by environment or profile.
///
/// An explicit CLI override wins over environment. Otherwise environment wins
/// over profile so scripts can target an ephemeral local node without editing
/// config.
pub(crate) fn load_configured_runtime_manifest(
    profile: &ResolvedProfile,
    key_name: &str,
) -> Result<Option<RuntimeManifest>, CliError> {
    let Some(path) = configured_runtime_manifest_path(profile)? else {
        return Ok(None);
    };
    load_runtime_manifest(&path, key_name).map(Some)
}

pub(crate) fn configured_runtime_manifest_path(
    profile: &ResolvedProfile,
) -> Result<Option<PathBuf>, CliError> {
    if profile.runtime_manifest_cli_override {
        return profile_runtime_manifest_path(profile);
    }

    if let Some(raw) = std::env::var_os(ENV_RUNTIME_MANIFEST)
        && !os_value_is_blank(raw.as_os_str())
    {
        return Ok(Some(resolve_from_cwd(PathBuf::from(raw))?));
    }

    if let Some(path) = profile_runtime_manifest_path(profile)? {
        return Ok(Some(path));
    }

    Ok(None)
}

fn profile_runtime_manifest_path(profile: &ResolvedProfile) -> Result<Option<PathBuf>, CliError> {
    let Some(path) = profile.runtime_manifest.as_ref() else {
        return Ok(None);
    };
    if path.is_absolute() {
        return Ok(Some(path.clone()));
    }
    let base = profile
        .source
        .as_ref()
        .and_then(|source| source.parent())
        .map_or_else(std::env::current_dir, |path| Ok(path.to_path_buf()))?;
    Ok(Some(base.join(path)))
}

fn os_value_is_blank(value: &OsStr) -> bool {
    value.to_string_lossy().trim().is_empty()
}

fn resolve_from_cwd(path: PathBuf) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn load_runtime_manifest(path: &Path, key_name: &str) -> Result<RuntimeManifest, CliError> {
    let file = open_runtime_manifest_file(path, key_name)?;
    let mut reader = file.take(MAX_RUNTIME_MANIFEST_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| manifest_error(path, key_name, &source))?;
    if bytes.len() as u64 > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(manifest_message(
            path,
            key_name,
            format!("runtime manifest exceeds {MAX_RUNTIME_MANIFEST_BYTES} bytes"),
        ));
    }

    let manifest = parse_runtime_manifest(&bytes).map_err(|source| {
        manifest_message(
            path,
            key_name,
            format!("failed to parse runtime manifest: {source}"),
        )
    })?;
    Ok(manifest)
}

fn open_runtime_manifest_file(path: &Path, key_name: &str) -> Result<std::fs::File, CliError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| manifest_error(path, key_name, &source))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(manifest_message(
            path,
            key_name,
            "runtime manifest must be a regular file, not a symlink",
        ));
    }

    let file = open_regular_file_no_symlink(path, key_name)?;
    let metadata = file
        .metadata()
        .map_err(|source| manifest_error(path, key_name, &source))?;
    if !metadata.file_type().is_file() {
        return Err(manifest_message(
            path,
            key_name,
            "runtime manifest must be a regular file, not a symlink",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_regular_file_no_symlink(path: &Path, key_name: &str) -> Result<std::fs::File, CliError> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| manifest_error(path, key_name, &source))
}

#[cfg(not(unix))]
fn open_regular_file_no_symlink(path: &Path, key_name: &str) -> Result<std::fs::File, CliError> {
    std::fs::File::open(path).map_err(|source| manifest_error(path, key_name, &source))
}

fn manifest_error(path: &Path, key_name: &str, source: &std::io::Error) -> CliError {
    manifest_message(path, key_name, source.to_string())
}

fn manifest_message(path: &Path, key_name: &str, message: impl Into<String>) -> CliError {
    CliError::LocalRuntimeUnavailable {
        key_name: key_name.into(),
        message: format!("{}: {}", path.display(), message.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn no_runtime_manifest_is_inferred_without_env_or_profile_config() {
        let profile = ResolvedProfile {
            name: "default".to_string(),
            network: None,
            rpc_url: Some("http://127.0.0.1:9545".to_string()),
            network_hash: None,
            rpc_url_cli_override: false,
            default_key: Some("dev-0".to_string()),
            default_wallet: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets: BTreeMap::default(),
            source: None,
        };

        assert!(
            configured_runtime_manifest_path(&profile)
                .expect("bug: config lookup should succeed")
                .is_none(),
            "bug: runtime manifest should come from CLI, env, or profile config"
        );
    }
}
