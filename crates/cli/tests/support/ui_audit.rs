//! Real command output review using isolated state and the existing RPC fixtures.
use super::*;

fn captured(mut command: Command, width: u16) -> (std::process::Output, String) {
    if !command.get_envs().any(|(name, _)| name == "NO_COLOR") {
        command.env_remove("NO_COLOR");
    }
    command
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8");
    let (child, terminal) = spawn_with_terminal_size(command, width, 100);
    let reader = terminal_output_reader(&terminal);
    let output = child.wait_with_output().unwrap();
    drop(terminal);
    let raw = reader.join().unwrap();
    assert!(!raw.contains("\x1b[2J"));
    (output, raw)
}

fn rendered(command: Command, width: u16) -> (ExitStatus, String) {
    let (output, raw) = captured(command, width);
    assert!(
        output.stdout.is_empty(),
        "human transaction data belongs on stderr"
    );
    (output.status, strip_terminal_styles(&raw))
}

#[test]
fn top_level_and_nested_commands_share_argument_error_policy() {
    for args in [vec!["balance"], vec!["wallet", "ls"]] {
        let help = thruput().args(&args).arg("--help").output().unwrap();
        assert!(help.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&help.stdout).contains("Usage:"),
            "{args:?}"
        );
        let mut command = thruput();
        command.args(&args).arg("--ui-audit-unknown-option");
        let (output, raw) = captured(command, 80);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {raw}");
        assert!(output.stdout.is_empty());
        assert!(raw.contains("38;2;248;113;113"), "{args:?}: {raw:?}");
        for flag in ["--no-ui", "--non-interactive"] {
            let mut plain = thruput();
            plain.args(&args).args([flag, "--ui-audit-unknown-option"]);
            let (output, raw) = captured(plain, 40);
            assert_eq!(output.status.code(), Some(2));
            assert!(!raw.contains('\x1b'), "{args:?}: {raw:?}");
        }
    }
}

#[test]
fn parser_errors_respect_no_color_on_a_terminal() {
    let mut command = thruput();
    command
        .args(["balance", "--ui-audit-unknown-option"])
        .env("NO_COLOR", "1");
    let (output, raw) = captured(command, 80);
    assert_eq!(output.status.code(), Some(2));
    assert!(!raw.contains('\x1b'));
}

#[test]
fn toolchain_cache_actions_render_results_and_which_stays_a_raw_path() {
    let temp = TempDir::new().unwrap();
    let cache = temp.path().join("cache");
    let config = temp.path().join("config");
    let catalog = groundhog_toolchain::Catalog::embedded().unwrap();
    let source = temp.path().join("hog-devnode-source");
    std::fs::write(
        &source,
        format!(
            "#!/bin/sh\nprintf 'hog-devnode {}\\n'\n",
            catalog.default_version()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    let binary = install_fake_managed_toolchain(&cache, &source);
    for args in [
        vec!["toolchain", "install", "--offline"],
        vec!["toolchain", "use", catalog.default_version()],
    ] {
        let mut command = thruput();
        command
            .args(args)
            .env("XDG_CACHE_HOME", &cache)
            .env("XDG_CONFIG_HOME", &config);
        let (status, text) = rendered(command, 60);
        assert!(status.success(), "{text}");
        assert!(
            text.contains('╭') && text.contains("hog-devnode toolchain"),
            "{text}"
        );
    }
    let path = thruput()
        .args(["toolchain", "which", "--quiet"])
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert!(path.status.success() && path.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&path.stdout).trim(),
        binary.to_str().unwrap()
    );
}

#[test]
fn missing_local_endpoint_explains_the_selected_profile_and_node_startup() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config");
    std::fs::write(temp.path().join("thruput.toml"), "default_profile = \"sandbox\"\n[profile.sandbox]\nnetwork = \"local\"\n[profile.devnet]\nnetwork = \"devnet\"\n").unwrap();
    let address = format!("0x{}", "11".repeat(32));
    for args in [
        vec!["balance", &address],
        vec!["tx", "list", &address],
        vec!["view", &address, "0x0123456789abcdef"],
        vec!["storage", &address, &address],
    ] {
        let output = thruput()
            .args(args)
            .arg("--json")
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", &config)
            .output()
            .unwrap();
        assert!(!output.status.success() && output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "missing_rpc_url");
        assert_eq!(error["error"]["details"]["profile"], "sandbox");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no local node endpoint")
        );
        assert!(
            error["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("thruput node")
        );
        assert!(
            error["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("--profile devnet")
        );
    }
    let remote = thruput()
        .args(["balance", &address, "--profile", "devnet", "--json"])
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    let error: serde_json::Value = serde_json::from_slice(&remote.stderr).unwrap();
    assert_eq!(error["error"]["code"], "missing_rpc_url");
    assert!(
        !error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("local node")
    );
}

#[test]
fn local_node_registration_and_rpc_overrides_are_used_before_missing_endpoint_errors() {
    for registered in [false, true] {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config");
        std::fs::write(
            temp.path().join("thruput.toml"),
            "default_profile = \"local\"\n[profile.local]\nnetwork = \"local\"\n",
        )
        .unwrap();
        let address = format!("0x{}", "11".repeat(32));
        let (rpc, server) = spawn_balance_rpc(&address, "0");
        let mut command = thruput();
        command
            .args(["balance", &address, "--json"])
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", &config);
        if registered {
            write_global_config(
                &config,
                &format!(
                    "[local_runtime_state]\nnode_id = \"{rpc}\"\nrpc_url = \"{rpc}\"\nruntime_manifest = \"{}\"\n",
                    temp.path().join("runtime.json").display()
                ),
            );
        } else {
            command.args(["--rpc-url", &rpc]);
        }
        let output = command.output().unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["data"]["rpcUrl"], rpc);
    }
}

