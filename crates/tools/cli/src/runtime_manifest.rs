//! CLI runtime-manifest loading and profile/env path resolution.

use std::ffi::OsStr;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use groundhog_json_rpc_types::runtime_manifest::{
    RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeManifest,
};
use serde::{Deserialize, Serialize};

use crate::{
    CliError,
    config::{ResolvedProfile, find_project_config},
    dev_keys,
};

pub(crate) const ENV_RUNTIME_MANIFEST: &str = "GROUNDHOG_RUNTIME_MANIFEST";
pub(crate) const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;
const LOCAL_NODE_STATE_SCHEMA_VERSION: u32 = 1;
const LOCAL_NODE_STATE_DIR: &str = ".groundhog";
const LOCAL_NODE_STATE_FILE: &str = "local-node.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalNodeState {
    schema_version: u32,
    rpc_url: String,
    runtime_manifest: PathBuf,
}

/// Load the runtime manifest configured by environment or profile.
///
/// An explicit CLI override wins over environment. Otherwise environment wins
/// over profile so scripts can target an ephemeral local node without editing
/// `groundhog.toml`. If neither is set, fall back to the project-local state
/// written by `groundhog node`.
pub(crate) fn load_configured_runtime_manifest(
    profile: &ResolvedProfile,
    key_name: &str,
) -> Result<Option<RuntimeManifest>, CliError> {
    let Some(path) = configured_runtime_manifest_path(profile)? else {
        return Ok(None);
    };
    load_runtime_manifest(&path, key_name).map(Some)
}

