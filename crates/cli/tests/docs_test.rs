use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn readiness(server: &mut Server) -> serde_json::Value {
    let stdout = server.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(15)).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let report: groundhog_cli::docs_json::DocsReport =
        serde_json::from_value(ready["data"].clone()).unwrap();
    assert_eq!(
        report.schema_version,
        groundhog_cli::docs_json::DOCS_JSON_SCHEMA_VERSION
    );
    assert_eq!(serde_json::to_value(report).unwrap(), ready["data"]);
    ready
}

fn snapshot() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("start")).unwrap();
    std::fs::write(dir.path().join("index.html"), "<h1>Local docs</h1>").unwrap();
    std::fs::write(dir.path().join("start/index.html"), "<h1>Start</h1>").unwrap();
    std::fs::write(dir.path().join("style.css"), "body { color: green; }").unwrap();
    std::fs::write(
        dir.path().join("snapshot.json"),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1,
            "sourceRevision": "a".repeat(40),
            "sourceDirty": false,
            "agentGuideCliVersion": "0.0.0-test"
        }))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn request(address: &str, method: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn docs_serves_static_snapshot_and_stops_on_signal() {
    let dir = snapshot();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .args(["docs", "--dir"])
            .arg(dir.path())
            .args(["--port", "0", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let ready = readiness(&mut server);
    assert_eq!(ready["ok"], true);
    assert_eq!(ready["command"], "docs");
    assert_eq!(ready["data"]["schemaVersion"], 1);
    assert_eq!(ready["data"]["guideMatchesCli"], false);
    let url = ready["data"]["url"].as_str().unwrap();
    let address = url.strip_prefix("http://").unwrap().trim_end_matches('/');
    assert!(address.starts_with("127.0.0.1:"));
    assert!(request(address, "GET", "/").contains("<h1>Local docs</h1>"));
    assert!(request(address, "GET", "/start/").contains("<h1>Start</h1>"));
    let css = request(address, "GET", "/style.css");
    assert!(css.contains("text/css"));
    assert!(css.contains("color: green"));
    let head = request(address, "HEAD", "/style.css");
    assert!(head.starts_with("HTTP/1.1 200"));
    assert!(!head.contains("color: green"));
    let outside = tempfile::NamedTempFile::new_in(dir.path().parent().unwrap()).unwrap();
    std::fs::write(outside.path(), "outside snapshot sentinel").unwrap();
    let name = outside.path().file_name().unwrap().to_str().unwrap();
    for path in [
        "/missing".to_owned(),
        format!("/../{name}"),
        format!("/%2e%2e/{name}"),
    ] {
        assert!(
            request(address, "GET", &path).starts_with("HTTP/1.1 404"),
            "{path}"
        );
    }
    assert!(request(address, "POST", "/").starts_with("HTTP/1.1 405"));
    #[cfg(unix)]
    {
        // Hold an unfinished request open: shutdown must not wait for this peer.
        let mut stalled = TcpStream::connect(address).unwrap();
        stalled.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        assert!(
            Command::new("kill")
                .args(["-TERM", &server.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        for _ in 0..100 {
            if let Some(status) = server.0.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("docs server did not stop on SIGTERM");
    }
}

#[test]
fn docs_warns_about_guide_version_only_for_explicit_snapshots() {
    let external = snapshot();
    let cache = tempfile::tempdir().unwrap();
    for use_external in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_groundhog"));
        command
            .args(["docs", "--port", "0"])
            .env("XDG_CACHE_HOME", cache.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if use_external {
            command.arg("--dir").arg(external.path());
        }
        let mut server = Server(command.spawn().unwrap());
        let stderr = server.0.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut output = String::new();
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                output.push_str(&line);
                output.push('\n');
                if line.contains("Ctrl-C to stop.") {
                    break;
                }
            }
            let _ = tx.send(output);
        });
        let output = rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert!(output.contains("Documentation ready at"), "{output}");
        assert!(output.contains("agent guide CLI"), "{output}");
        assert_eq!(
            output.contains("warning: guide targets CLI"),
            use_external,
            "{output}"
        );
    }
}

#[test]
fn docs_rejects_invalid_snapshots_and_reports_busy_port() {
    let dir = snapshot();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let run = |port: u16| {
        Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .args(["docs", "--dir"])
            .arg(dir.path())
            .args(["--json", "--port", &port.to_string()])
            .output()
            .unwrap()
    };
    let busy = run(listener.local_addr().unwrap().port());
    assert!(!busy.status.success());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("choose another --port"));
    std::fs::write(dir.path().join("snapshot.json"), "{broken").unwrap();
    let invalid = run(0);
    assert!(!invalid.status.success());
    let error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(error["error"]["code"], "docs_error");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc", dir.path().join("escape")).unwrap();
        let symlink = run(0);
        assert!(!symlink.status.success());
        assert!(String::from_utf8_lossy(&symlink.stderr).contains("not a regular file"));
    }
}

#[test]
fn docs_defaults_to_bundled_cache_without_network_or_project_setup() {
    let home = tempfile::tempdir().unwrap();
    let cache = home.path().join("custom-cache");
    let start = || {
        let mut server = Server(
            Command::new(env!("CARGO_BIN_EXE_groundhog"))
                .args(["docs", "--port", "0", "--json"])
                .current_dir(home.path())
                .env("HOME", home.path())
                .env("XDG_CACHE_HOME", &cache)
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("HTTPS_PROXY", "http://127.0.0.1:1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let ready = readiness(&mut server);
        (server, ready)
    };
    let (first, ready) = start();
    let root = std::path::PathBuf::from(ready["data"]["path"].as_str().unwrap());
    assert!(root.starts_with(cache.canonicalize().unwrap().join("groundhog/docs")));
    assert_eq!(ready["data"]["snapshot"]["sourceDirty"], false);
    let url = ready["data"]["url"].as_str().unwrap();
    let address = url.strip_prefix("http://").unwrap().trim_end_matches('/');
    assert!(request(address, "GET", "/start/").contains("Start Here"));
    let modified = std::fs::metadata(root.join("index.html"))
        .unwrap()
        .modified()
        .unwrap();
    drop(first);
    let (second, reused) = start();
    assert_eq!(reused["data"]["path"], ready["data"]["path"]);
    assert_eq!(
        std::fs::metadata(root.join("index.html"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    drop(second);
    std::fs::remove_file(root.join("index.html")).unwrap();
    let (_third, recovered) = start();
    assert_eq!(recovered["data"]["path"], ready["data"]["path"]);
    assert!(root.join("index.html").is_file());
}

fn markdown_snapshot() -> tempfile::TempDir {
    let dir = snapshot();
    std::fs::create_dir_all(dir.path().join("markdown/contracts")).unwrap();
    std::fs::write(
        dir.path().join("markdown/index.md"),
        "# Docs\n\n[Contracts](contracts/start.md)\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("markdown/contracts/start.md"),
        "# Contracts\n\n```sh\ngroundhog build\n```\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("markdown/all.md"), "# Full docs\n").unwrap();
    dir
}

#[test]
fn markdown_export_is_portable_and_preserves_existing_destinations() {
    let source = markdown_snapshot();
    let target = tempfile::tempdir().unwrap();
    let output = target.path().join("export");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .args(["docs", "export", "--dir"])
            .arg(source.path())
            .arg("--output")
            .arg(&output)
            .arg("--json")
            .output()
            .unwrap()
    };
    let result = run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["command"], "docs export");
    assert_eq!(
        report["data"]["index"],
        output.join("index.md").to_str().unwrap()
    );
    assert_eq!(
        std::fs::read(output.join("contracts/start.md")).unwrap(),
        std::fs::read(source.path().join("markdown/contracts/start.md")).unwrap()
    );
    assert!(output.join("snapshot.json").is_file());
    std::fs::write(output.join("keep.md"), "user content").unwrap();
    let repeated = run();
    assert!(!repeated.status.success());
    let error: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(error["ok"], false);
    assert_eq!(
        std::fs::read_to_string(output.join("keep.md")).unwrap(),
        "user content"
    );
}

#[test]
fn legacy_snapshot_export_reports_missing_markdown_without_creating_output() {
    let source = snapshot();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("export");
    let result = Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .args(["docs", "export", "--dir"])
        .arg(source.path())
        .arg("--output")
        .arg(&output)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no Markdown export")
    );
    assert!(!output.exists());
}

#[cfg(unix)]
#[test]
fn background_docs_reports_readiness_reuses_and_stops_only_its_instance() {
    use std::os::unix::fs::PermissionsExt;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = Command::new(env!("CARGO_BIN_EXE_groundhog"))
                .env("XDG_CACHE_HOME", &self.0)
                .args(["docs", "stop", "--json"])
                .output();
        }
    }
    let dir = markdown_snapshot();
    let cache = tempfile::tempdir().unwrap();
    let _cleanup = Cleanup(cache.path().to_owned());
    let invoke = |args: &[&str]| {
        let result = Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .env("XDG_CACHE_HOME", cache.path())
            .args(args)
            .arg("--json")
            .output()
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&result.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&result.stderr)));
        (result.status.success(), report)
    };
    let args = [
        "docs",
        "--background",
        "--port",
        "0",
        "--dir",
        dir.path().to_str().unwrap(),
    ];
    let (ok, first) = invoke(&args);
    assert!(ok, "{first}");
    assert_eq!(first["data"]["running"], true);
    assert_eq!(first["data"]["reused"], false);
    let url = first["data"]["server"]["url"].as_str().unwrap();
    let address = url.strip_prefix("http://").unwrap().trim_end_matches('/');
    assert!(request(address, "GET", "/markdown/index.md").contains("[Contracts]"));
    assert!(request(address, "POST", "/__groundhog_docs/stop").starts_with("HTTP/1.1 403"));
    assert!(
        first["data"]["server"]["markdownIndex"]
            .as_str()
            .unwrap()
            .ends_with("markdown/index.md")
    );
    let state = cache.path().join("groundhog/docs-service/state.json");
    let mut stale_state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(
        std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (ok, repeated) = invoke(&args);
    assert!(ok);
    assert_eq!(repeated["data"]["reused"], true);
    assert_eq!(repeated["data"]["server"]["url"], url);
    let (ok, status) = invoke(&["docs", "status"]);
    assert!(ok);
    assert_eq!(status["data"]["running"], true);
    let (ok, conflict) = invoke(&[
        "docs",
        "--background",
        "--port",
        "4321",
        "--dir",
        dir.path().to_str().unwrap(),
    ]);
    assert!(!ok, "{conflict}");
    let (ok, stopped) = invoke(&["docs", "stop"]);
    assert!(ok, "{stopped}");
    assert_eq!(stopped["data"]["running"], false);
    assert!(TcpStream::connect(address).is_err());
    assert!(dir.path().join("markdown/index.md").is_file());
    assert!(!state.exists());
    assert!(invoke(&["docs", "stop"]).0);
    assert_eq!(invoke(&["docs", "status"]).1["data"]["running"], false);
    // A different server may reuse a stale instance's port and return 204.
    let (address, peer) = unrelated_http_server();
    stale_state["report"]["url"] = format!("http://{address}/").into();
    std::fs::write(&state, serde_json::to_vec(&stale_state).unwrap()).unwrap();
    assert!(invoke(&["docs", "stop"]).0);
    assert!(peer.join().unwrap());
    assert!(!state.exists());
}

#[cfg(unix)]
fn unrelated_http_server() -> (std::net::SocketAddr, std::thread::JoinHandle<bool>) {
    let unrelated = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = unrelated.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = unrelated.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = String::new();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            request.push_str(&line);
        }
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .unwrap();
        request.starts_with("GET /__groundhog_docs/health ")
    });
    (address, peer)
}

#[cfg(unix)]
#[test]
fn background_start_failure_does_not_leave_running_state() {
    let dir = snapshot();
    let cache = tempfile::tempdir().unwrap();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .env("XDG_CACHE_HOME", cache.path())
        .args(["docs", "--background", "--json", "--dir"])
        .arg(dir.path())
        .arg("--port")
        .arg(occupied.local_addr().unwrap().port().to_string())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("startup failed")
    );
    assert!(
        !cache
            .path()
            .join("groundhog/docs-service/state.json")
            .exists()
    );
    assert!(
        std::fs::read_to_string(cache.path().join("groundhog/docs-service/server.log"))
            .unwrap()
            .contains("cannot listen")
    );
}