#[test]
fn version_warning_uses_shared_presentation_without_framing_the_version() {
    let temp = TempDir::new().unwrap();
    let manifest = temp.path().join("bundle.json");
    write_bundle_manifest(&manifest, env!("CARGO_PKG_VERSION"), "not-this-build");
    let mut command = thruput();
    command
        .arg("--version")
        .env("THRUPUT_BUNDLE_MANIFEST", manifest);
    let (output, raw) = captured(command, 80);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("thruput "));
    assert!(!output.stdout.contains(&0x1b));
    assert!(raw.contains("38;2;251;191;36"));
    assert!(strip_terminal_styles(&raw).contains("Warning"));
}

#[test]
fn framework_payloads_stay_raw_and_all_completion_shells_match_json() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let plain = thruput().args(["completion", shell]).output().unwrap();
        let json = thruput()
            .args(["completion", shell, "--json"])
            .output()
            .unwrap();
        assert!(plain.status.success() && json.status.success());
        assert!(plain.stderr.is_empty() && json.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(
            plain.stdout,
            value["data"]["script"].as_str().unwrap().as_bytes()
        );
        assert!(!plain.stdout.contains(&0x1b));
    }
    let temp = TempDir::new().unwrap();
    let installed = temp.path().join(".agents/skills/groundhog");
    std::fs::create_dir_all(&installed).unwrap();
    let entry = "---\nname: thruput\n---\n\nInstalled guidance\n";
    std::fs::write(installed.join("SKILL.md"), entry).unwrap();
    let skill = thruput()
        .current_dir(temp.path())
        .args(["skill", "show"])
        .output()
        .unwrap();
    assert!(skill.status.success() && skill.stderr.is_empty());
    assert_eq!(skill.stdout, entry.as_bytes());
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
            let mut command = thruput();
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
    std::fs::write(temp.path().join("thruput.toml"), "default_profile = \"devnet\"\n[profile.devnet]\nrpc_url = \"http://127.0.0.1:9\"\nfaucet_url = \"http://127.0.0.1:9\"\n").unwrap();
    let args = [
        "airdrop",
        "0x1111111111111111111111111111111111111111111111111111111111111111",
        "--dry-run",
    ];
    let mut command = thruput();
    command.args(args).current_dir(temp.path());
    let (status, text) = rendered(command, 60);
    assert!(status.success(), "{text}");
    assert!(text.contains("Dry run (not submitted)"), "{text}");
    assert!(text.contains("submitted: no"), "{text}");
}

