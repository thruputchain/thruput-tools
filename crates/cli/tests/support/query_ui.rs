use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

use super::Pty;

#[test]
fn skill_setup_collects_consent_before_forwarding_the_installer_ui() {
    use std::os::unix::fs::PermissionsExt;
    let root = TempDir::new().unwrap();
    for (name, script) in [
        ("node", "#!/bin/sh\nprintf 'v22.20.0\\n'\n"),
        ("npm", "#!/bin/sh\nexit 0\n"),
        ("git", "#!/bin/sh\nexit 0\n"),
        (
            "npx",
            "#!/bin/sh\nif [ \"$3\" = list ]; then\n  if [ \"$SKILL_UI_GLOBAL\" = 1 ] && [ \"$5\" != --global ]; then\n    printf '[]\\n'\n  else\n    printf '%s\\n' '[{\"name\":\"thruput\",\"source\":\"thruputchain/agent-skills\"}]'\n  fi\n  exit 0\nfi\nprintf 'installer-agent-selection: '\nread -r agent\nprintf '%s' \"$agent\" > selected-agent\n",
        ),
    ] {
        let path = root.path().join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (approved, global) in [(false, false), (true, false), (true, true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
        command
            .args(["skill", "install", "--no-ui"])
            .env("SKILL_UI_GLOBAL", if global { "1" } else { "0" })
            .env("PATH", root.path())
            .current_dir(root.path());
        let mut pty = Pty::spawn(command);
        pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("[y/N]"));
        assert!(!root.path().join("selected-agent").exists());
        pty.master
            .write_all(if approved { b"y\n" } else { b"n\n" })
            .unwrap();
        if approved {
            pty.until(|pty| {
                String::from_utf8_lossy(&pty.raw).contains("installer-agent-selection:")
            });
            pty.master.write_all(b"opencode\n").unwrap();
        }
        assert_eq!(pty.finish().success(), approved);
        if approved {
            assert_eq!(
                fs::read_to_string(root.path().join("selected-agent")).unwrap(),
                "opencode"
            );
            fs::remove_file(root.path().join("selected-agent")).unwrap();
        } else {
            assert!(!root.path().join("selected-agent").exists());
        }
    }
}

struct Query {
    root: TempDir,
    server: thread::JoinHandle<Value>,
}

impl Query {
    fn new(response: Value, delay: Duration) -> Self {
        let root = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let rpc = format!("http://{}", listener.local_addr().unwrap());
        fs::write(root.path().join("thruput.toml"), format!(
            "default_profile = \"local\"\n[profile.local]\nrpc_url = \"{rpc}\"\ndefault_wallet = \"primary\"\n[profile.local.contracts]\ncounter = \"{address}\"\n[profile.local.wallets.primary]\naddress = \"{address}\"\nowner_public_key = \"{address}\"\n",
            address = address(),
        )).unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_mins(1);
            let mut query_request = None;
            loop {
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "query never reached RPC");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("RPC accept failed: {error}"),
                    }
                };
                // Darwin inherits the listener's nonblocking flag on accepted sockets.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                let method = request["method"].as_str().unwrap();
                let mut reply = match method {
                    "getVersionInfo" => {
                        json!({"result":{"wireFormat":"14.0.0","implementation":"test","supportedMethods":["getBalance","getStorage","simulateInvocation","getTransactionStatus","getTransactionReceipt","getTransactionsByRootAddress","getEvents"]}})
                    }
                    "getTransactionReceipt" => json!({"result":null}),
                    _ => {
                        query_request = Some(request.clone());
                        response.clone()
                    }
                };
                reply["jsonrpc"] = "2.0".into();
                reply["id"] = request["id"].clone();
                let body = reply.to_string();
                if method != "getVersionInfo" {
                    thread::sleep(delay);
                }
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                if method != "getVersionInfo" && method != "getTransactionStatus" {
                    return query_request.unwrap();
                }
            }
        });
        Self { root, server }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("THRUPUT_PROFILE")
            .env_remove("THRUPUT_BUNDLE_MANIFEST")
            .env_remove("NO_COLOR");
        crate::isolation::isolate_from_developer_env(&mut command);
        command
    }

    fn finish(self, method: &str) {
        let request = self.server.join().unwrap();
        assert_eq!(request["method"], method);
    }
}

fn address() -> String {
    thruput_encoding::encode(&[0x11; 32])
}

fn balance(raw: &str) -> Value {
    json!({"result": {"address": address(), "rawBalance": raw, "stateBlockHeight": "34"}})
}

fn view(output: Value) -> Value {
    let mut response = json!({"result": {"events": [], "stateBlockHeight": "12", "gasUsed": "7"}});
    response["result"]["output"] = output;
    response
}

const VIEW: &[&str] = &["view", "counter", "0x0102030405060708"];
const SLOW: Duration = Duration::from_millis(500);

