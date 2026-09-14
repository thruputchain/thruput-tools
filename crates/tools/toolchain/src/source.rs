use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{ACCEPT_ENCODING, HeaderValue};
use reqwest::{Client, StatusCode};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

use crate::{Artifact, CatalogError};

/// Default public-read root provisioned for immutable Groundhog release artifacts.
pub const DEFAULT_PUBLIC_ARTIFACT_BASE_URL: &str =
    "https://groundhog-public-release-artifacts-017820684264.s3.us-west-2.amazonaws.com";

/// Failures while acquiring untrusted archive bytes from an artifact source.
#[derive(Debug, Error)]
pub enum SourceError {
    /// A catalog object could not be resolved beneath the configured base URL.
    #[error(transparent)]
    Catalog(#[from] CatalogError),

    /// The HTTP client could not be constructed.
    #[error("failed to construct artifact HTTP client: {0}")]
    Client(#[source] reqwest::Error),

    /// The artifact request or response body failed.
    #[error("artifact request failed for {url}: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    /// The artifact source did not return an exact successful object response.
    #[error("artifact request for {url} returned HTTP {status}")]
    HttpStatus { url: String, status: StatusCode },

    /// The source advertised or delivered a different byte length than the catalog.
    #[error("artifact size mismatch for {url}: expected {expected} bytes, observed {observed}")]
    SizeMismatch {
        url: String,
        expected: u64,
        observed: u64,
    },

    /// The destination temporary file could not be opened or written.
    #[error("failed to write artifact download {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A byte source for one catalog-authorized archive.
///
/// Implementations locate bytes, but the installer independently verifies the resulting file's
/// exact size and SHA-256 before parsing it.
#[async_trait]
pub trait BundleSource: Send + Sync {
    /// Write the selected archive to an existing private temporary file.
    async fn fetch_to(&self, artifact: &Artifact, destination: &Path) -> Result<u64, SourceError>;
}

/// An HTTP source rooted at a configurable public artifact base URL.
#[derive(Debug, Clone)]
pub struct HttpSource {
    base_url: String,
    client: Client,
}

impl HttpSource {
    /// Construct a bounded HTTP source that does not follow redirects.
    pub fn new(base_url: impl Into<String>) -> Result<Self, SourceError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_mins(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(SourceError::Client)?;
        Ok(Self::with_client(base_url, client))
    }

    /// Use an application-configured HTTP client for artifact downloads.
    ///
    /// Callers should configure connection/request timeouts and disable redirects to preserve
    /// the bounded, direct-fetch policy of [`Self::new`]. Archive verification remains mandatory.
    pub fn with_client(base_url: impl Into<String>, client: Client) -> Self {
        Self {
            base_url: base_url.into(),
            client,
        }
    }
}

#[async_trait]
impl BundleSource for HttpSource {
    async fn fetch_to(&self, artifact: &Artifact, destination: &Path) -> Result<u64, SourceError> {
        let url = artifact.public_url(&self.base_url)?;
        let url_text = url.to_string();
        let mut response = self
            .client
            .get(url)
            .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
            .send()
            .await
            .map_err(|source| SourceError::Request {
                url: url_text.clone(),
                source,
            })?;
        if response.status() != StatusCode::OK {
            return Err(SourceError::HttpStatus {
                url: url_text,
                status: response.status(),
            });
        }
        if let Some(observed) = response.content_length()
            && observed != artifact.size()
        {
            return Err(SourceError::SizeMismatch {
                url: url_text,
                expected: artifact.size(),
                observed,
            });
        }

        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(destination)
            .await
            .map_err(|source| SourceError::Write {
                path: destination.to_path_buf(),
                source,
            })?;
        let mut observed = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| SourceError::Request {
                url: url_text.clone(),
                source,
            })?
        {
            observed = observed.checked_add(chunk.len() as u64).ok_or_else(|| {
                SourceError::SizeMismatch {
                    url: url_text.clone(),
                    expected: artifact.size(),
                    observed: u64::MAX,
                }
            })?;
            if observed > artifact.size() {
                return Err(SourceError::SizeMismatch {
                    url: url_text,
                    expected: artifact.size(),
                    observed,
                });
            }
            file.write_all(&chunk)
                .await
                .map_err(|source| SourceError::Write {
                    path: destination.to_path_buf(),
                    source,
                })?;
        }
        file.flush().await.map_err(|source| SourceError::Write {
            path: destination.to_path_buf(),
            source,
        })?;
        file.sync_all().await.map_err(|source| SourceError::Write {
            path: destination.to_path_buf(),
            source,
        })?;
        if observed != artifact.size() {
            return Err(SourceError::SizeMismatch {
                url: url_text,
                expected: artifact.size(),
                observed,
            });
        }
        Ok(observed)
    }
}
