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
            "cargo:rerun-if-env-changed=GROUNDHOG_INTEGRATION_TEST",
            "cargo:rustc-cfg=groundhog_integration_runner",
            "cargo:rustc-check-cfg=cfg(groundhog_integration_runner)",
        ],
    );

    let integration = fs::read_to_string(dir.join("tests/integration.rs")).unwrap();
    assert_contains_all(
        &integration,
        &[
            "groundhog test --integration",
            "#[ignore =",
            "GROUNDHOG_RPC_URL",
            "GROUNDHOG_INTEGRATION_TEST",
            "not(groundhog_integration_runner)",
            "GROUNDHOG_CONTRACT_ADDRESS",
            "GROUNDHOG_RUNTIME_MANIFEST",
            "HttpClientBuilder",
            ".chain_tip()",
        ],
    );
}

#[test]
fn scaffold_counter_creates_expected_files() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-counter");
    fs::create_dir(&dir).unwrap();

    groundhog_cli::scaffold::generate(&dir, "my-counter", "counter").unwrap();

    assert!(dir.join("Cargo.toml").exists());
    assert!(dir.join("build.rs").exists());
    assert!(dir.join(".gitignore").exists());
    assert!(dir.join("groundhog.toml").exists());
    assert!(dir.join("rust-toolchain.toml").exists());
    assert!(dir.join(".cargo/config.toml").exists());
    assert!(dir.join(".cargo/contract.toml").exists());
    assert!(dir.join("src/lib.rs").exists());
    assert!(dir.join("src/implementation.rs").exists());
    assert!(dir.join("src/main.rs").exists());
    assert!(dir.join("tests/integration.rs").exists());
    assert!(dir.join("tests/test.rs").exists());
    assert!(dir.join("tests/test_rv32.rs").exists());
    assert!(dir.join("tests/test_wasm.rs").exists());

    let cargo = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert!(cargo.contains(r#"name = "my-counter""#));
    assert!(cargo.contains("registry = \"groundhog\""));
    assert!(!cargo.contains("sdk/spec"));
    assert!(cargo.contains(r#"sdk = { version = "0.17.7", registry = "groundhog" }"#));
    assert!(cargo.contains("groundhog-integration-test = ["));
    assert!(cargo.contains(r#"groundhog-json-rpc-client = { version = "0.6""#));
    assert!(cargo.contains("optional = true"));
    assert!(cargo.contains(r#"tokio = { version = "1", features = ["macros", "rt-multi-thread"]"#));
    assert!(!cargo.contains(r#"features = ["storage"]"#));

    assert_integration_scaffold(&dir);
    // RV32 bin wrapper target: the bin name must be the snake-case package
    // name because `groundhog build --target rv32` looks for the ELF at
    // `release/<snake_name>`.
    assert!(cargo.contains("[[bin]]"));
    assert!(cargo.contains(r#"name = "my_counter""#));
    assert!(cargo.contains(r#"rv32-entrypoints = ["entrypoints"]"#));
    assert!(cargo.contains(r#"required-features = ["rv32-entrypoints"]"#));
    // The host test harness must not try to link the no_std bin when the
    // entrypoints feature is enabled.
    assert!(cargo.contains("test = false"));
    assert!(cargo.contains("bench = false"));

    let gitignore = fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert!(gitignore.contains("/target/"));
    assert!(gitignore.contains("/.groundhog/"));

    let groundhog = fs::read_to_string(dir.join("groundhog.toml")).unwrap();
    assert!(groundhog.contains("[profile.local]"));
    assert!(groundhog.contains("network = \"local\""));
    assert!(groundhog.contains("[profile.local.contracts]"));
    assert!(groundhog.contains("# [profile.local.wallets.alice]"));
    assert!(groundhog.contains("[profile.devnet]"));
    assert!(groundhog.contains("network = \"devnet\""));
    assert!(groundhog.contains("[profile.devnet.contracts]"));
    assert!(groundhog.contains("# [profile.devnet.wallets.alice]"));
    assert!(groundhog.contains("# owner_public_key ="));
    assert!(groundhog.contains("# my_counter ="));
    assert!(groundhog.contains("default_profile = \"local\""));
    assert!(!groundhog.contains("rpc_url"));
    assert!(!groundhog.contains("faucet_url"));
    assert!(!groundhog.contains("runtime_manifest"));
    assert!(!groundhog.contains("default_key"));

    let main = fs::read_to_string(dir.join("src/main.rs")).unwrap();
    assert!(main.contains("#![no_std]"));
    assert!(main.contains("#![no_main]"));
    assert!(main.contains("extern crate my_counter;"));

    let config = fs::read_to_string(dir.join(".cargo/config.toml")).unwrap();
    assert!(config.contains("[target.riscv32im-unknown-none-elf]"));
    assert!(config.contains("target-feature=+unaligned-scalar-mem"));
    assert!(config.contains("link-arg=-Tgroundhog-rv32.ld"));
    assert!(config.contains("trap-unreachable=false"));

    let toolchain = fs::read_to_string(dir.join("rust-toolchain.toml")).unwrap();
    assert!(toolchain.contains("riscv32im-unknown-none-elf"));

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
    assert!(test.contains("test_contract.as_hash().clone()"));
    assert!(test.contains("env.transact(|tx|"));
    assert!(test.contains("tx.call(&caller).run(move ||"));
    assert!(test.contains("MY_COUNTER_NONCE"));

    let test_wasm = fs::read_to_string(dir.join("tests/test_wasm.rs")).unwrap();
    assert!(test_wasm.contains("load_wasm_contract"));
    assert!(test_wasm.contains("CARGO_TARGET_DIR"));
    assert!(test_wasm.contains("cargo_wasm_artifact(\"my_counter.wasm\")"));
    assert!(test_wasm.contains("my_counter"));

    let test_rv32 = fs::read_to_string(dir.join("tests/test_rv32.rs")).unwrap();
    assert!(test_rv32.contains("load_rv32_contract"));
    assert!(test_rv32.contains("CARGO_TARGET_DIR"));
    assert!(test_rv32.contains("groundhog_artifact(\"my_counter.rv32a\")"));
    assert!(test_rv32.contains("requires RV32 build — run via `groundhog test`"));
    assert!(test_rv32.contains("my_counter"));
}

#[test]
fn scaffold_stub_has_empty_contract() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-stub");
    fs::create_dir(&dir).unwrap();

    groundhog_cli::scaffold::generate(&dir, "my-stub", "stub").unwrap();

    let lib = fs::read_to_string(dir.join("src/lib.rs")).unwrap();
    assert!(lib.contains("mod implementation"));
    assert!(!lib.contains("increment"));

    let imp = fs::read_to_string(dir.join("src/implementation.rs")).unwrap();
    assert!(imp.contains("pub struct Contract"));
    assert!(!imp.contains("increment"));
    assert!(imp.contains("#[contract_exception]"));

    assert!(dir.join("tests/test_rv32.rs").exists());
    assert!(dir.join("tests/test_wasm.rs").exists());
    assert!(dir.join("tests/integration.rs").exists());
    let test_rv32 = fs::read_to_string(dir.join("tests/test_rv32.rs")).unwrap();
    assert!(test_rv32.contains("load_rv32_contract"));
    assert!(test_rv32.contains("CARGO_TARGET_DIR"));
    assert!(test_rv32.contains("groundhog_artifact(\"my_stub.rv32a\")"));
    assert!(test_rv32.contains("requires RV32 build — run via `groundhog test`"));

    let test_wasm = fs::read_to_string(dir.join("tests/test_wasm.rs")).unwrap();
    assert!(test_wasm.contains("load_wasm_contract"));
    assert!(test_wasm.contains("CARGO_TARGET_DIR"));
    assert!(test_wasm.contains("cargo_wasm_artifact(\"my_stub.wasm\")"));

    let main = fs::read_to_string(dir.join("src/main.rs")).unwrap();
    assert!(main.contains("extern crate my_stub;"));
}

#[test]
fn scaffold_unknown_template_errors() {
    let tmp = TempDir::new().unwrap();

    let result = groundhog_cli::scaffold::generate(tmp.path(), "test", "nope");
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_empty_name() {
    let tmp = TempDir::new().unwrap();
    let result = groundhog_cli::scaffold::generate(tmp.path(), "", "counter");
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_uppercase_name() {
    let tmp = TempDir::new().unwrap();
    let result = groundhog_cli::scaffold::generate(tmp.path(), "MyProject", "counter");
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_name_starting_with_digit() {
    let tmp = TempDir::new().unwrap();
    let result = groundhog_cli::scaffold::generate(tmp.path(), "1foo", "counter");
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_name_with_underscore() {
    let tmp = TempDir::new().unwrap();
    let result = groundhog_cli::scaffold::generate(tmp.path(), "my_project", "counter");
    assert!(result.is_err());
}

#[test]
fn scaffold_accepts_valid_name_with_hyphens() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-project-2");
    fs::create_dir(&dir).unwrap();
    let result = groundhog_cli::scaffold::generate(&dir, "my-project-2", "counter");
    assert!(result.is_ok());
}

#[test]
fn scaffold_rejects_trailing_hyphen() {
    let tmp = TempDir::new().unwrap();
    let result = groundhog_cli::scaffold::generate(tmp.path(), "my-project-", "counter");
    assert!(result.is_err());
}

#[test]
fn scaffold_rejects_consecutive_hyphens() {
    let tmp = TempDir::new().unwrap();
    let result = groundhog_cli::scaffold::generate(tmp.path(), "my--project", "counter");
    assert!(result.is_err());
}
