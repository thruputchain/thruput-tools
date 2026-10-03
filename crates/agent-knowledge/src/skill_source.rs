//! Import the maintained skill without requiring its repository at CLI build time.

use super::{BundleArtifacts, KnowledgeError, read_to_string, sha256, write_file};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, process::Command};

const REPOSITORY: &str = "https://github.com/groundhogchain/agent-skills";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Source {
    schema_version: u32,
    repository: String,
    revision: Option<String>,
    dirty: bool,
    sha256: String,
    files: BTreeMap<String, String>,
}

fn git(root: &Path, args: &[&str]) -> Result<String, KnowledgeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|source| KnowledgeError::Read {
            path: root.to_owned(),
            source,
        })?;
    if !output.status.success() {
        return Err(KnowledgeError::Invalid(
            "cannot read skill repository identity".into(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Import a selected checkout. Uncommitted previews require explicit permission.
pub fn import_skill(
    repo: &Path,
    destination: &Path,
    allow_dirty: bool,
) -> Result<(), KnowledgeError> {
    git(repo, &["rev-parse", "--show-toplevel"])?;
    let origin = git(repo, &["remote", "get-url", "origin"])?;
    if ![
        REPOSITORY,
        &format!("{REPOSITORY}.git"),
        "git@github.com:groundhogchain/agent-skills.git",
        "ssh://git@github.com/groundhogchain/agent-skills.git",
    ]
    .contains(&origin.as_str())
    {
        return Err(KnowledgeError::Invalid(
            "skill checkout origin must be groundhogchain/agent-skills".into(),
        ));
    }
    let revision = git(repo, &["rev-parse", "HEAD"]).ok();
    let dirty = revision.is_none() || !git(repo, &["status", "--porcelain"])?.is_empty();
    if dirty && !allow_dirty {
        return Err(KnowledgeError::Invalid(
            "skill source is uncommitted; use --allow-dirty only for a local preview".into(),
        ));
    }
    let root = repo.join("skills/groundhog");
    for path in [&root, &root.join("references"), &root.join("SKILL.md")] {
        if std::fs::symlink_metadata(path)
            .map_err(|source| KnowledgeError::Read {
                path: path.to_owned(),
                source,
            })?
            .file_type()
            .is_symlink()
        {
            return Err(KnowledgeError::Invalid(
                "skill source cannot contain symlinks".into(),
            ));
        }
    }
    let mut files = BTreeMap::new();
    files.insert("SKILL.md".into(), read_to_string(&root.join("SKILL.md"))?);
    let references = root.join("references");
    let entries = std::fs::read_dir(&references).map_err(|source| KnowledgeError::Read {
        path: references.clone(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| KnowledgeError::Read {
            path: references.clone(),
            source,
        })?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|source| KnowledgeError::Read {
                path: path.clone(),
                source,
            })?
            .is_file()
        {
            return Err(KnowledgeError::Invalid(
                "skill references must be regular Markdown files".into(),
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| KnowledgeError::Invalid("invalid reference name".into()))?;
        files.insert(format!("references/{name}"), read_to_string(&path)?);
    }
    let source = Source {
        schema_version: 1,
        repository: REPOSITORY.into(),
        revision,
        dirty,
        sha256: sha256(&serde_json::to_string(&files).map_err(KnowledgeError::Json)?),
        files,
    };
    validate(&source)?;
    if !dirty {
        for path in source.files.keys() {
            git(
                repo,
                &[
                    "ls-files",
                    "--error-unmatch",
                    &format!("skills/groundhog/{path}"),
                ],
            )?;
        }
    }
    write_file(
        &destination.join("skill-source.json"),
        &(serde_json::to_string_pretty(&source).map_err(KnowledgeError::Json)? + "\n"),
    )?;
    super::write_bundle(destination)
}

fn validate(source: &Source) -> Result<(), KnowledgeError> {
    let valid_revision = source.revision.as_ref().is_some_and(|revision| {
        revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    });
    if source.schema_version != 1
        || source.repository != REPOSITORY
        || (!valid_revision && !source.dirty)
        || source.sha256
            != sha256(&serde_json::to_string(&source.files).map_err(KnowledgeError::Json)?)
    {
        return Err(KnowledgeError::Invalid(
            "invalid skill source identity or content digest".into(),
        ));
    }
    let entry = source
        .files
        .get("SKILL.md")
        .ok_or_else(|| KnowledgeError::Invalid("missing SKILL.md".into()))?;
    if !entry.starts_with("---\nname: groundhog\n") || !entry.contains("\n---\n") {
        return Err(KnowledgeError::Invalid(
            "invalid skill entry frontmatter".into(),
        ));
    }
    for (name, content) in &source.files {
        if name != "SKILL.md" {
            let topic = name
                .strip_prefix("references/")
                .and_then(|name| name.strip_suffix(".md"))
                .ok_or_else(|| KnowledgeError::Invalid(format!("invalid reference: {name}")))?;
            if topic.is_empty()
                || !topic.bytes().all(|c| c.is_ascii_lowercase() || c == b'-')
                || !entry.contains(&format!("]({name})"))
            {
                return Err(KnowledgeError::Invalid(format!(
                    "unlinked or invalid reference: {name}"
                )));
            }
        }
        if content.is_empty() || !content.ends_with('\n') || content.contains('\r') {
            return Err(KnowledgeError::Invalid(format!("invalid Markdown: {name}")));
        }
    }
    Ok(())
}

/// Verify a release bundle and require a committed, clean source revision.
pub fn check_release(root: &Path) -> Result<(), KnowledgeError> {
    super::check_bundle(root)?;
    let source: Source = serde_json::from_str(&read_to_string(&root.join("skill-source.json"))?)
        .map_err(KnowledgeError::Json)?;
    if source.dirty || source.revision.is_none() {
        return Err(KnowledgeError::Invalid("release requires a clean skill revision; publish the source and import it without --allow-dirty".into()));
    }
    Ok(())
}

pub(super) fn build(root: &Path) -> Result<BundleArtifacts, KnowledgeError> {
    let source: Source = serde_json::from_str(&read_to_string(&root.join("skill-source.json"))?)
        .map_err(KnowledgeError::Json)?;
    validate(&source)?;
    let entry = &source.files["SKILL.md"];
    let (header, body) = entry
        .strip_prefix("---\n")
        .and_then(|text| text.split_once("\n---\n"))
        .ok_or_else(|| KnowledgeError::Invalid("invalid skill frontmatter".into()))?;
    let identity = format!(
        "<!-- groundhog-cli-version: {{{{GROUNDHOG_CLI_VERSION}}}} -->\n<!-- skill-source: {}; revision: {}; sha256: {}; dirty: {} -->",
        source.repository,
        source.revision.as_deref().unwrap_or("uncommitted"),
        source.sha256,
        source.dirty
    );
    let mut skill = format!("---\n{header}\n---\n\n{identity}\n\n{}", body.trim());
    for (name, content) in &source.files {
        if name != "SKILL.md" {
            skill.push_str("\n\n<!-- ");
            skill.push_str(name);
            skill.push_str(" -->\n\n");
            skill.push_str(content.trim());
        }
    }
    skill.push('\n');
    // The complete guide is self-contained: its links target the expanded sections.
    for path in source
        .files
        .keys()
        .filter(|path| path.starts_with("references/"))
    {
        let topic = path
            .trim_start_matches("references/")
            .trim_end_matches(".md");
        skill = skill.replace(&format!("]({path})"), &format!("](#{topic})"));
        skill = skill.replace(&format!("]({topic}.md#"), "](#");
    }
    let guide = skill.replace(super::CLI_VERSION_PLACEHOLDER, env!("CARGO_PKG_VERSION"));
    let records = source
        .files
        .iter()
        .map(|(path, content)| {
            serde_json::to_string(&BTreeMap::from([("source", path), ("content", content)]))
                .map(|line| line + "\n")
                .map_err(KnowledgeError::Json)
        })
        .collect::<Result<String, _>>()?;
    let mut manifest = serde_json::json!({
        "schemaVersion": 1, "id": "groundhog", "repository": source.repository,
        "sourceRevision": source.revision, "sourceDirty": source.dirty, "sourceSha256": source.sha256,
        "compatibility": {"groundhog-cli": env!("CARGO_PKG_VERSION")},
        "artifacts": {
            "agentGuide": {"path": "generated/agent-guide.md", "sha256": sha256(&guide)},
            "records": {"path": "generated/records.jsonl", "sha256": sha256(&records)},
            "skillTemplate": {"path": "generated/SKILL.md", "sha256": sha256(&skill)}
        }
    });
    // Other workspace crates enable serde_json/preserve_order. Assembly must be
    // identical with or without that feature's global Cargo unification.
    manifest.sort_all_objects();
    Ok(BundleArtifacts {
        records_jsonl: records,
        manifest_json: serde_json::to_string_pretty(&manifest).map_err(KnowledgeError::Json)?
            + "\n",
        skill_markdown: skill,
        agent_guide_markdown: guide,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        let files = BTreeMap::from([
            ("SKILL.md".into(), "---\nname: groundhog\ndescription: Guidance\n---\n# Groundhog\n[Contracts](references/contracts.md)\n".into()),
            ("references/contracts.md".into(), "# Contracts\n".into()),
        ]);
        Source {
            schema_version: 1,
            repository: REPOSITORY.into(),
            revision: Some("a".repeat(40)),
            dirty: false,
            sha256: sha256(&serde_json::to_string(&files).unwrap()),
            files,
        }
    }

    #[test]
    fn rejects_unlinked_topics_even_with_matching_digest() {
        let mut source = source();
        source
            .files
            .insert("references/unused.md".into(), "# Unused\n".into());
        source.sha256 = sha256(&serde_json::to_string(&source.files).unwrap());
        assert!(
            validate(&source)
                .unwrap_err()
                .to_string()
                .contains("unlinked")
        );
    }

    #[test]
    fn release_check_rejects_preview_but_accepts_a_clean_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let mut source = source();
        for dirty in [true, false] {
            source.dirty = dirty;
            write_file(
                &root.path().join("skill-source.json"),
                &serde_json::to_string(&source).unwrap(),
            )
            .unwrap();
            super::super::write_bundle(root.path()).unwrap();
            assert_eq!(check_release(root.path()).is_err(), dirty);
        }
    }
}
