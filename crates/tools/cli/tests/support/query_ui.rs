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
        fs::write(root.path().join("groundhog.toml"), format!(
            "default_profile = \"local\"\n[profile.local]\nrpc_url = \"{rpc}\"\ndefault_wallet = \"primary\"\n[profile.local.contracts]\ncounter = \"{address}\"\n[profile.local.wallets.primary]\naddress = \"{address}\"\nowner_public_key = \"{address}\"\n",
            address = address(),
        )).unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
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
            let mut reply = response;
            reply["jsonrpc"] = "2.0".into();
            reply["id"] = request["id"].clone();
            let body = reply.to_string();
            thread::sleep(delay);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            request
        });
        Self { root, server }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("GROUNDHOG_PROFILE")
            .env_remove("GROUNDHOG_BUNDLE_MANIFEST")
            .env_remove("NO_COLOR")
            .env_remove("CARGO_TARGET_DIR");
        command
    }

    fn finish(self, method: &str) {
        let request = self.server.join().unwrap();
        assert_eq!(request["method"], method);
    }
}

fn address() -> String {
    format!("0x{}", "11".repeat(32))
}

fn balance(raw: &str) -> Value {
    json!({"result": {"address": address(), "rawBalance": raw, "height": 34}})
}

fn view(output: Value) -> Value {
    let mut response = json!({"result": {"events": [], "height": 12}});
    response["result"]["output"] = output;
    response
}

const VIEW: &[&str] = &["view", "counter", "0x0102030405060708"];
const SLOW: Duration = Duration::from_millis(500);

