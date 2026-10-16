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
        #[cfg(unix)]
        {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &self.child.id().to_string()])
                .env_clear()
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if self.child.try_wait().is_ok_and(|status| status.is_some()) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
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
        Self::start_with_environment(delay, &[])
    }

    fn start_with_environment(delay: Option<Duration>, environment: &[(&str, &str)]) -> Self {
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
                let marker = format!(
                    "'{}'",
                    temp.path()
                        .join("probe-starts.log")
                        .to_string_lossy()
                        .replace('\'', "'\\''")
                );
                fs::write(
                    &path,
                    format!("#!/bin/sh\nprintf 'started\\n' >> {marker}\nsleep {seconds}\nprintf '{{\"streams\":[]}}'\n"),
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
        command.env_clear();
        for name in ["PATH", "SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("TMP", temp.path())
            .env("TEMP", temp.path())
            .env("TMPDIR", temp.path());
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
            .env("CONVERTER_ENABLE_GPU", "false")
            .env("CONVERTER_SCAN_INTERVAL", "3600");
        for (name, value) in environment {
            command.env(name, value);
        }
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
        command.env_clear();
        for name in ["PATH", "SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("TMP", self._temp.path())
            .env("TEMP", self._temp.path())
            .env("TMPDIR", self._temp.path());
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
            .env("CONVERTER_ENABLE_GPU", "false")
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

    fn json_request(&self, method: &str, path: &str, csrf: &str, body: &Value) -> Response {
        let auth = self.auth_header();
        self.request(
            method,
            path,
            &[
                ("Authorization", &auth),
                ("X-CSRF-Token", csrf),
                ("Content-Type", "application/json"),
            ],
            Some(&body.to_string()),
        )
        .expect("JSON API request")
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
fn dashboard_auth_can_be_disabled_without_disabling_csrf_and_reenabled_on_restart() {
    let mut server = Server::start_with_environment(
        None,
        &[
            ("CONVERTER_AUTH_ENABLED", "false"),
            ("CONVERTER_UI_PASSWORD", "invalid"),
        ],
    );
    assert!(!server.data.join("ui-password").exists());
    for path in ["/", "/api/status", "/api/inventory", "/api/settings"] {
        assert_eq!(server.request("GET", path, &[], None).unwrap().status, 200);
    }
    let session = server.request("GET", "/api/session", &[], None).unwrap();
    assert_eq!(session.status, 200);
    let csrf = session.body["csrf_token"].as_str().unwrap();
    let body = r#"{"languages":["en","fr"]}"#;
    assert_eq!(
        server
            .request(
                "PUT",
                "/api/settings",
                &[("Content-Type", "application/json")],
                Some(body)
            )
            .unwrap()
            .status,
        403
    );
    let updated = server
        .request(
            "PUT",
            "/api/settings",
            &[("Content-Type", "application/json"), ("X-CSRF-Token", csrf)],
            Some(body),
        )
        .unwrap();
    assert_eq!(updated.status, 200);
    assert_eq!(updated.body["languages"], json!(["en", "fr"]));
    server.restart();
    assert_eq!(
        server
            .request("GET", "/api/status", &[], None)
            .unwrap()
            .status,
        401
    );
    let auth = server.auth_header();
    assert_eq!(
        server
            .request("GET", "/api/status", &[("Authorization", &auth)], None)
            .unwrap()
            .status,
        200
    );
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
        json!(["en", "fr", "pt-br"])
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
fn track_selection_api_validates_scope_types_and_known_ids() {
    let mut server = Server::start();
    let source = server.root.join("Selected fixture.mkv");
    fs::write(&source, b"disposable media fixture").unwrap();
    fs::write(server.data.join("inventory.json"), json!({"updated":10.0,"items":[{"path":source,"type":"video","tracks":[{"number":3,"language":"en","format":"SubRip"},{"number":4,"language":"en","format":"SubRip"}]}]}).to_string()).unwrap();
    server.restart();
    let csrf = server.csrf();
    for selection in [
        json!([]),
        json!([3, 3]),
        json!([99]),
        json!(["3"]),
        json!([-1]),
        json!([3.0]),
        json!(null),
        json!(vec![3; 65]),
    ] {
        let response = server.json_request(
            "POST",
            "/api/jobs",
            &csrf,
            &json!({"kind":"extract","paths":[source],"track_ids":selection}),
        );
        assert_eq!(response.status, 400, "invalid selection must fail closed");
    }
    let wrong_kind = server.json_request(
        "POST",
        "/api/jobs",
        &csrf,
        &json!({"kind":"scan","paths":[source],"track_ids":[3]}),
    );
    assert_eq!(wrong_kind.status, 400);
    let too_many_files = server.json_request(
        "POST",
        "/api/jobs",
        &csrf,
        &json!({"kind":"extract","paths":[source,source],"track_ids":[3]}),
    );
    assert_eq!(too_many_files.status, 400);
    let accepted = server.json_request(
        "POST",
        "/api/jobs",
        &csrf,
        &json!({"kind":"extract","paths":[source],"track_ids":[4],"include_ocr":false}),
    );
    assert_eq!(accepted.status, 202);
    assert_eq!(accepted.body["job"]["track_ids"], json!([4]));
    let legacy = server.job_request(&csrf, &[utf8_path(&source)]);
    assert_eq!(legacy.status, 202);
    assert!(legacy.body["job"].get("track_ids").is_none());
    let status = server
        .request(
            "GET",
            "/api/status",
            &[("Authorization", &server.auth_header())],
            None,
        )
        .unwrap();
    assert_eq!(status.body["pipeline"]["generated_srt_count"], 0);
    assert_eq!(status.body["pipeline"]["files_inspected"], 1);
    for secret in [PASSWORD, WIDGET_TOKEN, WEBHOOK_TOKEN] {
        assert!(!status.body.to_string().contains(secret));
    }
    for key in [
        "password",
        "api_key",
        "csrf_token",
        "widget_token",
        "webhook_token",
    ] {
        assert!(
            !status.body["pipeline"]
                .to_string()
                .contains(&format!("\"{key}\":"))
        );
    }
    for provider in ["sonarr", "radarr", "jellyfin", "bazarr"] {
        assert_eq!(
            status.body["settings"]["integrations"][provider]["api_key"],
            ""
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

#[test]
fn api_edition_is_compiled_and_ignores_deployment_labels() {
    let spoof = if subforge::edition::CUDA_ENABLED {
        "cpu"
    } else {
        "cuda"
    };
    let server = Server::start_with_environment(
        None,
        &[("CONVERTER_EDITION", spoof), ("SUBFORGE_EDITION", spoof)],
    );
    let auth = server.auth_header();
    for path in ["/api/status", "/api/system", "/api/workers", "/api/tasks"] {
        let response = server
            .request("GET", path, &[("Authorization", &auth)], None)
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body["edition"], subforge::edition::NAME);
    }
    let status = server
        .request("GET", "/api/status", &[("Authorization", &auth)], None)
        .unwrap();
    assert_eq!(status.body["settings"]["edition"], subforge::edition::NAME);
    let csrf = server.csrf();
    let tasks = server.json_request("PUT", "/api/tasks", &csrf, &json!({"paused":true}));
    assert_eq!(tasks.status, 200);
    assert_eq!(tasks.body["edition"], subforge::edition::NAME);
}

#[cfg(not(feature = "cuda"))]
#[test]
fn cpu_edition_retains_gpu_target_without_granting_execution() {
    let server = Server::start_with_environment(
        None,
        &[
            ("CONVERTER_ENABLE_OCR", "true"),
            ("CONVERTER_ENABLE_GPU", "true"),
        ],
    );
    let auth = server.auth_header();
    let response = server
        .request("GET", "/api/workers", &[("Authorization", &auth)], None)
        .unwrap();
    assert_eq!(response.body["edition"], "cpu");
    assert_eq!(response.body["deployment"]["max_gpu_workers"], 0);
    assert_eq!(response.body["effective"]["gpu_workers"], 0);
    assert_eq!(response.body["detected"]["gpu_available"], false);
    assert_eq!(
        response.body["detected"]["gpu_reason"],
        "GPU OCR requires the CUDA image edition"
    );
    let configuration_path = server.data.join("workers.conf");
    let csrf = server.csrf();
    let response = server.json_request(
        "PUT",
        "/api/workers",
        &csrf,
        &json!({"cpu_workers":1,"ocr_workers":1,"gpu_workers":1,"paused":false}),
    );
    assert_eq!(response.status, 200);
    assert_eq!(response.body["allocation"]["gpu_workers"], 1);
    assert_eq!(response.body["effective"]["gpu_workers"], 0);
    assert_eq!(response.body["detected"]["gpu_available"], false);
    let saved: Value = serde_json::from_slice(&fs::read(configuration_path).unwrap()).unwrap();
    assert_eq!(saved["gpu_workers"], 1);
}

#[test]
fn task_and_system_endpoints_require_auth_and_report_ocr_permission() {
    let server = Server::start();
    for path in ["/api/tasks", "/api/system"] {
        assert_eq!(server.request("GET", path, &[], None).unwrap().status, 401);
    }

    let auth = server.auth_header();
    let tasks = server
        .request("GET", "/api/tasks", &[("Authorization", &auth)], None)
        .unwrap();
    let system = server
        .request("GET", "/api/system", &[("Authorization", &auth)], None)
        .unwrap();
    assert_eq!(tasks.status, 200);
    assert_eq!(system.status, 200);
    let ocr = tasks.body["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "ocr")
        .unwrap();
    assert_eq!(ocr["available"], false);
    assert_eq!(ocr["enabled"], false);
    assert_eq!(
        ocr["blocked_reason"],
        "OCR is disabled by deployment permissions"
    );
    assert_eq!(system.body["resource_policy"]["ocr_permission"], false);
    assert_eq!(system.body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        system.body["resource_usage"]["metadata_cache"]["max_entries"],
        5000
    );
    assert_eq!(
        system.body["resource_policy"]["metadata_cache_max_entries"],
        5000
    );
    assert_eq!(system.body["resource_usage"]["worker"]["active"], 0);

    let first_uptime = system.body["uptime_seconds"].as_u64().unwrap();
    thread::sleep(Duration::from_millis(1100));
    let repeated = server
        .request("GET", "/api/system", &[("Authorization", &auth)], None)
        .unwrap();
    assert_eq!(repeated.status, 200);
    assert!(repeated.body["uptime_seconds"].as_u64().unwrap() > first_uptime);
}

#[cfg(unix)]
#[test]
fn system_telemetry_stays_responsive_while_inspection_tool_is_running() {
    let server = Server::start_with_slow_probe();
    fs::write(server.root.join("Fixture.mkv"), b"fixture media").expect("write media fixture");
    let csrf = server.csrf();
    let job_id = server.enqueue_inspection(&csrf);
    let auth = server.auth_header();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = server
            .request("GET", "/api/status", &[("Authorization", &auth)], None)
            .expect("read inspection status");
        if status.body["running"]["id"] == job_id
            && status.body["running"]["current"]["status"] == "inspecting"
        {
            break;
        }
        assert!(Instant::now() < deadline, "slow inspection did not start");
        thread::sleep(Duration::from_millis(5));
    }

    let started = Instant::now();
    let system = server
        .request("GET", "/api/system", &[("Authorization", &auth)], None)
        .expect("read system telemetry during inspection");
    let elapsed = started.elapsed();
    assert_eq!(system.status, 200);
    assert!(
        elapsed < Duration::from_millis(250),
        "system request took {elapsed:?}"
    );
    assert_eq!(system.body["resource_usage"]["worker"]["active"], 1);
    assert_eq!(system.body["resource_usage"]["worker"]["kind"], "inspect");
}

#[test]
fn task_updates_validate_and_persist_across_backend_restart() {
    let mut server = Server::start();
    let csrf = server.csrf();
    let updated = server.json_request(
        "PUT",
        "/api/tasks",
        &csrf,
        &json!({
            "paused": true,
            "tasks": [{"id":"discovery", "enabled":false, "interval_seconds":43200}]
        }),
    );
    assert_eq!(updated.status, 200);
    assert_eq!(updated.body["paused"], true);
    let discovery = updated.body["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "discovery")
        .unwrap();
    assert_eq!(discovery["enabled"], false);
    assert_eq!(discovery["interval_seconds"], 43200);

    for invalid in [
        json!({"tasks":[{"id":"unknown", "enabled":true}]}),
        json!({"tasks":[{"id":"..%2Fbackup", "enabled":true}]}),
        json!({"tasks":[{"id":"discovery", "enabled":true, "command":"/bin/sh"}]}),
        json!({"paused":"yes"}),
        json!({"tasks":[{"id":"discovery", "interval_seconds":1}]}),
    ] {
        let response = server.json_request("PUT", "/api/tasks", &csrf, &invalid);
        assert_eq!(response.status, 400, "invalid update unexpectedly accepted");
    }

    server.restart();
    let auth = server.auth_header();
    let tasks = server
        .request("GET", "/api/tasks", &[("Authorization", &auth)], None)
        .unwrap();
    assert_eq!(tasks.status, 200);
    assert_eq!(tasks.body["paused"], true);
    let discovery = tasks.body["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "discovery")
        .unwrap();
    assert_eq!(discovery["enabled"], false);
    assert_eq!(discovery["interval_seconds"], 43200);
}

#[test]
fn worker_configuration_is_created_private_validated_and_preserved() {
    let mut server = Server::start();
    let path = server.data.join("workers.conf");
    let configuration: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(configuration["cpu_workers"], 1);
    assert_eq!(configuration["ocr_workers"], 1);
    assert_eq!(configuration["gpu_workers"], 0);
    assert_eq!(configuration["max_cpu_workers"], 32);
    assert_eq!(configuration["max_gpu_workers"], 4);
    assert_eq!(configuration["gpu_min_free_mib"], 2048);
    assert!(fs::read_to_string(&path).unwrap().contains('\n'));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        server
            .request("GET", "/api/workers", &[], None)
            .unwrap()
            .status,
        401
    );
    let auth = server.auth_header();
    let body = r#"{"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":true}"#;
    assert_eq!(
        server
            .request(
                "PUT",
                "/api/workers",
                &[
                    ("Authorization", &auth),
                    ("Content-Type", "application/json")
                ],
                Some(body)
            )
            .unwrap()
            .status,
        403
    );
    let csrf = server.csrf();
    let update = json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":true});
    assert_eq!(
        server
            .json_request("PUT", "/api/workers", &csrf, &update)
            .status,
        200
    );
    let written = fs::read(&path).unwrap();
    for invalid in [
        json!({"cpu_workers":0,"ocr_workers":1,"gpu_workers":0,"paused":false}),
        json!({"cpu_workers":1_000_001,"ocr_workers":1,"gpu_workers":0,"paused":false}),
        json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":false,"max_cpu_workers":8}),
    ] {
        assert_eq!(
            server
                .json_request("PUT", "/api/workers", &csrf, &invalid)
                .status,
            400
        );
        assert_eq!(fs::read(&path).unwrap(), written);
    }
    server.restart();
    let workers = server
        .request("GET", "/api/workers", &[("Authorization", &auth)], None)
        .unwrap();
    assert_eq!(workers.body["allocation"], update);
    assert_eq!(workers.body["configuration_path"], "/data/workers.conf");
    assert_eq!(workers.body["configuration"]["max_cpu_workers"], 32);
}

#[test]
fn legacy_worker_allocation_migrates_without_configuration_environment() {
    let mut server = Server::start();
    fs::remove_file(server.data.join("workers.conf")).unwrap();
    let allocation = json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":true});
    fs::write(server.data.join("workers.json"), allocation.to_string()).unwrap();
    server.restart();
    let auth = server.auth_header();
    let workers = server
        .request("GET", "/api/workers", &[("Authorization", &auth)], None)
        .unwrap();
    assert_eq!(workers.body["allocation"], allocation);
    assert!(server.data.join("workers.conf").is_file());
    assert!(server.data.join("workers.json").is_file());
}

#[test]
fn settings_changes_appear_in_operation_logs_without_request_contents() {
    let server = Server::start();
    let csrf = server.csrf();
    let changed = server.json_request("PUT", "/api/settings", &csrf, &json!({"languages":["fr"]}));
    assert_eq!(changed.status, 200);
    let auth = server.auth_header();
    let status = server
        .request("GET", "/api/status", &[("Authorization", &auth)], None)
        .unwrap();
    let logs = status.body["logs"].as_array().unwrap();
    assert!(
        logs.iter()
            .any(|entry| entry["message"] == "Settings update processed")
    );
    let serialized = serde_json::to_string(logs).unwrap();
    assert!(!serialized.contains(PASSWORD));
    assert!(!serialized.contains(&csrf));
}

#[cfg(unix)]
#[test]
fn two_inspection_children_overlap_and_pause_stops_the_third_file() {
    let server = Server::start_with_probe_delay(Some(Duration::from_secs(2)));
    for episode in 1..=3 {
        fs::write(
            server.root.join(format!("show.S01E{episode:02}.mkv")),
            b"fixture media",
        )
        .unwrap();
    }
    let csrf = server.csrf();
    let allocation = json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":false});
    let workers = server.json_request("PUT", "/api/workers", &csrf, &allocation);
    assert_eq!(workers.status, 200);
    if workers.body["effective"]["cpu_workers"]
        .as_u64()
        .unwrap_or(0)
        < 2
    {
        eprintln!(
            "Parallel tool fixture requires two effective CPUs and the bounded two-worker RAM budget"
        );
        return;
    }
    let job_id = server.enqueue_inspection(&csrf);
    let marker = server._temp.path().join("probe-starts.log");
    let starts = || {
        fs::read_to_string(&marker)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while starts() < 2 {
        assert!(
            Instant::now() < deadline,
            "two real fixture children must overlap"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(starts(), 2);
    let mut paused = allocation.clone();
    paused["paused"] = json!(true);
    assert_eq!(
        server
            .json_request("PUT", "/api/workers", &csrf, &paused)
            .status,
        200
    );
    let auth = server.auth_header();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let workers = server
            .request("GET", "/api/workers", &[("Authorization", &auth)], None)
            .unwrap();
        if workers.body["active"]["cpu_workers"] == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "existing files must finish while dispatch is paused"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(starts(), 2);
    assert_eq!(
        server
            .json_request("PUT", "/api/workers", &csrf, &allocation)
            .status,
        200
    );
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let status = server
            .request("GET", "/api/status", &[("Authorization", &auth)], None)
            .unwrap();
        if let Some(job) = status.body["history"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == job_id)
        {
            assert_eq!(job["status"], "completed");
            assert_eq!(job["summary"]["inspected"], 3);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "resuming must dispatch the remaining file"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(starts(), 3);
}

#[test]
fn task_run_requires_csrf_rejects_invalid_options_and_denies_disabled_ocr() {
    let server = Server::start();
    let body = "{}";
    let auth = server.auth_header();
    let no_auth = server
        .request(
            "POST",
            "/api/tasks/inventory/run",
            &[("Content-Type", "application/json")],
            Some(body),
        )
        .unwrap();
    assert_eq!(no_auth.status, 401);
    let no_csrf = server
        .request(
            "POST",
            "/api/tasks/inventory/run",
            &[
                ("Authorization", &auth),
                ("Content-Type", "application/json"),
            ],
            Some(body),
        )
        .unwrap();
    assert_eq!(no_csrf.status, 403);

    let csrf = server.csrf();
    for (path, invalid) in [
        ("/api/tasks/not-a-task/run", json!({})),
        ("/api/tasks/../../backup/run", json!({})),
        ("/api/tasks/inventory/run", json!({"retry":"true"})),
        (
            "/api/tasks/inventory/run",
            json!({"retry":false,"shell":"true"}),
        ),
    ] {
        let response = server.json_request("POST", path, &csrf, &invalid);
        assert!(response.status == 400 || response.status == 404);
    }

    let denied = server.json_request("POST", "/api/tasks/ocr/run", &csrf, &json!({}));
    assert_eq!(denied.status, 400);
    assert!(
        denied.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("OCR is disabled")
    );

    let accepted = server.json_request("POST", "/api/tasks/inventory/run", &csrf, &json!({}));
    assert_eq!(accepted.status, 202);
    let job_id = accepted.body["job"]["id"]
        .as_str()
        .expect("manual inventory task job id")
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let auth = server.auth_header();
        let status = server
            .request("GET", "/api/status", &[("Authorization", &auth)], None)
            .expect("read manual task status");
        if status.body["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| {
                job["id"] == job_id
                    && (job["status"] == "completed" || job["status"] == "completed_with_errors")
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "manual inventory task did not finish"
        );
        thread::sleep(Duration::from_millis(20));
    }
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
