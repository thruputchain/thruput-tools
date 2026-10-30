//! Embedded project recipes and offline discovery.

use crate::{CliError, Print, result::CommandResult};
use schemars::JsonSchema;
use serde::Serialize;

pub(crate) type File = (&'static str, &'static str);

/// Public description of an installable whole-project template.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    /// Stable identifier accepted by `new --template`.
    pub id: &'static str,
    /// One-line description for discovery.
    pub description: &'static str,
    /// Major components included in the generated project.
    pub contents: &'static [&'static str],
    /// Requirements for building and running the project, not for creating files.
    pub prerequisites: &'static [&'static str],
    /// Commands to run from the generated project directory.
    pub next_steps: &'static [&'static str],
    #[serde(skip)]
    pub(crate) files: &'static [&'static [File]],
    #[serde(skip)]
    pub(crate) agent_notes: Option<&'static str>,
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
        "thruput.toml",
        include_str!("../templates/thruput.toml.tmpl"),
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

const CONTRACT_README: &[File] = &[("README.md", include_str!("../templates/README.md.tmpl"))];

pub(crate) const MINIMAL: &[File] = &[
    ("README.md", include_str!("../templates/README.md.tmpl")),
    (
        "tests/common/mod.rs",
        include_str!("../templates/stub/common.rs.tmpl"),
    ),
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
        "tests/common/mod.rs",
        include_str!("../templates/counter/common.rs.tmpl"),
    ),
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

const APP: &[File] = &[
    (
        "web/AGENTS.md",
        include_str!("../templates/counter-app/agents.md.tmpl"),
    ),
    (
        "package.json",
        include_str!("../templates/counter-app/package.json.tmpl"),
    ),
    (
        "tsconfig.json",
        include_str!("../templates/counter-app/tsconfig.json.tmpl"),
    ),
    (
        "web/index.html",
        include_str!("../templates/counter-app/index.html.tmpl"),
    ),
    (
        "web/main.tsx",
        include_str!("../templates/counter-app/main.tsx.tmpl"),
    ),
    (
        "web/wallet.ts",
        include_str!("../templates/counter-app/wallet.ts.tmpl"),
    ),
    (
        "web/refresh-counter.ts",
        include_str!("../templates/counter-app/refresh-counter.ts.tmpl"),
    ),
    (
        "web/style.css",
        include_str!("../templates/counter-app/style.css.tmpl"),
    ),
    (
        "scripts/thruput.mjs",
        include_str!("../templates/counter-app/thruput.mjs.tmpl"),
    ),
    (
        "scripts/build.mjs",
        include_str!("../templates/counter-app/build.mjs.tmpl"),
    ),
    (
        "scripts/dev.mjs",
        include_str!("../templates/counter-app/dev.mjs.tmpl"),
    ),
    (
        "scripts/contract-build.mjs",
        include_str!("../templates/counter-app/contract-build.mjs.tmpl"),
    ),
    (
        "scripts/deploy.mjs",
        include_str!("../templates/counter-app/deploy.mjs.tmpl"),
    ),
    (
        "scripts/deployment.mjs",
        include_str!("../templates/counter-app/deployment.mjs.tmpl"),
    ),
    (
        "README.md",
        include_str!("../templates/counter-app/README.md.tmpl"),
    ),
    ("web/.gitignore", "/generated/\n"),
];

/// Available templates. The minimal default is deliberately not a catalog entry.
pub const CATALOG: &[Template] = &[
    Template {
        id: "counter",
        description: "A counter contract with native, RV32, and RPC integration tests",
        contents: &[
            "Rust counter contract",
            "contract tests",
            "Thruput project configuration",
        ],
        prerequisites: &["Rust with rustup"],
        next_steps: &["cargo test", "thruput build", "thruput test"],
        files: &[COUNTER, CONTRACT_README],
        agent_notes: None,
    },
    Template {
        id: "counter-app",
        description: "A counter contract and a local React application with a temporary browser wallet",
        contents: &[
            "counter contract and tests",
            "generated TypeScript bindings",
            "React + Vite frontend",
            "local deployment scripts",
        ],
        prerequisites: &[
            "Thruput CLI configured via thruput init",
            "running thruput node",
            "Rust with rustup",
            "Node.js 22.12+ and npm",
        ],
        next_steps: &[
            "thruput node  # in another terminal",
            "npm install",
            "npm run contract:deploy",
            "npm run dev",
        ],
        files: &[COUNTER, APP],
        agent_notes: Some(include_str!("../templates/counter-app/agents-root.md.tmpl")),
    },
];

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
    let rows = CATALOG
        .iter()
        .map(|template| vec![template.id.into(), template.description.into()])
        .collect::<Vec<_>>();
    print.data_table(
        "Project templates",
        &["ID".into(), "Description".into()],
        &rows,
        "No templates available",
        CATALOG
            .iter()
            .map(|template| format!("{}\t{}", template.id, template.description)),
    )?;
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
    let items = [
        ("Description".into(), template.description.into()),
        ("Contents".into(), template.contents.join(", ")),
        ("Prerequisites".into(), template.prerequisites.join(", ")),
        ("Next steps".into(), template.next_steps.join("; ")),
    ];
    print.data_summary(
        template.id,
        &items,
        items
            .iter()
            .map(|(label, value)| format!("{label}: {value}")),
    )?;
    print.json(&CommandResult::new(
        cmd,
        ShowReport {
            schema_version: 1,
            template,
        },
    ))
}
