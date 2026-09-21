//! Shared build-version metadata for Groundhog binaries.

use std::fmt;

/// Immutable metadata carried by a component binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildVersion {
    crate_version: &'static str,
    platform_version: Option<&'static str>,
    source_commit: Option<&'static str>,
}

impl BuildVersion {
    /// Create build-version metadata with an optional legacy platform stamp.
    pub const fn new(crate_version: &'static str, platform_version: Option<&'static str>) -> Self {
        let platform_version = match platform_version {
            Some(value) if value.is_empty() => None,
            value => value,
        };
        Self {
            crate_version,
            platform_version,
            source_commit: None,
        }
    }

    /// Attach the source commit embedded when the component was built.
    #[must_use]
    pub const fn with_source_commit(self, source_commit: Option<&'static str>) -> Self {
        let source_commit = match source_commit {
            Some(value) if value.is_empty() => None,
            value => value,
        };
        Self {
            crate_version: self.crate_version,
            platform_version: self.platform_version,
            source_commit,
        }
    }

    /// Return the version of the Rust crate that produced the binary.
    pub const fn crate_version(self) -> &'static str {
        self.crate_version
    }

    /// Return the legacy compile-time platform stamp, when set.
    pub const fn platform_version(self) -> Option<&'static str> {
        self.platform_version
    }

    /// Return the source commit embedded when the component was built.
    pub const fn source_commit(self) -> Option<&'static str> {
        self.source_commit
    }
}

impl fmt::Display for BuildVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.crate_version)?;
        if let Some(platform_version) = self.platform_version {
            write!(formatter, " (platform {platform_version})")?;
        }
        Ok(())
    }
}

/// Capture the calling crate's component version, legacy platform stamp, and source commit.
#[macro_export]
macro_rules! build_version {
    () => {
        $crate::BuildVersion::new(
            env!("CARGO_PKG_VERSION"),
            option_env!("GROUNDHOG_DISTRIBUTION_VERSION"),
        )
        .with_source_commit(option_env!("GIT_COMMIT"))
    };
}
