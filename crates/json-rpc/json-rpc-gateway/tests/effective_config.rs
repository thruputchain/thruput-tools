use std::process::Command;

#[test]
fn configuration_dump_is_only_json_with_default_or_verbose_logging() {
    for filter in [None, Some("trace")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hog-gateway"));
        command.args([
            "--dump-effective-config",
            "--network-hash",
            &"12".repeat(32),
            "--listen",
            "not-a-socket-address",
            "--indexer-pool-size",
            "32",
            "--trace-spans",
        ]);
        command.env_remove("RUST_LOG");
        if let Some(filter) = filter {
            command.env("RUST_LOG", filter);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let config: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(config["indexer_pool_size"], 32);
        assert_eq!(config["listen_address"], "not-a-socket-address");
    }
}
