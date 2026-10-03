//! Local browsing of a prebuilt developer-site snapshot.

pub(crate) use crate::docs_json::Snapshot;
use crate::{
    CliError, Print,
    docs_json::{DOCS_JSON_SCHEMA_VERSION, DocsReport},
    result::CommandResult,
};
use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
};
use tower_http::services::ServeDir;

mod export;
pub mod service;
pub use export::export;

/// Failures preparing or serving a local documentation snapshot.
#[derive(Debug, thiserror::Error)]
pub enum DocsError {
    #[error("cannot encode documentation report: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "this snapshot has no Markdown export; use --dir with a snapshot containing markdown/index.md"
    )]
    MarkdownUnavailable,
    #[error("docs service: {0}")]
    Service(String),
    #[error("export destination already exists: {}; select a new --output directory", .0.display())]
    ExportExists(PathBuf),
    #[error(
        "bundled documentation is invalid: {0}; reinstall the CLI or use --dir \
         with a trusted snapshot"
    )]
    Bundle(String),
    #[error(
        "cannot prepare docs cache at {}: {source}; check permissions or set XDG_CACHE_HOME \
         to a writable directory",
        path.display()
    )]
    Cache {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "cannot read docs snapshot at {}: {source}; pass a directory produced by the \
         documentation snapshot build",
        path.display()
    )]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid docs snapshot: {0}; rebuild it with the documentation snapshot build")]
    Invalid(String),
    #[error("cannot listen on 127.0.0.1:{port}: {source}; choose another --port (or --port 0)")]
    Bind { port: u16, source: std::io::Error },
}

pub(crate) fn regular_files(root: &Path) -> Result<Vec<PathBuf>, DocsError> {
    let mut files = Vec::new();
    // Only static regular files belong in a snapshot. Reject symlinks and special
    // files before exposing it, including symlinked manifest/index files.
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| DocsError::Read {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| DocsError::Read {
                path: directory.clone(),
                source,
            })?;
            let kind = entry.file_type().map_err(|source| DocsError::Read {
                path: entry.path(),
                source,
            })?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            } else {
                return Err(DocsError::Invalid(format!(
                    "{} is not a regular file or directory",
                    entry.path().display()
                )));
            }
        }
    }
    Ok(files)
}

pub(crate) fn read_snapshot(root: &Path) -> Result<Snapshot, DocsError> {
    regular_files(root)?;
    let path = root.join("snapshot.json");
    let bytes = std::fs::read(&path).map_err(|source| DocsError::Read { path, source })?;
    let snapshot: Snapshot =
        serde_json::from_slice(&bytes).map_err(|e| DocsError::Invalid(e.to_string()))?;
    if snapshot.schema_version != 1
        || snapshot.source_revision.len() != 40
        || !snapshot
            .source_revision
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || snapshot.agent_guide_cli_version.trim().is_empty()
        || !root.join("index.html").is_file()
        || !root.join("start/index.html").is_file()
    {
        return Err(DocsError::Invalid(
            "unsupported schema, missing identity, or missing site entry points".into(),
        ));
    }
    Ok(snapshot)
}

/// Serve a trusted, immutable local snapshot on loopback until interrupted.
///
/// The default snapshot is bundled with the CLI and cached without network access. Keep the
/// snapshot directory unchanged while serving; it is not a live source tree.
pub fn run(
    dir: Option<&Path>,
    port: u16,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    serve(dir, port, cmd, print, None, &std::sync::Arc::default())
}

pub(crate) fn prepare(dir: Option<&Path>) -> Result<(PathBuf, Snapshot), CliError> {
    let root = match dir {
        Some(dir) => dir.to_owned(),
        None => crate::docs_snapshot::bundled()?,
    };
    let root = root
        .canonicalize()
        .map_err(|source| DocsError::Read { path: root, source })?;
    let snapshot = read_snapshot(&root)?;
    Ok((root, snapshot))
}

pub(crate) fn serve(
    dir: Option<&Path>,
    port: u16,
    cmd: &'static str,
    print: &Print,
    control: Option<service::Control>,
    shutdown: &std::sync::Arc<tokio::sync::Notify>,
) -> Result<(), CliError> {
    let task = crate::print::progress::Task::start(print, "Preparing documentation", true);
    let external_snapshot = dir.is_some();
    let (root, snapshot) = prepare(dir)?;
    let markdown_index = root
        .join("markdown/index.md")
        .is_file()
        .then(|| root.join("markdown/index.md"));
    let runtime =
        tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    runtime.block_on(async {
        let stop = crate::node::prepare_stop_signal()?;
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|source| DocsError::Bind { port, source })?;
        let url = format!("http://{}/", listener.local_addr()?);
        let _ = task.finish();
        let guide_matches_cli = snapshot.agent_guide_cli_version == env!("CARGO_PKG_VERSION");
        print.group(crate::print::Outcome::Ready, "Documentation", |print| {
            print.success(format_args!("Documentation ready at {url}"));
            print.detail("snapshot", root.display());
            print.detail("source", &snapshot.source_revision);
            print.detail("agent guide CLI", &snapshot.agent_guide_cli_version);
            if let Some(index) = &markdown_index {
                print.detail("Markdown index", index.display());
                print.detail("Markdown pages", root.join("markdown").display());
            } else {
                print.status("This snapshot predates Markdown exports.");
            }
            // The bundled snapshot is selected by the CLI release; a differing guide
            // version is provenance, not an actionable warning for its users.
            if external_snapshot && !guide_matches_cli {
                print.warn(format_args!(
                    "guide targets CLI {}; installed CLI is {}",
                    snapshot.agent_guide_cli_version,
                    env!("CARGO_PKG_VERSION")
                ));
            }
            print.status(
                "Local preview; linked services and external API docs still need network access. \
             Ctrl-C to stop.",
            );
        });
        print.json(&CommandResult::new(
            cmd,
            DocsReport {
                schema_version: DOCS_JSON_SCHEMA_VERSION,
                url,
                path: root.clone(),
                snapshot,
                cli_version: env!("CARGO_PKG_VERSION").to_owned(),
                guide_matches_cli,
                markdown_index,
            },
        ))?;
        let app = service::routes(control, shutdown.clone()).fallback_service(ServeDir::new(root));
        // Dropping the server on either signal closes outstanding connections too.
        // A stalled local client must not prevent the foreground command stopping.
        tokio::select! {
            result = axum::serve(listener, app) => result?,
            result = stop => result?,
            () = shutdown.notified() => {},
        }
        print.service_stopped("Documentation server stopped.");
        Ok(())
    })
}
