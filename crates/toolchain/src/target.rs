use std::fmt;

use crate::{CatalogError, Result};

/// A host target supported by the v1 managed `hog-devnode` catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostTarget {
    /// 64-bit x86 Linux using glibc.
    LinuxX86_64,
    /// 64-bit ARM Linux using glibc.
    LinuxAarch64,
    /// Apple Silicon macOS.
    MacosAarch64,
}

impl HostTarget {
    /// Every host target for which automatic acquisition is enabled.
    pub const ALL: [Self; 3] = [Self::LinuxX86_64, Self::LinuxAarch64, Self::MacosAarch64];

    /// Detect the current compilation target.
    pub fn current() -> Result<Self> {
        Self::from_os_arch(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// Resolve Rust operating-system and architecture names into a managed target.
    pub fn from_os_arch(os: &str, arch: &str) -> Result<Self> {
        match (os, arch) {
            ("linux", "x86_64") => Ok(Self::LinuxX86_64),
            ("linux", "aarch64") => Ok(Self::LinuxAarch64),
            ("macos", "aarch64") => Ok(Self::MacosAarch64),
            _ => Err(CatalogError::UnsupportedHost {
                os: os.to_string(),
                arch: arch.to_string(),
            }),
        }
    }

    /// Parse an exact managed artifact target triple.
    pub fn from_triple(triple: &str) -> Result<Self> {
        match triple {
            "x86_64-unknown-linux-gnu" => Ok(Self::LinuxX86_64),
            "aarch64-unknown-linux-gnu" => Ok(Self::LinuxAarch64),
            "aarch64-apple-darwin" => Ok(Self::MacosAarch64),
            _ => Err(CatalogError::UnsupportedTarget {
                target: triple.to_string(),
            }),
        }
    }

    /// Return the exact target triple used by the publication catalog.
    pub const fn triple(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Self::LinuxAarch64 => "aarch64-unknown-linux-gnu",
            Self::MacosAarch64 => "aarch64-apple-darwin",
        }
    }

    pub(crate) const fn is_macos(self) -> bool {
        matches!(self, Self::MacosAarch64)
    }
}

impl fmt::Display for HostTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.triple())
    }
}
