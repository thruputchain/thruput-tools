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
