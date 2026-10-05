//! Embedded project recipes and offline discovery.

use crate::{CliError, Print, result::CommandResult};
use schemars::JsonSchema;
use serde::Serialize;

pub(crate) type File = (&'static str, &'static str);

/// Public description of an installable whole-project template.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: &'static str,
    pub description: &'static str,
    pub contents: &'static [&'static str],
    pub prerequisites: &'static [&'static str],
    pub next_steps: &'static [&'static str],
    #[serde(skip)]
    pub(crate) files: &'static [File],
}

pub(crate) const BASE: &[File] = &[
    ("build.rs", include_str!("../templates/build.rs.tmpl")),
    ("Cargo.toml", include_str!("../templates/Cargo.toml.tmpl")),
    (
        ".cargo/config.toml",
        include_str!("../templates/config.toml.tmpl"),
    ),
    (
        ".cargo/contract.toml",
        include_str!("../templates/contract.toml.tmpl"),
    ),
    ("AGENTS.md", include_str!("../templates/agents.md.tmpl")),
    (".gitignore", include_str!("../templates/gitignore.tmpl")),
    (
        "groundhog.toml",
        include_str!("../templates/groundhog.toml.tmpl"),
    ),
    (
        "tests/integration.rs",
        include_str!("../templates/integration.rs.tmpl"),
    ),
    (
        "rust-toolchain.toml",
        include_str!("../templates/rust-toolchain.toml.tmpl"),
    ),
    ("src/main.rs", include_str!("../templates/main.rs.tmpl")),
];

pub(crate) const MINIMAL: &[File] = &[
    ("src/lib.rs", include_str!("../templates/stub/lib.rs.tmpl")),
    (
        "src/implementation.rs",
        include_str!("../templates/stub/implementation.rs.tmpl"),
    ),
    (
        "tests/test.rs",
        include_str!("../templates/stub/test.rs.tmpl"),
    ),
    (
        "tests/test_rv32.rs",
        include_str!("../templates/stub/test_rv32.rs.tmpl"),
    ),
];

const COUNTER: &[File] = &[
    (
        "src/lib.rs",
        include_str!("../templates/counter/lib.rs.tmpl"),
    ),
    (
        "src/implementation.rs",
        include_str!("../templates/counter/implementation.rs.tmpl"),
    ),
    (
        "tests/test.rs",
        include_str!("../templates/counter/test.rs.tmpl"),
    ),
    (
        "tests/test_rv32.rs",
        include_str!("../templates/counter/test_rv32.rs.tmpl"),
    ),
];

/// Available templates. The minimal default is deliberately not a catalog entry.
pub const CATALOG: &[Template] = &[Template {
    id: "counter",
    description: "A counter contract with native, RV32, and RPC integration tests",
    contents: &[
        "Rust counter contract",
        "contract tests",
        "Groundhog project configuration",
    ],
    prerequisites: &["Rust with rustup", "Groundhog Cargo registry credentials"],
    next_steps: &["cargo test", "groundhog build", "groundhog test"],
    files: COUNTER,
}];

/// Resolve an explicit template before creating any files.
pub fn lookup(id: &str) -> Result<&'static Template, CliError> {
    if id == "stub" {
        return Err(CliError::InvalidArgument {
            argument: "--template".into(),
            message: "omit --template to create the minimal contract; `stub` is no longer a named template".into(),
        });
    }
    CATALOG
        .iter()
        .find(|template| template.id == id)
        .ok_or_else(|| CliError::UnknownTemplate(id.into()))
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ListReport {
    schema_version: u32,
    templates: &'static [Template],
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ShowReport {
    schema_version: u32,
    template: &'static Template,
}

/// List embedded templates without network access or environment setup.
pub fn list(cmd: &'static str, print: &Print) -> Result<(), CliError> {
    for template in CATALOG {
        print.status(format_args!("{} — {}", template.id, template.description));
    }
    print.json(&CommandResult::new(
        cmd,
        ListReport {
            schema_version: 1,
            templates: CATALOG,
        },
    ))
}

/// Describe a template's contents, prerequisites, and next steps.
pub fn show(id: &str, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let template = lookup(id)?;
    print.summary_card(
        template.id,
        &[
            ("Description".into(), template.description.into()),
            ("Contents".into(), template.contents.join(", ")),
            ("Prerequisites".into(), template.prerequisites.join(", ")),
        ],
    );
    print.next_steps(
        &template
            .next_steps
            .iter()
            .map(|step| (*step).to_owned())
            .collect::<Vec<_>>(),
    );
    print.json(&CommandResult::new(
        cmd,
        ShowReport {
            schema_version: 1,
            template,
        },
    ))
}
