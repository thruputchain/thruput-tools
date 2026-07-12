use crate::CliError;
use std::fs;
use std::path::Path;

const CARGO_TOML: &str = include_str!("../templates/Cargo.toml.tmpl");
const CARGO_CONFIG: &str = include_str!("../templates/config.toml.tmpl");
const CONTRACT_TOML: &str = include_str!("../templates/contract.toml.tmpl");
const GITIGNORE: &str = include_str!("../templates/gitignore.tmpl");
const GROUNDHOG_TOML: &str = include_str!("../templates/groundhog.toml.tmpl");
const RUST_TOOLCHAIN: &str = include_str!("../templates/rust-toolchain.toml.tmpl");
const RV32_MAIN: &str = include_str!("../templates/main.rs.tmpl");

const COUNTER_LIB: &str = include_str!("../templates/counter/lib.rs.tmpl");
const COUNTER_IMPL: &str = include_str!("../templates/counter/implementation.rs.tmpl");
const COUNTER_TEST: &str = include_str!("../templates/counter/test.rs.tmpl");
const COUNTER_TEST_RV32: &str = include_str!("../templates/counter/test_rv32.rs.tmpl");
const COUNTER_TEST_WASM: &str = include_str!("../templates/counter/test_wasm.rs.tmpl");

const STUB_LIB: &str = include_str!("../templates/stub/lib.rs.tmpl");
const STUB_IMPL: &str = include_str!("../templates/stub/implementation.rs.tmpl");
const STUB_TEST: &str = include_str!("../templates/stub/test.rs.tmpl");
const STUB_TEST_RV32: &str = include_str!("../templates/stub/test_rv32.rs.tmpl");
const STUB_TEST_WASM: &str = include_str!("../templates/stub/test_wasm.rs.tmpl");

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
}

fn validate_name(name: &str) -> Result<(), CliError> {
    if name.is_empty() {
        return Err(CliError::Other("project name cannot be empty".into()));
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(CliError::Other(
            "project name must start with a \
             lowercase letter"
                .into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(CliError::Other(
            "project name must contain only \
             lowercase letters, digits, \
             and hyphens"
                .into(),
        ));
    }
    if name.ends_with('-') || name.contains("--") {
        return Err(CliError::Other(
            "project name must not end with a \
             hyphen or contain consecutive hyphens"
                .into(),
        ));
    }
    Ok(())
}

/// Generate a contract project from a template.
pub fn generate(dir: &Path, name: &str, template: &str) -> Result<(), CliError> {
    validate_name(name)?;

    let (lib_tmpl, impl_tmpl, test_tmpl, test_rv32_tmpl, test_wasm_tmpl) = match template {
        "counter" => (
            COUNTER_LIB,
            COUNTER_IMPL,
            COUNTER_TEST,
            COUNTER_TEST_RV32,
            COUNTER_TEST_WASM,
        ),
        "stub" => (
            STUB_LIB,
            STUB_IMPL,
            STUB_TEST,
            STUB_TEST_RV32,
            STUB_TEST_WASM,
        ),
        other => {
            return Err(CliError::UnknownTemplate(other.to_string()));
        }
    };

    fs::create_dir_all(dir.join("src"))?;
    fs::create_dir_all(dir.join("tests"))?;
    fs::create_dir_all(dir.join(".cargo"))?;

    fs::write(dir.join("Cargo.toml"), apply(CARGO_TOML, name))?;
    fs::write(dir.join(".gitignore"), GITIGNORE)?;
    fs::write(dir.join("groundhog.toml"), apply(GROUNDHOG_TOML, name))?;
    fs::write(dir.join("rust-toolchain.toml"), apply(RUST_TOOLCHAIN, name))?;
    fs::write(dir.join(".cargo/config.toml"), apply(CARGO_CONFIG, name))?;
    fs::write(dir.join(".cargo/contract.toml"), apply(CONTRACT_TOML, name))?;
    fs::write(dir.join("src/lib.rs"), apply(lib_tmpl, name))?;
    fs::write(dir.join("src/implementation.rs"), apply(impl_tmpl, name))?;
    fs::write(dir.join("src/main.rs"), apply(RV32_MAIN, name))?;
    fs::write(dir.join("tests/test.rs"), apply(test_tmpl, name))?;
    fs::write(dir.join("tests/test_rv32.rs"), apply(test_rv32_tmpl, name))?;
    fs::write(dir.join("tests/test_wasm.rs"), apply(test_wasm_tmpl, name))?;

    Ok(())
}
