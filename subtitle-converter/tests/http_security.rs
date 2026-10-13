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
    data: PathBuf,
    ffprobe: Option<PathBuf>,
    mkvmerge: Option<PathBuf>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn start() -> Self {
        Self::start_with_probe_delay(None)
    }

    #[cfg(unix)]
    fn start_with_slow_probe() -> Self {
        Self::start_with_probe_delay(Some(Duration::from_millis(500)))
    }

    fn start_with_probe_delay(delay: Option<Duration>) -> Self {
        let temp = tempfile::tempdir().expect("temporary test directory");
        let root = temp.path().join("media");
        let data = temp.path().join("data");
        fs::create_dir_all(&root).expect("media fixture directory");
        fs::create_dir_all(&data).expect("state fixture directory");
        let (ffprobe, mkvmerge) = if let Some(delay) = delay {
            #[cfg(unix)]
            {
                let path = temp.path().join("fixture-ffprobe.sh");
                let seconds = format!("{}.{:03}", delay.as_secs(), delay.subsec_millis());
                fs::write(
                    &path,
                    format!("#!/bin/sh\nsleep {seconds}\nprintf '{{\"streams\":[]}}'\n"),
                )
                .expect("write local fake ffprobe");
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    .expect("make local fake ffprobe executable");
                let mkvmerge = temp.path().join("fixture-mkvmerge.sh");
                fs::write(&mkvmerge, "#!/bin/sh\nprintf '{\"tracks\":[]} '\n")
                    .expect("write local fake mkvmerge");
                fs::set_permissions(&mkvmerge, fs::Permissions::from_mode(0o700))
                    .expect("make local fake mkvmerge executable");
                (Some(path), Some(mkvmerge))
            }
            #[cfg(not(unix))]
            {
                let _ = delay;
                (None, None)
            }
        } else {
            (None, None)
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve local port");
        let port = listener.local_addr().expect("local address").port();
        drop(listener);
        let port_text = port.to_string();

        let mut command = Command::new(env!("CARGO_BIN_EXE_subforge"));
        command
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
            .env("CONVERTER_SCAN_INTERVAL", "3600");
        if let Some(path) = &ffprobe {
            command.env("CONVERTER_FFPROBE", path);
        }
        if let Some(path) = &mkvmerge {
            command.env("CONVERTER_MKVMERGE", path);
        }
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start isolated Subforge process");

        let server = Self {
            _temp: temp,
            child,
            port,
            root,
            data,
            ffprobe,
            mkvmerge,
        };
        server.wait_until_ready();
        server
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let port_text = self.port.to_string();
        let mut command = Command::new(env!("CARGO_BIN_EXE_subforge"));
        command
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &port_text,
                "--data",
                self.data.to_str().expect("data path is UTF-8"),
                "--root",
                self.root.to_str().expect("root path is UTF-8"),
                "--no-auto-scan",
            ])
            .env("CONVERTER_UI_PASSWORD", PASSWORD)
            .env("CONVERTER_ALLOWED_HOSTS", "localhost,127.0.0.1")
            .env("CONVERTER_WIDGET_TOKEN", WIDGET_TOKEN)
            .env("CONVERTER_WEBHOOK_TOKEN", WEBHOOK_TOKEN)
            .env("CONVERTER_ENABLE_OCR", "false")
            .env("CONVERTER_SCAN_INTERVAL", "3600");
        if let Some(path) = &self.ffprobe {
            command.env("CONVERTER_FFPROBE", path);
        }
        if let Some(path) = &self.mkvmerge {
            command.env("CONVERTER_MKVMERGE", path);
        }
        self.child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("restart isolated Subforge process");
        self.wait_until_ready();
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

    fn enqueue_inspection(&self, csrf: &str) -> String {
        let auth = self.auth_header();
        let body = json!({"kind":"inspect"}).to_string();
        let response = self
            .request(
                "POST",
                "/api/jobs",
                &[
                    ("Authorization", &auth),
                    ("X-CSRF-Token", csrf),
                    ("Content-Type", "application/json"),
                ],
                Some(&body),
            )
            .expect("inspection request");
        assert_eq!(response.status, 202);
        response.body["job"]["id"]
            .as_str()
            .expect("inspection job id")
            .to_owned()
    }

    #[cfg(unix)]
    fn cancel_job(&self, csrf: &str, id: &str) -> Response {
        let auth = self.auth_header();
        self.request(
            "POST",
            &format!("/api/jobs/{id}/cancel"),
            &[
                ("Authorization", &auth),
                ("X-CSRF-Token", csrf),
                ("Content-Type", "application/json"),
            ],
            Some("{}"),
        )
        .expect("cancel inspection request")
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
    assert_eq!(
        server
            .request("GET", "/api/inventory", &[], None)
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

#[test]
fn inventory_api_is_separate_from_status_and_snapshot_survives_backend_restart() {
    let mut server = Server::start();
    let media = server.root.join("Inventory fixture.mkv");
    fs::write(&media, b"fixture media").expect("write inventory fixture");
    let csrf = server.csrf();
    let job_id = server.enqueue_inspection(&csrf);
    let auth = server.auth_header();

    let deadline = Instant::now() + Duration::from_secs(8);
    let finished = loop {
        let status = server
            .request("GET", "/api/status", &[("Authorization", &auth)], None)
            .expect("read job status");
        assert_eq!(status.status, 200);
        let job = status.body["history"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == job_id);
        if let Some(job) = job
            && ["completed", "completed_with_errors", "failed", "cancelled"]
                .contains(&job["status"].as_str().unwrap_or(""))
        {
            break status.body;
        }
        assert!(Instant::now() < deadline, "inspection did not finish");
        thread::sleep(Duration::from_millis(25));
    };

    let inventory = server
        .request("GET", "/api/inventory", &[("Authorization", &auth)], None)
        .expect("read inventory snapshot");
    assert_eq!(inventory.status, 200);
    assert_eq!(inventory.body["items"].as_array().unwrap().len(), 1);
    let first_updated = inventory.body["updated"].as_f64().unwrap();
    assert_eq!(finished["inventory"]["count"], 1);
    assert!(finished["inventory"].get("items").is_none());
    let inspect_history = finished["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == job_id)
        .unwrap();
    assert_eq!(inspect_history["results"], json!([]));
    assert!(inspect_history["results"].as_array().unwrap().is_empty());

    server.restart();
    let restarted_status = server
        .request("GET", "/api/status", &[("Authorization", &auth)], None)
        .expect("read status after restart");
    let restarted_inventory = server
        .request("GET", "/api/inventory", &[("Authorization", &auth)], None)
        .expect("read inventory after restart");
    assert_eq!(restarted_status.status, 200);
    assert_eq!(restarted_inventory.status, 200);
    assert_eq!(
        restarted_inventory.body["updated"].as_f64(),
        Some(first_updated)
    );
    assert_eq!(restarted_inventory.body["items"], inventory.body["items"]);
    assert_eq!(restarted_status.body["inventory"]["count"], 1);
    assert!(restarted_status.body["inventory"].get("items").is_none());
    assert!(
        restarted_status.body["history"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == job_id)
            .unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn cancelling_running_inspection_keeps_the_previous_inventory_snapshot() {
    let server = Server::start_with_slow_probe();
    fs::write(server.root.join("Existing.mkv"), b"fixture media").expect("write initial media");
    let csrf = server.csrf();
    let first_job_id = server.enqueue_inspection(&csrf);
    let auth = server.auth_header();
    let deadline = Instant::now() + Duration::from_secs(8);
    let initial = loop {
        let status = server
            .request("GET", "/api/status", &[("Authorization", &auth)], None)
            .expect("read initial inspection status");
        if let Some(job) = status.body["history"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == first_job_id)
            && job["status"] == "completed"
        {
            break server
                .request("GET", "/api/inventory", &[("Authorization", &auth)], None)
                .expect("read initial snapshot")
                .body;
        }
        assert!(
            Instant::now() < deadline,
            "initial inspection did not finish"
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(initial["items"].as_array().unwrap().len(), 1);
    let initial_updated = initial["updated"].as_f64().unwrap();

    fs::write(server.root.join("New media.mkv"), b"new fixture media")
        .expect("write media to be discovered by cancelled inspection");
    let second_job_id = server.enqueue_inspection(&csrf);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = server
            .request("GET", "/api/status", &[("Authorization", &auth)], None)
            .expect("wait for running inspection");
        if status.body["running"]["id"] == second_job_id
            && status.body["running"]["current"]["status"] == "inspecting"
            && status.body["running"]["current"]["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("New media.mkv"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "second inspection did not start");
        thread::sleep(Duration::from_millis(5));
    }
    let cancelled = server.cancel_job(&csrf, &second_job_id);
    assert_eq!(cancelled.status, 200);
    assert_eq!(cancelled.body["status"], "cancelling");

    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = server
            .request("GET", "/api/status", &[("Authorization", &auth)], None)
            .expect("wait for cancellation");
        if status.body["running"].is_null()
            && status.body["history"]
                .as_array()
                .unwrap()
                .iter()
                .any(|job| job["id"] == second_job_id && job["status"] == "cancelled")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cancelled inspection did not stop"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let after_cancel = server
        .request("GET", "/api/inventory", &[("Authorization", &auth)], None)
        .expect("read retained inventory snapshot");
    assert_eq!(after_cancel.status, 200);
    assert_eq!(after_cancel.body["updated"].as_f64(), Some(initial_updated));
    assert_eq!(after_cancel.body["items"], initial["items"]);
}
