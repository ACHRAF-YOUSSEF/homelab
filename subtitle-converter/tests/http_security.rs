#![forbid(unsafe_code)]

use base64::Engine;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const PASSWORD: &str = "fixture-password-long-enough";
const WIDGET_TOKEN: &str = "fixture-widget-token-01234567890";
const WEBHOOK_TOKEN: &str = "fixture-webhook-token-0123456789";

struct Server {
    _temp: TempDir,
    child: Child,
    port: u16,
    root: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn start() -> Self {
        let temp = tempfile::tempdir().expect("temporary test directory");
        let root = temp.path().join("media");
        let data = temp.path().join("data");
        fs::create_dir_all(&root).expect("media fixture directory");
        fs::create_dir_all(&data).expect("state fixture directory");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve local port");
        let port = listener.local_addr().expect("local address").port();
        drop(listener);
        let port_text = port.to_string();

        let child = Command::new(env!("CARGO_BIN_EXE_subforge"))
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &port_text,
                "--data",
                data.to_str().expect("data path is UTF-8"),
                "--root",
                root.to_str().expect("root path is UTF-8"),
                "--no-auto-scan",
            ])
            .env("CONVERTER_UI_PASSWORD", PASSWORD)
            .env("CONVERTER_ALLOWED_HOSTS", "localhost,127.0.0.1")
            .env("CONVERTER_WIDGET_TOKEN", WIDGET_TOKEN)
            .env("CONVERTER_WEBHOOK_TOKEN", WEBHOOK_TOKEN)
            .env("CONVERTER_ENABLE_OCR", "false")
            .env("CONVERTER_SCAN_INTERVAL", "3600")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start isolated Subforge process");

        let server = Self {
            _temp: temp,
            child,
            port,
            root,
        };
        server.wait_until_ready();
        server
    }

    fn wait_until_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if let Ok(reply) = self.request("GET", "/health", &[], None)
                && reply.status == 200
                && reply.body.get("ok") == Some(&Value::Bool(true))
            {
                return;
            }
            thread::sleep(Duration::from_millis(30));
        }
        panic!("Subforge did not become ready on its isolated loopback port");
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> std::io::Result<Response> {
        let mut socket = TcpStream::connect(("127.0.0.1", self.port))?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        socket.set_write_timeout(Some(Duration::from_secs(3)))?;
        let body = body.unwrap_or("");
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        )?;
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("host"))
        {
            write!(socket, "Host: 127.0.0.1:{}\r\n", self.port)?;
        }
        for (name, value) in headers {
            write!(socket, "{name}: {value}\r\n")?;
        }
        socket.write_all(b"\r\n")?;
        socket.write_all(body.as_bytes())?;
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes)?;
        let split = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response headers");
        let head = std::str::from_utf8(&bytes[..split]).expect("HTTP response header encoding");
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
            .expect("HTTP status code");
        let body = &bytes[split + 4..];
        let body = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(body).unwrap_or(Value::Null)
        };
        Ok(Response { status, body })
    }

    fn auth_header(&self) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("admin:{PASSWORD}"))
        )
    }

    fn csrf(&self) -> String {
        let auth = self.auth_header();
        self.request("GET", "/api/session", &[("Authorization", &auth)], None)
            .expect("session request")
            .body["csrf_token"]
            .as_str()
            .expect("CSRF token")
            .to_owned()
    }

    fn job_request(&self, csrf: &str, paths: &[String]) -> Response {
        let auth = self.auth_header();
        let body = json!({"kind":"extract", "paths":paths, "languages":["en"]}).to_string();
        self.request(
            "POST",
            "/api/jobs",
            &[
                ("Authorization", &auth),
                ("X-CSRF-Token", csrf),
                ("Content-Type", "application/json"),
            ],
            Some(&body),
        )
        .expect("job request")
    }
}

struct Response {
    status: u16,
    body: Value,
}

fn utf8_path(path: &Path) -> String {
    path.to_str().expect("fixture path is UTF-8").to_owned()
}

#[test]
fn dashboard_http_auth_csrf_host_widget_and_webhook_boundaries() {
    let server = Server::start();

    assert_eq!(
        server.request("GET", "/health", &[], None).unwrap().status,
        200
    );
    assert_eq!(
        server
            .request("GET", "/api/status", &[], None)
            .unwrap()
            .status,
        401
    );

    let auth = server.auth_header();
    let bad_host = server
        .request("GET", "/health", &[("Host", "attacker.invalid")], None)
        .unwrap();
    assert_eq!(bad_host.status, 403);

    let bad_origin = server
        .request(
            "GET",
            "/api/session",
            &[
                ("Authorization", &auth),
                ("Origin", "http://attacker.invalid"),
            ],
            None,
        )
        .unwrap();
    assert_eq!(bad_origin.status, 403);

    let missing_csrf = server
        .request(
            "PUT",
            "/api/settings",
            &[
                ("Authorization", &auth),
                ("Content-Type", "application/json"),
            ],
            Some(r#"{"languages":["en"]}"#),
        )
        .unwrap();
    assert_eq!(missing_csrf.status, 403);

    let csrf = server.csrf();
    let selected_language = server
        .request(
            "PUT",
            "/api/settings",
            &[
                ("Authorization", &auth),
                ("X-CSRF-Token", &csrf),
                ("Content-Type", "application/json"),
            ],
            Some(r#"{"languages":["en","fr","pt-BR"]}"#),
        )
        .unwrap();
    assert_eq!(selected_language.status, 200);
    assert_eq!(
        selected_language.body["languages"],
        json!(["en", "fr", "pt"])
    );

    let widget = server
        .request(
            "GET",
            "/api/widget",
            &[("Authorization", &format!("Bearer {WIDGET_TOKEN}"))],
            None,
        )
        .unwrap();
    assert_eq!(widget.status, 200);
    assert_eq!(
        server
            .request("GET", "/api/widget", &[], None)
            .unwrap()
            .status,
        401
    );

    let webhook_test = server
        .request(
            "POST",
            "/hooks/sonarr",
            &[
                ("X-Webhook-Token", WEBHOOK_TOKEN),
                ("Content-Type", "application/json"),
            ],
            Some(r#"{"eventType":"Test"}"#),
        )
        .unwrap();
    assert_eq!(webhook_test.status, 200);
    assert_eq!(
        server
            .request(
                "POST",
                "/hooks/sonarr",
                &[("Content-Type", "application/json")],
                Some(r#"{"eventType":"Test"}"#),
            )
            .unwrap()
            .status,
        401
    );
}

#[test]
fn manual_extraction_rejects_traversal_and_symlinks_without_touching_targets() {
    let server = Server::start();
    assert!(server.root.is_dir());
    let csrf = server.csrf();
    let outside = server._temp.path().join("outside.srt");
    fs::write(&outside, "must remain untouched\n").expect("outside fixture");

    let traversal = server.job_request(&csrf, &[utf8_path(&outside)]);
    assert_eq!(traversal.status, 400);
    assert_eq!(
        fs::read_to_string(&outside).unwrap(),
        "must remain untouched\n"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = server.root.join("escape.srt");
        symlink(&outside, &link).expect("fixture symlink");
        let escaped = server.job_request(&csrf, &[utf8_path(&link)]);
        assert_eq!(escaped.status, 400);
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "must remain untouched\n"
        );
    }
}
