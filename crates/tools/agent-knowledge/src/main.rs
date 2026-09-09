use std::path::PathBuf;

use groundhog_agent_knowledge::{check_bundle, write_bundle};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        fail("usage: groundhog-agent-knowledge <build|check> [bundle-root]");
    };
    let root = args.next().map_or_else(default_bundle_root, PathBuf::from);
    if args.next().is_some() {
        fail("usage: groundhog-agent-knowledge <build|check> [bundle-root]");
    }

    let result = match command.to_str() {
        Some("build") => write_bundle(&root),
        Some("check") => check_bundle(&root),
        _ => fail("usage: groundhog-agent-knowledge <build|check> [bundle-root]"),
    };

    if let Err(error) = result {
        fail(&error.to_string());
    }
}

fn default_bundle_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cli/assets/groundhog/knowledge")
}

fn fail(message: &str) -> ! {
    eprintln!("groundhog-agent-knowledge: {message}");
    std::process::exit(1);
}
