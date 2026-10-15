#![cfg(unix)]

use std::{
    fs,
    io::Read,
    os::fd::FromRawFd,
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;
mod support;

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        fs::write(
            root.path().join("thruput.toml"),
            "default_profile = \"local\"\n[profile.local]\n",
        )
        .unwrap();
        Self { root }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("NO_COLOR")
            .env_remove("THRUPUT_PROFILE");
        command
    }

    fn populate(&self) {
        assert!(
            self.command(&["keys", "generate", "alice"])
                .output()
                .unwrap()
                .status
                .success()
        );
        fs::write(self.root.path().join("thruput.toml"), format!(
            "default_profile = \"local\"\n[profile.local]\ndefault_wallet = \"alice\"\n[profile.local.wallets.alice]\naddress = \"0x{}\"\nowner_public_key = \"0x{}\"\n[profile.local.wallets.legacy]\naddress = \"0x{}\"\n",
            "ab".repeat(32), "cd".repeat(32), "ef".repeat(32),
        )).unwrap();
    }
}

// Static tables only need a stdout terminal; stderr remains a separate pipe.
fn terminal_output(command: Command, columns: u16) -> Output {
    terminal_capture(command, columns, false)
}

fn terminal_capture(command: Command, columns: u16, status: bool) -> Output {
    terminal_capture_with_input(command, columns, status, None)
}

fn terminal_capture_with_input(
    mut command: Command,
    columns: u16,
    status: bool,
    input: Option<&[u8]>,
) -> Output {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 100,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes both descriptors on success; all pointers are valid.
    assert_eq!(
        unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
            )
        },
        0
    );
    // SAFETY: each fresh descriptor is owned by exactly one File.
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if status {
        command.stderr(slave);
    } else {
        command.stdout(slave);
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    drop(command);
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => panic!("terminal read failed: {error}"),
            }
        }
        bytes
    });
    let mut output = child.wait_with_output().unwrap();
    if status {
        output.stderr = reader.join().unwrap();
    } else {
        output.stdout = reader.join().unwrap();
    }
    output
}

#[test]
fn key_import_shows_only_public_results_in_all_output_modes() {
    let fixture = Fixture::new();
    let seed = format!("{}\n", thruput_encoding::encode(&[0x11; 32]));
    for (name, flags) in [
        ("imported", vec![]),
        ("plain", vec!["--non-interactive"]),
        ("structured", vec!["--json"]),
    ] {
        let mut command = fixture.command(&["keys", "import", name]);
        command.args(&flags);
        let output = terminal_capture_with_input(command, 80, true, Some(seed.as_bytes()));
        assert!(output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(seed.trim()));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(seed.trim()));
        if flags.is_empty() {
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains('╭'));
        } else if flags == ["--json"] {
            assert!(output.stderr.is_empty());
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["data"]["key"]["name"], name);
        } else {
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.contains(&0x1b));
        }
    }
}

#[test]
fn config_mutations_distinguish_changed_and_unchanged_results() {
    let fixture = Fixture::new();
    fixture.populate();
    for (args, expected) in [
        (
            vec!["config", "new", "review", "--network", "local", "--project"],
            "Created profile",
        ),
        (
            vec![
                "config",
                "unset",
                "--profile",
                "review",
                "rpc-url",
                "--project",
            ],
            "No change:",
        ),
    ] {
        let output = terminal_capture(fixture.command(&args), 60, true);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let mut screen = vt100::Parser::new(100, 60, 0);
        screen.process(&output.stderr);
        assert!(
            screen.screen().contents().contains(expected),
            "{args:?}: {}",
            screen.screen().contents()
        );
    }
}

