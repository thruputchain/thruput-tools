//! Deterministic CLI bundles imported from the maintained Groundhog skill repository.

use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

mod skill_source;
pub use skill_source::{check_release, import_skill};

const CLI_VERSION_PLACEHOLDER: &str = "{{GROUNDHOG_CLI_VERSION}}";

/// Release artifacts assembled without network access from a selected skill snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleArtifacts {
    /// Source-file paths and contents, one JSON object per line.
    pub records_jsonl: String,
    /// Source identity, CLI version, and generated artifact hashes.
    pub manifest_json: String,
    /// Self-contained guide with a CLI version placeholder.
    pub skill_markdown: String,
    /// Complete guide with this CLI's concrete version.
    pub agent_guide_markdown: String,
}

/// A source import or bundle validation failure.
#[derive(Debug, Error)]
pub enum KnowledgeError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid agent knowledge: {0}")]
    Invalid(String),
    #[error("generated agent knowledge is stale: {0}")]
    Stale(PathBuf),
    #[error("invalid agent knowledge JSON: {0}")]
    Json(serde_json::Error),
}

/// Assemble the imported snapshot into deterministic release artifacts.
pub fn build_bundle(root: &Path) -> Result<BundleArtifacts, KnowledgeError> {
    skill_source::build(root)
}

fn artifact_files(bundle: &BundleArtifacts) -> [(&str, &str); 4] {
    [
        ("records.jsonl", &bundle.records_jsonl),
        ("manifest.json", &bundle.manifest_json),
        ("SKILL.md", &bundle.skill_markdown),
        ("agent-guide.md", &bundle.agent_guide_markdown),
    ]
}

/// Regenerate the selected snapshot's CLI and hosting artifacts.
pub fn write_bundle(root: &Path) -> Result<(), KnowledgeError> {
    let bundle = build_bundle(root)?;
    let generated = root.join("generated");
    fs::create_dir_all(&generated).map_err(|source| KnowledgeError::Write {
        path: generated.clone(),
        source,
    })?;
    for (name, content) in artifact_files(&bundle) {
        write_file(&generated.join(name), content)?;
    }
    Ok(())
}

/// Verify that generated files match the selected source and this CLI version.
pub fn check_bundle(root: &Path) -> Result<(), KnowledgeError> {
    let bundle = build_bundle(root)?;
    for (name, content) in artifact_files(&bundle) {
        let path = root.join("generated").join(name);
        if read_to_string(&path)? != content {
            return Err(KnowledgeError::Stale(path));
        }
    }
    Ok(())
}

fn read_to_string(path: &Path) -> Result<String, KnowledgeError> {
    fs::read_to_string(path).map_err(|source| KnowledgeError::Read {
        path: path.to_owned(),
        source,
    })
}

fn write_file(path: &Path, content: &str) -> Result<(), KnowledgeError> {
    fs::write(path, content).map_err(|source| KnowledgeError::Write {
        path: path.to_owned(),
        source,
    })
}

fn sha256(content: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(content.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cli/assets/groundhog/knowledge")
    }

    #[test]
    fn checked_in_bundle_is_current_and_deterministic() {
        check_bundle(&root()).unwrap();
        assert_eq!(
            build_bundle(&root()).unwrap(),
            build_bundle(&root()).unwrap()
        );
    }

    #[test]
    fn hosted_guide_matches_rendered_skill_and_manifest_hash() {
        let bundle = build_bundle(&root()).unwrap();
        assert_eq!(
            bundle.agent_guide_markdown,
            bundle
                .skill_markdown
                .replace(CLI_VERSION_PLACEHOLDER, env!("CARGO_PKG_VERSION"))
        );
        let manifest: serde_json::Value = serde_json::from_str(&bundle.manifest_json).unwrap();
        assert_eq!(
            manifest["artifacts"]["agentGuide"]["sha256"],
            sha256(&bundle.agent_guide_markdown)
        );
        assert_eq!(
            manifest["compatibility"]["groundhog-cli"],
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn changed_source_digest_and_changed_generated_artifacts_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let source = read_to_string(&root().join("skill-source.json")).unwrap();
        fs::write(temp.path().join("skill-source.json"), &source).unwrap();
        write_bundle(temp.path()).unwrap();
        fs::write(temp.path().join("generated/agent-guide.md"), "changed").unwrap();
        assert!(matches!(
            check_bundle(temp.path()),
            Err(KnowledgeError::Stale(_))
        ));
        fs::write(
            temp.path().join("skill-source.json"),
            source.replace("# Groundhog", "# Modified"),
        )
        .unwrap();
        assert!(matches!(
            build_bundle(temp.path()),
            Err(KnowledgeError::Invalid(_))
        ));
    }
}
