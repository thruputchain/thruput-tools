use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Deserialize;
use url::Url;

use crate::{CatalogError, HostTarget, Result};

const CATALOG_SCHEMA_VERSION: u32 = 1;
const ARCHIVE_LAYOUT_VERSION: u32 = 1;
const COMPONENT: &str = "hog-devnode";
const SOURCE_REPOSITORY: &str = "groundhogchain/hogd";
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const EMBEDDED_CATALOG: &[u8] = include_bytes!("../catalog/hog-devnode.json");

/// The trusted catalog of managed `hog-devnode` versions and target bundles.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Catalog {
    schema_version: u32,
    component: String,
    default_version: String,
    versions: BTreeMap<String, VersionCatalog>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionCatalog {
    source: Source,
    targets: BTreeMap<String, Artifact>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    #[serde(rename = "ref")]
    source_ref: String,
    repository: String,
}

/// One target-specific archive trusted by the embedded catalog.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Artifact {
    archive: String,
    compatibility: Compatibility,
    files: Vec<DeclaredFile>,
    layout_version: u32,
    object_key: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Compatibility {
    #[serde(default)]
    libc: Option<String>,
    minimum_version: String,
    #[serde(default)]
    os: Option<String>,
}

/// A regular file allowed to appear in a managed bundle.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredFile {
    kind: FileKind,
    path: String,
}

/// The purpose and required permissions of a declared bundle file.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    /// A required runtime contract artifact.
    Artifact,
    /// The `hog-devnode` executable.
    Executable,
    /// A macOS dynamic library loaded by the executable.
    RuntimeLibrary,
}

impl Catalog {
    /// Parse and validate the catalog embedded in this crate.
    pub fn embedded() -> Result<Self> {
        Self::parse(Self::embedded_bytes())
    }

    /// Return the exact embedded catalog bytes used to build this crate.
    pub const fn embedded_bytes() -> &'static [u8] {
        EMBEDDED_CATALOG
    }

    /// Parse catalog bytes and validate every trusted path, checksum, and target entry.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let catalog: Self = serde_json::from_slice(bytes)?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// Return the version selected when no explicit or persisted override is present.
    pub fn default_version(&self) -> &str {
        &self.default_version
    }

    /// Resolve an optional exact version without performing range or remote lookup.
    pub fn resolve_version<'catalog>(
        &'catalog self,
        requested: Option<&str>,
    ) -> Result<&'catalog str> {
        let version = requested.unwrap_or(&self.default_version);
        self.versions
            .get_key_value(version)
            .map(|(known, _)| known.as_str())
            .ok_or_else(|| CatalogError::UnknownVersion {
                version: version.to_string(),
            })
    }

    /// Resolve one exact version and supported managed target.
    pub fn artifact(&self, version: &str, target: HostTarget) -> Result<&Artifact> {
        let version = self.resolve_version(Some(version))?;
        self.versions[version]
            .targets
            .get(target.triple())
            .ok_or_else(|| CatalogError::MissingTarget {
                version: version.to_string(),
                target: target.triple().to_string(),
            })
    }

    /// Resolve one exact version and publication target triple.
    pub fn artifact_for_triple(&self, version: &str, triple: &str) -> Result<&Artifact> {
        self.artifact(version, HostTarget::from_triple(triple)?)
    }

    /// Iterate over catalog versions that publish an artifact for the selected target.
    pub fn artifacts_for_target(
        &self,
        target: HostTarget,
    ) -> impl Iterator<Item = (&str, &Artifact)> {
        self.versions.iter().filter_map(move |(version, entry)| {
            entry
                .targets
                .get(target.triple())
                .map(|artifact| (version.as_str(), artifact))
        })
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::UnsupportedSchema {
                expected: CATALOG_SCHEMA_VERSION,
                found: self.schema_version,
            });
        }
        if self.component != COMPONENT {
            return Err(CatalogError::InvalidComponent {
                expected: COMPONENT,
                found: self.component.clone(),
            });
        }
        let default = self.versions.get(&self.default_version).ok_or_else(|| {
            CatalogError::MissingDefaultVersion {
                version: self.default_version.clone(),
            }
        })?;

        for (version, entry) in &self.versions {
            validate_version(version, entry)?;
        }
        for target in HostTarget::ALL {
            if !default.targets.contains_key(target.triple()) {
                return Err(CatalogError::MissingDefaultTarget {
                    version: self.default_version.clone(),
                    target: target.triple().to_string(),
                });
            }
        }
        Ok(())
    }
}