#[test]
fn codegen_file_result_and_raw_stdout_preserve_the_same_source() {
    let fixture = Fixture::new();
    let root = fixture.root.path();
    let elf = root.join("fixture.elf");
    fs::write(
        &elf,
        support::minimal_rv32_elf_with_section(
            thruput_spec_tool::RAW_SECTION_NAME,
            &support::minimal_contract_spec_raw(),
        ),
    )
    .unwrap();
    thruput_cli::contract_spec::finalize_rv32_spec(
        &elf,
        &root.join("contract.rv32a"),
        &thruput_cli::Print::quiet(),
    )
    .unwrap();
    let output = terminal_capture(
        fixture.command(&[
            "codegen",
            "ts",
            "contract.spec.xdr",
            "--output",
            "contract.ts",
        ]),
        80,
        true,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let raw = fixture
        .command(&["codegen", "ts", "contract.spec.xdr"])
        .output()
        .unwrap();
    assert!(raw.status.success() && raw.stderr.is_empty());
    assert_eq!(raw.stdout, fs::read(root.join("contract.ts")).unwrap());
}

#[test]
fn scaffolds_and_ready_init_use_shared_cards() {
    let fixture = Fixture::new();
    fixture.populate();
    for args in [
        vec!["new", "counter-example", "--template", "counter"],
        vec!["new", "minimal-example"],
        vec!["init"],
    ] {
        let output = terminal_capture(fixture.command(&args), 40, true);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(text.contains('╭'));
        assert!(!text.contains("\x1b[2J"));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn wallet_selection_and_secret_export_keep_their_output_contracts() {
    let fixture = Fixture::new();
    let generated = fixture
        .command(&["keys", "generate", "owner", "--json"])
        .output()
        .unwrap();
    assert!(generated.status.success());
    let key: serde_json::Value = serde_json::from_slice(&generated.stdout).unwrap();
    fs::write(fixture.root.path().join("thruput.toml"), format!("default_profile = \"local\"\n[profile.local]\n[profile.local.wallets.owned]\naddress = \"{}\"\nowner_public_key = \"{}\"\n", thruput_encoding::encode(&[0xab; 32]), key["data"]["key"]["public_key"].as_str().unwrap())).unwrap();
    let selected = terminal_capture(fixture.command(&["wallet", "use", "owned"]), 80, true);
    assert!(
        selected.status.success() && selected.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    assert!(String::from_utf8_lossy(&selected.stderr).contains('╭'));
    let plain = fixture
        .command(&["keys", "export", "owner", "--force"])
        .output()
        .unwrap();
    let json = fixture
        .command(&["keys", "export", "owner", "--force", "--json"])
        .output()
        .unwrap();
    assert!(plain.status.success() && json.status.success());
    assert!(plain.stderr.is_empty() && json.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    // Compare without printing ephemeral secret bytes if an assertion fails.
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim()
            == value["data"]["key"]["privateKey"].as_str().unwrap()
    );
    assert!(!plain.stdout.contains(&0x1b));
}

fn visible(output: &Output, columns: u16) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let mut terminal = vt100::Parser::new(100, columns, 0);
    terminal.process(&output.stdout);
    terminal.screen().contents()
}

#[test]
fn listings_render_on_stdout_at_wide_and_narrow_sizes_without_losing_values() {
    let fixture = Fixture::new();
    fixture.populate();
    let keys = fixture.command(&["keys", "ls", "--json"]).output().unwrap();
    let keys: serde_json::Value = serde_json::from_slice(&keys.stdout).unwrap();
    let address = keys["data"]["keys"][0]["address"].as_str().unwrap();
    for columns in [24, 80] {
        for (args, values) in [
            (vec!["keys", "ls"], vec![address.to_owned()]),
            (
                vec!["wallet", "ls"],
                vec![
                    format!("0x{}", "ab".repeat(32)),
                    format!("0x{}", "cd".repeat(32)),
                    format!("0x{}", "ef".repeat(32)),
                ],
            ),
        ] {
            let output = terminal_output(fixture.command(&args), columns);
            let text = visible(&output, columns);
            if columns >= 40 {
                assert!(text.contains('╭'), "{text}");
            }
            let compact: String = text
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '│')
                .collect();
            assert!(compact.contains("alice(default)"), "{text}");
            for value in values {
                assert!(compact.contains(&value), "missing {value}: {text}");
            }
            if args[0] == "wallet" {
                assert!(compact.contains("Notrecorded"), "{text}");
                assert!(text.contains("local"));
            }
        }
    }
}

#[test]
fn quiet_plain_and_redirected_listings_keep_the_existing_data_rows() {
    let fixture = Fixture::new();
    fixture.populate();
    for command in ["keys", "wallet"] {
        let original = fixture.command(&[command, "ls"]).output().unwrap();
        assert!(original.status.success());
        assert!(String::from_utf8_lossy(&original.stdout).starts_with("* alice "));
        for flag in ["--quiet", "--no-ui"] {
            let output = terminal_output(fixture.command(&[command, "ls", flag]), 80);
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
                String::from_utf8_lossy(&original.stdout)
            );
        }
    }
}

#[test]
fn empty_lists_and_monochrome_ascii_modes_use_shared_presentation() {
    let fixture = Fixture::new();
    for (command, message) in [("keys", "No keys stored."), ("wallet", "No wallets saved.")] {
        let output = terminal_output(fixture.command(&[command, "ls"]), 80);
        let text = visible(&output, 80);
        assert!(text.contains(message), "{text}");
        assert!(text.contains('╭'));
        let mut command = fixture.command(&[command, "ls"]);
        command.env("NO_COLOR", "1").env("LC_ALL", "C");
        let output = terminal_output(command, 80);
        assert!(output.stdout.is_ascii());
        assert!(!output.stdout.contains(&0x1b));
        assert!(visible(&output, 80).contains(message));
    }
}

#[test]
fn listing_errors_do_not_produce_empty_or_partial_success_reports() {
    let fixture = Fixture::new();
    fixture.populate();
    let listed = fixture.command(&["keys", "ls", "--json"]).output().unwrap();
    let keys: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let path = keys["data"]["keys"][0]["path"].as_str().unwrap();
    fs::write(path, "not valid key JSON").unwrap();
    for args in [
        vec!["keys", "ls"],
        vec!["wallet", "ls", "--profile", "missing"],
    ] {
        let output = terminal_output(fixture.command(&args), 80);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn config_cards_preserve_merged_values_and_field_sources() {
    let fixture = Fixture::new();
    let config_dir = fixture.root.path().join("config/thruput");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.toml"), "[profile.local]\nnetwork = \"local\"\nrpc_url = \"https://global.example\"\n[profile.local.contracts]\nshared = \"0x1234\"\n").unwrap();
    fs::write(fixture.root.path().join("thruput.toml"), "default_profile = \"local\"\n[profile.local]\nrpc_url = \"https://project.example/long/path\"\n").unwrap();
    for columns in [24, 80] {
        for args in [vec!["config", "get"], vec!["config", "get", "--sources"]] {
            let output = terminal_output(fixture.command(&args), columns);
            let text = visible(&output, columns);
            let compact: String = text
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '│')
                .collect();
            for value in [
                "Configuration",
                "local",
                "https://project.example/long/path",
                "0x1234",
                "notset",
                "None",
            ] {
                assert!(compact.contains(value), "missing {value}: {text}");
            }
            assert!(!compact.contains("https://global.example"));
            if args.contains(&"--sources") {
                assert!(compact.contains("local(global)"), "{text}");
                assert!(
                    compact.contains("https://project.example/long/path(project)"),
                    "{text}"
                );
            }
        }
    }
}

#[test]
fn config_profiles_keep_invalid_and_missing_rows_with_findings_below() {
    let fixture = Fixture::new();
    fs::write(fixture.root.path().join("thruput.toml"), "default_profile = \"local\"\n[profile.local]\nnetwork = \"local\"\n[profile.broken]\nnetwork = \"not-a-network\"\n").unwrap();
    for missing in [false, true] {
        let mut command = fixture.command(&["config", "profiles"]);
        if missing {
            command.env("THRUPUT_PROFILE", "missing");
        }
        let output = terminal_output(command, 100);
        let text = visible(&output, 100);
        assert!(text.contains("Invalid"), "{text}");
        assert!(text.contains("profile_invalid"), "{text}");
        assert!(
            text.find("Invalid").unwrap() < text.find("Warning").unwrap(),
            "{text}"
        );
        if missing {
            assert!(text.contains("missing (active)"), "{text}");
            assert!(text.contains("profile_missing"), "{text}");
        } else {
            assert!(text.contains("local (active)"), "{text}");
        }
    }
}

#[test]
fn config_plain_modes_retain_the_same_stdout_and_json_payloads() {
    let fixture = Fixture::new();
    for args in [
        vec!["config", "get"],
        vec!["config", "get", "--sources"],
        vec!["config", "profiles"],
    ] {
        let baseline = fixture.command(&args).output().unwrap();
        assert!(baseline.status.success());
        for flag in ["--quiet", "--no-ui", "--json"] {
            let mut args = args.clone();
            args.push(flag);
            let piped = fixture.command(&args).output().unwrap();
            let terminal = terminal_output(fixture.command(&args), 80);
            assert!(terminal.status.success());
            assert!(terminal.stderr.is_empty());
            assert_eq!(
                String::from_utf8_lossy(&terminal.stdout).replace("\r\n", "\n"),
                String::from_utf8_lossy(&piped.stdout)
            );
            if flag != "--json" {
                assert_eq!(piped.stdout, baseline.stdout);
            }
        }
    }
    fs::write(fixture.root.path().join("thruput.toml"), "").unwrap();
    let args = ["config", "profiles"];
    let output = terminal_output(fixture.command(&args), 80);
    assert!(visible(&output, 80).contains("No profiles configured."));
    let baseline = fixture.command(&args).output().unwrap();
    let mut command = fixture.command(&args);
    command.arg("--no-ui");
    let plain = terminal_output(command, 80);
    assert_eq!(
        String::from_utf8_lossy(&plain.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&baseline.stdout)
    );
}
