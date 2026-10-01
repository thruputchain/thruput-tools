use crate::CliError;
use std::fs;
use std::path::Path;

const BUILD_RS: &str = include_str!("../templates/build.rs.tmpl");
const CARGO_TOML: &str = include_str!("../templates/Cargo.toml.tmpl");
const CARGO_CONFIG: &str = include_str!("../templates/config.toml.tmpl");
const CONTRACT_TOML: &str = include_str!("../templates/contract.toml.tmpl");
const AGENTS_MD: &str = include_str!("../templates/agents.md.tmpl");
const GITIGNORE: &str = include_str!("../templates/gitignore.tmpl");
const GROUNDHOG_TOML: &str = include_str!("../templates/groundhog.toml.tmpl");
const INTEGRATION_TEST: &str = include_str!("../templates/integration.rs.tmpl");
const RUST_TOOLCHAIN: &str = include_str!("../templates/rust-toolchain.toml.tmpl");
const RV32_MAIN: &str = include_str!("../templates/main.rs.tmpl");

const COUNTER_LIB: &str = include_str!("../templates/counter/lib.rs.tmpl");
const COUNTER_IMPL: &str = include_str!("../templates/counter/implementation.rs.tmpl");
const COUNTER_TEST: &str = include_str!("../templates/counter/test.rs.tmpl");
const COUNTER_TEST_RV32: &str = include_str!("../templates/counter/test_rv32.rs.tmpl");

const STUB_LIB: &str = include_str!("../templates/stub/lib.rs.tmpl");
const STUB_IMPL: &str = include_str!("../templates/stub/implementation.rs.tmpl");
const STUB_TEST: &str = include_str!("../templates/stub/test.rs.tmpl");
const STUB_TEST_RV32: &str = include_str!("../templates/stub/test_rv32.rs.tmpl");

/// SDK version exact-pinned into both scaffold target dependencies.
///
/// Generated projects resolve outside this workspace, so this must be a
/// version published to the Groundhog registry, and one whose raw spec
/// format this CLI reads — otherwise `groundhog build` refuses the fresh
/// scaffold at the preflight. Bump together with
/// [`SCAFFOLD_TOOLCHAIN_CHANNEL`] when a new SDK release is validated on a
/// newer nightly. The CI `cli-smoke` job builds and tests a fresh scaffold
/// against the real registry, so an unpublished or incompatible pin fails
/// there (and stays red until the pinned SDK is published).
pub const SCAFFOLD_SDK_VERSION: &str = "0.42.2";

/// `groundhog-json-rpc-client` version pinned into scaffolded projects.
///
/// Published before the matching CLI in the release workflow. Release PR CI
/// tests the checked-out client until publication. The caret requirement lets
/// scaffolds pick up newer compatible releases.
pub const SCAFFOLD_JSON_RPC_CLIENT_VERSION: &str = "0.20.0";

/// Toolchain channel pinned into scaffolded projects' `rust-toolchain.toml`.
///
/// Keep in step with the nightly [`SCAFFOLD_SDK_VERSION`] is validated on.
pub const SCAFFOLD_TOOLCHAIN_CHANNEL: &str = "nightly-2026-07-01";

/// Convert a kebab-case name to `snake_case`.
pub(crate) fn to_snake(name: &str) -> String {
    name.replace('-', "_")
}

fn to_upper_snake(name: &str) -> String {
    name.replace('-', "_").to_uppercase()
}

fn apply(template: &str, name: &str) -> String {
    template
        .replace("{{PROJECT_NAME}}", name)
        .replace("{{PROJECT_NAME_SNAKE}}", &to_snake(name))
        .replace("{{PROJECT_NAME_UPPER_SNAKE}}", &to_upper_snake(name))
        .replace("{{SDK_VERSION}}", SCAFFOLD_SDK_VERSION)
        .replace(
            "{{JSON_RPC_CLIENT_VERSION}}",
            SCAFFOLD_JSON_RPC_CLIENT_VERSION,
        )
        .replace("{{TOOLCHAIN_CHANNEL}}", SCAFFOLD_TOOLCHAIN_CHANNEL)
}

/// Rust's sysroot crates, in the extern-prelude spelling.
///
/// A scaffold depends on itself (`[dev-dependencies]` with `path = "."`) to
/// turn the `dev` feature on for host tests, so its package name lands in the
/// extern prelude of every test target. Named after one of these, it shadows
/// the sysroot crate and the `#[test]` expansion stops resolving.
const RUST_SYSROOT_CRATES: &[&str] = &["alloc", "core", "proc_macro", "std", "test"];

/// Rust keywords, strict and reserved, that a lowercase name could spell.
///
/// Weak keywords (`union`, `macro_rules`) are contextual and remain usable as
/// crate names, so they are deliberately absent.
const RUST_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Dependency names the scaffold's `Cargo.toml` already declares.
///
/// A project named after one of these declares the same extern name twice.
/// `every_scaffold_dependency_name_is_reserved` in `tests/scaffold_test.rs`
/// keeps this in step with the template.
const SCAFFOLD_DEPENDENCIES: &[&str] = &["groundhog_json_rpc_client", "sdk", "tokio"];

