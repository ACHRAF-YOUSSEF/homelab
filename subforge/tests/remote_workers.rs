#![forbid(unsafe_code)]

use base64::Engine;
use serde_json::{Value, json};
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(binary: &str, temporary: &Path) -> Command {
    let mut command = Command::new(binary);
    command.env_clear();
    for name in ["PATH", "SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("TMP", temporary)
        .env("TEMP", temporary)
        .env("TMPDIR", temporary)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(predicate(), "isolated worker fixture must make progress");
}

#[test]
fn real_worker_registers_pulls_converts_and_reports_over_bounded_http() {
    let temporary = tempfile::tempdir().unwrap();
    // This one end-to-end fixture intentionally exercises the installed local
    // text conversion executable. Other protocol tests need no external tools.
    let mut probe = command("ffmpeg", temporary.path());
    probe.arg("-version");
    if subforge::util::bounded_process(
        &mut probe,
        Duration::from_secs(3),
        &AtomicBool::new(false),
        16384,
    )
    .is_err()
    {
        eprintln!("FFmpeg unavailable; real remote conversion fixture skipped");
        return;
    }
    let media = temporary.path().join("media");
    let data = temporary.path().join("server-data");
    let worker_data = temporary.path().join("worker-data");
    fs::create_dir(&media).unwrap();
    let media = subforge::converter::normalize_canonical_local_path(media.canonicalize().unwrap());
    let source = media.join("fixture.en.ass");
    fs::write(&source, "[Script Info]\nScriptType: v4.00+\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,Remote fixture caption\n").unwrap();
    let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let endpoint = format!("http://127.0.0.1:{port}");
    let token = "fixture-worker-token-0123456789-abcdefghijklmnopqrstuvwxyz";
    let password = "fixture-dashboard-password-0123456789";
    let mut server = command(env!("CARGO_BIN_EXE_subforge"), temporary.path());
    server
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--no-auto-scan",
            "--data",
        ])
        .arg(&data)
        .arg("--root")
        .arg(&media)
        .env("CONVERTER_UI_PASSWORD", password)
        .env("CONVERTER_ALLOWED_HOSTS", "127.0.0.1,localhost")
        .env("CONVERTER_WORKER_TOKEN", token)
        .env("CONVERTER_ENABLE_OCR", "false")
        .env("CONVERTER_MIN_AGE", "0");
    let _server = Process(server.spawn().unwrap());
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .max_redirects(0)
            .proxy(None)
            .build(),
    );
    wait(|| agent.get(format!("{endpoint}/health")).call().is_ok());
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("admin:{password}"))
    );
    assert!(matches!(
        agent
            .post(format!("{endpoint}/api/remote/register"))
            .send_json(json!({})),
        Err(ureq::Error::StatusCode(401))
    ));
    let mut session = agent
        .get(format!("{endpoint}/api/session"))
        .header("Authorization", &auth)
        .call()
        .unwrap();
    let session: Value = session.body_mut().read_json().unwrap();
    let csrf = session["csrf_token"].as_str().unwrap();
    let worker_log = temporary.path().join("worker.jsonl");
    let mut worker = command(env!("CARGO_BIN_EXE_subforge-worker"), temporary.path());
    worker
        .env("CONVERTER_COORDINATOR_URL", &endpoint)
        .env("CONVERTER_WORKER_TOKEN", token)
        .env(
            "CONVERTER_WORKER_MAPPINGS",
            json!([{"server":media,"local":media}]).to_string(),
        )
        .env("CONVERTER_WORKER_DATA", &worker_data)
        .env("CONVERTER_WORKER_NAME", "fixture-node")
        .env("CONVERTER_WORKER_CPU_SLOTS", "1")
        .env("CONVERTER_WORKER_OCR_SLOTS", "0")
        .env("CONVERTER_ENABLE_OCR", "false")
        .env("CONVERTER_MIN_AGE", "0")
        .stdout(fs::File::create(&worker_log).unwrap());
    let _worker = Process(worker.spawn().unwrap());
    wait(|| {
        let Ok(mut response) = agent
            .get(format!("{endpoint}/api/nodes"))
            .header("Authorization", &auth)
            .call()
        else {
            return false;
        };
        let Ok(nodes) = response.body_mut().read_json::<Value>() else {
            return false;
        };
        nodes["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.iter().any(|node| node["status"] == "idle"))
    });
    let mut queued = agent
        .post(format!("{endpoint}/api/jobs"))
        .header("Authorization", &auth)
        .header("X-CSRF-Token", csrf)
        .send_json(json!({"kind":"extract","paths":[source],"languages":["en"]}))
        .unwrap();
    assert_eq!(queued.status().as_u16(), 202);
    let queued: Value = queued.body_mut().read_json().unwrap();
    let job_id = queued["job"]["id"].clone();
    wait(|| {
        let Ok(mut response) = agent
            .get(format!("{endpoint}/api/status"))
            .header("Authorization", &auth)
            .call()
        else {
            return false;
        };
        let Ok(status) = response.body_mut().read_json::<Value>() else {
            return false;
        };
        status["history"].as_array().is_some_and(|history| {
            history
                .iter()
                .any(|job| job["id"] == job_id && job["status"] == "completed")
        })
    });
    let output = media.join("fixture.en.srt");
    assert!(
        fs::read_to_string(&output)
            .unwrap()
            .contains("Remote fixture caption")
    );
    let state = subforge::util::read_json(&data.join("state.json"), 65536).unwrap();
    assert_eq!(state[source.to_string_lossy().as_ref()]["ok"], true);
    let logs = fs::read_to_string(&worker_log).unwrap();
    assert!(
        logs.contains("\"status\":\"extracting\""),
        "real remote process must perform conversion"
    );
    assert!(
        logs.contains("\"status\":\"reported\""),
        "worker must report staged result to coordinator"
    );
    assert!(!logs.contains(token));
    assert!(!logs.contains("Remote fixture caption"));
}
