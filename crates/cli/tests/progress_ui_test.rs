#![cfg(unix)]

//! Exercise command-scoped progress, including DSR replies, signals, and scrollback.
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::PermissionsExt,
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

#[path = "support/query_ui.rs"]
mod queries;
mod support;

const PARTIAL_OUTPUT_WAIT: &str = r#"
if [ "$BUILD_UI_MODE" = partial-wait ]; then
  printf 'waiting-for-input-without-newline'
  while [ ! -f release-cargo ]; do sleep 0.05; done
  printf '\n'
fi
"#;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::create_dir_all(root.join("target/riscv32im-unknown-none-elf/release")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(root.join(".cargo/contract.toml"), "").unwrap();
        fs::write(
            root.join("groundhog.toml"),
            "default_profile = \"local\"\n[profile.local]\n",
        )
        .unwrap();
        fs::write(
            root.join("fixture.elf"),
            support::minimal_rv32_elf_with_section(
                groundhog_spec_tool::RAW_SECTION_NAME,
                &support::minimal_contract_spec_raw(),
            ),
        )
        .unwrap();
        let metadata = serde_json::json!({
            "workspace_root": root, "target_directory": root.join("target"),
            "packages": [{"name":"demo", "version":"0.1.0", "manifest_path":root.join("Cargo.toml")}],
        });
        fs::write(root.join("bin/cargo"), Self::cargo_script(&metadata)).unwrap();
        fs::set_permissions(root.join("bin/cargo"), fs::Permissions::from_mode(0o755)).unwrap();
        Self { _temp: temp, root }
    }

    fn cargo_script(metadata: &serde_json::Value) -> String {
        format!(
            r#"#!/bin/sh
if [ "$1" = metadata ]; then
  case " $* " in
    *" --no-deps "*) printf '%s' '{metadata}';;
    *) printf '    Updating fake-registry index\n' >&2
       printf '%s' '{{"packages":[],"resolve":{{"nodes":[]}}}}';;
  esac
  exit 0
fi
if [ "$1" = config ]; then
  printf '%s' '["-C","link-arg=-Tgroundhog-rv32.ld"]'
  exit 0
fi
if [ "$1" = test ]; then
  sleep 0.3
  if [ "$BUILD_UI_MODE" = test-fail ]; then
    printf 'running 1 test\ntest tests::fails ... FAILED\n\nassertion failed: expected value\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n'
    exit 101
  fi
  printf 'running 1 test\ntest tests::passes ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n'
  exit 0
fi
# The CLI is this PTY session leader. Its SIGINT exit can send SIGHUP before
# the shell runs its INT trap; ignore that independent signal in the fixture.
trap '' HUP
trap 'printf stopped > cargo-stopped; exit 130' INT
printf '%s' "$$" > cargo.pid
printf '%s' "$*" > cargo.args
if [ "$BUILD_UI_MODE" = diagnostics ] || [ "$BUILD_UI_MODE" = diagnostic-fail ]; then
  case " $* " in
    *" --message-format=json "*)
      printf '%s\n' '{warning}' '{warning}'
      printf 'warning: `demo` (lib) generated 2 warnings\n' >&2
      printf '    Finished release profile\n' >&2
      if [ "$BUILD_UI_MODE" = diagnostic-fail ]; then
        printf '%s\n' '{error}'
      fi;;
    *) printf 'warning: original verbose diagnostic\n' >&2;;
  esac
  sleep 0.5
  if [ "$BUILD_UI_MODE" = diagnostic-fail ]; then exit 42; fi
  cp fixture.elf target/riscv32im-unknown-none-elf/release/demo
  exit 0
fi
printf 'compiler-stdout-first\n'
printf 'compiler-stderr-first\n' >&2
if [ "$BUILD_UI_MODE" = hang ]; then while :; do sleep 1; done; fi
if [ "$BUILD_UI_MODE" != fast ]; then sleep 0.5; fi
if [ "$BUILD_UI_MODE" = flood ]; then
  i=0
  while [ "$i" -lt 300 ]; do
    printf 'stdout-record-%04d\n' "$i"
    printf 'stderr-record-%04d\n' "$i" >&2
    i=$((i+1))
  done
fi
if [ "$BUILD_UI_MODE" = split ]; then
  printf '\342' >&2
  sleep 0.2
  printf '\234\223 utf8-complete\r' >&2
  sleep 0.2
  printf '\n' >&2
fi
if [ "$BUILD_UI_MODE" = continued ]; then
  printf 'stdout-prefix-\342'
  sleep 0.3
  printf '\234\223suffix\n'
  printf 'stderr-prefix-' >&2
  sleep 0.3
  printf 'suffix\n' >&2
