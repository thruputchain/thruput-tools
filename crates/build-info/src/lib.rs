//! Shared build-version metadata for Groundhog binaries.

use std::fmt;

/// Crate and packaging-platform versions carried by a binary build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildVersion {
    crate_version: &'static str,
    platform_version: Option<&'static str>,
}

impl BuildVersion {
    /// Create build-version metadata from its two independent dimensions.
    pub const fn new(crate_version: &'static str, platform_version: Option<&'static str>) -> Self {
        Self {
            crate_version,
            platform_version,
        }
    }

    /// Return the version of the Rust crate that produced the binary.
    pub const fn crate_version(self) -> &'static str {
        self.crate_version
    }

    /// Return the platform release this binary was packaged for, when set.
    pub const fn platform_version(self) -> Option<&'static str> {
        self.platform_version
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

/// Capture the calling crate's version and optional platform packaging version.
#[macro_export]
macro_rules! build_version {
    () => {
        $crate::BuildVersion::new(
            env!("CARGO_PKG_VERSION"),
            option_env!("GROUNDHOG_DISTRIBUTION_VERSION"),
        )
    };
}

#[cfg(test)]
mod tests {
    use super::BuildVersion;

    #[test]
    fn display_keeps_crate_and_platform_versions_distinct() {
        let version = BuildVersion::new("0.9.6", Some("0.1.23"));

        assert_eq!(version.to_string(), "0.9.6 (platform 0.1.23)");
        assert_eq!(version.crate_version(), "0.9.6");
        assert_eq!(version.platform_version(), Some("0.1.23"));
    }

    #[test]
    fn display_omits_platform_for_unbundled_builds() {
        assert_eq!(BuildVersion::new("0.9.6", None).to_string(), "0.9.6");
    }
}
