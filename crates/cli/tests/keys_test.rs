use serial_test::serial;
use std::env;
use std::fs;
use tempfile::TempDir;
use thruput_cli::Print;
use thruput_signing::{DEFAULT_PUBKEY_ACCOUNT_NONCE, PubkeyAccount};

fn quiet() -> Print {
    Print::quiet()
}

#[test]
#[serial]
fn generate_use_and_remove_key_updates_default_selection() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    thruput_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();
    let default_key = thruput_cli::config::load_default_key().unwrap();
    assert_eq!(default_key.as_deref(), Some("alice"));

    thruput_cli::keys::generate(&Some("bob".into()), false, "keys.generate", &quiet()).unwrap();
    thruput_cli::keys::use_key("bob", "keys.use", &quiet()).unwrap();

    let default_key = thruput_cli::config::load_default_key().unwrap();
    assert_eq!(default_key.as_deref(), Some("bob"));

    thruput_cli::keys::remove("bob", true, "keys.rm", &quiet()).unwrap();
    let default_key = thruput_cli::config::load_default_key().unwrap();
    assert!(default_key.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn use_nonexistent_key_fails() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let result = thruput_cli::keys::use_key("nonexistent", "keys.use", &quiet());
    assert!(result.is_err());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn remove_nonexistent_key_fails() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let result = thruput_cli::keys::remove("nonexistent", true, "keys.rm", &quiet());
    assert!(matches!(
        result,
        Err(thruput_cli::CliError::KeyNotFound(name)) if name == "nonexistent"
    ));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn key_entry_points_do_not_treat_quiet_or_json_as_authorization() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    thruput_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();

    for print in [Print::quiet(), Print::new(false, true)] {
        let export = thruput_cli::keys::export("alice", false, "keys.export", &print).unwrap_err();
        assert!(matches!(
            export,
            thruput_cli::CliError::ConfirmationRequired { .. }
        ));
        let remove = thruput_cli::keys::remove("alice", false, "keys.rm", &print).unwrap_err();
        assert!(matches!(
            remove,
            thruput_cli::CliError::ConfirmationRequired { .. }
        ));
    }

    assert_eq!(thruput_cli::keys::load_key_summaries().unwrap().len(), 1);
    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn generate_duplicate_key_fails() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    thruput_cli::keys::generate(&Some("dup".into()), false, "keys.generate", &quiet()).unwrap();

    let result = thruput_cli::keys::generate(&Some("dup".into()), false, "keys.generate", &quiet());
    assert!(result.is_err());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn use_invalid_key_does_not_update_default_selection() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let keys_dir = thruput_cli::config::keys_dir().unwrap();
    fs::create_dir_all(&keys_dir).unwrap();
    fs::write(
        keys_dir.join("broken.json"),
        r#"{"version":1,"scheme":"ed25519","keypair_hex":"zz"}"#,
    )
    .unwrap();

    let result = thruput_cli::keys::use_key("broken", "keys.use", &quiet());
    assert!(result.is_err());

    let default_key = thruput_cli::config::load_default_key().unwrap();
    assert!(default_key.is_none());

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}

#[test]
#[serial]
fn generated_key_address_matches_shared_signing_crate() {
    let tmp = TempDir::new().unwrap();
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    thruput_cli::keys::generate(&Some("alice".into()), false, "keys.generate", &quiet()).unwrap();

    let key_path = thruput_cli::config::keys_dir().unwrap().join("alice.json");
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
    let keys = thruput_cli::keys::load_key_summaries().unwrap();

    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].address, thruput_encoding::encode(expected.as_ref()));

    unsafe { env::remove_var("XDG_CONFIG_HOME") };
}
