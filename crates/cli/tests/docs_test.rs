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
fn managed_docs_stops_if_the_starting_process_disappears_before_handoff() {
    let dir = snapshot();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .args(["docs", "--managed", "--non-interactive", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let startup = serde_json::json!({
        "root": dir.path(), "port": 0, "control": { "token": "a".repeat(64) },
    });
    let mut pipe = server.0.stdin.take().unwrap();
    writeln!(pipe, "{startup}").unwrap();
    // Closing this pipe models the parent being interrupted before it saves
    // the managed server's state. There is no controller that can stop it yet.
    drop(pipe);
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("abandoned docs startup left an untracked server running");
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
    assert!(request(address, "GET", "/get-started/").contains("Get Started"));
    assert_eq!(
        ready["data"]["markdownIndex"],
        root.join("markdown/index.md").to_str().unwrap()
    );
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
    let identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("snapshot.json")).unwrap()).unwrap();
    std::fs::write(
        dir.path().join("markdown/manifest.json"),
        serde_json::to_vec(
            &serde_json::json!({ "schemaVersion": 1, "snapshot": identity, "pages": [] }),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}

#[test]
fn external_markdown_export_rejects_missing_malformed_or_mismatched_identity() {
    for case in [
        "missing",
        "malformed",
        "schema",
        "revision",
        "dirty",
        "guide",
    ] {
        let source = markdown_snapshot();
        let manifest_path = source.path().join("markdown/manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        match case {
            "missing" => std::fs::remove_file(&manifest_path).unwrap(),
            "malformed" => std::fs::write(&manifest_path, "{").unwrap(),
            other => {
                match other {
                    "schema" => manifest["schemaVersion"] = 2.into(),
                    "revision" => manifest["snapshot"]["sourceRevision"] = "b".repeat(40).into(),
                    "dirty" => manifest["snapshot"]["sourceDirty"] = true.into(),
                    "guide" => manifest["snapshot"]["agentGuideCliVersion"] = "999.0.0".into(),
                    _ => unreachable!(),
                }
                std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            }
        }
        let output = source.path().join("export");
        let result = Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .args(["docs", "export", "--dir"])
            .arg(source.path())
            .arg("--output")
            .arg(&output)
            .arg("--json")
            .output()
            .unwrap();
        assert!(!result.status.success(), "accepted {case} manifest");
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["ok"], false);
        assert!(!output.exists(), "created output for {case} manifest");
    }
}

#[test]
fn bundled_markdown_export_contains_every_page_and_section_without_network_access() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("export");
    let result = Command::new(env!("CARGO_BIN_EXE_groundhog"))
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .args(["docs", "export", "--offline", "--json", "--output"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["snapshot"], report["data"]["snapshot"]);
    assert_eq!(manifest["snapshot"]["sourceDirty"], false);
    let pages = manifest["pages"].as_array().unwrap();
    assert!(pages.len() > 50);
    for page in pages {
        assert!(output.join(page["path"].as_str().unwrap()).is_file());
        assert!(
            output
                .join("sections")
                .join(format!("{}.md", page["section"].as_str().unwrap()))
                .is_file()
        );
    }
    assert!(output.join("index.md").is_file());
    assert!(output.join("full.md").is_file());
    let quickstart = std::fs::read_to_string(output.join("pages/get-started/react.md")).unwrap();
    assert!(quickstart.contains("useContractView"));
    assert!(quickstart.contains("```tsx"));
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
        output
            .canonicalize()
            .unwrap()
            .join("index.md")
            .to_str()
            .unwrap()
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
struct ServiceCleanup(std::path::PathBuf);

#[cfg(unix)]
impl Drop for ServiceCleanup {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .env("XDG_CACHE_HOME", &self.0)
            .args(["docs", "stop", "--json"])
            .output();
    }
}

#[cfg(unix)]
#[test]
fn background_docs_reports_readiness_reuses_and_stops_only_its_instance() {
    use std::os::unix::fs::PermissionsExt;
    let dir = markdown_snapshot();
    let cache = tempfile::tempdir().unwrap();
    let _cleanup = ServiceCleanup(cache.path().to_owned());
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
    let backup = state.with_file_name("state.backup.json");
    assert_eq!(
        std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::write(&state, "{").unwrap();
    let (ok, recovered) = invoke(&["docs", "status"]);
    assert!(ok, "{recovered}");
    assert_eq!(recovered["data"]["server"]["url"], url);
    assert_eq!(
        std::fs::read(&state).unwrap(),
        std::fs::read(&backup).unwrap()
    );
    std::fs::remove_file(&state).unwrap();
    let (ok, recovered_start) = invoke(&args);
    assert!(ok, "{recovered_start}");
    assert_eq!(recovered_start["data"]["reused"], true);
    let (ok, conflict) = invoke(&[
        "docs",
        "--background",
        "--port",
        "4321",
        "--dir",
        dir.path().to_str().unwrap(),
    ]);
    assert!(!ok, "{conflict}");
    std::fs::write(&state, "{").unwrap();
    let (ok, stopped) = invoke(&["docs", "stop"]);
    assert!(ok, "{stopped}");
    assert_eq!(stopped["data"]["running"], false);
    assert!(TcpStream::connect(address).is_err());
    assert!(dir.path().join("markdown/index.md").is_file());
    assert!(!state.exists());
    assert!(!backup.exists());
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

#[test]
fn background_docs_preserves_unrecoverable_state_without_claiming_it_stopped() {
    let cache = tempfile::tempdir().unwrap();
    let service = cache.path().join("groundhog/docs-service");
    std::fs::create_dir_all(&service).unwrap();
    let state = service.join("state.json");
    let backup = service.join("state.backup.json");
    std::fs::write(&state, "{").unwrap();
    std::fs::write(&backup, "{}").unwrap();
    for action in ["status", "stop", "--background"] {
        let result = Command::new(env!("CARGO_BIN_EXE_groundhog"))
            .env("XDG_CACHE_HOME", cache.path())
            .args(["docs", action, "--json"])
            .output()
            .unwrap();
        assert!(!result.status.success());
        let failure: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert!(failure["data"].get("running").is_none());
        assert_eq!(std::fs::read(&state).unwrap(), b"{");
        assert_eq!(std::fs::read(&backup).unwrap(), b"{}");
        assert!(!service.join("server.log").exists());
    }
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
