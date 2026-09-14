use thiserror::Error;

/// Errors produced while validating or resolving the managed toolchain catalog.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// The catalog bytes are not valid JSON or do not match the schema shape.
    #[error("invalid toolchain catalog JSON: {0}")]
    CatalogJson(#[from] serde_json::Error),

    /// The catalog uses a schema this CLI does not understand.
    #[error("unsupported toolchain catalog schema {found}; expected {expected}")]
    UnsupportedSchema { expected: u32, found: u32 },

    /// The catalog describes a component other than `hog-devnode`.
    #[error("invalid toolchain catalog component {found:?}; expected {expected:?}")]
    InvalidComponent {
        expected: &'static str,
        found: String,
    },

    /// The catalog default does not name one of its versions.
    #[error("toolchain catalog default version {version:?} is missing")]
    MissingDefaultVersion { version: String },

    /// A requested exact version is absent from the trusted catalog.
    #[error("hog-devnode version {version:?} is not present in the trusted catalog")]
    UnknownVersion { version: String },

    /// A catalog version key is empty or unsafe to use in an object path.
    #[error("invalid toolchain catalog version {version:?}")]
    InvalidVersion { version: String },

    /// The upstream source reference does not match the catalog version.
    #[error("catalog version {version:?} has source ref {found:?}; expected {expected:?}")]
    SourceRefMismatch {
        version: String,
        expected: String,
        found: String,
    },

    /// The catalog names an unexpected source repository.
    #[error("catalog version {version:?} has unexpected source repository {repository:?}")]
    InvalidSourceRepository { version: String, repository: String },

    /// The host operating-system and architecture pair has no managed bundle.
    #[error("managed hog-devnode is unsupported on {os}/{arch}")]
    UnsupportedHost { os: String, arch: String },

    /// The bundle target differs from the current compilation target.
    #[error("managed hog-devnode target {target} cannot run on {host}")]
    HostTargetMismatch { target: String, host: String },

    /// The local runtime is older than the trusted bundle's minimum.
    #[error(
        "managed hog-devnode for {target} requires {runtime} >= {minimum}; found {found}; upgrade the host or use a compatible local build with --hog-devnode-binary"
    )]
    IncompatibleRuntime {
        target: String,
        runtime: &'static str,
        minimum: String,
        found: String,
    },

    /// The local runtime could not be identified reliably; managed use fails closed.
    #[error(
        "cannot verify {runtime} >= {minimum} for managed hog-devnode on {target}: {reason}; check the system runtime or use a compatible local build with --hog-devnode-binary"
    )]
    RuntimeProbeFailed {
        target: String,
        runtime: &'static str,
        minimum: String,
        reason: String,
    },

    /// A target triple is outside the supported managed target matrix.
    #[error("managed hog-devnode target {target:?} is unsupported")]
    UnsupportedTarget { target: String },

    /// A known version does not contain an artifact for a supported target.
    #[error("hog-devnode version {version:?} has no artifact for target {target:?}")]
    MissingTarget { version: String, target: String },

    /// The default version does not cover every supported managed target.
    #[error("default hog-devnode version {version:?} is missing target {target:?}")]
    MissingDefaultTarget { version: String, target: String },

    /// An archive name does not follow the deterministic publication contract.
    #[error(
        "catalog archive mismatch for {version}/{target}: found {found:?}, expected {expected:?}"
    )]
    ArchiveNameMismatch {
        version: String,
        target: String,
        expected: String,
        found: String,
    },

    /// An S3 object key does not follow the immutable publication layout.
    #[error(
        "catalog object key mismatch for {version}/{target}: found {found:?}, expected {expected:?}"
    )]
    ObjectKeyMismatch {
        version: String,
        target: String,
        expected: String,
        found: String,
    },

    /// An archive checksum is not canonical lowercase SHA-256 hex.
    #[error("invalid SHA-256 for {version}/{target}: {sha256:?}")]
    InvalidSha256 {
        version: String,
        target: String,
        sha256: String,
    },

    /// A declared archive size is zero or exceeds the supported bound.
    #[error("invalid archive size for {version}/{target}: {size} bytes")]
    InvalidArchiveSize {
        version: String,
        target: String,
        size: u64,
    },

    /// An archive layout version is not implemented by this crate.
    #[error("unsupported archive layout {layout_version} for {version}/{target}")]
    UnsupportedLayout {
        version: String,
        target: String,
        layout_version: u32,
    },

    /// The declared compatibility information is missing or inconsistent with its target.
    #[error("invalid compatibility metadata for {version}/{target}: {reason}")]
    InvalidCompatibility {
        version: String,
        target: String,
        reason: &'static str,
    },

    /// The declared bundle files are incomplete, duplicated, or unsafe.
    #[error("invalid file set for {version}/{target}: {reason}")]
    InvalidFileSet {
        version: String,
        target: String,
        reason: &'static str,
    },

    /// The configured artifact base URL is malformed.
    #[error("invalid artifact base URL {url:?}: {source}")]
    InvalidBaseUrl {
        url: String,
        #[source]
        source: url::ParseError,
    },

    /// The configured artifact base URL is not an HTTP(S) source.
    #[error("artifact base URL {url:?} must use http or https")]
    UnsupportedBaseUrlScheme { url: String },

    /// The configured artifact base URL includes state that must not affect object resolution.
    #[error("artifact base URL {url:?} must not contain credentials, a query, or a fragment")]
    StatefulBaseUrl { url: String },
}

/// Result type for managed toolchain catalog operations.
pub type Result<T> = std::result::Result<T, CatalogError>;
