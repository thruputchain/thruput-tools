use super::{fixtures::*, generate_file};
use std::{fs, path::Path};

#[test]
fn generated_bindings_match_snapshots() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/codegen");
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("binding.ts");
    let update = std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1");
    if update {
        fs::create_dir_all(&fixtures).unwrap();
    }

    for (name, spec) in [
        ("erc20", erc20_spec()),
        ("recursive_type", recursive_type_spec()),
        ("typed_contract", typed_contract_spec()),
        ("shadowed_names", shadowed_names_spec()),
        ("namespace_merges", namespace_merges_spec()),
        ("escaped_names", escaped_names_spec()),
    ] {
        let source = fixtures.join(format!("{name}.spec.xdr"));
        let expected = fixtures.join(format!("{name}.ts"));
        let encoded = xdrs::txs::encode(&spec).unwrap();
        if update {
            fs::write(&source, encoded).unwrap();
        } else {
            assert_eq!(fs::read(&source).unwrap(), encoded, "{name} spec changed");
        }

        let binding = generate_file(&source, &output).unwrap();
        if update {
            fs::write(expected, binding).unwrap();
        } else {
            assert_eq!(
                fs::read_to_string(expected).unwrap(),
                binding,
                "{name} binding changed"
            );
        }
    }
}