/// Subdirectories Cargo creates under `target/<triple>/<profile>`.
///
/// The scaffold's RV32 `[[bin]]` target takes the snake-case package name, and
/// `groundhog build` reads the ELF from `release/<snake_name>` — which one of
/// these turns into a directory.
const CARGO_BUILD_DIRS: &[&str] = &["build", "deps", "examples", "incremental"];

/// Whether `name` is a reserved Windows device name, which cannot be a
/// directory on that platform.
fn is_windows_device_name(name: &str) -> bool {
    const RESERVED: &[&str] = &["aux", "con", "nul", "prn"];
    if RESERVED.contains(&name) {
        return true;
    }
    ["com", "lpt"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| matches!(rest.as_bytes(), [b'1'..=b'9']))
    })
}

/// Why a project name cannot be used, or `None` when it is usable.
///
/// Each case names a collision that makes the generated project fail to build
/// rather than a style preference.
fn reserved_name_reason(name: &str) -> Option<&'static str> {
    let crate_name = to_snake(name);
    if RUST_SYSROOT_CRATES.contains(&crate_name.as_str()) {
        return Some(
            "conflicts with one of Rust's built-in crates; the generated project depends on \
             itself for host tests, which would shadow it",
        );
    }
    if RUST_KEYWORDS.contains(&crate_name.as_str()) {
        return Some(
            "is a Rust keyword, so the generated `use` and `extern crate` statements would \
             not parse",
        );
    }
    if SCAFFOLD_DEPENDENCIES.contains(&crate_name.as_str()) {
        return Some("is already a dependency of the generated project");
    }
    if CARGO_BUILD_DIRS.contains(&crate_name.as_str()) {
        return Some(
            "conflicts with one of Cargo's build directory names, so `groundhog build` could \
             not find the RV32 artifact",
        );
    }
    if is_windows_device_name(name) {
        return Some(
            "is a reserved Windows device name, so the project directory cannot be created",
        );
    }
    None
}

/// Check that `name` can be a Groundhog project name.
///
/// Rejects names a generated project could not build under, so the failure
/// lands on the argument rather than on the first `cargo` invocation inside a
/// scaffold that is already on disk. [`generate`] applies this itself;
/// callers that create the project directory first should call it earlier.
pub fn validate_name(name: &str) -> Result<(), CliError> {
    let reject = |message: &str| {
        Err(CliError::InvalidArgument {
            argument: "project name".into(),
            message: message.into(),
        })
    };
    if name.is_empty() {
        return reject("cannot be empty");
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return reject("must start with a lowercase letter");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return reject("must contain only lowercase letters, digits, and hyphens");
    }
    if name.ends_with('-') || name.contains("--") {
        return reject("must not end with a hyphen or contain consecutive hyphens");
    }
    if let Some(reason) = reserved_name_reason(name) {
        return reject(&format!("`{name}` {reason}"));
    }
    Ok(())
}

/// Generate a contract project from a template.
pub fn generate(dir: &Path, name: &str, template: &str) -> Result<(), CliError> {
    validate_name(name)?;

    let (lib_tmpl, impl_tmpl, test_tmpl, test_rv32_tmpl) = match template {
        "counter" => (COUNTER_LIB, COUNTER_IMPL, COUNTER_TEST, COUNTER_TEST_RV32),
        "stub" => (STUB_LIB, STUB_IMPL, STUB_TEST, STUB_TEST_RV32),
        other => {
            return Err(CliError::UnknownTemplate(other.to_string()));
        }
    };

    fs::create_dir_all(dir.join("src"))?;
    fs::create_dir_all(dir.join("tests"))?;
    fs::create_dir_all(dir.join(".cargo"))?;

    fs::write(dir.join("Cargo.toml"), apply(CARGO_TOML, name))?;
    fs::write(dir.join("build.rs"), BUILD_RS)?;
    fs::write(dir.join(".gitignore"), GITIGNORE)?;
    fs::write(dir.join("AGENTS.md"), apply(AGENTS_MD, name))?;
    fs::write(dir.join("groundhog.toml"), apply(GROUNDHOG_TOML, name))?;
    fs::write(dir.join("rust-toolchain.toml"), apply(RUST_TOOLCHAIN, name))?;
    fs::write(dir.join(".cargo/config.toml"), apply(CARGO_CONFIG, name))?;
    fs::write(dir.join(".cargo/contract.toml"), apply(CONTRACT_TOML, name))?;
    fs::write(dir.join("src/lib.rs"), apply(lib_tmpl, name))?;
    fs::write(dir.join("src/implementation.rs"), apply(impl_tmpl, name))?;
    fs::write(dir.join("src/main.rs"), apply(RV32_MAIN, name))?;
    fs::write(
        dir.join("tests/integration.rs"),
        apply(INTEGRATION_TEST, name),
    )?;
    fs::write(dir.join("tests/test.rs"), apply(test_tmpl, name))?;
    fs::write(dir.join("tests/test_rv32.rs"), apply(test_rv32_tmpl, name))?;

    Ok(())
}
