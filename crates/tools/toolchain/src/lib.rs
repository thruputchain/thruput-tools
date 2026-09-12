//! Verified catalog and host-target contracts for managed `hog-devnode` bundles.
//!
//! The embedded catalog is the trust root for managed downloads. Remote metadata may locate
//! objects, but it cannot add versions, targets, files, sizes, or checksums at runtime.

mod catalog;
mod error;
mod install;
mod source;
mod target;

pub use catalog::{Artifact, Catalog, DeclaredFile, FileKind};
pub use error::{CatalogError, Result};
pub use install::{InstallError, Installation, Installer};
pub use source::{BundleSource, DEFAULT_PUBLIC_ARTIFACT_BASE_URL, HttpSource, SourceError};
pub use target::HostTarget;