fi
{PARTIAL_OUTPUT_WAIT}
if [ "$BUILD_UI_MODE" = fail ]; then
  printf 'compiler-failure-detail\n' >&2
  exit 42
fi
cp fixture.elf target/riscv32im-unknown-none-elf/release/demo
printf 'compiler-stdout-last\n'
if [ "$BUILD_UI_MODE" = partial ]; then
  printf 'compiler-stderr-last' >&2
else
  printf 'compiler-stderr-last\n' >&2
fi
"#,
            warning = serde_json::json!({"reason":"compiler-message", "package_id":"demo", "message": {
                "level":"warning", "code":{"code":"unused_variables"},
                "rendered":"warning: unused variable: `value`\n --> src/main.rs:3:9\n  |\n3 |     let value = 1;\n  |         ^^^^^\n  = note: compiler note\n\n"
            }}),
            error = serde_json::json!({"reason":"compiler-message", "package_id":"demo", "message": {
                "level":"error", "code":{"code":"E0425"},
                "rendered":"error[E0425]: cannot find value `missing`\n --> src/main.rs:4:5\n  |\n4 |     missing;\n  |     ^^^^^^^\n\n"
            }}),
        )
    }

    fn command(&self, mode: &str) -> Command {
        self.command_for("build", mode)
    }

    fn command_for(&self, name: &str, mode: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
        command
            .arg(name)
            .current_dir(&self.root)
            .env("BUILD_UI_MODE", mode)
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("CARGO_HOME", self.root.join("cargo-home"))
            .env_remove("GROUNDHOG_PROFILE")
            .env_remove("GROUNDHOG_BUNDLE_MANIFEST")
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("CARGO_TERM_COLOR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("NO_COLOR");
        let mut paths = vec![self.root.join("bin")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command.env("PATH", std::env::join_paths(paths).unwrap());
        command
    }

    fn verbose_command(&self, mode: &str) -> Command {
        let mut command = self.command(mode);
        command.arg("--verbose");
        command
    }
}

#[test]
fn test_summary_condenses_passing_logs_and_retains_failure_details() {
    for (mode, success) in [("slow", true), ("test-fail", false)] {
        let fixture = Fixture::new();
        let mut pty = Pty::spawn(fixture.command_for("test", mode));
        assert_eq!(pty.finish().success(), success, "{}", pty.history());
        let history = pty.history();
        assert!(
            history.contains("Tests") && history.contains("Package"),
            "{history}"
        );
        if success {
            assert!(history.contains("1 passed"));
            assert!(!history.contains("tests::passes ... ok"));
        } else {
            assert!(
                history.contains("assertion failed: expected value"),
                "{history}"
            );
            assert!(
                history.find("Tests").unwrap() < history.find("assertion failed").unwrap(),
                "{history}"
            );
        }
    }
    let fixture = Fixture::new();
    let mut command = fixture.command_for("test", "slow");
    command.arg("--show-output");
    let mut pty = Pty::spawn(command);
    assert!(pty.finish().success());
    assert!(pty.history().contains("tests::passes ... ok"));
}

struct Pty {
    child: Child,
    master: File,
    slave: File,
    original: libc::termios,
    parser: vt100::Parser,
    raw: Vec<u8>,
    queries: usize,
    progress_rows: usize,
    recent: Vec<u8>,
}

impl Pty {
    fn spawn(mut command: Command) -> Self {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: 12,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initializes two descriptors and reads the initialized size.
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
        // SAFETY: each new descriptor is transferred to exactly one File.
        let master = unsafe { File::from_raw_fd(master) };
        // SAFETY: slave is the other descriptor returned by openpty.
        let mut slave = unsafe { File::from_raw_fd(slave) };
        // Darwin revokes slave descriptors when their controlling session exits.
        // The master retains access to the same terminal attributes after exit.
        let original = attributes(&master);
        // SAFETY: preserve descriptor flags while enabling nonblocking test reads.
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        slave.write_all(b"PREEXISTING-SHELL-LINE\n").unwrap();
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap());
        // SAFETY: only async-signal-safe syscalls run after fork; the test child
        // owns its session and controlling PTY, including its Cargo process group.
        unsafe {
            command.pre_exec(|| {
                #[cfg(target_os = "macos")]
                let request = libc::c_ulong::from(libc::TIOCSCTTY);
                #[cfg(not(target_os = "macos"))]
                let request = libc::TIOCSCTTY;
                if libc::setsid() < 0
                    || libc::ioctl(0, request, 0) < 0
                    || libc::tcsetpgrp(0, libc::getpgrp()) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        Self {
            child,
            master,
            slave,
            original,
            parser: vt100::Parser::new(12, 80, 10000),
            raw: Vec::new(),
            queries: 0,
            progress_rows: 0,
            recent: Vec::new(),
        }
    }

    fn pump(&mut self) {
        let mut data = [0; 8192];
        loop {
            match self.master.read(&mut data) {
                Ok(0) => break,
                Ok(count) => {
                    self.raw.extend_from_slice(&data[..count]);
                    for byte in &data[..count] {
                        self.parser.process(&[*byte]);
                        self.recent.push(*byte);
                        if self.recent.len() > 4 {
                            self.recent.remove(0);
                        }
                        if self.recent == b"\x1b[6n" {
                            self.queries += 1;
                        }
                        if self.recent == b"\x1b[2K" {
                            self.progress_rows += 1;
                        }
                    }
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) => panic!("PTY read failed: {error}"),
            }
        }
    }

    fn until(&mut self, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            self.pump();
            if predicate(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "terminal timed out: {}",
                String::from_utf8_lossy(&self.raw)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn finish(&mut self) -> ExitStatus {
        self.finish_with_refresh(false)
    }

    fn finish_guided(&mut self) -> ExitStatus {
        self.finish_with_refresh(true)
    }

    fn finish_with_refresh(&mut self, guided: bool) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            self.pump();
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "child did not exit");
            thread::sleep(Duration::from_millis(5));
        };
        self.pump();
        assert_eq!(self.queries, 0, "progress must not query terminal input");
        let current = attributes(&self.master);
        assert_eq!(current.c_lflag, self.original.c_lflag);
        assert_eq!(current.c_iflag, self.original.c_iflag);
        assert_eq!(current.c_oflag, self.original.c_oflag);
        assert_eq!(current.c_cflag, self.original.c_cflag);
        assert_eq!(current.c_cc, self.original.c_cc);
        assert!(!self.parser.screen().hide_cursor());
        assert!(!self.parser.screen().alternate_screen());
        if !guided {
            assert!(
                !self.raw.windows(4).any(|bytes| bytes == b"\x1b[2J"),
                "ordinary commands must not clear previous output"
            );
        }
        assert!(
            !self.raw.windows(4).any(|bytes| bytes == b"\x1b[3J"),
            "must never purge scrollback"
        );
        assert!(!self.raw.windows(8).any(|bytes| bytes == b"\x1b[?1049h"));
        status
    }

    fn history(&mut self) -> String {
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let rows = self.parser.screen().scrollback();
        let mut history = String::new();
        for offset in (0..=rows).rev() {
            self.parser.screen_mut().set_scrollback(offset);
            if let Some(line) = self.parser.screen().rows(0, 80).next() {
                history.push_str(&line);
                history.push('\n');
            }
        }
        self.parser.screen_mut().set_scrollback(0);
        for line in self.parser.screen().rows(0, 80).skip(1) {
            history.push_str(&line);
            history.push('\n');
        }
        history
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        let size = libc::winsize {
            ws_col: columns,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        self.parser.screen_mut().set_size(rows, columns);
        // SAFETY: resize only this test's PTY.
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) },
            0
        );
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: the child's isolated process group belongs to this test.
        unsafe {
            libc::kill(-i32::try_from(self.child.id()).unwrap(), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn attributes(file: &File) -> libc::termios {
    let mut result = std::mem::MaybeUninit::uninit();
    // SAFETY: tcgetattr initializes the termios on success.
    assert_eq!(
        unsafe { libc::tcgetattr(file.as_raw_fd(), result.as_mut_ptr()) },
        0,
        "tcgetattr failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe { result.assume_init() }
}

#[test]
fn verbose_build_preserves_both_streams_scrollback_and_artifacts() {
    let fixture = Fixture::new();
    let mut command = fixture.verbose_command("flood");
    command.args(["--bindings", "ts", "--show-cargo-commands"]);
    let mut pty = Pty::spawn(command);
    assert!(pty.finish().success());
    assert_eq!(pty.progress_rows, 0, "verbose output bypasses animation");
    let history = pty.history();
    assert!(history.contains("PREEXISTING-SHELL-LINE"));
    for index in 0..300 {
        for stream in ["stdout", "stderr"] {
            let record = format!("{stream}-record-{index:04}");
            assert_eq!(
                history.matches(&record).count(),
                1,
                "{record} missing or duplicated"
            );
        }
    }
    assert!(history.contains("Build complete"));
    assert!(history.contains("Artifact size"));
    assert!(fixture.root.join("target/groundhog/demo.rv32a").exists());
    assert!(fixture.root.join("target/groundhog/demo.ts").exists());
}

#[test]
fn fast_and_plain_builds_do_not_start_a_live_viewport() {
    let fixture = Fixture::new();
    for (mode, args) in [
        ("fast", vec![]),
        ("slow", vec!["--no-ui"]),
        ("slow", vec!["--quiet"]),
        ("slow", vec!["--", "--message-format=json"]),
    ] {
        let mut command = fixture.command(mode);
        command.args(args);
        let started = Instant::now();
        let mut pty = Pty::spawn(command);
        assert!(pty.finish().success());
        // A loaded CI host may make even this fixture genuinely slow. Only assert
        // no flash when the measured whole invocation was shorter than the delay.
        if mode != "fast" || started.elapsed() < Duration::from_millis(150) {
            assert_eq!(pty.progress_rows, 0);
        }
    }
}

#[test]
fn json_build_report_and_error_contracts_are_preserved() {
    let fixture = Fixture::new();
    let output = fixture
        .command("fast")
        .arg("--json")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data"]["schemaVersion"], 2);
    assert!(
        report["data"]["cargoStdout"]
            .as_str()
            .unwrap()
            .contains("compiler-stdout-last")
    );
    assert!(output.stderr.is_empty());
    let output = fixture.command("fail").arg("--json").output().unwrap();
    assert!(!output.status.success());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "cargo_failed");
    assert!(
        error["error"]["details"]["stderr"]
            .as_str()
            .unwrap()
            .contains("compiler-failure-detail")
    );
}

#[test]
fn resize_falls_back_without_erasing_diagnostics_or_losing_utf8() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.command("split"));
    pty.until(|pty| pty.progress_rows > 0 && pty.parser.screen().contents().contains("Compiling"));
    pty.resize(40, 8);
    assert!(pty.finish().success());
    let output = String::from_utf8_lossy(&pty.raw);
    assert!(output.contains("utf8-complete"));
    assert!(!output.contains('\u{fffd}'));
    assert!(output.contains("compiler-stderr-last"));
}

#[test]
fn ctrl_c_retains_foreground_signal_behaviour_and_no_success_card() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.command("hang"));
    pty.until(|pty| {
        pty.progress_rows > 0
            && pty.parser.screen().contents().contains("Compiling")
            && fixture.root.join("cargo.pid").exists()
    });
    pty.master.write_all(b"\x03").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        pty.pump();
        if let Some(status) = pty.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "Ctrl+C did not stop build");
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.signal(), Some(libc::SIGINT));
    assert_eq!(attributes(&pty.master).c_lflag, pty.original.c_lflag);
    assert!(!String::from_utf8_lossy(&pty.raw).contains("Build complete"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fixture.root.join("cargo-stopped").exists() {
        assert!(
            Instant::now() < deadline,
            "Cargo did not receive the foreground interrupt"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!fixture.root.join("target/groundhog/demo.rv32a").exists());
}

#[test]
fn live_failure_finishes_progress_before_the_shared_error() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.command("fail"));
    assert!(!pty.finish().success());
    assert!(pty.progress_rows > 0);
    let history = pty.history();
    assert!(history.contains("compiler-failure-detail"));
    assert!(history.contains("cargo_failed"));
    assert!(history.contains("cargo failed with exit code 42"));
    assert!(!history.contains("Build complete"));
}

