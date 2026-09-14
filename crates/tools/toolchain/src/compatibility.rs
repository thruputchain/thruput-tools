use std::process::{Command, Output};

use crate::{CatalogError, HostTarget, Result};

pub(crate) fn validate_current_host(target: HostTarget, minimum: &str) -> Result<()> {
    let host = HostTarget::current()?;
    if host != target {
        return Err(CatalogError::HostTargetMismatch {
            target: target.to_string(),
            host: host.to_string(),
        });
    }
    let output = if target.is_macos() {
        // Fixed system paths avoid accepting a version reported by a PATH override.
        // This variable makes sw_vers report 10.16 on newer macOS versions.
        Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .env_remove("SYSTEM_VERSION_COMPAT")
            .output()
    } else {
        Command::new("/usr/bin/getconf")
            .arg("GNU_LIBC_VERSION")
            .output()
    };
    validate_probe(target, minimum, output)
}

fn validate_probe(
    target: HostTarget,
    minimum: &str,
    output: std::io::Result<Output>,
) -> Result<()> {
    let runtime = if target.is_macos() { "macOS" } else { "glibc" };
    let failed = |reason: String| CatalogError::RuntimeProbeFailed {
        target: target.to_string(),
        runtime,
        minimum: minimum.to_string(),
        reason,
    };
    let output = output.map_err(|error| failed(error.to_string()))?;
    if !output.status.success() {
        return Err(failed(format!(
            "system version probe exited with {}",
            output.status
        )));
    }
    let output = std::str::from_utf8(&output.stdout)
        .map_err(|_| failed("system version probe returned invalid UTF-8".to_string()))?
        .trim();
    let found = if target.is_macos() {
        output
    } else {
        output
            .strip_prefix("glibc ")
            .ok_or_else(|| failed("system version probe did not identify GNU libc".to_string()))?
    };
    let found_parts = version_components(found)
        .ok_or_else(|| failed("system version probe returned an invalid version".to_string()))?;
    let minimum_parts = version_components(minimum)
        .ok_or_else(|| failed("catalog contains an invalid minimum version".to_string()))?;
    let length = found_parts.len().max(minimum_parts.len());
    let found_padded = found_parts
        .into_iter()
        .chain(std::iter::repeat(0))
        .take(length);
    let minimum_padded = minimum_parts
        .into_iter()
        .chain(std::iter::repeat(0))
        .take(length);
    if found_padded.cmp(minimum_padded).is_lt() {
        return Err(CatalogError::IncompatibleRuntime {
            target: target.to_string(),
            runtime,
            minimum: minimum.to_string(),
            found: found.to_string(),
        });
    }
    Ok(())
}

pub(crate) fn version_components(version: &str) -> Option<Vec<u64>> {
    version
        .split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                None
            } else {
                part.parse().ok()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    fn output(bytes: &[u8], code: i32) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: bytes.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn checks_numeric_versions_with_zero_padded_components() {
        for (target, minimum, reported, compatible) in [
            (HostTarget::LinuxX86_64, "2.38", "glibc 2.37\n", false),
            (HostTarget::LinuxAarch64, "2.38", "glibc 2.38\n", true),
            (HostTarget::LinuxX86_64, "2.38", "glibc 2.40\n", true),
            (HostTarget::LinuxX86_64, "2.9", "glibc 2.10", true),
            (HostTarget::LinuxX86_64, "2.38.0", "glibc 2.38", true),
            (HostTarget::MacosAarch64, "26.0", "15.7\n", false),
            (HostTarget::MacosAarch64, "26.0", "26.0\n", true),
            (HostTarget::MacosAarch64, "26.0", "26.1\n", true),
            (HostTarget::MacosAarch64, "26.0.1", "26.0", false),
        ] {
            let result = validate_probe(target, minimum, Ok(output(reported.as_bytes(), 0)));
            if compatible {
                assert!(result.is_ok(), "{reported}: {result:?}");
            } else {
                assert!(matches!(
                    result,
                    Err(CatalogError::IncompatibleRuntime { .. })
                ));
            }
        }
    }

    #[test]
    fn fails_closed_when_runtime_probe_is_unavailable_or_unreliable() {
        for probe in [
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing getconf",
            )),
            Ok(output(b"glibc 2.40", 1)),
            Ok(output(b"musl 1.2.5", 0)),
            Ok(output(b"", 0)),
            Ok(output(b"glibc 2..40", 0)),
            Ok(output(b"glibc 2.40 extra", 0)),
            Ok(output(b"glibc \xff", 0)),
        ] {
            assert!(matches!(
                validate_probe(HostTarget::LinuxX86_64, "2.38", probe),
                Err(CatalogError::RuntimeProbeFailed { .. })
            ));
        }
        assert!(matches!(
            validate_probe(HostTarget::MacosAarch64, "26.0", Ok(output(b"26.x", 0))),
            Err(CatalogError::RuntimeProbeFailed { .. })
        ));
    }

    #[test]
    fn rejects_malformed_and_overflowing_catalog_versions() {
        for version in [
            "",
            ".",
            "2.",
            ".2",
            "2..38",
            "2.x",
            "2. 38",
            "18446744073709551616",
        ] {
            assert_eq!(version_components(version), None, "{version}");
        }
    }

    #[test]
    fn probes_the_current_host_with_a_trivially_satisfied_minimum() {
        validate_current_host(HostTarget::current().unwrap(), "0.0").unwrap();
    }

    #[test]
    fn rejects_a_foreign_target_before_probing() {
        let host = HostTarget::current().unwrap();
        let foreign = HostTarget::ALL
            .into_iter()
            .find(|target| *target != host)
            .unwrap();
        assert!(matches!(
            validate_current_host(foreign, "0.0"),
            Err(CatalogError::HostTargetMismatch { .. })
        ));
    }
}
