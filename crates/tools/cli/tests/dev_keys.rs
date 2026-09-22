use groundhog_cli::dev_keys::*;

#[test]
fn dev_key_detection() {
    assert!(is_dev_key("dev-0"));
    assert!(is_dev_key("dev-99"));
    assert!(!is_dev_key("default"));
    assert!(!is_dev_key("dev-"));
    assert!(!is_dev_key("dev-abc"));
    assert!(!is_dev_key("developer-0"));
}

#[test]
fn dev_key_derivation_is_deterministic() {
    let signer_a = load_dev_signer("dev-0").unwrap();
    let signer_b = load_dev_signer("dev-0").unwrap();
    assert_eq!(
        signer_a.verifying_key_bytes(),
        signer_b.verifying_key_bytes(),
    );
}

#[test]
fn dev_keys_are_distinct() {
    let signer_0 = load_dev_signer("dev-0").unwrap();
    let signer_1 = load_dev_signer("dev-1").unwrap();
    assert_ne!(
        signer_0.verifying_key_bytes(),
        signer_1.verifying_key_bytes(),
    );
}