#[test]
fn build_warnings_follow_the_result_card() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("fixture.elf"),
        support::minimal_rv32_elf(),
    )
    .unwrap();
    let mut pty = Pty::spawn(fixture.command("slow"));
    assert!(pty.finish().success());
    let history = pty.history();
    let complete = history.find("Build complete").unwrap();
    let warning = history.find("no contract spec found").unwrap();
    assert!(warning > complete, "{history}");
}

#[test]
fn partial_diagnostics_precede_later_status_lines() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.verbose_command("partial"));
    assert!(pty.finish().success());
    let history = pty.history();
    assert!(
        history.find("compiler-stderr-last").unwrap() < history.find("Wrote").unwrap(),
        "{history}"
    );
}

#[test]
fn delayed_child_fragments_keep_their_line_boundaries() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.verbose_command("continued"));
    pty.until(|pty| pty.parser.screen().contents().contains("stdout-prefix-"));
    let rows = pty.progress_rows;
    assert_eq!(rows, 0, "verbose output bypasses animation");
    assert!(pty.finish().success());
    let history = pty.history();
    assert!(history.contains("stdout-prefix-✓suffix"), "{history}");
    assert!(history.contains("stderr-prefix-suffix"), "{history}");
}

#[test]
fn verbose_unfinished_output_is_visible_before_the_child_can_finish() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.verbose_command("partial-wait"));
    pty.until(|pty| {
        pty.parser
            .screen()
            .contents()
            .contains("waiting-for-input-without-newline")
    });
    assert!(pty.child.try_wait().unwrap().is_none());
    assert_eq!(pty.progress_rows, 0);
    fs::write(fixture.root.join("release-cargo"), "").unwrap();
    assert!(pty.finish().success());
}

