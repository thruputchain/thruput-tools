use groundhog_signing::{LocalSigner, strkey};
use std::io::Write;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

fn run(home: &TempDir, args: &[&str], input: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
    command
        .args(args)
        .current_dir(home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"));
    for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("GROUNDHOG_")) {
        command.env_remove(name);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        let _ = child.stdin.as_mut().unwrap().write_all(input.as_bytes());
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

#[test]
fn strkey_import_export_preserves_legacy_storage_and_public_identity() {
    let home = TempDir::new().unwrap();
    let seed = [42; 32];
    let secret = strkey::format_secret_seed(&seed);
    let public =
        strkey::format_public_key(&LocalSigner::from_secret_key(seed).verifying_key_bytes());
    let imported = run(
        &home,
        &["--json", "keys", "import", "alice"],
        Some(&format!("{}\n", secret.as_str())),
    );
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(value["command"], "keys.import");
    assert_eq!(value["data"]["key"]["public_key_strkey"], public);
    for stream in [&imported.stdout, &imported.stderr] {
        assert!(!String::from_utf8_lossy(stream).contains(secret.as_str()));
    }
    let key_path = home.path().join("groundhog/keys/alice.json");
    let bytes = std::fs::read(&key_path).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stored["version"], 1);
    assert_eq!(stored["keypair_hex"].as_str().unwrap().len(), 128);
    assert_eq!(
        &stored["keypair_hex"].as_str().unwrap()[..64],
        hex::encode(seed)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let exported = run(
        &home,
        &["keys", "export", "alice", "--format", "strkey", "--force"],
        None,
    );
    assert!(exported.status.success());
    assert_eq!(
        String::from_utf8(exported.stdout).unwrap().trim(),
        secret.as_str()
    );
    assert!(exported.stderr.is_empty());
    let json = run(
        &home,
        &[
            "--json", "keys", "export", "alice", "--format", "strkey", "--force",
        ],
        None,
    );
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(json["data"]["key"]["privateKey"], secret.as_str());
    let legacy = run(&home, &["keys", "export", "alice", "--force"], None);
    assert_eq!(
        String::from_utf8(legacy.stdout).unwrap().trim(),
        format!("0x{}", hex::encode(seed))
    );
    let denied = run(
        &home,
        &["--json", "keys", "export", "alice", "--format", "strkey"],
        None,
    );
    assert!(!denied.status.success());
    assert!(!String::from_utf8_lossy(&denied.stdout).contains(secret.as_str()));
    let duplicate = run(&home, &["keys", "import", "alice"], Some(&"ab".repeat(32)));
    assert!(!duplicate.status.success());
    assert_eq!(std::fs::read(&key_path).unwrap(), bytes);
    let listed = run(&home, &["keys", "ls"], None);
    assert!(String::from_utf8_lossy(&listed.stdout).contains(&format!("public-key={public}")));
}

#[test]
fn import_rejects_invalid_secret_types_and_reserved_names_without_echoing_input() {
    for input in [
        String::new(),
        strkey::format_public_key(&[42; 32]),
        strkey::format_contract_address(&[42; 32].into()),
        "ab".repeat(64),
        "x".repeat(1000),
    ] {
        let home = TempDir::new().unwrap();
        let out = run(&home, &["--json", "keys", "import", "alice"], Some(&input));
        assert!(!out.status.success());
        if !input.is_empty() {
            assert!(!String::from_utf8_lossy(&out.stdout).contains(&input));
            assert!(!String::from_utf8_lossy(&out.stderr).contains(&input));
        }
        assert!(!home.path().join("groundhog/keys/alice.json").exists());
    }
    let home = TempDir::new().unwrap();
    let out = run(&home, &["keys", "import", "dev-1"], Some(&"ab".repeat(32)));
    assert!(!out.status.success());
    assert!(!home.path().join("groundhog/keys/dev-1.json").exists());
    let good = run(&home, &["keys", "import", "alice"], Some(&"ab".repeat(32)));
    assert!(good.status.success());
}
