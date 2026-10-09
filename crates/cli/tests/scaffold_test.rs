use std::fs;
use tempfile::TempDir;

fn assert_contains_all(contents: &str, expected: &[&str]) {
    for text in expected {
        assert!(contents.contains(text), "missing `{text}`");
    }
}

fn assert_integration_scaffold(dir: &std::path::Path) {
    let build = fs::read_to_string(dir.join("build.rs")).unwrap();
    assert_contains_all(
        &build,
        &[
            "cargo:rerun-if-env-changed=THRUPUT_INTEGRATION_TEST",
            "cargo:rustc-cfg=thruput_integration_runner",
            "cargo:rustc-check-cfg=cfg(thruput_integration_runner)",
        ],
    );

    let integration = fs::read_to_string(dir.join("tests/integration.rs")).unwrap();
    assert_contains_all(
        &integration,
        &[
            "thruput test --integration",
            "#[ignore =",
            "THRUPUT_RPC_URL",
            "THRUPUT_INTEGRATION_TEST",
            "not(thruput_integration_runner)",
            "THRUPUT_CONTRACT_ADDRESS",
            "THRUPUT_RUNTIME_MANIFEST",
            "HttpClientBuilder",
            ".chain_tip()",
        ],
    );
}

fn assert_rv32_only_test_scaffold(dir: &std::path::Path) {
    assert!(dir.join("tests/test.rs").exists());
    assert!(dir.join("tests/test_rv32.rs").exists());
    assert!(dir.join("tests/integration.rs").exists());
    assert!(!dir.join("tests/test_wasm.rs").exists());
}

