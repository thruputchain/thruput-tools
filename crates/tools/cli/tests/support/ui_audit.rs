//! Real command output review using isolated state and the existing RPC fixtures.
use super::*;

fn rendered(mut command: Command, width: u16) -> (ExitStatus, String) {
    command
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env_remove("NO_COLOR");
    let (child, terminal) = spawn_with_terminal_size(command, width, 100);
    let reader = terminal_output_reader(&terminal);
    let output = child.wait_with_output().unwrap();
    drop(terminal);
    assert!(
        output.stdout.is_empty(),
        "human transaction data belongs on stderr"
    );
    let raw = reader.join().unwrap();
    assert!(!raw.contains("\x1b[2J"));
    (output.status, strip_terminal_styles(&raw))
}

#[test]
fn live_call_and_deploy_results_cover_success_and_execution_failure() {
    for deploy in [false, true] {
        for fail in [false, true] {
            let temp = TempDir::new().unwrap();
            let config = temp.path().join("config");
            let project = temp.path().join("project");
            std::fs::create_dir(&project).unwrap();
            let receipt = if deploy {
                if fail {
                    sample_insufficient_funding_deploy_receipt(42)
                } else {
                    sample_deploy_receipt(42)
                }
            } else if fail {
                sample_deterministic_error_call_receipt(42)
            } else {
                sample_call_receipt(42)
            };
            let (rpc, server) =
                spawn_transaction_rpc(42, receipt, DevDetailRpcResponse::MethodNotFound);
            write_project_config_with_alice_wallet(&project, &rpc, &config);
            let mut command = groundhog();
            if deploy {
                command
                    .args(["deploy", "--artifact"])
                    .arg(write_opaque_rv32_artifact(temp.path()));
            } else {
                command.args([
                    "call",
                    &format!("0x{}", "11".repeat(32)),
                    "0x0123456789abcdef",
                    "0x0102",
                ]);
            }
            command
                .arg("--force")
                .current_dir(&project)
                .env("XDG_CONFIG_HOME", &config);
            let (status, text) = rendered(command, 80);
            server.join().unwrap();
            assert_eq!(status.success(), !fail, "{text}");
            assert!(
                text.contains('╭') && text.contains("prepared transaction"),
                "{text}"
            );
            assert!(text.contains("height"), "{text}");
            if fail {
                assert!(text.contains("error:"), "{text}");
            }
        }
    }
}

#[test]
fn remote_funding_dry_runs_are_explicit_and_do_not_contact_services() {
    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join("groundhog.toml"), "default_profile = \"devnet\"\n[profile.devnet]\nrpc_url = \"http://127.0.0.1:9\"\nfaucet_url = \"http://127.0.0.1:9\"\n").unwrap();
    for args in [
        vec!["wallet", "create", "--key", "alice", "--dry-run"],
        vec![
            "airdrop",
            "0x1111111111111111111111111111111111111111111111111111111111111111",
            "--dry-run",
        ],
    ] {
        let mut command = groundhog();
        command.args(args).current_dir(temp.path());
        let (status, text) = rendered(command, 60);
        assert!(status.success(), "{text}");
        assert!(text.contains("Dry run (not submitted)"), "{text}");
        assert!(text.contains("submitted: no"), "{text}");
    }
}

#[test]
fn services_report_ready_without_exiting_and_stop_cleanly() {
    for node in [false, true] {
        let temp = TempDir::new().unwrap();
        let mut command = groundhog();
        let needle = if node {
            command
                .args(["node", "--port", "0", "--hog-devnode-binary"])
                .arg(fake_hogd_dev_binary())
                .env("GROUNDHOG_FAKE_HOGD_DEV_MODE", "ready-with-indexer");
            "dev-key manifest"
        } else {
            command.args(["docs", "--port", "0"]);
            "Ctrl-C to stop"
        };
        command
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("NO_COLOR");
        let (child, terminal) = spawn_with_terminal_size(command, 80, 100);
        let mut child = ChildGuard::new(child);
        let raw = read_until_with_timeout(terminal, needle, Duration::from_secs(15)).unwrap();
        let text = strip_terminal_styles(&raw);
        assert!(text.contains("Ready:") && text.contains('╭'), "{text}");
        assert!(child.child_mut().try_wait().unwrap().is_none());
        assert!(!raw.contains("\x1b[2J"));
        send_sigint(child.child());
        assert!(wait_child(child.into_inner(), Duration::from_secs(10)).success());
    }
}
