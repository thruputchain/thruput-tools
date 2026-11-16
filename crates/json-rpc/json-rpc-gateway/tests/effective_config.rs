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
        assert_eq!(config["client_admission"]["max_in_flight_calls"], 4096);
        assert_eq!(
            config["client_admission"]["max_in_flight_per_connection"],
            64
        );
        assert_eq!(config["client_admission"]["client_rate_per_sec"], 50);
        assert_eq!(config["client_admission"]["client_burst"], 200);
        assert_eq!(config["client_admission"]["max_tracked_clients"], 100_000);
        assert_eq!(
            config["client_admission"]["trusted_proxies"],
            serde_json::json!(["127.0.0.1/32", "::1/128"])
        );
    }
}

#[test]
fn client_admission_overrides_appear_in_effective_config() {
    let config_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(config_file.path(), "[client_admission]\nmax_in_flight_calls = 32\nmax_in_flight_per_connection = 4\nclient_rate_per_sec = 2\nclient_burst = 8\nmax_tracked_clients = 16\ntrusted_proxies = ['192.0.2.0/24', '2001:db8::/32']\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hog-gateway"))
        .args([
            "--dump-effective-config",
            "--network-hash",
            thruput_json_rpc_types::NetworkHashText::from([0x12; 32]).as_str(),
            "--config",
        ])
        .arg(config_file.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let config: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        config["client_admission"],
        serde_json::json!({
            "max_in_flight_calls":32, "max_in_flight_per_connection":4,
            "client_rate_per_sec":2, "client_burst":8, "max_tracked_clients":16,
            "trusted_proxies":["192.0.2.0/24", "2001:db8::/32"]
        })
    );
}