/// Assert the scaffolded `.cargo` pair splits build configuration correctly:
/// every target rustflag in `config.toml`, and `build-std` alone in the
/// `contract.toml` overlay.
fn assert_scaffold_cargo_config(dir: &std::path::Path) {
    let config = fs::read_to_string(dir.join(".cargo/config.toml")).unwrap();
    assert!(config.contains("[target.riscv32im-unknown-none-elf]"));
    // The canonical rv32 flag set, verbatim: `+relax` and the two extra link
    // args are measured size wins, and a substring match on the bare
    // `+unaligned-scalar-mem` would not notice them going missing.
    assert_contains_all(
        &config,
        &[
            "target-feature=+unaligned-scalar-mem,+relax",
            "link-arg=-Tgroundhog-rv32.ld",
            "link-arg=--icf=all",
            "link-arg=--relax-gp",
            "trap-unreachable=false",
            "[profile.dev.build-override]",
            "# [profile.dev.package.\"*\"]",
        ],
    );
    // The RV32 target sets `panic=immediate-abort` in config.toml, not in the
    // contract.toml overlay.
    assert_eq!(config.matches("panic=immediate-abort").count(), 1);
    assert!(!config.contains(r#""panic=abort""#));
    assert!(!config.contains("wasm"));

    // The overlay is `build-std` and nothing else: cargo JOINS target
    // rustflags across config layers, so a `[target.*]` section here would
    // duplicate the rv32 linker script and fail the link.
    let contract = fs::read_to_string(dir.join(".cargo/contract.toml")).unwrap();
    assert_contains_all(
        &contract,
        &["[unstable]", r#"build-std = ["core", "alloc"]"#],
    );
    assert!(
        !contract.lines().any(|line| line.starts_with("[target.")),
        "contract.toml: {contract}"
    );
}

/// Assert the scaffolded agent guide states the load-bearing toolchain
/// contracts a coding agent must not refactor away.
fn assert_agents_guide(dir: &std::path::Path, snake_name: &str) {
    let agents = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert_contains_all(
        &agents,
        &[
            "thruput skill show contracts",
            "thruput skill show cli",
            "thruput skill show typescript",
            "thruput docs export --output <new-directory>",
            "temporary directory outside the project",
            "thruput build",
            "bare Cargo RV32 build misses that configuration",
            "`entrypoints`",
            "Run both host and RV32",
            "release builds wrap on integer overflow",
            "tests/test_rv32.rs",
            "Do not make `dev` a default feature",
            "thruput-integration-test",
            "unification",
            "self-referential dev-dependency",
            "resolved SDK version",
            ".cargo/contract.toml",
            "rust-toolchain.toml",
            "without overwriting unrelated settings",
            "region 'CODE' already defined",
            "sdk::declare_mock_contract!()",
            "Keep contract logic in `implementation.rs`",
            "`extern crate` identifier",
        ],
    );
    assert!(agents.contains(snake_name));
    assert!(agents.contains(&format!(
        "Scaffolded with Thruput {}",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(!agents.contains("thruput --skill"));
    assert!(
        !agents.contains("npm run"),
        "contract-only guides must not invent a frontend"
    );
    assert!(!agents.contains("{{"), "unsubstituted template placeholder");
}

fn assert_counter_manifest(cargo: &str) {
    assert!(cargo.contains(r#"name = "my-counter""#));
    assert!(cargo.contains("registry = \"groundhog\""));
    assert!(!cargo.contains("sdk/spec"));
    // The version pins live once, in the `scaffold` constants; here we assert
    // they were substituted (no placeholder survives below) and land on the
    // right dependencies.
    // `sdk` is declared once per target: plain on the guest, with `std` on
    // the host so `cargo build` / `cargo check` work without `dev`.
    assert!(cargo.contains(&format!(
        r#"sdk = {{ version = "={}", registry = "groundhog" }}"#,
        thruput_cli::scaffold::SCAFFOLD_SDK_VERSION
    )));
    assert!(cargo.contains(&format!(
        r#"sdk = {{ version = "={}", registry = "groundhog", features = ["std"] }}"#,
        thruput_cli::scaffold::SCAFFOLD_SDK_VERSION
    )));
    assert!(cargo.contains(r#"[target.'cfg(target_arch = "riscv32")'.dependencies]"#));
    assert!(cargo.contains("thruput-integration-test = ["));
    assert!(cargo.contains(
        r#"[target.'cfg(not(target_arch = "riscv32"))'.dependencies.groundhog-json-rpc-client]"#
    ));
    assert!(cargo.contains(&format!(
        "version = \"{}\"",
        thruput_cli::scaffold::SCAFFOLD_JSON_RPC_CLIENT_VERSION
    )));
    assert!(!cargo.contains("{{"), "unsubstituted template placeholder");
    assert!(cargo.contains("optional = true"));
    assert!(cargo.contains(r#"tokio = { version = "1", features = ["macros", "rt-multi-thread"]"#));
    assert!(!cargo.contains(r#"features = ["storage"]"#));
}

#[test]
fn scaffold_commented_alias_key_matches_the_name_deploy_saves() {
    // `thruput deploy` saves the contract alias under the Cargo *package*
    // name (deploy.rs: `prepared.package_name`, which artifacts.rs fills from
    // the manifest), and alias lookup is an exact map get with no
    // normalisation. The commented example in thruput.toml is an
    // instruction to the author, so the key it shows has to be the key the
    // CLI will actually write and resolve -- otherwise uncommenting it and
    // pasting an address yields an alias `thruput call` cannot find.
    //
    // Asserting the literal string alone would not catch that: it pins the
    // template against itself. This pins it against the manifest instead.
    for name in ["my-counter", "plain", "a-b-c"] {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(name);
        fs::create_dir(&dir).unwrap();
        thruput_cli::scaffold::generate(&dir, name, Some("counter")).unwrap();

        let cargo = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        let package_name = cargo
            .lines()
            .skip_while(|l| l.trim() != "[package]")
            .find_map(|l| l.strip_prefix("name = "))
            .map(|v| v.trim().trim_matches('"').to_owned())
            .expect("bug: scaffolded manifest should declare a package name");

        let thruput = fs::read_to_string(dir.join("thruput.toml")).unwrap();
        assert!(
            thruput.contains(&format!("# {package_name} =")),
            "thruput.toml should comment the alias under the package name \
             `{package_name}`, which is what deploy saves, but it reads:\n{thruput}"
        );
    }
}

#[test]
fn scaffold_counter_creates_expected_files() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-counter");
    fs::create_dir(&dir).unwrap();

    thruput_cli::scaffold::generate(&dir, "my-counter", Some("counter")).unwrap();

    assert!(dir.join("Cargo.toml").exists());
    assert!(dir.join("build.rs").exists());
    assert!(dir.join(".gitignore").exists());
    assert_agents_guide(&dir, "my_counter");
    assert!(dir.join("thruput.toml").exists());
    assert!(dir.join("rust-toolchain.toml").exists());
    assert!(dir.join(".cargo/config.toml").exists());
    assert!(dir.join(".cargo/contract.toml").exists());
    assert!(dir.join("src/lib.rs").exists());
    assert!(dir.join("src/implementation.rs").exists());
    assert!(dir.join("src/main.rs").exists());
    assert_rv32_only_test_scaffold(&dir);

    let cargo = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert_counter_manifest(&cargo);

    assert_integration_scaffold(&dir);
    // RV32 bin wrapper target: the bin name must be the snake-case package
    // name because `thruput build` looks for the ELF at
    // `release/<snake_name>`.
    assert!(cargo.contains("[[bin]]"));
    assert!(cargo.contains(r#"name = "my_counter""#));
    // `entrypoints` is the name the #[contract] macro's cfg matches; the
    // WASM-era `rv32-entrypoints` wrapper is gone from new scaffolds, and
    // `dev` must not be a default feature — a dependent crate would drag
    // `sdk/dev` (std) into guest builds through feature unification.
    assert!(cargo.contains(r#"required-features = ["entrypoints"]"#));
    assert!(!cargo.contains("rv32-entrypoints"));
    assert!(!cargo.contains("default = "));
    assert!(!cargo.contains("cdylib"));
    // The host test harness must not try to link the no_std bin when the
    // entrypoints feature is enabled.
    assert!(cargo.contains("test = false"));
    assert!(cargo.contains("bench = false"));

    let gitignore = fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert!(gitignore.contains("/target/"));
    assert!(gitignore.contains("/.thruput/"));

    let thruput = fs::read_to_string(dir.join("thruput.toml")).unwrap();
    assert!(thruput.contains("[profile.local]"));
    assert!(thruput.contains("network = \"local\""));
    assert!(thruput.contains("[profile.local.contracts]"));
    assert!(thruput.contains("# [profile.local.wallets.alice]"));
    assert!(thruput.contains("[profile.devnet]"));
    assert!(thruput.contains("network = \"devnet\""));
    assert!(thruput.contains("[profile.devnet.contracts]"));
    assert!(thruput.contains("# [profile.devnet.wallets.alice]"));
    assert!(thruput.contains("# owner_public_key ="));
    assert!(thruput.contains("# my-counter ="));
    assert!(thruput.contains("default_profile = \"local\""));
    assert!(!thruput.contains("rpc_url"));
    assert!(!thruput.contains("faucet_url"));
    assert!(!thruput.contains("runtime_manifest"));
    assert!(!thruput.contains("default_key"));

    let main = fs::read_to_string(dir.join("src/main.rs")).unwrap();
    assert!(main.contains("#![no_std]"));
    assert!(main.contains("#![no_main]"));
    assert!(main.contains("extern crate my_counter;"));
    assert!(!main.contains("--target"));

    assert_scaffold_cargo_config(&dir);

    let toolchain = fs::read_to_string(dir.join("rust-toolchain.toml")).unwrap();
    assert!(toolchain.contains(&format!(
        "channel = \"{}\"",
        thruput_cli::scaffold::SCAFFOLD_TOOLCHAIN_CHANNEL
    )));
    assert!(toolchain.contains(r#"targets = ["riscv32im-unknown-none-elf"]"#));
    assert!(!toolchain.contains("wasm"));
    assert!(
        !toolchain.contains("{{"),
        "unsubstituted template placeholder"
    );

    let lib = fs::read_to_string(dir.join("src/lib.rs")).unwrap();
    assert!(lib.contains("mod implementation"));

    let imp = fs::read_to_string(dir.join("src/implementation.rs")).unwrap();
    assert!(imp.contains("pub struct Counter"));
    assert!(imp.contains("#[contract_exception]"));
    assert!(!imp.contains("#[contract_error]"));
    // Under sdk 0.15.x's declaration-based event inclusion, the event only
    // lands in the contract spec when declared via emits(...).
    assert!(imp.contains("#[contract(throws = CounterError, emits(PingEvent))]"));

    let test = fs::read_to_string(dir.join("tests/test.rs")).unwrap();
    assert!(test.contains("register_mock_contract"));
    assert!(test.contains("CounterClient"));
    assert!(test.contains("fn setup()"));
    assert!(test.contains("*test_contract.as_hash()"));
    assert!(test.contains("env.transact(|tx|"));
    assert!(test.contains("tx.call(&caller).run(move ||"));
    assert!(test.contains("MY_COUNTER_NONCE"));

    let test_rv32 = fs::read_to_string(dir.join("tests/test_rv32.rs")).unwrap();
    assert!(test_rv32.contains("load_rv32_contract"));
    assert!(test_rv32.contains("CARGO_TARGET_DIR"));
    assert!(test_rv32.contains("thruput_artifact(\"my_counter.rv32a\")"));
    assert!(test_rv32.contains("requires RV32 build — run via `thruput test`"));
    assert!(test_rv32.contains("my_counter"));
}

#[test]
fn scaffold_default_has_empty_contract() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-stub");
    fs::create_dir(&dir).unwrap();

    thruput_cli::scaffold::generate(&dir, "my-stub", None).unwrap();

    let lib = fs::read_to_string(dir.join("src/lib.rs")).unwrap();
    assert!(lib.contains("mod implementation"));
    assert!(!lib.contains("increment"));

    let imp = fs::read_to_string(dir.join("src/implementation.rs")).unwrap();
    assert!(imp.contains("pub struct Contract"));
    assert!(!imp.contains("increment"));
    assert!(!imp.contains("#[contract_exception]"));

    assert_rv32_only_test_scaffold(&dir);
    // The stub renders its own tests/test.rs; keep it on the same
    // clippy-clean shape as the counter's.
    let test = fs::read_to_string(dir.join("tests/test.rs")).unwrap();
    assert!(test.contains("*test_contract.as_hash()"));
    assert!(!test.contains(".clone()"));
    let test_rv32 = fs::read_to_string(dir.join("tests/test_rv32.rs")).unwrap();
    assert!(test_rv32.contains("load_rv32_contract"));
    assert!(test_rv32.contains("CARGO_TARGET_DIR"));
    assert!(test_rv32.contains("thruput_artifact(\"my_stub.rv32a\")"));
    assert!(test_rv32.contains("requires RV32 build — run via `thruput test`"));

    let main = fs::read_to_string(dir.join("src/main.rs")).unwrap();
    assert!(main.contains("extern crate my_stub;"));
}

#[test]
fn scaffold_unknown_template_errors() {
    let tmp = TempDir::new().unwrap();

    let result = thruput_cli::scaffold::generate(tmp.path(), "my-project", Some("nope"));
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_empty_name() {
    let tmp = TempDir::new().unwrap();
    let result = thruput_cli::scaffold::generate(tmp.path(), "", Some("counter"));
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_uppercase_name() {
    let tmp = TempDir::new().unwrap();
    let result = thruput_cli::scaffold::generate(tmp.path(), "MyProject", Some("counter"));
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_name_starting_with_digit() {
    let tmp = TempDir::new().unwrap();
    let result = thruput_cli::scaffold::generate(tmp.path(), "1foo", Some("counter"));
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_name_with_underscore() {
    let tmp = TempDir::new().unwrap();
    let result = thruput_cli::scaffold::generate(tmp.path(), "my_project", Some("counter"));
    assert!(result.is_err());
}

#[test]
fn scaffold_accepts_valid_name_with_hyphens() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-project-2");
    fs::create_dir(&dir).unwrap();
    let result = thruput_cli::scaffold::generate(&dir, "my-project-2", Some("counter"));
    assert!(result.is_ok());
}

#[test]
fn scaffold_rejects_trailing_hyphen() {
    let tmp = TempDir::new().unwrap();
    let result = thruput_cli::scaffold::generate(tmp.path(), "my-project-", Some("counter"));
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_consecutive_hyphens() {
    let tmp = TempDir::new().unwrap();
    let result = thruput_cli::scaffold::generate(tmp.path(), "my--project", Some("counter"));
    assert!(result.is_err());
}

/// Names that must be refused because the scaffold they would produce cannot
/// build. Each entry pairs the name with a phrase the rejection must explain.
const RESERVED_NAME_CASES: &[(&str, &str)] = &[
    // Rust sysroot crates: the self dev-dependency puts the package name in
    // the extern prelude of every test target, shadowing these.
    ("test", "built-in"),
    ("std", "built-in"),
    ("core", "built-in"),
    ("alloc", "built-in"),
    ("proc-macro", "built-in"),
    // Rust keywords: `use <name>::..` and `extern crate <name>;` will not parse.
    ("crate", "keyword"),
    ("self", "keyword"),
    ("super", "keyword"),
    ("fn", "keyword"),
    ("async", "keyword"),
    ("await", "keyword"),
    ("dyn", "keyword"),
    ("try", "keyword"),
    ("gen", "keyword"),
    ("match", "keyword"),
    ("box", "keyword"),
    ("yield", "keyword"),
    // The scaffold's own dependencies: a duplicate extern name.
    ("sdk", "dependency"),
    ("tokio", "dependency"),
    ("groundhog-json-rpc-client", "dependency"),
    // Cargo build directories: the RV32 bin target name would collide with a
    // directory under `target/<triple>/release`, which is where `thruput
    // build` looks for the ELF.
    ("build", "build directory"),
    ("deps", "build directory"),
    ("examples", "build directory"),
    ("incremental", "build directory"),
    // Reserved Windows device names: the project directory cannot be created.
    ("con", "Windows"),
    ("prn", "Windows"),
    ("aux", "Windows"),
    ("nul", "Windows"),
    ("com1", "Windows"),
    ("com9", "Windows"),
    ("lpt1", "Windows"),
    ("lpt9", "Windows"),
];

#[test]
fn scaffold_rejects_reserved_names() {
    for (name, expected_reason) in RESERVED_NAME_CASES {
        let tmp = TempDir::new().unwrap();
        let err = thruput_cli::scaffold::generate(tmp.path(), name, Some("counter"))
            .expect_err(&format!("`{name}` must be rejected"));
        let message = err.to_string();
        assert!(
            message.contains(expected_reason),
            "`{name}` rejected without explaining `{expected_reason}`: {message}"
        );
    }
}

#[test]
fn scaffold_accepts_names_that_merely_resemble_reserved_ones() {
    // Weak keywords are contextual and work fine as crate names, and a
    // reserved word as one hyphenated segment is not a collision.
    for name in [
        "union",
        "macro-rules",
        "my-test",
        "test-helper",
        "gen-counter",
        "sdk2",
        "build-tools",
        "console",
        "com10",
    ] {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("project");
        fs::create_dir(&dir).unwrap();
        thruput_cli::scaffold::generate(&dir, name, Some("counter"))
            .unwrap_or_else(|e| panic!("`{name}` must be accepted: {e}"));
    }
}

#[test]
fn every_scaffold_dependency_name_is_reserved() {
    // A dependency added to the template without being reserved would make
    // `thruput new <that-name>` produce a project that cannot build.
    let manifest = include_str!("../templates/Cargo.toml.tmpl");
    let mut in_dependencies = false;
    for line in manifest.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            // `[target.'cfg(..)'.dependencies.groundhog-json-rpc-client]` names
            // the dependency in the header itself.
            in_dependencies = false;
            if let Some((prefix, name)) = header.rsplit_once(".dependencies.") {
                let _ = prefix;
                assert_reserved_dependency(name);
                continue;
            }
            in_dependencies = header.ends_with("dependencies");
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name == "{{PROJECT_NAME}}" {
            continue;
        }
        assert_reserved_dependency(name);
    }
}

fn assert_reserved_dependency(name: &str) {
    let tmp = TempDir::new().unwrap();
    assert!(
        thruput_cli::scaffold::generate(tmp.path(), name, Some("counter")).is_err(),
        "template dependency `{name}` is not a reserved project name"
    );
}