#[test]
fn resume_preserves_output_written_while_the_job_was_stopped() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.command("slow"));
    pty.until(|pty| pty.progress_rows > 0 && pty.parser.screen().contents().contains("Compiling"));
    let pid = i32::try_from(pty.child.id()).unwrap();
    // SAFETY: stop the isolated test process group. SIGSTOP works even when the
    // new session is an orphaned process group that would ignore terminal SIGTSTP.
    assert_eq!(unsafe { libc::kill(-pid, libc::SIGSTOP) }, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut status = 0;
        // SAFETY: observe only the child owned by this test.
        let waited =
            unsafe { libc::waitpid(pid, &raw mut status, libc::WUNTRACED | libc::WNOHANG) };
        if waited == pid && libc::WIFSTOPPED(status) {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    pty.slave.write_all(b"\nSHELL-DURING-SUSPEND\n").unwrap();
    pty.pump();
    // SAFETY: resume that same owned group.
    assert_eq!(unsafe { libc::kill(-pid, libc::SIGCONT) }, 0);
    assert!(pty.finish().success());
    let history = pty.history();
    assert!(history.contains("SHELL-DURING-SUSPEND"), "{history}");
    assert!(history.contains("Build complete"));
}

#[test]
fn monochrome_progress_and_explicit_cargo_color_use_the_correct_modes() {
    let fixture = Fixture::new();
    let mut command = fixture.command("slow");
    command.env("NO_COLOR", "1");
    let mut pty = Pty::spawn(command);
    assert!(pty.finish().success());
    assert!(pty.progress_rows > 0);
    assert!(!String::from_utf8_lossy(&pty.raw).contains("\x1b[38;"));

    let mut command = fixture.command("slow");
    command.env("CARGO_TERM_COLOR", "always");
    let mut pty = Pty::spawn(command);
    assert!(pty.finish().success());
    assert_eq!(pty.progress_rows, 0);
}

#[test]
fn progress_initialization_preserves_input_and_interrupts_without_a_cursor_reply() {
    for keyboard_interrupt in [true, false] {
        let fixture = Fixture::new();
        let mut pty = Pty::spawn(fixture.command("hang"));
        // Never answer cursor queries. This reaches the old raw-mode query window
        // as well as the replacement's first output-only progress frame.
        pty.until(|pty| pty.queries > 0 || pty.progress_rows > 0);
        if keyboard_interrupt {
            pty.master.write_all(b"\x03").unwrap();
        } else {
            // SAFETY: signal only the child owned by this isolated terminal fixture.
            assert_eq!(
                unsafe { libc::kill(i32::try_from(pty.child.id()).unwrap(), libc::SIGTERM) },
                0
            );
        }
        // Also checks every termios flag and control character after termination.
        assert!(!pty.finish().success());
    }
}

#[test]
fn background_build_does_not_query_terminal_input() {
    let fixture = Fixture::new();
    let base = fixture.command("slow");
    let mut shell = Command::new(std::env::current_exe().unwrap());
    shell
        .args(["--exact", "background_process_fixture", "--nocapture"])
        .env("GROUNDHOG_UI_BACKGROUND_FIXTURE", "1")
        .current_dir(&fixture.root);
    for (name, value) in base.get_envs() {
        if let Some(value) = value {
            shell.env(name, value);
        } else {
            shell.env_remove(name);
        }
    }
    let mut pty = Pty::spawn(shell);
    assert!(pty.finish().success());
    assert_eq!(pty.progress_rows, 0);
    assert!(pty.history().contains("Build complete"));
}

#[test]
fn background_process_fixture() {
    if std::env::var_os("GROUNDHOG_UI_BACKGROUND_FIXTURE").is_none() {
        return;
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
    command.arg("build");
    // Shell job-control behavior differs between Linux and Darwin. Establish a
    // real background group explicitly while this fixture owns the foreground.
    // SAFETY: setpgid is async-signal-safe and only changes this new child.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            // SAFETY: this child owns the process group created above.
            unsafe {
                libc::kill(-i32::try_from(child.id()).unwrap(), libc::SIGKILL);
            }
            let _ = child.wait();
            panic!("background build did not finish");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn new_guides_name_destination_and_catalog_selection() {
    for (template, no_ui) in [("", false), ("counter", false), ("", true)] {
        let tmp = TempDir::new().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
        command
            .arg("new")
            .current_dir(tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path().join("config"))
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("NO_COLOR");
        if no_ui {
            command.arg("--no-ui");
        }
        let mut pty = Pty::spawn(command);
        pty.resize(100, 40);
        pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Project name:"));
        let screen = pty.parser.screen().contents();
        assert!(screen.contains("New Groundhog Project"), "{screen}");
        assert_eq!(screen.contains("Progress"), !no_ui, "{screen}");
        if no_ui {
            assert!(screen.contains("[1/3] Project name"), "{screen}");
        } else {
            assert!(
                screen
                    .lines()
                    .any(|line| line.contains("Project name") && line.contains("current")),
                "{screen}"
            );
        }
        pty.master.write_all(b"guided-project\n").unwrap();
        pty.until(|pty| {
            String::from_utf8_lossy(&pty.raw).contains("Destination [guided-project]:")
        });
        pty.master.write_all(b"chosen-directory\n").unwrap();
        pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Enter for minimal"));
        let screen = pty.parser.screen().contents();
        assert!(screen.contains("Empty contract"), "{screen}");
        if no_ui {
            assert!(screen.contains("[3/3] Template"), "{screen}");
        } else {
            for label in ["Project name", "Destination"] {
                assert!(
                    screen
                        .lines()
                        .any(|line| line.contains(label) && line.contains("done")),
                    "{screen}"
                );
            }
            assert!(
                screen
                    .lines()
                    .any(|line| line.contains("Template") && line.contains("current")),
                "{screen}"
            );
        }
        pty.master
            .write_all(format!("{template}\n").as_bytes())
            .unwrap();
        assert!(pty.finish_guided().success(), "{}", pty.history());
        if no_ui {
            assert!(!pty.raw.windows(4).any(|bytes| bytes == b"\x1b[2J"));
        } else {
            let screen = pty.parser.screen().contents();
            assert!(screen.contains("Project Ready"), "{screen}");
            assert!(!screen.contains("Progress"), "{screen}");
        }
        assert!(!tmp.path().join("guided-project").exists());
        let project = tmp.path().join("chosen-directory");
        let manifest = fs::read_to_string(project.join("Cargo.toml")).unwrap();
        assert!(manifest.contains("name = \"guided-project\""));
        let source = fs::read_to_string(project.join("src/implementation.rs")).unwrap();
        assert_eq!(source.contains("pub fn increment"), template == "counter");
    }
}

#[test]
fn new_wizard_preserves_explicit_destination_and_template() {
    let tmp = TempDir::new().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
    command
        .args(["new", "--path", "chosen-directory", "--template", "counter"])
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("TERM", "xterm-256color");
    let mut pty = Pty::spawn(command);
    pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Project name:"));
    pty.master.write_all(b"guided-project\n").unwrap();
    assert!(pty.finish_guided().success(), "{}", pty.history());
    let raw = String::from_utf8_lossy(&pty.raw);
    assert!(!raw.contains("Destination ["));
    assert!(!raw.contains("Enter for minimal"));
    let source =
        fs::read_to_string(tmp.path().join("chosen-directory/src/implementation.rs")).unwrap();
    assert!(source.contains("pub fn increment"));
}

#[test]
fn guided_setup_keeps_scaffold_warnings_on_the_final_screen() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("groundhog.toml")).unwrap();
    fs::write(
        fixture.root.join("bin/cargo"),
        "#!/bin/sh\nprintf 'cargo 1.96.0\\n'\n",
    )
    .unwrap();
    let mut pty = Pty::spawn(fixture.command_for("init", "fast"));
    pty.resize(80, 40);
    pty.until(|pty| {
        String::from_utf8_lossy(&pty.raw).contains("Generate a local signing key now?")
    });
    pty.master.write_all(b"y\n").unwrap();
    pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Create a starter project?"));
    pty.master.write_all(b"y\n").unwrap();
    pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Project name:"));
    pty.master.write_all(b"wizard-contract\n").unwrap();
    assert!(pty.finish_guided().success(), "{}", pty.history());
    let screen = pty.parser.screen().contents();
    assert!(screen.contains("Setup Complete"), "{screen}");
    assert!(
        screen.contains("cargo_not_rustup_managed") && screen.contains("Hint"),
        "{screen}"
    );
    assert!(fixture.root.join("wizard-contract/groundhog.toml").exists());
}

#[test]
fn confirmation_fixture() {
    let Ok(mode) = std::env::var("GROUNDHOG_UI_CONFIRM_FIXTURE") else {
        return;
    };
    let print = groundhog_cli::Print::new(false, false).with_no_ui(mode == "plain");
    print.status("KEEP THIS CONTEXT");
    let approved = print
        .confirm_review(
            "Preview fixture",
            mode == "force",
            "test preview",
            "Continue?",
            |print| {
                print.detail("Operation", "fixture action");
                print.warn("Review warning");
            },
        )
        .unwrap();
    if approved {
        print.outcome_card(
            groundhog_cli::print::Outcome::Success,
            "Action complete",
            &[("Result".into(), "done".into())],
        );
    } else {
        print.note("Cancelled");
    }
}

#[test]
fn confirmation_fallbacks_preserve_output_and_consent() {
    for (mode, answer, approved) in [
        ("decline", "n\n", false),
        ("eof", "\x04", false),
        ("eof-after-answer", "y\x04", true),
        ("plain", "y\n", true),
        ("force", "", true),
    ] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "confirmation_fixture", "--nocapture"])
            .env("GROUNDHOG_UI_CONFIRM_FIXTURE", mode)
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("NO_COLOR");
        let mut pty = Pty::spawn(command);
        if mode != "force" {
            pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Continue? [y/N]"));
            pty.master.write_all(answer.as_bytes()).unwrap();
            // A second EOF ends read_line after the first delivered the partial line.
            if mode == "eof-after-answer" {
                pty.master.write_all(b"\x04").unwrap();
            }
        }
        assert!(pty.finish().success());
        let history = pty.history();
        assert!(
            history.contains("KEEP THIS CONTEXT") && history.contains("fixture action"),
            "{mode}: {history}"
        );
        assert_eq!(
            history.contains("Action complete"),
            approved,
            "{mode}: {history}"
        );
        assert_eq!(
            history.contains("Cancelled"),
            !approved,
            "{mode}: {history}"
        );
        assert!(history.contains("Review warning"), "{mode}: {history}");
        assert!(
            !String::from_utf8_lossy(&pty.raw).contains("\x1b[J"),
            "{mode}"
        );
    }
}