fn configured_runtime_manifest_path(
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

    if profile.rpc_url_cli_override {
        return Ok(None);
    }

    if profile_rpc_allows_local_node_state(profile.rpc_url.as_deref()) {
        local_node_state_runtime_manifest_path(profile)
    } else {
        Ok(None)
    }
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

/// Apply the project-local `groundhog node` state for dev-key workflows.
///
/// Env/profile runtime-manifest settings are explicit and keep their existing
/// RPC URL. When neither is set, the local-node state supplies the matched
/// runtime manifest and RPC URL written by the same `groundhog node` process.
/// Explicit non-local RPC URLs are left untouched; loopback URLs may be
/// replaced so scaffolded local projects follow the active local node port.
pub(crate) fn apply_local_node_state_for_dev_key(
    profile: &mut ResolvedProfile,
    key_name: &str,
) -> Result<(), CliError> {
    if dev_keys::dev_key_index(key_name).is_none()
        || explicit_runtime_manifest_path_configured(profile)
        || profile.rpc_url_cli_override
        || !profile_rpc_allows_local_node_state(profile.rpc_url.as_deref())
    {
        return Ok(());
    }
    let Some(state) = read_local_node_state(profile)? else {
        return Ok(());
    };
    profile.rpc_url = Some(state.rpc_url);
    profile.runtime_manifest = Some(state.runtime_manifest);
    Ok(())
}

/// Apply project-local `groundhog node` state for commands that intentionally
/// use local bootstrap accounts rather than the user's selected signing key.
pub(crate) fn apply_local_node_state_for_local_bootstrap(
    profile: &mut ResolvedProfile,
) -> Result<(), CliError> {
    if explicit_runtime_manifest_path_configured(profile)
        || profile.rpc_url_cli_override
        || !profile_rpc_allows_local_node_state(profile.rpc_url.as_deref())
    {
        return Ok(());
    }
    let Some(state) = read_local_node_state(profile)? else {
        return Ok(());
    };
    profile.rpc_url = Some(state.rpc_url);
    profile.runtime_manifest = Some(state.runtime_manifest);
    Ok(())
}

pub(crate) fn write_local_node_state(
    start: &Path,
    rpc_url: &str,
    runtime_manifest: &Path,
) -> Result<Option<PathBuf>, CliError> {
    let Some(path) = local_node_state_path(start) else {
        return Ok(None);
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let state = LocalNodeState {
        schema_version: LOCAL_NODE_STATE_SCHEMA_VERSION,
        rpc_url: rpc_url.to_owned(),
        runtime_manifest: runtime_manifest.to_path_buf(),
    };
    let bytes = serde_json::to_vec_pretty(&state)
        .map_err(|error| CliError::Other(format!("failed to encode local node state: {error}")))?;
    write_file_atomically(&path, &bytes)?;
    Ok(Some(path))
}

pub(crate) fn remove_local_node_state_if_matches(
    path: &Path,
    rpc_url: &str,
    runtime_manifest: &Path,
) -> Result<(), CliError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let state: LocalNodeState = serde_json::from_str(&contents).map_err(|error| {
        CliError::Other(format!(
            "failed to parse local node state {}: {error}",
            path.display()
        ))
    })?;
    if state.schema_version == LOCAL_NODE_STATE_SCHEMA_VERSION
        && state.rpc_url == rpc_url
        && state.runtime_manifest == runtime_manifest
    {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn local_node_state_runtime_manifest_path(
    profile: &ResolvedProfile,
) -> Result<Option<PathBuf>, CliError> {
    Ok(read_local_node_state(profile)?.map(|state| state.runtime_manifest))
}

fn read_local_node_state(profile: &ResolvedProfile) -> Result<Option<LocalNodeState>, CliError> {
    let start = profile
        .source
        .as_deref()
        .and_then(Path::parent)
        .map_or_else(std::env::current_dir, |path| Ok(path.to_path_buf()))?;
    let Some(path) = local_node_state_path(&start) else {
        return Ok(None);
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Ok(state) = serde_json::from_str::<LocalNodeState>(&contents) else {
        return Ok(None);
    };
    if state.schema_version != LOCAL_NODE_STATE_SCHEMA_VERSION {
        return Ok(None);
    }
    Ok(Some(state))
}

fn local_node_state_path(start: &Path) -> Option<PathBuf> {
    let root = find_project_config(start)?
        .parent()
        .map(Path::to_path_buf)?;
    Some(root.join(LOCAL_NODE_STATE_DIR).join(LOCAL_NODE_STATE_FILE))
}

fn explicit_runtime_manifest_path_configured(profile: &ResolvedProfile) -> bool {
    std::env::var_os(ENV_RUNTIME_MANIFEST)
        .as_deref()
        .is_some_and(|raw| !os_value_is_blank(raw))
        || profile.runtime_manifest.is_some()
}

fn profile_rpc_allows_local_node_state(rpc_url: Option<&str>) -> bool {
    rpc_url.is_none_or(is_loopback_rpc_url)
}

fn is_loopback_rpc_url(rpc_url: &str) -> bool {
    let Some(rest) = rpc_url
        .strip_prefix("http://")
        .or_else(|| rpc_url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed
            .split_once(']')
            .map_or(bracketed, |(host, _)| host)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
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

    let manifest: RuntimeManifest = serde_json::from_slice(&bytes).map_err(|source| {
        manifest_message(
            path,
            key_name,
            format!("failed to parse runtime manifest: {source}"),
        )
    })?;
    if manifest.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
        return Err(manifest_message(
            path,
            key_name,
            format!(
                "unsupported runtime manifest schemaVersion {}",
                manifest.schema_version
            ),
        ));
    }
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

fn write_file_atomically(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("local-node.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let tmp_path = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));

    let result = (|| -> Result<(), CliError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn manifest_error(path: &Path, key_name: &str, source: &std::io::Error) -> CliError {
    manifest_message(path, key_name, source.to_string())
}

fn manifest_message(path: &Path, key_name: &str, message: impl Into<String>) -> CliError {
    CliError::LocalDevAccountUnavailable {
        key_name: key_name.into(),
        message: format!("{}: {}", path.display(), message.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn local_node_state_only_overrides_missing_or_loopback_rpc() {
        assert!(profile_rpc_allows_local_node_state(None));
        assert!(profile_rpc_allows_local_node_state(Some(
            "http://127.0.0.1:9545"
        )));
        assert!(profile_rpc_allows_local_node_state(Some(
            "http://localhost:9545"
        )));
        assert!(profile_rpc_allows_local_node_state(Some(
            "http://[::1]:9545"
        )));

        assert!(!profile_rpc_allows_local_node_state(Some(
            "https://testnet.groundhog.io"
        )));
        assert!(!profile_rpc_allows_local_node_state(Some(
            "http://192.0.2.10:9545"
        )));
    }

    #[test]
    fn local_node_state_cleanup_only_removes_matching_state() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        std::fs::write(tmp.path().join("groundhog.toml"), "")
            .expect("bug: project config should be written");
        let manifest = tmp.path().join("runtime-manifest.json");
        let state_path = write_local_node_state(tmp.path(), "http://127.0.0.1:9545", &manifest)
            .expect("bug: local node state should be written")
            .expect("bug: project state path should exist");

        remove_local_node_state_if_matches(&state_path, "http://127.0.0.1:9999", &manifest)
            .expect("bug: mismatched cleanup should not fail");
        assert!(
            state_path.exists(),
            "bug: mismatched cleanup should leave state file in place"
        );

        remove_local_node_state_if_matches(&state_path, "http://127.0.0.1:9545", &manifest)
            .expect("bug: matching cleanup should not fail");
        assert!(
            !state_path.exists(),
            "bug: matching cleanup should remove state file"
        );
    }

    #[test]
    fn local_node_state_manifest_fallback_respects_rpc_precedence() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let config_path = tmp.path().join("groundhog.toml");
        std::fs::write(&config_path, "").expect("bug: project config should be written");
        let manifest = tmp.path().join("runtime-manifest.json");
        write_local_node_state(tmp.path(), "http://127.0.0.1:9545", &manifest)
            .expect("bug: local node state should be written");

        let mut profile = ResolvedProfile {
            name: "default".to_string(),
            rpc_url: Some("https://testnet.groundhog.io".to_string()),
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".to_string()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::default(),
            wallets: BTreeMap::default(),
            source: Some(config_path),
        };

        assert!(
            configured_runtime_manifest_path(&profile)
                .expect("bug: config lookup should succeed")
                .is_none(),
            "bug: local-node state should not pair with an explicit non-local RPC URL"
        );

        profile.rpc_url = Some("http://127.0.0.1:1".to_string());
        assert_eq!(
            configured_runtime_manifest_path(&profile).expect("bug: config lookup should succeed"),
            Some(manifest)
        );
    }

    #[test]
    fn invalid_local_node_state_is_treated_as_absent() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let config_path = tmp.path().join("groundhog.toml");
        std::fs::write(&config_path, "").expect("bug: project config should be written");
        let state_dir = tmp.path().join(LOCAL_NODE_STATE_DIR);
        std::fs::create_dir(&state_dir).expect("bug: state dir should be created");
        let state_path = state_dir.join(LOCAL_NODE_STATE_FILE);
        std::fs::write(
            &state_path,
            r#"{"schemaVersion":999,"rpcUrl":"http://127.0.0.1:9545","runtimeManifest":"/tmp/runtime-manifest.json"}"#,
        )
        .expect("bug: local state should be written");
        let profile = ResolvedProfile {
            name: "default".to_string(),
            rpc_url: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".to_string()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::default(),
            wallets: BTreeMap::default(),
            source: Some(config_path),
        };

        assert!(
            configured_runtime_manifest_path(&profile)
                .expect("bug: config lookup should succeed")
                .is_none(),
            "bug: unsupported local state schemas should not block CLI commands"
        );

        std::fs::write(&state_path, "{").expect("bug: malformed local state should be written");
        assert!(
            configured_runtime_manifest_path(&profile)
                .expect("bug: config lookup should succeed")
                .is_none(),
            "bug: malformed local state should not block CLI commands"
        );
    }
}
