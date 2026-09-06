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

/// SDK version pinned into scaffolded projects.
///
/// Generated projects resolve outside this workspace, so this must be a
/// version published to the Groundhog registry, and one whose raw spec
/// format this CLI reads — otherwise `groundhog build` refuses the fresh
/// scaffold at the preflight. Bump together with
/// [`SCAFFOLD_TOOLCHAIN_CHANNEL`] when a new SDK release is validated on a
/// newer nightly. The CI `cli-smoke` job builds and tests a fresh scaffold
/// against the real registry, so an unpublished or incompatible pin fails
/// there (and stays red until the pinned SDK is published).
pub const SCAFFOLD_SDK_VERSION: &str = "0.31.0";

/// `groundhog-json-rpc-client` version pinned into scaffolded projects.
///
/// Must already be published to the Groundhog registry. The pin is a caret
/// requirement, so scaffolds pick up newer compatible releases on their own.
pub const SCAFFOLD_JSON_RPC_CLIENT_VERSION: &str = "0.13.0";

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

fn validate_name(name: &str) -> Result<(), CliError> {
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