fn init_command(root: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
    command
        .arg("init")
        .current_dir(root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env_remove("THRUPUT_PROFILE")
        .env_remove("NO_COLOR");
    command
}

#[test]
fn init_refreshes_the_step_overview_and_keeps_canonical_prompts() {
    let root = TempDir::new().unwrap();
    let mut pty = Pty::spawn(init_command(&root));
    pty.resize(80, 24);
    pty.until(|pty| {
        String::from_utf8_lossy(&pty.raw).contains("Generate a local signing key now?")
    });
    let screen = pty.parser.screen().contents();
    for text in [
        "Thruput Setup",
        "Progress",
        "Environment",
        "Profiles",
        "Signing key",
        "current",
        "Project",
        "pending",
        "No signing key found",
    ] {
        assert!(screen.contains(text), "missing {text}: {screen}");
    }
    assert!(
        !screen.contains("Cargo available"),
        "completed step details should not crowd the current question"
    );
    assert!(String::from_utf8_lossy(&pty.raw).contains("\x1b[2J"));
    assert!(String::from_utf8_lossy(&pty.raw).contains("38;5;6m"));
    pty.resize(40, 20);
    pty.master.write_all(b"n\n").unwrap();
    pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Create a starter project?"));
    let screen = pty.parser.screen().contents();
    assert!(
        screen.contains("skipped") && screen.contains("current"),
        "{screen}"
    );
    pty.master.write_all(b"n\n").unwrap();
    assert!(!pty.finish_guided().success());
    let history = pty.history();
    assert!(history.contains("Partially completed"), "{history}");
}

#[test]
fn init_refresh_is_optional_and_interrupts_leave_terminal_input_intact() {
    use std::os::unix::process::ExitStatusExt;
    for height in [6, 24] {
        let root = TempDir::new().unwrap();
        let mut pty = Pty::spawn(init_command(&root));
        pty.resize(80, height);
        pty.until(|pty| {
            String::from_utf8_lossy(&pty.raw).contains("Generate a local signing key now?")
        });
        assert_eq!(
            String::from_utf8_lossy(&pty.raw).contains("\x1b[2J"),
            height == 24
        );
        pty.master.write_all(b"\x03").unwrap();
        let status = if height == 24 {
            pty.finish_guided()
        } else {
            pty.finish()
        };
        assert_eq!(status.signal(), Some(libc::SIGINT));
    }
}

#[test]
fn docs_shutdown_is_a_short_message_on_its_own_line() {
    let root = TempDir::new().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_thruput"));
    command
        .args(["docs", "--port", "0"])
        .current_dir(root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env_remove("NO_COLOR");
    let mut pty = Pty::spawn(command);
    pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Ctrl-C to stop."));
    pty.master.write_all(b"\x03").unwrap();
    assert!(pty.finish().success());
    let history = pty.history();
    assert_eq!(
        history.lines().filter(|line| line.starts_with('╭')).count(),
        1,
        "{history}"
    );
    assert!(
        history
            .lines()
            .any(|line| line.trim_end() == "Documentation server stopped."),
        "{history}"
    );
}

#[test]
fn inspection_commands_render_complete_empty_and_pending_results() {
    let hash = address();
    let storage_key = format!("0x{}", "11".repeat(32));
    for (args, result, method, title) in [
        (
            vec!["storage", "counter", &storage_key],
            json!({"item":null,"stateBlockHeight":"12"}),
            "getStorage",
            "Storage",
        ),
        (
            vec!["tx", "show", &hash],
            json!({"status":"pending"}),
            "getTransactionStatus",
            "Transaction",
        ),
        (
            vec!["tx", "list"],
            json!({"transactions":[],"nextCursor":"0xaabb","latestIndexedBlockHeight":"12","oldestIndexedBlockHeight":"0"}),
            "getTransactionsByRootAddress",
            "Transaction history",
        ),
        (
            vec!["events", "list", "counter"],
            json!({"events":[],"nextCursor":null,"latestIndexedBlockHeight":"12","oldestIndexedBlockHeight":"0"}),
            "getEvents",
            "Events",
        ),
        (
            vec!["token", "transfers", "counter"],
            json!({"events":[],"nextCursor":null,"latestIndexedBlockHeight":"12","oldestIndexedBlockHeight":"0"}),
            "getEvents",
            "Token transfers",
        ),
    ] {
        let query = Query::new(json!({"result": result}), SLOW);
        let mut pty = Pty::spawn(query.command(&args));
        assert!(pty.finish().success(), "{}", pty.history());
        let history = pty.history();
        assert!(
            history.contains(title) && history.contains('╭'),
            "{history}"
        );
        if args == ["tx", "list"] {
            assert!(history.find(title).unwrap() < history.find("root invocations only").unwrap());
            assert!(history.contains("0xaabb"));
        }
        query.finish(method);
    }
}

#[test]
fn balance_progress_finishes_before_full_precision_result_and_survives_resize() {
    for raw in ["0", "170141183460469231731687303715884105727"] {
        let query = Query::new(balance(raw), SLOW);
        let mut pty = Pty::spawn(query.command(&["balance"]));
        pty.until(|pty| pty.progress_rows > 0);
        pty.resize(40, 12);
        assert!(pty.finish().success());
        let history = pty.history();
        let compact: String = history
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│')
            .collect();
        for value in [raw, &address(), "Rawbalance", "Height", "primary", "local"] {
            assert!(compact.contains(value), "missing {value}: {history}");
        }
        assert!(history.contains("PREEXISTING-SHELL-LINE"));
        query.finish("getBalance");
    }
}

#[test]
fn view_cards_distinguish_raw_returns_aborts_faults_and_events() {
    for (output, title, detail) in [
        (
            json!({"result":"return", "returnData":"0x0102"}),
            "Simulation succeeded",
            "0x0102",
        ),
        (
            json!({"result":"return", "returnData":"0x"}),
            "Simulation succeeded",
            "Return",
        ),
        (
            json!({"result":"abort", "abortData":"0xdeadbeef"}),
            "Simulation aborted",
            "0xdeadbeef",
        ),
        (
            json!({"result":"fault"}),
            "Simulation faulted",
            "State block height",
        ),
    ] {
        let mut response = view(output);
        response["result"]["events"] =
            json!([{"address": address(), "topics": [], "data": "0xcafe"}]);
        let query = Query::new(response, SLOW);
        let mut pty = Pty::spawn(query.command(VIEW));
        assert_eq!(pty.finish().success(), title == "Simulation succeeded");
        assert!(pty.progress_rows > 0);
        let history = pty.history();
        for value in [title, detail, "Event 0", "data=0xcafe", "Method"] {
            assert!(history.contains(value), "missing {value}: {history}");
        }
        query.finish("simulateInvocation");
    }
}

#[test]
fn named_view_shows_decoded_value_and_method() {
    let bytes = xdrs::cxs::encode(&7_u64).unwrap();
    let query = Query::new(
        view(json!({"result":"return", "returnData":format!("0x{}", hex::encode(bytes))})),
        SLOW,
    );
    fs::write(
        query.root.path().join("Cargo.toml"),
        "[package]\nname = \"counter\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir(query.root.path().join("src")).unwrap();
    fs::write(
        query.root.path().join("src/lib.rs"),
        "#[contract]\nimpl Counter { pub fn get(&self) -> u64 { 7 } }\n",
    )
    .unwrap();
    let mut pty = Pty::spawn(query.command(&["view", "counter", "get"]));
    assert!(pty.finish().success(), "{}", pty.history());
    let history = pty.history();
    assert!(history.contains("Method"));
    assert!(history.contains("Return"));
    let compact: String = history
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '│')
        .collect();
    assert!(compact.contains("Return:\"7\""), "{history}");
    query.finish("simulateInvocation");
}

#[test]
fn rpc_errors_and_invalid_balances_stop_progress_without_success_cards() {
    for (args, reply, method) in [
        (vec!["balance"], balance("not-a-number"), "getBalance"),
        (
            vec!["balance"],
            json!({"error":{"code":-32000,"message":"node unavailable"}}),
            "getBalance",
        ),
        (
            VIEW.to_vec(),
            json!({"error":{"code":-32000,"message":"node unavailable"}}),
            "simulateInvocation",
        ),
    ] {
        let query = Query::new(reply, SLOW);
        let mut pty = Pty::spawn(query.command(&args));
        assert!(!pty.finish().success());
        assert!(pty.progress_rows > 0);
        let history = pty.history();
        assert!(history.contains("Error"), "{history}");
        assert!(!history.contains('╭'), "{history}");
        query.finish(method);
    }
}

#[test]
fn query_json_quiet_and_plain_modes_preserve_stream_contracts() {
    for flag in ["--json", "--quiet", "--no-ui"] {
        for (args, reply, method) in [
            (vec!["balance"], balance("0"), "getBalance"),
            (
                VIEW.to_vec(),
                view(json!({"result":"abort", "abortData":"0xdeadbeef"})),
                "simulateInvocation",
            ),
        ] {
            let query = Query::new(reply, Duration::ZERO);
            let mut command = query.command(&args);
            let output = command.arg(flag).output().unwrap();
            assert_eq!(output.status.success(), args[0] == "balance");
            if flag == "--json" {
                let json: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(json["ok"], args[0] == "balance");
                assert!(output.stderr.is_empty());
            } else {
                assert!(output.stdout.is_empty());
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(!stderr.contains('\x1b'));
                if flag == "--quiet" {
                    if args[0] == "view" {
                        assert!(
                            stderr.contains("failed to read the exception selector"),
                            "{stderr}"
                        );
                        assert!(!stderr.contains("Simulation aborted"), "{stderr}");
                    } else {
                        assert!(stderr.is_empty(), "{stderr}");
                    }
                } else if args[0] == "view" {
                    assert!(stderr.contains("Simulation aborted"), "{stderr}");
                } else {
                    assert!(stderr.contains("Balance: 0 native tokens"));
                }
            }
            query.finish(method);
        }
    }
}
