//! Runtime binding between a component build and its installed bundle manifest.

use std::{
    collections::BTreeMap,
    env, fmt, fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thruput_build_info::BuildVersion;

/// Environment override for nonstandard bundle-manifest installations.
pub const BUNDLE_MANIFEST_ENV: &str = "THRUPUT_BUNDLE_MANIFEST";

const BUNDLE_MANIFEST_RELATIVE_PATH: &str = "share/thruput/bundle-manifest.json";
const BUNDLE_MANIFEST_SCHEMA: &str = "groundhog-bundle-v1";

/// The exact manifest record that identifies a binary inside a bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleSubject {
    /// A component repository entry, used by service binaries.
    Component(&'static str),
    /// A client asset entry and its owning component, used by distributed client binaries.
    Client {
        /// Name in the manifest's `clients` object.
        client: &'static str,
        /// Expected owning component name.
        component: &'static str,
        /// Expected packaging role for this client asset.
        asset_role: &'static str,
    },
}

impl BundleSubject {
    /// Match a component entry directly.
    pub const fn component(component: &'static str) -> Self {
        Self::Component(component)
    }

    /// Match a client entry and its owning component.
    pub const fn client(
        client: &'static str,
        component: &'static str,
        asset_role: &'static str,
    ) -> Self {
        Self::Client {
            client,
            component,
            asset_role,
        }
    }

    const fn component_name(self) -> &'static str {
        match self {
            Self::Component(component) | Self::Client { component, .. } => component,
        }
    }
}

/// Result of binding a component binary to an installed bundle manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleIdentity {
    build: BuildVersion,
    status: BundleStatus,
}

impl BundleIdentity {
    /// Discover and validate the bundle manifest associated with the running executable.
    pub fn discover(subject: BundleSubject, build: BuildVersion) -> Self {
        let Some(path) = discover_manifest_path() else {
            return Self::unbundled(build);
        };
        Self::from_manifest_path(subject, build, &path)
    }

    /// Validate an explicitly located bundle manifest.
    ///
    /// A missing or invalid explicit manifest is a mismatch, not an unbundled binary.
    pub fn from_manifest_path(subject: BundleSubject, build: BuildVersion, path: &Path) -> Self {
        let status = match read_manifest(path) {
            Ok(validated) => match_manifest(subject, build, validated),
            Err(reason) => BundleStatus::Mismatch(reason),
        };
        Self { build, status }
    }

    /// Return an identity that is not associated with an installed bundle.
    pub const fn unbundled(build: BuildVersion) -> Self {
        Self {
            build,
            status: BundleStatus::Unbundled,
        }
    }

    /// Return the immutable component build identity.
    pub const fn build_version(&self) -> BuildVersion {
        self.build
    }

    /// Return the containing release version only after a complete manifest match.
    pub fn release_version(&self) -> Option<&str> {
        match &self.status {
            BundleStatus::Matched(release) => Some(&release.version),
            BundleStatus::Unbundled | BundleStatus::Mismatch(_) => None,
        }
    }

    /// Return the validated bundle-manifest digest after a complete match.
    pub fn bundle_manifest_digest(&self) -> Option<&str> {
        match &self.status {
            BundleStatus::Matched(release) => Some(&release.manifest_digest),
            BundleStatus::Unbundled | BundleStatus::Mismatch(_) => None,
        }
    }

    /// Return a diagnostic when an installed or explicitly selected manifest did not match.
    pub fn mismatch_reason(&self) -> Option<&str> {
        match &self.status {
            BundleStatus::Mismatch(reason) => Some(reason),
            BundleStatus::Unbundled | BundleStatus::Matched(_) => None,
        }
    }

    /// Return a mismatch diagnostic with terminal controls rendered as visible escapes.
    pub fn terminal_safe_mismatch_reason(&self) -> Option<String> {
        self.mismatch_reason().map(terminal_safe)
    }
}

