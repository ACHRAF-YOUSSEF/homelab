use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Server {
    child: Child,
    port: u16,
    _root: tempfile::TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "library",
            "output",
            "data/job_outputs/0123456789ab",
            "models",
        ] {
            fs::create_dir_all(root.path().join(path)).unwrap();
        }
        let srt = "1\n00:00:00,000 --> 00:00:02,000\nSaved subtitle\n";
        fs::write(
            root.path()
                .join("data/job_outputs/0123456789ab/movie.vi.srt"),
            srt,
        )
        .unwrap();
        fs::write(
            root.path().join("output/movie.vi.srt"),
            "overwritten shared export",
        )
        .unwrap();
        let jobs = json!([{"id":"0123456789ab","path":"movie.mkv","filename":"movie.mkv","source_language":"auto","detected_language":"vi","targets":[],"transcript":true,"status":"completed","progress":100,"review_enabled":false,"outputs":{"vi":{"path":"/old/mount/movie.vi.srt","name":"movie.vi.srt"}},"created_at":"2026-01-01T00:00:00+00:00","step_task":{"status":"running","step":"extraction","token":"old"},"steps":{"extraction":{"status":"running","progress":1},"transcription":{"status":"completed","progress":100}},"quality":{}}]);
        fs::write(
            root.path().join("data/jobs.json"),
            serde_json::to_vec(&jobs).unwrap(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut command = Command::new(env!("CARGO_BIN_EXE_subtitle-studio"));
        command
            .arg("serve")
            .env_clear()
            .env("SUBTITLE_LIBRARY_ROOT", root.path().join("library"))
            .env("SUBTITLE_OUTPUT_ROOT", root.path().join("output"))
            .env("SUBTITLE_DATA_ROOT", root.path().join("data"))
            .env("WHISPER_MODEL_ROOT", root.path().join("models"))
            .env("SUBTITLE_BIND_ADDRESS", "127.0.0.1")
            .env("SUBTITLE_PORT", port.to_string())
            .env("SUBTITLE_MAX_CLIENTS", "4")
            .env("SUBTITLE_MAX_STREAMS", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for key in ["PATH", "SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let child = command.spawn().unwrap();
        let mut server = Self {
            child,
            port,
            _root: root,
        };
        let started = Instant::now();
        loop {
            if let Ok(mut socket) = TcpStream::connect(("127.0.0.1", port)) {
                socket
                    .write_all(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    .unwrap();
                break;
            }
            assert!(server.child.try_wait().unwrap().is_none(), "server exited");
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "server did not start"
            );
            thread::sleep(Duration::from_millis(25));
        }
        server
    }
    fn request(&self, method: &str, target: &str, extra: &str, body: &str) -> (u16, String) {
        self.raw(&format!("{method} {target} HTTP/1.1\r\nHost: localhost:{}\r\n{extra}Content-Length: {}\r\n\r\n{body}",self.port,body.len()))
    }
    fn raw(&self, request: &str) -> (u16, String) {
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
        (
            status,
            response.split_once("\r\n\r\n").unwrap().1.to_string(),
        )
    }
}

#[test]
fn http_preserves_snapshots_recovers_interrupted_work_and_bounds_input() {
    let server = Server::start();
    let (status, body) = server.request("GET", "/api/jobs?page=1&page_size=5", "", "");
    assert_eq!(status, 200);
    let snapshot: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(snapshot["total"], 1);
    assert_eq!(snapshot["jobs"][0]["status"], "completed");
    assert_eq!(snapshot["jobs"][0]["step_task"]["status"], "failed");
    let (status, body) = server.request("GET", "/api/jobs/0123456789ab/files/vi", "", "");
    assert_eq!(status, 200);
    assert!(body.contains("Saved subtitle"));
    assert!(!body.contains("overwritten"));
    for target in [
        "/api/jobs?page=0",
        "/api/jobs?page_size=51",
        "/api/jobs/events?page=-1",
        "/api/jobs/events?status=unknown",
    ] {
        assert_eq!(server.request("GET", target, "", "").0, 400, "{target}");
    }
    assert_eq!(
        server
            .raw("GET /api/library HTTP/1.1\r\nHost: attacker.example\r\n\r\n")
            .0,
        403
    );
    assert_eq!(
        server
            .raw("GET /api/library HTTP/1.1\r\nHost: localhost\r\nHost: localhost\r\n\r\n")
            .0,
        400
    );
    assert_eq!(
        server
            .raw("POST /api/jobs HTTP/1.1\r\nHost: localhost\r\nContent-Length: 20000\r\n\r\n")
            .0,
        400
    );
    assert_eq!(server.raw("POST /api/jobs HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}").0,400);
    assert_eq!(
        server
            .request(
                "POST",
                "/api/jobs",
                "Content-Type: application/json\r\nOrigin: http://attacker.example\r\n",
                "{}"
            )
            .0,
        403
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/api/jobs/0123456789ab/review",
                "Content-Type: application/json\r\n",
                "{\"review_model\":\"translategemma\"}"
            )
            .0,
        400
    );
    let (status, body) = server.request("GET", "/api/resources", "", "");
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["cpu_threads"],
        2
    );
}

#[test]
fn sse_sends_named_paged_snapshot_and_enforces_stream_limit() {
    let server = Server::start();
    let mut socket = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket
        .write_all(b"GET /api/jobs/events?page=1&page_size=5 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = socket.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&bytes);
        if text.contains("event: jobs\ndata:") && text.ends_with("\n\n") {
            break;
        }
    }
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("retry: 3000"));
    assert!(text.contains("\"total\":1"));
    assert_eq!(server.request("GET", "/api/jobs/events", "", "").0, 429);
    assert_eq!(server.request("GET", "/api/health", "", "").0, 200);
}

#[test]
fn validate_cli_returns_failure_for_alignment_errors() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target.srt");
    let source = root.path().join("source.srt");
    fs::write(&target, "1\n00:00:01,000 --> 00:00:02,000\nTarget\n").unwrap();
    fs::write(&source, "1\n00:00:00,000 --> 00:00:02,000\nSource\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_subtitle-studio"))
        .arg("validate-srt")
        .arg(target)
        .arg("--source")
        .arg(source)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["structural_issues"], 1);
}
