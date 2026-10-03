//! Portable, server-independent copies of the snapshot's Markdown export tree.

use super::{DocsError, Snapshot, prepare, regular_files};
use crate::{
    CliError, Print,
    result::{CommandError, CommandResult},
};
use schemars::JsonSchema;
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ExportReport {
    schema_version: u32,
    path: PathBuf,
    index: Option<PathBuf>,
    snapshot: Option<Snapshot>,
}

/// Export the snapshot's Markdown tree into a new directory, without running a server.
pub fn export(
    dir: Option<&Path>,
    output: &Path,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let mut report = ExportReport {
        schema_version: 1,
        path: output.to_owned(),
        index: None,
        snapshot: None,
    };
    let result = export_files(dir, output);
    match result {
        Ok((path, snapshot)) => {
            report.index = Some(path.join("index.md"));
            report.path = path;
            report.snapshot = Some(snapshot);
            print.success(format_args!(
                "Markdown documentation exported to {}",
                report.path.display()
            ));
            print.detail("index", report.path.join("index.md").display());
            print.json(&CommandResult::new(cmd, report))
        }
        Err(error) if print.is_json() => {
            print.json(&CommandResult::failure_with_error(
                cmd,
                report,
                CommandError::from_cli_error(&error),
            ))?;
            Err(CliError::CommandReportedFailure)
        }
        Err(error) => Err(error),
    }
}

fn export_files(dir: Option<&Path>, output: &Path) -> Result<(PathBuf, Snapshot), CliError> {
    let (root, snapshot) = prepare(dir)?;
    let markdown = root.join("markdown");
    if !markdown.join("index.md").is_file() {
        return Err(DocsError::MarkdownUnavailable.into());
    }
    let files = regular_files(&markdown)?;
    // Creating the destination exclusively preserves existing files and symlinks.
    match fs::create_dir(output) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(DocsError::ExportExists(output.to_owned()).into());
        }
        Err(error) => return Err(error.into()),
    }
    let result = (|| {
        for path in files {
            let relative = path
                .strip_prefix(&markdown)
                .map_err(|_| DocsError::Invalid("Markdown path outside export tree".into()))?;
            let target = output.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, target)?;
        }
        fs::write(
            output.join("snapshot.json"),
            serde_json::to_vec_pretty(&snapshot).map_err(DocsError::Json)?,
        )?;
        Ok((output.canonicalize()?, snapshot))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(output);
    }
    result
}