#[test]
fn init_declines_preserve_scrollback_and_show_partial_setup() {
    let root = TempDir::new().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
    command
        .arg("init")
        .current_dir(root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env_remove("GROUNDHOG_PROFILE")
        .env_remove("NO_COLOR");
    let mut pty = Pty::spawn(command);
    pty.until(|pty| {
        String::from_utf8_lossy(&pty.raw).contains("Generate a local signing key now?")
    });
    pty.master.write_all(b"n\n").unwrap();
    pty.until(|pty| String::from_utf8_lossy(&pty.raw).contains("Create a starter project?"));
    pty.master.write_all(b"n\n").unwrap();
    assert!(!pty.finish().success());
    let history = pty.history();
    assert!(history.contains("PREEXISTING-SHELL-LINE"));
    assert!(
        history.contains("skipped") && history.contains("Partially completed"),
        "{history}"
    );
}

#[test]
fn inspection_commands_render_complete_empty_and_pending_results() {
    let hash = address();
    for (args, result, method, title) in [
        (
            vec!["storage", "counter", &hash],
            json!({"item":null,"blockHeight":12}),
            "groundhog_getStorageAt",
            "Storage",
        ),
        (
            vec!["tx", "show", &hash],
            json!({"status":"pending"}),
            "groundhog_getTransactionStatus",
            "Transaction",
        ),
        (
            vec!["tx", "list"],
            json!({"refs":[],"cursor":"0xaabb","latestBlock":12,"oldestBlock":0}),
            "groundhog_getTransactionsByAddress",
            "Transaction history",
        ),
        (
            vec!["events", "list", "counter"],
            json!({"events":[],"cursor":"0x","latestBlock":12,"oldestBlock":0}),
            "groundhog_getEvents",
            "Events",
        ),
        (
            vec!["token", "transfers", "counter"],
            json!({"events":[],"cursor":"0x","latestBlock":12,"oldestBlock":0}),
            "groundhog_getEvents",
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
    for raw in ["0", "340282366920938463463374607431768211455"] {
        let query = Query::new(balance(raw), SLOW);
        let mut pty = Pty::spawn(query.command(&["balance"]));
        pty.until(|pty| pty.queries > 0);
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
        query.finish("groundhog_getBalance");
    }
}

#[test]
fn view_cards_distinguish_raw_returns_aborts_faults_and_events() {
    for (output, title, detail) in [
        (
            json!({"result":"return", "returnData":"0x0102"}),
            "View succeeded",
            "0x0102",
        ),
        (
            json!({"result":"return", "returnData":"0x"}),
            "View succeeded",
            "Return (hex)",
        ),
        (
            json!({"result":"abort", "abortData":"0xdeadbeef"}),
            "View aborted",
            "0xdeadbeef",
        ),
        (json!({"result":"fault"}), "View faulted", "Height"),
    ] {
        let mut response = view(output);
        response["result"]["events"] =
            json!([{"contractAddress": address(), "topics": [], "data": "0xcafe"}]);
        let query = Query::new(response, SLOW);
        let mut pty = Pty::spawn(query.command(VIEW));
        assert!(
            pty.finish().success(),
            "view outcomes retain the existing exit status"
        );
        assert!(pty.queries > 0);
        let history = pty.history();
        for value in [title, detail, "Event 0", "data=0xcafe", "Entrypoint"] {
            assert!(history.contains(value), "missing {value}: {history}");
        }
        if title == "View aborted" {
            assert!(
                history.find("View aborted").unwrap()
                    < history.find("Could not decode abort data").unwrap()
            );
        }
        query.finish("groundhog_view");
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
    assert!(history.contains("Value"));
    assert!(!history.contains("Return (hex)"));
    let compact: String = history
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '│')
        .collect();
    assert!(compact.contains("Value:7"), "{history}");
    query.finish("groundhog_view");
}

#[test]
fn rpc_errors_and_invalid_balances_stop_progress_without_success_cards() {
    for (args, reply, method) in [
        (
            vec!["balance"],
            balance("not-a-number"),
            "groundhog_getBalance",
        ),
        (
            vec!["balance"],
            json!({"error":{"code":-32000,"message":"node unavailable"}}),
            "groundhog_getBalance",
        ),
        (
            VIEW.to_vec(),
            json!({"error":{"code":-32000,"message":"node unavailable"}}),
            "groundhog_view",
        ),
    ] {
        let query = Query::new(reply, SLOW);
        let mut pty = Pty::spawn(query.command(&args));
        assert!(!pty.finish().success());
        assert!(pty.queries > 0);
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
            (vec!["balance"], balance("0"), "groundhog_getBalance"),
            (
                VIEW.to_vec(),
                view(json!({"result":"abort", "abortData":"0xdeadbeef"})),
                "groundhog_view",
            ),
        ] {
            let query = Query::new(reply, Duration::ZERO);
            let mut command = query.command(&args);
            let output = command.arg(flag).output().unwrap();
            assert!(output.status.success());
            if flag == "--json" {
                let json: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(json["ok"], true);
                assert!(output.stderr.is_empty());
            } else {
                assert!(output.stdout.is_empty());
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(!stderr.contains('\x1b'));
                if args[0] == "view" {
                    assert!(stderr.contains("View aborted"));
                } else if flag == "--quiet" {
                    assert!(stderr.is_empty());
                } else {
                    assert!(stderr.contains("Balance: 0 HOG"));
                }
            }
            query.finish(method);
        }
    }
}

#[test]
fn fast_monochrome_query_keeps_the_card_without_flashing_progress() {
    let query = Query::new(balance("1"), Duration::ZERO);
    let mut command = query.command(&["balance"]);
    command.env("NO_COLOR", "1");
    let started = Instant::now();
    let mut pty = Pty::spawn(command);
    assert!(pty.finish().success());
    if started.elapsed() < Duration::from_millis(150) {
        assert_eq!(pty.queries, 0);
    }
    let history = pty.history();
    assert!(history.contains("Raw balance"));
    assert!(history.contains("0.0000001 HOG"));
    assert!(!String::from_utf8_lossy(&pty.raw).contains("\x1b[38;"));
    query.finish("groundhog_getBalance");
}