#[test]
fn services_report_ready_without_exiting_and_stop_cleanly() {
    for node in [false, true] {
        let temp = TempDir::new().unwrap();
        let mut command = thruput();
        let needle = if node {
            command
                .args(["node", "--port", "0", "--hog-devnode-binary"])
                .arg(fake_hogd_dev_binary())
                .env("THRUPUT_FAKE_HOGD_DEV_MODE", "ready-with-indexer");
            "account manifest"
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

#[test]
fn transfer_cards_distinguish_commitment_rejection_and_uncertainty() {
    for (poll, success, label) in [
        (
            TransferPollMock::Receipt {
                status: serde_json::json!({"result":"ok"}),
                events: serde_json::json!([]),
                return_data: None,
                abort_data: None,
                mismatched_hash: false,
            },
            true,
            "Transferred 1.5 HOG",
        ),
        (
            TransferPollMock::Status(serde_json::json!({"status":"invalid", "reason":"denied"})),
            false,
            "Transfer rejected",
        ),
        (
            TransferPollMock::Status(serde_json::json!({"status":"unknown"})),
            false,
            "Outcome unconfirmed",
        ),
    ] {
        let temp = TempDir::new().unwrap();
        let sender = advertised_dev_wallet_address(0);
        let manifest = write_runtime_manifest(temp.path(), 0, &sender, None);
        let (rpc, server, hashes) = spawn_transfer_rpc(
            TransferRpcMock {
                submit: TransferSubmitMock::MatchingHash,
                poll: Some(poll),
            },
            sender,
        );
        std::fs::write(temp.path().join("thruput.toml"), format!("default_profile = \"default\"\n[profile.default]\nrpc_url = \"{rpc}\"\ndefault_key = \"dev-0\"\nruntime_manifest = \"{}\"\n", manifest.file_name().unwrap().to_str().unwrap())).unwrap();
        let mut command = thruput();
        command
            .args([
                "transfer",
                &format!("0x{}", "ab".repeat(32)),
                "1.5",
                "--force",
            ])
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("fixture-config"))
            .env("THRUPUT_TEST_RECEIPT_POLL_ATTEMPTS", "1")
            .env("THRUPUT_TEST_RECEIPT_POLL_INTERVAL_MS", "0");
        let (status, text) = rendered(command, 80);
        assert_eq!(status.success(), success, "{text}");
        assert!(text.contains(label) && text.contains('╭'), "{text}");
        let compact: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│')
            .collect();
        assert!(compact.contains(&hashes.recv().unwrap()), "{text}");
        server.join().unwrap();
    }
}

#[test]
fn native_wallet_creation_reports_offline_save_and_dry_run() {
    for dry_run in [false, true] {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config");
        std::fs::write(temp.path().join("thruput.toml"), "default_profile = \"default\"\n[profile.default]\nrpc_url = \"http://127.0.0.1:9\"\nfaucet_url = \"http://127.0.0.1:9\"\n").unwrap();
        generate_key_public_key(&config, "owner");
        let mut command = thruput();
        command
            .args([
                "wallet", "create", "--key", "owner", "--name", "trading", "--force",
            ])
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", config);
        if dry_run {
            command.arg("--dry-run");
        }
        let (status, text) = rendered(command, 80);
        assert!(status.success(), "{text}");
        assert!(
            text.contains('╭') && text.contains("Native wallet"),
            "{text}"
        );
        assert!(
            text.contains(if dry_run {
                "not saved"
            } else {
                "No funding was requested"
            }),
            "{text}"
        );
        let saved: toml::Value =
            toml::from_str(&std::fs::read_to_string(temp.path().join("thruput.toml")).unwrap())
                .unwrap();
        assert_eq!(
            saved["profile"]["default"]
                .get("wallets")
                .and_then(|wallets| wallets.get("trading"))
                .is_some(),
            !dry_run
        );
    }
}
