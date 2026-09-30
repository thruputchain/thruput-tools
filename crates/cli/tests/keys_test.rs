use groundhog_cli::Print;
use groundhog_signing::{DEFAULT_PUBKEY_ACCOUNT_NONCE, PubkeyAccount};
use serial_test::serial;
use std::env;
use std::fs;
use tempfile::TempDir;

fn quiet() -> Print {
    Print::quiet()
}

#[test]
#[serial]
fn generate_use_and_remove_key_updates_default_selection() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    groundhog_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();
    let default_key = groundhog_cli::config::load_default_key().unwrap();
    assert_eq!(default_key.as_deref(), Some("alice"));

    groundhog_cli::keys::generate(&Some("bob".into()), false, "keys.generate", &quiet()).unwrap();
    groundhog_cli::keys::use_key("bob", "keys.use", &quiet()).unwrap();

    let default_key = groundhog_cli::config::load_default_key().unwrap();
    assert_eq!(default_key.as_deref(), Some("bob"));

    groundhog_cli::keys::remove("bob", true, "keys.rm", &quiet()).unwrap();
    let default_key = groundhog_cli::config::load_default_key().unwrap();
    assert!(default_key.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn use_nonexistent_key_fails() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let result = groundhog_cli::keys::use_key("nonexistent", "keys.use", &quiet());
    assert!(result.is_err());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn remove_nonexistent_key_fails() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let result = groundhog_cli::keys::remove("nonexistent", true, "keys.rm", &quiet());
    assert!(matches!(
        result,
        Err(groundhog_cli::CliError::KeyNotFound(name)) if name == "nonexistent"
    ));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn key_entry_points_do_not_treat_quiet_or_json_as_authorization() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    groundhog_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();

    for print in [Print::quiet(), Print::new(false, true)] {
        let export =
            groundhog_cli::keys::export("alice", false, "keys.export", &print).unwrap_err();
        assert!(matches!(
            export,
            groundhog_cli::CliError::ConfirmationRequired { .. }
        ));
        let remove = groundhog_cli::keys::remove("alice", false, "keys.rm", &print).unwrap_err();
        assert!(matches!(
            remove,
            groundhog_cli::CliError::ConfirmationRequired { .. }
        ));
    }

    assert_eq!(groundhog_cli::keys::load_key_summaries().unwrap().len(), 1);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn generate_duplicate_key_fails() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    groundhog_cli::keys::generate(&Some("dup".into()), false, "keys.generate", &quiet()).unwrap();

    let result =
        groundhog_cli::keys::generate(&Some("dup".into()), false, "keys.generate", &quiet());
    assert!(result.is_err());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn use_invalid_key_does_not_update_default_selection() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let keys_dir = groundhog_cli::config::keys_dir().unwrap();
    fs::create_dir_all(&keys_dir).unwrap();
    fs::write(
        keys_dir.join("broken.json"),
        r#"{"version":1,"scheme":"ed25519","keypair_hex":"zz"}"#,
    )
    .unwrap();

    let result = groundhog_cli::keys::use_key("broken", "keys.use", &quiet());
    assert!(result.is_err());

    let default_key = groundhog_cli::config::load_default_key().unwrap();
    assert!(default_key.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn generated_key_address_matches_shared_signing_crate() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    groundhog_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();

    let key_path = groundhog_cli::config::keys_dir()
        .unwrap()
        .join("alice.json");
    let keyfile: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(key_path).unwrap()).unwrap();
    let public_key_hex = keyfile
        .get("public_key_hex")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let public_key_bytes = hex::decode(public_key_hex).unwrap();
    let public_key: [u8; 32] = public_key_bytes.try_into().unwrap();

    let expected =
        PubkeyAccount::from_public_key_bytes(public_key, DEFAULT_PUBKEY_ACCOUNT_NONCE).address();
    let keys = groundhog_cli::keys::load_key_summaries().unwrap();

    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0].address,
        format!("0x{}", hex::encode(expected.as_ref()))
    );

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}