#[test]
fn approved_preview_disappears_but_warnings_and_prior_output_remain() {
    for resize in [false, true] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "confirmation_fixture", "--nocapture"])
            .env("GROUNDHOG_UI_CONFIRM_FIXTURE", "1")
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("NO_COLOR");
        let mut pty = Pty::spawn(command);
        pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Continue? [y/N]"));
        if resize {
            pty.resize(60, 12);
        }
        pty.master.write_all(b"y\n").unwrap();
        assert!(pty.finish().success());
        let history = pty.history();
        assert!(
            history.contains("KEEP THIS CONTEXT") && history.contains("Action complete"),
            "{history}"
        );
        assert_eq!(history.contains("Preview fixture"), resize, "{history}");
        assert!(!history.contains("• Preview fixture"), "{history}");
        assert!(history.contains("Review warning"));
        if !resize {
            assert!(!history.contains("Continue?"), "{history}");
            assert!(
                history.find("Action complete").unwrap() < history.find("Review warning").unwrap(),
                "{history}"
            );
        }
    }
}

#[test]
fn active_build_phase_is_muted_but_spinner_keeps_the_accent() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.command("hang"));
    pty.until(|pty| {
        pty.progress_rows > 0 && pty.parser.screen().contents().contains("Compiling demo")
    });
    let (row, col) = pty
        .parser
        .screen()
        .rows(0, 80)
        .enumerate()
        .find_map(|(row, line)| line.find("Compiling demo").map(|col| (row, col)))
        .unwrap();
    assert!(
        pty.parser
            .screen()
            .cell(u16::try_from(row).unwrap(), u16::try_from(col).unwrap())
            .unwrap()
            .dim()
    );
    let spinner = pty
        .parser
        .screen()
        .cell(u16::try_from(row).unwrap(), 0)
        .unwrap();
    assert!(!spinner.dim());
    assert_eq!(spinner.fgcolor(), vt100::Color::Idx(6));
}