impl Artifact {
    /// Check the local runtime against this bundle's catalog minimum before local use.
    ///
    /// Cache lookup and cross-target installation deliberately do not perform this check.
    /// Consumers that run a managed bundle must call it before accepting cached files or
    /// acquiring a bundle. Failure to determine the runtime version fails closed.
    pub fn validate_current_host(&self, target: HostTarget) -> Result<()> {
        crate::compatibility::validate_current_host(target, &self.compatibility.minimum_version)
    }

    /// Return the archive filename published for this artifact.
    pub fn archive(&self) -> &str {
        &self.archive
    }

    /// Return the immutable public object key.
    pub fn object_key(&self) -> &str {
        &self.object_key
    }

    /// Return the expected archive byte length.
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Return the expected lowercase SHA-256 archive digest.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Return the archive layout version understood by the extractor.
    pub const fn layout_version(&self) -> u32 {
        self.layout_version
    }

    /// Return the complete allowlist of regular files in the archive.
    pub fn files(&self) -> &[DeclaredFile] {
        &self.files
    }

    /// Return whether all mandatory bundle files are declared with their required kinds.
    pub fn has_required_bundle_files(&self) -> bool {
        self.files
            .iter()
            .any(|file| file.path == "hog-devnode" && file.kind == FileKind::Executable)
            && self.files.iter().any(|file| {
                file.path == "contracts/genesis_root.rv32a" && file.kind == FileKind::Artifact
            })
    }

    /// Resolve this catalog object's URL beneath an HTTP(S) artifact base URL.
    pub fn public_url(&self, base_url: &str) -> Result<Url> {
        let mut base =
            Url::parse(base_url).map_err(|source| CatalogError::InvalidBaseUrl { source })?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(CatalogError::UnsupportedBaseUrlScheme);
        }
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(CatalogError::StatefulBaseUrl);
        }
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        base.join(&self.object_key)
            .map_err(|source| CatalogError::InvalidBaseUrl { source })
    }
}

impl DeclaredFile {
    /// Return the file's purpose and required permission class.
    pub const fn kind(&self) -> FileKind {
        self.kind
    }

    /// Return the normalized relative archive path.
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Check a version before using it as a cache-directory component.
pub(crate) fn validate_version_name(version: &str) -> Result<()> {
    if version.is_empty()
        || matches!(version, "." | "..")
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(CatalogError::InvalidVersion {
            version: version.to_string(),
        });
    }
    Ok(())
}

fn validate_version(version: &str, entry: &VersionCatalog) -> Result<()> {
    validate_version_name(version)?;
    let expected_ref = format!("v{version}");
    if entry.source.source_ref != expected_ref {
        return Err(CatalogError::SourceRefMismatch {
            version: version.to_string(),
            expected: expected_ref,
            found: entry.source.source_ref.clone(),
        });
    }
    if entry.source.repository != SOURCE_REPOSITORY {
        return Err(CatalogError::InvalidSourceRepository {
            version: version.to_string(),
            repository: entry.source.repository.clone(),
        });
    }
    for (triple, artifact) in &entry.targets {
        let target = HostTarget::from_triple(triple)?;
        validate_artifact(version, target, artifact)?;
    }
    Ok(())
}