impl fmt::Display for BundleIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.build.crate_version())?;
        if let Some(source_commit) = self.build.source_commit() {
            write!(formatter, " (commit {}", abbreviate_commit(source_commit))?;
        } else {
            formatter.write_str(" (commit unavailable")?;
        }
        match &self.status {
            BundleStatus::Matched(release) => write!(formatter, ", release {})", release.version),
            BundleStatus::Unbundled => formatter.write_str(")"),
            BundleStatus::Mismatch(_) => formatter.write_str(", bundle manifest mismatch)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BundleStatus {
    Unbundled,
    Matched(ReleaseMetadata),
    Mismatch(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseMetadata {
    version: String,
    manifest_digest: String,
}

struct ValidatedManifest {
    manifest: BundleManifest,
    digest: String,
}

#[derive(Debug, Deserialize)]
struct BundleManifest {
    schema: String,
    bundle: BundleManifestMetadata,
    components: BTreeMap<String, BundleManifestComponent>,
    #[serde(default)]
    clients: BTreeMap<String, BundleManifestClient>,
}

#[derive(Debug, Deserialize)]
struct BundleManifestMetadata {
    version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleManifestComponent {
    active: bool,
    version: String,
    source_commit: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleManifestClient {
    asset_role: String,
    component: String,
    version: String,
    source_commit: String,
}

fn discover_manifest_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os(BUNDLE_MANIFEST_ENV) {
        return Some(PathBuf::from(path));
    }

    let executable = env::current_exe().ok()?;
    let bin_dir = executable.parent()?;
    let installation_root = bin_dir.parent()?;
    let candidate = installation_root.join(BUNDLE_MANIFEST_RELATIVE_PATH);
    candidate.is_file().then_some(candidate)
}

fn read_manifest(path: &Path) -> Result<ValidatedManifest, String> {
    let contents =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut value: Value = serde_json::from_slice(&contents)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    let digest = validate_manifest_digest(&mut value)?;
    let manifest = serde_json::from_slice(&contents)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    Ok(ValidatedManifest { manifest, digest })
}

fn validate_manifest_digest(value: &mut Value) -> Result<String, String> {
    let bundle = value
        .get_mut("bundle")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "bundle manifest has no bundle object".to_owned())?;
    let claimed = bundle
        .remove("manifestDigest")
        .and_then(|digest| digest.as_str().map(str::to_owned))
        .ok_or_else(|| "bundle manifest has no manifestDigest".to_owned())?;

    value.sort_all_objects();
    let mut canonical = serde_json::to_vec(value)
        .map_err(|error| format!("cannot canonicalize bundle manifest: {error}"))?;
    canonical.push(b'\n');
    let actual = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
    if claimed != actual {
        return Err(format!(
            "bundle manifest digest mismatch: expected {claimed}, computed {actual}"
        ));
    }
    Ok(claimed)
}

fn match_manifest(
    subject: BundleSubject,
    build: BuildVersion,
    validated: ValidatedManifest,
) -> BundleStatus {
    let manifest = validated.manifest;
    if manifest.schema != BUNDLE_MANIFEST_SCHEMA {
        return BundleStatus::Mismatch(format!(
            "unsupported bundle manifest schema {:?}",
            manifest.schema
        ));
    }
    if manifest.bundle.version.is_empty() {
        return BundleStatus::Mismatch("bundle manifest release version is empty".to_owned());
    }
    if let Err(reason) = match_component(subject.component_name(), build, &manifest) {
        return BundleStatus::Mismatch(reason);
    }
    if let BundleSubject::Client {
        client,
        component,
        asset_role,
    } = subject
        && let Err(reason) = match_client(client, component, asset_role, build, &manifest)
    {
        return BundleStatus::Mismatch(reason);
    }
    BundleStatus::Matched(ReleaseMetadata {
        version: manifest.bundle.version,
        manifest_digest: validated.digest,
    })
}

fn match_component(
    component: &str,
    build: BuildVersion,
    manifest: &BundleManifest,
) -> Result<(), String> {
    let Some(expected) = manifest.components.get(component) else {
        return Err(format!(
            "bundle manifest has no component entry for {component}"
        ));
    };
    if !expected.active {
        return Err(format!("bundle manifest component {component} is inactive"));
    }
    match_build_identity(
        component,
        build,
        &expected.version,
        expected.source_commit.as_deref(),
    )
}

fn match_client(
    client: &str,
    component: &str,
    asset_role: &str,
    build: BuildVersion,
    manifest: &BundleManifest,
) -> Result<(), String> {
    let Some(expected) = manifest.clients.get(client) else {
        return Err(format!("bundle manifest has no client entry for {client}"));
    };
    if expected.component != component {
        return Err(format!(
            "bundle manifest client {client} belongs to {}, expected {component}",
            expected.component
        ));
    }
    if expected.asset_role != asset_role {
        return Err(format!(
            "bundle manifest client {client} has asset role {}, expected {asset_role}",
            expected.asset_role
        ));
    }
    match_build_identity(
        &format!("client {client}"),
        build,
        &expected.version,
        Some(&expected.source_commit),
    )
}

fn match_build_identity(
    subject: &str,
    build: BuildVersion,
    expected_version: &str,
    expected_commit: Option<&str>,
) -> Result<(), String> {
    if expected_version != build.crate_version() {
        return Err(format!(
            "bundle manifest expects {subject} version {expected_version}, binary reports {}",
            build.crate_version()
        ));
    }
    let Some(source_commit) = build.source_commit() else {
        return Err(format!(
            "binary does not carry a source commit for {subject}"
        ));
    };
    let Some(expected_commit) = expected_commit else {
        return Err(format!(
            "bundle manifest has no source commit for {subject}"
        ));
    };
    if expected_commit != source_commit {
        return Err(format!(
            "bundle manifest expects {subject} source commit {expected_commit}, binary reports {source_commit}"
        ));
    }
    Ok(())
}

fn abbreviate_commit(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

fn terminal_safe(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}
