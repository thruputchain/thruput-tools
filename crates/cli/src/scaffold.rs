use crate::CliError;
use std::fs;
use std::io::Write;
use std::path::Path;

/// SDK version exact-pinned into both scaffold target dependencies.
///
/// Generated projects resolve outside this workspace, so this must be a
/// version published to the Thruput registry, and one whose raw spec
/// format this CLI reads — otherwise `thruput build` refuses the fresh
/// scaffold at the preflight. Bump together with
/// [`SCAFFOLD_TOOLCHAIN_CHANNEL`] when a new SDK release is validated on a
/// newer nightly. The CI `cli-smoke` job builds and tests a fresh scaffold
/// against the real registry, so an unpublished or incompatible pin fails
/// there (and stays red until the pinned SDK is published).
pub const SCAFFOLD_SDK_VERSION: &str = "0.57.1";

/// `thruput-json-rpc-client` version pinned into scaffolded projects.
///
/// Shares the CLI's workspace version and is published before the matching CLI.
/// PR CI builds scaffolds against this checkout's crate
/// (`scripts/use-checkout-client.sh`); `publish.yml` checks the published
/// version from the registry. The caret requirement lets scaffolds pick up
/// newer compatible releases.
pub const SCAFFOLD_JSON_RPC_CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

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
const SCAFFOLD_DEPENDENCIES: &[&str] = &[
    "serde_json",
    "thruput_json_rpc_client",
    "thruput_sdk",
    "tokio",
];

/// Subdirectories Cargo creates under `target/<triple>/<profile>`.
///
/// The scaffold's RV32 `[[bin]]` target takes the snake-case package name, and
/// `thruput build` reads the ELF from `release/<snake_name>` — which one of
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
            "conflicts with one of Cargo's build directory names, so `thruput build` could \
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

/// Check that `name` can be a Thruput project name.
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
pub fn generate(dir: &Path, name: &str, template: Option<&str>) -> Result<(), CliError> {
    validate_name(name)?;
    let recipe = template.map(crate::templates::lookup).transpose()?;
    let files = recipe.map_or(&[crate::templates::MINIMAL][..], |recipe| recipe.files);
    for (path, contents) in crate::templates::BASE
        .iter()
        .chain(files.iter().flat_map(|files| files.iter()))
    {
        let mut contents = apply(contents, name);
        if *path == "AGENTS.md" {
            contents = contents
                .replace("{{CLI_VERSION}}", env!("CARGO_PKG_VERSION"))
                .replace(
                    "{{TEMPLATE_NAME}}",
                    recipe.map_or("minimal", |recipe| recipe.id),
                );
            if let Some(notes) = recipe.and_then(|recipe| recipe.agent_notes) {
                contents.push('\n');
                contents.push_str(&apply(notes, name));
            }
        }
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(contents.as_bytes())?;
    }
    Ok(())
}
