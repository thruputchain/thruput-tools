//! CLI runtime-manifest loading and profile/env path resolution.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use groundhog_rpc_api_types::runtime_manifest::{RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeManifest};

use crate::{CliError, config::ResolvedProfile};

pub(crate) const ENV_RUNTIME_MANIFEST: &str = "GROUNDHOG_RUNTIME_MANIFEST";
pub(crate) const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Load the runtime manifest configured by environment or profile.
///
/// Environment wins over profile so scripts can target an ephemeral local node
/// without editing `groundhog.toml`.
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
    if let Some(raw) = std::env::var_os(ENV_RUNTIME_MANIFEST)
        && !os_value_is_blank(raw.as_os_str())
    {
        return Ok(Some(resolve_from_cwd(PathBuf::from(raw))?));
    }

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
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| manifest_error(path, key_name, &source))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(manifest_message(
            path,
            key_name,
            "runtime manifest must be a regular file, not a symlink",
        ));
    }
    let file =
        std::fs::File::open(path).map_err(|source| manifest_error(path, key_name, &source))?;
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

fn manifest_error(path: &Path, key_name: &str, source: &std::io::Error) -> CliError {
    manifest_message(path, key_name, source.to_string())
}

fn manifest_message(path: &Path, key_name: &str, message: impl Into<String>) -> CliError {
    CliError::LocalDevAccountUnavailable {
        key_name: key_name.into(),
        message: format!("{}: {}", path.display(), message.into()),
    }
}
