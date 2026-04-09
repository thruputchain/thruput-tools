use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn find_project_root_from_src_subdir() {
    let tmp = TempDir::new().unwrap();
    groundhog_cli::scaffold::generate(tmp.path(), "test-proj", "counter").unwrap();

    let src_dir = tmp.path().join("src");
    let root = groundhog_cli::build::find_project_root(&src_dir).unwrap();
    assert_eq!(root, tmp.path());
}

#[test]
fn find_project_root_fails_outside_project() {
    let tmp = TempDir::new().unwrap();
    let result = groundhog_cli::build::find_project_root(tmp.path());
    assert!(result.is_err());
}

#[test]
fn wasm_output_path_derives_from_name() {
    let path = groundhog_cli::build::wasm_output_path("my-counter");
    assert_eq!(
        path,
        PathBuf::from("target/wasm32-unknown-unknown/release/my_counter.wasm")
    );
}