fn validate_artifact(version: &str, target: HostTarget, artifact: &Artifact) -> Result<()> {
    let triple = target.triple();
    let expected_archive = format!("hog-devnode_{version}_{triple}.tar.gz");
    if artifact.archive != expected_archive {
        return Err(CatalogError::ArchiveNameMismatch {
            version: version.to_string(),
            target: triple.to_string(),
            expected: expected_archive,
            found: artifact.archive.clone(),
        });
    }
    let expected_object_key = format!("hog-devnode/{version}/{}", artifact.archive);
    if artifact.object_key != expected_object_key {
        return Err(CatalogError::ObjectKeyMismatch {
            version: version.to_string(),
            target: triple.to_string(),
            expected: expected_object_key,
            found: artifact.object_key.clone(),
        });
    }
    if artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CatalogError::InvalidSha256 {
            version: version.to_string(),
            target: triple.to_string(),
            sha256: artifact.sha256.clone(),
        });
    }
    if artifact.size == 0 || artifact.size > MAX_ARCHIVE_BYTES {
        return Err(CatalogError::InvalidArchiveSize {
            version: version.to_string(),
            target: triple.to_string(),
            size: artifact.size,
        });
    }
    if artifact.layout_version != ARCHIVE_LAYOUT_VERSION {
        return Err(CatalogError::UnsupportedLayout {
            version: version.to_string(),
            target: triple.to_string(),
            layout_version: artifact.layout_version,
        });
    }
    validate_compatibility(version, target, &artifact.compatibility)?;
    validate_files(version, target, &artifact.files)
}

fn validate_compatibility(
    version: &str,
    target: HostTarget,
    compatibility: &Compatibility,
) -> Result<()> {
    if crate::compatibility::version_components(&compatibility.minimum_version).is_none() {
        return Err(invalid_compatibility(
            version,
            target,
            "minimum version must contain only decimal components",
        ));
    }
    if target.is_macos() {
        if compatibility.os.as_deref() != Some("macos") || compatibility.libc.is_some() {
            return Err(invalid_compatibility(
                version,
                target,
                "macOS targets require os=macos and no libc",
            ));
        }
    } else if compatibility.libc.as_deref() != Some("glibc") || compatibility.os.is_some() {
        return Err(invalid_compatibility(
            version,
            target,
            "Linux targets require libc=glibc and no os",
        ));
    }
    Ok(())
}

fn validate_files(version: &str, target: HostTarget, files: &[DeclaredFile]) -> Result<()> {
    let mut paths = HashSet::new();
    let mut has_executable = false;
    let mut has_genesis_root = false;
    let mut runtime_libraries = 0usize;

    for file in files {
        if !is_safe_relative_path(&file.path) || !paths.insert(file.path.as_str()) {
            return Err(invalid_file_set(
                version,
                target,
                "paths must be unique normalized relative paths",
            ));
        }
        match (file.path.as_str(), file.kind) {
            ("hog-devnode", FileKind::Executable) => has_executable = true,
            ("contracts/dev_wallet.rv32a", FileKind::Artifact) => {}
            ("contracts/genesis_root.rv32a", FileKind::Artifact) => has_genesis_root = true,
            (path, FileKind::RuntimeLibrary)
                if target.is_macos() && is_macos_runtime_library(path) =>
            {
                runtime_libraries += 1;
            }
            _ => {
                return Err(invalid_file_set(
                    version,
                    target,
                    "file path and kind are not allowed for this target",
                ));
            }
        }
    }
    if !has_executable || !has_genesis_root {
        return Err(invalid_file_set(
            version,
            target,
            "hog-devnode and contracts/genesis_root.rv32a are required",
        ));
    }
    if target.is_macos() && runtime_libraries == 0 {
        return Err(invalid_file_set(
            version,
            target,
            "macOS bundles must declare at least one runtime library",
        ));
    }
    Ok(())
}

pub(crate) fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn is_macos_runtime_library(path: &str) -> bool {
    path.strip_prefix("lib/").is_some_and(|name| {
        !name.is_empty()
            && !name.contains('/')
            && Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "dylib")
    })
}

fn invalid_compatibility(version: &str, target: HostTarget, reason: &'static str) -> CatalogError {
    CatalogError::InvalidCompatibility {
        version: version.to_string(),
        target: target.triple().to_string(),
        reason,
    }
}

fn invalid_file_set(version: &str, target: HostTarget, reason: &'static str) -> CatalogError {
    CatalogError::InvalidFileSet {
        version: version.to_string(),
        target: target.triple().to_string(),
        reason,
    }
}
