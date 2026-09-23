#![cfg(unix)]

use std::{
    fs,
    io::Read,
    os::fd::FromRawFd,
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        fs::write(
            root.path().join("groundhog.toml"),
            "default_profile = \"local\"\n[profile.local]\n",
        )
        .unwrap();
        Self { root }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("NO_COLOR")
            .env_remove("GROUNDHOG_PROFILE");
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
        fs::write(self.root.path().join("groundhog.toml"), format!(
            "default_profile = \"local\"\n[profile.local]\ndefault_wallet = \"alice\"\n[profile.local.wallets.alice]\naddress = \"0x{}\"\nowner_public_key = \"0x{}\"\n[profile.local.wallets.legacy]\naddress = \"0x{}\"\n",
            "ab".repeat(32), "cd".repeat(32), "ef".repeat(32),
        )).unwrap();
    }
}

// Static tables only need a stdout terminal; stderr remains a separate pipe.
fn terminal_output(mut command: Command, columns: u16) -> Output {
    let mut master = -1;
    let mut slave = -1;
    let size = libc::winsize {
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
                std::ptr::null(),
                &raw const size,
            )
        },
        0
    );
    // SAFETY: each fresh descriptor is owned by exactly one File.
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let child = command
        .stdout(slave)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
    output.stdout = reader.join().unwrap();
    output
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
    for columns in [24, 40, 80, 120] {
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
