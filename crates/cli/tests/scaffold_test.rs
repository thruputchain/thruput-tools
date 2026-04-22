use std::fs;
use tempfile::TempDir;

#[test]
fn scaffold_counter_creates_expected_files() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-counter");
    fs::create_dir(&dir).unwrap();

    groundhog_cli::scaffold::generate(&dir, "my-counter", "counter").unwrap();

    assert!(dir.join("Cargo.toml").exists());
    assert!(dir.join("rust-toolchain.toml").exists());
    assert!(dir.join(".cargo/config.toml").exists());
    assert!(dir.join(".cargo/contract.toml").exists());
    assert!(dir.join("src/lib.rs").exists());
    assert!(dir.join("src/implementation.rs").exists());
    assert!(dir.join("tests/test.rs").exists());
    assert!(dir.join("tests/test_wasm.rs").exists());

    let cargo = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert!(cargo.contains(r#"name = "my-counter""#));
    assert!(cargo.contains("registry = \"groundhog\""));
    assert!(cargo.contains(r#"sdk = { version = "0.11.3""#));

    let lib = fs::read_to_string(dir.join("src/lib.rs")).unwrap();
    assert!(lib.contains("mod implementation"));

    let imp = fs::read_to_string(dir.join("src/implementation.rs")).unwrap();
    assert!(imp.contains("pub struct Counter"));
    assert!(imp.contains("#[contract_exception]"));
    assert!(!imp.contains("#[contract_error]"));

    let test = fs::read_to_string(dir.join("tests/test.rs")).unwrap();
    assert!(test.contains("register_mock_contract"));
    assert!(test.contains("CounterClient"));
    assert!(test.contains("fn setup()"));
    assert!(test.contains("test_contract.as_hash().clone()"));
    assert!(test.contains("contract_exec(env, move ||"));
    assert!(test.contains("MY_COUNTER_NONCE"));

    let test_wasm = fs::read_to_string(dir.join("tests/test_wasm.rs")).unwrap();
    assert!(test_wasm.contains("load_wasm_contract"));
    assert!(test_wasm.contains("my_counter"));
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

    assert!(dir.join("tests/test_wasm.rs").exists());
    let test_wasm = fs::read_to_string(dir.join("tests/test_wasm.rs")).unwrap();
    assert!(test_wasm.contains("load_wasm_contract"));
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
