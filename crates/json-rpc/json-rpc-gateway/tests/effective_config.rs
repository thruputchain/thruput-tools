use std::process::Command;

#[test]
fn configuration_dump_is_only_json_with_default_or_verbose_logging() {
    for filter in [None, Some("trace")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hog-gateway"));
        command.args([
            "--dump-effective-config",
            "--network-hash",
            thruput_json_rpc_types::NetworkHashText::from([0x12; 32]).as_str(),
            "--listen",
            "not-a-socket-address",
            "--x23-multiplex-connections",
            "2",
            "--scheduler-lookup-pool-size",
            "8",
            "--trace-spans",
        ]);
        command.env_remove("RUST_LOG");
        // Diagnostics must work even on hosts without jemalloc profiling support.
        command.env("GROUNDHOG_HEAP_PROFILING", "1");
        command.env("MALLOC_CONF", "prof:false");
        if let Some(filter) = filter {
            command.env("RUST_LOG", filter);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let config: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(config["x23_multiplex_connections"], 2);
        assert_eq!(config["scheduler_lookup_pool_size"], 8);
        assert_eq!(config["listen_address"], "not-a-socket-address");
    }
}