#[path = "support/devnode.rs"]
mod devnode;

fn node_command(root: &TempDir, binary: &std::path::Path, mode: &str) -> Command {
    let wrapper = root.path().join("hog-devnode");
    std::fs::write(&wrapper, r#"#!/bin/sh
if [ "$1" = "--version" ]; then exec "$GROUNDHOG_UI_REAL_DEVNODE" "$@"; fi
printf '%s\n' '{"level":"warn","message":"startup warning fixture","timestamp_ms":123,"target":"ramdb","fields":{"file":"fixture.cc","line":43}}' >&2
sleep 0.5
exec "$GROUNDHOG_UI_REAL_DEVNODE" "$@"
"#).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
    command
        .args(["node", "--port", "0", "--hog-devnode-binary"])
        .arg(&wrapper)
        .current_dir(root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("GROUNDHOG_UI_REAL_DEVNODE", binary)
        .env("GROUNDHOG_FAKE_HOGD_DEV_MODE", "ready-with-indexer")
        .env(
            "GROUNDHOG_FAKE_HOGD_DEV_UI_TRIGGER",
            root.path().join("log-now"),
        )
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env_remove("NO_COLOR")
        .env_remove("RUST_LOG");
    match mode {
        "failure" => {
            command.args(["--hogd-protocol", "hogp/wrong"]);
        }
        "verbose" => {
            command.arg("--verbose");
        }
        "plain" => {
            command.arg("--no-ui");
        }
        _ => {}
    }
    command
}

#[test]
fn node_phases_keep_warnings_after_results_and_leave_a_short_stop_message() {
    let binary = devnode::binary();
    for mode in ["normal", "failure", "verbose", "plain"] {
        let root = TempDir::new().unwrap();
        let mut pty = Pty::spawn(node_command(&root, &binary, mode));
        if matches!(mode, "normal" | "failure") {
            pty.until(|pty| {
                pty.parser
                    .screen()
                    .contents()
                    .contains("Starting Groundhog local node")
            });
            assert!(!String::from_utf8_lossy(&pty.raw).contains("startup warning fixture"));
        }
        if mode != "failure" {
            pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("account manifest"));
            std::fs::write(root.path().join("log-now"), "").unwrap();
            pty.until(|pty| {
                pty.parser
                    .screen()
                    .contents()
                    .contains("live warning fixture")
            });
            if mode == "normal" {
                let row = pty
                    .parser
                    .screen()
                    .rows(0, 80)
                    .position(|line| line.contains("WARN live warning fixture"))
                    .unwrap();
                assert!(
                    pty.parser
                        .screen()
                        .cell(u16::try_from(row).unwrap(), 5)
                        .unwrap()
                        .dim()
                );
            }
            pty.master.write_all(b"\x03").unwrap();
            if mode == "normal" {
                pty.until(|pty| {
                    pty.parser
                        .screen()
                        .contents()
                        .contains("Stopping Groundhog local node")
                });
            }
        }
        assert_eq!(pty.finish().success(), mode != "failure");
        let history = pty.history();
        let warning = history.find("startup warning fixture").unwrap();
        if mode == "failure" {
            assert!(
                history.find("node_supervisor_error").unwrap() < warning,
                "{history}"
            );
        } else {
            assert!(history.contains("Local node stopped."), "{history}");
            if mode == "normal" {
                assert!(
                    history.find("Local node stopped.").unwrap()
                        < history.find("shutdown warning fixture").unwrap(),
                    "{history}"
                );
                assert!(
                    !history.contains("Stopping Groundhog local node"),
                    "{history}"
                );
                assert!(
                    history.find("Ready: Local node").unwrap() < warning,
                    "{history}"
                );
                assert!(
                    !history.contains("Shutting down") && !history.contains("Starting Groundhog"),
                    "{history}"
                );
                assert!(!history.contains("hogd_timestamp_ms"), "{history}");
            } else {
                assert!(
                    warning < history.find("local node ready").unwrap(),
                    "{history}"
                );
                assert!(history.contains("hogd_timestamp_ms"), "{history}");
            }
        }
    }
}

#[test]
fn compact_build_defers_and_deduplicates_structured_warnings() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.command("diagnostics"));
    pty.until(|pty| pty.progress_rows > 0);
    assert!(!String::from_utf8_lossy(&pty.raw).contains("unused variable"));
    assert!(pty.finish().success());
    let history = pty.history();
    assert!(
        history.find("Build complete").unwrap() < history.find("unused variable").unwrap(),
        "{history}"
    );
    assert_eq!(history.matches("unused variable").count(), 1, "{history}");
    for text in [
        "src/main.rs:3:9",
        "let value = 1;",
        "^^^^^",
        "compiler note",
        "unused_variables",
    ] {
        assert!(history.contains(text), "missing {text}: {history}");
    }
    for text in [
        "Wrote ",
        "generated 2 warnings",
        "Finished release",
        "Updating fake-registry",
        "compiler-message",
    ] {
        assert!(!history.contains(text), "unexpected {text}: {history}");
    }
    assert!(
        fs::read_to_string(fixture.root.join("cargo.args"))
            .unwrap()
            .contains("--message-format=json")
    );
}

#[test]
fn compact_failure_shows_summary_before_compiler_diagnostics() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.command("diagnostic-fail"));
    assert!(!pty.finish().success());
    let history = pty.history();
    assert!(
        history.find("cargo failed with exit code 42").unwrap()
            < history.find("cannot find value").unwrap(),
        "{history}"
    );
    assert!(history.contains("src/main.rs:4:5"));
    assert!(history.contains("E0425"));
    assert!(!history.contains("Build complete"));
}

#[test]
fn verbose_build_keeps_original_live_cargo_diagnostics() {
    let fixture = Fixture::new();
    let mut pty = Pty::spawn(fixture.verbose_command("diagnostics"));
    pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("original verbose diagnostic"));
    assert!(!String::from_utf8_lossy(&pty.raw).contains("Build complete"));
    assert!(pty.finish().success());
    assert!(
        !fs::read_to_string(fixture.root.join("cargo.args"))
            .unwrap()
            .contains("--message-format=json")
    );
}
