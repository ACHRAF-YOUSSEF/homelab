#![forbid(unsafe_code)]

//! Disposable, network-isolated smoke tests used by the container CI test stage.

use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PASSWORD: &str = "fixture-password-long-enough";

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("smoke test failed: {message}");
    std::process::exit(1)
}

fn command(program: &str, args: &[&str], timeout: Duration) {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|_| fail(format!("required fixture tool unavailable: {program}")));
    wait_child(&mut child, timeout)
        .unwrap_or_else(|_| fail(format!("fixture tool timed out: {program}")));
}

fn wait_child(child: &mut Child, timeout: Duration) -> std::io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(std::io::Error::other("child process failed"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "child process deadline",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            subforge::util::token(8)
        ));
        fs::create_dir(&path).unwrap_or_else(|error| fail(error));
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, text: &str) {
    fs::write(path, text).unwrap_or_else(|error| fail(error));
}

fn timestamp(seconds: usize) -> String {
    format!(
        "{:02}:{:02}:{:02},000",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr().map(|addr| addr.port()))
        .unwrap_or_else(|error| fail(error))
}

fn basic_auth() -> String {
    format!(
        "Basic {}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            format!("admin:{PASSWORD}")
        )
    )
}

fn api(port: u16, method: &str, path: &str, body: Option<&Value>, csrf: &str) -> Value {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_secs(3),
    )
    .unwrap_or_else(|error| fail(error));
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap_or_else(|error| fail(error));
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap_or_else(|error| fail(error));
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: {}\r\nConnection: close\r\n",
        basic_auth()
    );
    if body.is_some() {
        request.push_str(&format!(
            "Content-Type: application/json\r\nX-CSRF-Token: {csrf}\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    request.push_str("\r\n");
    request.push_str(&payload);
    stream
        .write_all(request.as_bytes())
        .unwrap_or_else(|error| fail(error));
    let mut response = Vec::new();
    stream
        .take(8 * 1024 * 1024)
        .read_to_end(&mut response)
        .unwrap_or_else(|error| fail(error));
    let split = response
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap_or_else(|| fail("invalid HTTP response"));
    let headers = String::from_utf8_lossy(&response[..split]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok());
    if !status.is_some_and(|code| (200..300).contains(&code)) {
        fail(format!(
            "API request {method} {path} returned {}",
            headers.lines().next().unwrap_or("unknown status")
        ));
    }
    serde_json::from_slice(&response[split + 4..]).unwrap_or_else(|error| fail(error))
}

fn start_server(port: u16, root: &Path, data: &Path, ocr: bool, gpu: bool) -> Child {
    let mut process = Command::new("/usr/local/bin/subforge")
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--data",
            data.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
            "--no-auto-scan",
        ])
        .env("CONVERTER_UI_PASSWORD", PASSWORD)
        .env("CONVERTER_ALLOWED_HOSTS", "localhost,127.0.0.1")
        .env("CONVERTER_ENABLE_OCR", ocr.to_string())
        .env("CONVERTER_ENABLE_GPU", gpu.to_string())
        .env("CONVERTER_OCR_LANGUAGE", "eng")
        .env("CONVERTER_MIN_AGE", "0")
        .env("CONVERTER_TIMEOUT", "300")
        .env("CONVERTER_RETRY_INTERVAL", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| fail(error));
    let deadline = Instant::now() + Duration::from_secs(if gpu { 30 } else { 10 });
    loop {
        if process
            .try_wait()
            .unwrap_or_else(|error| fail(error))
            .is_some()
        {
            fail("Subforge exited during startup");
        }
        if let Ok(mut stream) = TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(250),
        ) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            let mut response = String::new();
            if stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 200 ")
            {
                return process;
            }
        }
        if Instant::now() >= deadline {
            let _ = process.kill();
            fail("Subforge health endpoint did not become ready");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn stop_server(mut process: Child) {
    let _ = process.kill();
    let _ = process.wait();
}

fn csrf(port: u16) -> String {
    api(port, "GET", "/api/session", None, "")["csrf_token"]
        .as_str()
        .unwrap_or_else(|| fail("missing CSRF token"))
        .to_owned()
}

fn wait_job(port: u16, csrf: &str, id: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let status = api(port, "GET", "/api/status", None, csrf);
        if let Some(job) = status["history"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        {
            return job.clone();
        }
        if Instant::now() >= deadline {
            fail("job did not finish before deadline");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn job(port: u16, csrf: &str, kind: &str, paths: &[PathBuf]) -> String {
    let paths: Vec<_> = paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    api(
        port,
        "POST",
        "/api/jobs",
        Some(&json!({"kind":kind,"paths":paths,"languages":["*"]})),
        csrf,
    )["job"]["id"]
        .as_str()
        .unwrap_or_else(|| fail("job request returned no ID"))
        .to_owned()
}

fn assert_file(path: &Path, expected: &str) {
    let value = fs::read_to_string(path).unwrap_or_else(|_| {
        fail(format!(
            "expected output missing: {}",
            path.file_name().unwrap().to_string_lossy()
        ))
    });
    if !value.contains(expected) {
        fail(format!(
            "output content mismatch: {}",
            path.file_name().unwrap().to_string_lossy()
        ));
    }
}

fn generate_sup_fixture(workspace: &Path, media: &Path, language: &str, text: &str) -> PathBuf {
    let source = workspace.join(format!("ocr-source-{language}.srt"));
    let generated = workspace.join(format!("ocr-generated-{language}"));
    fs::create_dir(&generated).unwrap_or_else(|error| fail(error));
    write(
        &source,
        &format!("1\n00:00:01,000 --> 00:00:04,000\n{text}\n"),
    );
    let source_s = source.to_string_lossy().into_owned();
    let generated_s = generated.to_string_lossy().into_owned();
    command(
        "seconv",
        &[
            &source_s,
            "bluraysup",
            "--resolution:1280x720",
            "--font-name:DejaVu Sans",
            "--font-size:44",
            "--overwrite",
            "--json",
            &format!("--output-folder:{generated_s}"),
        ],
        Duration::from_secs(45),
    );
    let sup = fs::read_dir(&generated)
        .unwrap_or_else(|error| fail(error))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "sup"))
        .unwrap_or_else(|| fail(format!("Subtitle Edit produced no {language} OCR fixture")));
    let target = media.join(format!("OCR.{language}.sup"));
    fs::copy(sup, &target).unwrap_or_else(|error| fail(error));
    target
}

fn assert_language_ocr(path: &Path, language: &str) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|_| fail(format!("{language} OCR output is missing")))
        .to_lowercase();
    let recognized = text
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    let valid = match language {
        "en" => recognized.contains("hello") && recognized.contains("world"),
        "fr" => {
            recognized.contains("paris")
                && text
                    .chars()
                    .any(|character| matches!(character, 'é' | 'è' | 'ê' | 'à' | 'ç'))
        }
        "ar" => {
            recognized
                .chars()
                .any(|character| ('\u{0600}'..='\u{06ff}').contains(&character))
                && (recognized.contains("مرحبا") || recognized.contains("بالعالم"))
        }
        _ => false,
    };
    if !valid {
        fail(format!(
            "{language} OCR did not recognize its expected script and fixture words: {text:?}"
        ));
    }
}

fn primary_language_fixtures(workspace: &Path, media: &Path) -> Vec<PathBuf> {
    [
        ("en", "HELLO WORLD"),
        ("fr", "L'ÉTÉ À PARIS"),
        ("ar", "مرحبا بالعالم"),
    ]
    .into_iter()
    .map(|(language, phrase)| generate_sup_fixture(workspace, media, language, phrase))
    .collect()
}

fn secondary_language_fixtures(workspace: &Path, media: &Path) -> Vec<PathBuf> {
    [("fr", "L'ÉTÉ À PARIS"), ("ar", "مرحبا بالعالم")]
        .into_iter()
        .map(|(language, phrase)| generate_sup_fixture(workspace, media, language, phrase))
        .collect()
}

fn extraction(text_only: bool) {
    let workspace = TempDir::new("subforge-smoke");
    let media = workspace.0.join("media");
    let data = workspace.0.join("data");
    fs::create_dir(&media).unwrap_or_else(|error| fail(error));
    fs::create_dir(&data).unwrap_or_else(|error| fail(error));
    let vtt = media.join("Film.en.vtt");
    write(
        &vtt,
        "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nVTT fixture line\n",
    );
    let existing = media.join("Film.en.srt");
    write(&existing, "existing subtitle must survive\n");
    let vtt_es = media.join("Film.es.vtt");
    write(
        &vtt_es,
        "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nVTT fixture line\n",
    );
    let ass = media.join("Film.fr.ass");
    write(
        &ass,
        "[Script Info]\nScriptType: v4.00+\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,1,2,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,ASS fixture line\n",
    );
    let ttml = media.join("Film.de.ttml");
    write(
        &ttml,
        "<?xml version=\"1.0\"?><tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div><p begin=\"00:00:01.000\" end=\"00:00:02.000\">TTML fixture line</p></div></body></tt>",
    );
    let sbv = media.join("Film.it.sbv");
    write(&sbv, "0:00:01.000,0:00:02.000\nSBV fixture line\n");
    let en = workspace.0.join("track-en.srt");
    write(
        &en,
        "1\n00:00:01,000 --> 00:00:02,000\nEmbedded English fixture\n",
    );
    let fr = workspace.0.join("track-fr.srt");
    write(
        &fr,
        "1\n00:00:01,000 --> 00:00:02,000\nEmbedded French fixture\n",
    );
    let mkv = media.join("Tracks.mkv");
    let en_s = en.to_string_lossy().into_owned();
    let fr_s = fr.to_string_lossy().into_owned();
    let mkv_s = mkv.to_string_lossy().into_owned();
    command(
        "mkvmerge",
        &[
            "-o",
            &mkv_s,
            "--language",
            "0:eng",
            &en_s,
            "--language",
            "0:fra",
            &fr_s,
        ],
        Duration::from_secs(20),
    );
    let mut paths = vec![vtt, vtt_es, ass, ttml, sbv, mkv];
    if !text_only {
        paths.extend(primary_language_fixtures(&workspace.0, &media));
    }
    let port = free_port();
    let server = start_server(port, &media, &data, !text_only, false);
    let token = csrf(port);
    let id = job(port, &token, "extract", &paths);
    let result = wait_job(port, &token, &id, Duration::from_secs(180));
    if result["status"] != "completed" || result["summary"]["failed"].as_u64().unwrap_or(0) != 0 {
        stop_server(server);
        fail(format!(
            "unexpected extraction result: {}",
            result["status"]
        ));
    }
    if fs::read_to_string(&existing).unwrap_or_default() != "existing subtitle must survive\n" {
        stop_server(server);
        fail("existing SRT was overwritten");
    }
    for (filename, expected) in [
        ("Film.es.srt", "VTT fixture line"),
        ("Film.fr.srt", "ASS fixture line"),
        ("Film.de.srt", "TTML fixture line"),
        ("Film.it.srt", "SBV fixture line"),
        ("Tracks.en.srt", "Embedded English fixture"),
        ("Tracks.fr.srt", "Embedded French fixture"),
    ] {
        assert_file(&media.join(filename), expected);
    }
    if !text_only {
        for language in ["en", "fr", "ar"] {
            assert_language_ocr(&media.join(format!("OCR.{language}.srt")), language);
        }
    }
    stop_server(server);
    println!(
        "Rust HTTP service smoke passed: {}; existing SRT preserved",
        if text_only {
            "text conversions"
        } else {
            "text and OCR conversions"
        }
    );
}

fn resources(expected_cpus: f64, memory_mib: u64) {
    let workspace = TempDir::new("subforge-resources");
    let media = workspace.0.join("media");
    let data = workspace.0.join("data");
    fs::create_dir(&media).unwrap_or_else(|error| fail(error));
    fs::create_dir(&data).unwrap_or_else(|error| fail(error));
    let track = workspace.0.join("track.srt");
    write(
        &track,
        "1\n00:00:00,000 --> 00:00:01,000\nDisposable subtitle fixture\n",
    );
    let source = media.join("fixture.mkv");
    let source_s = source.to_string_lossy().into_owned();
    let track_s = track.to_string_lossy().into_owned();
    command(
        "mkvmerge",
        &["-q", "-o", &source_s, "--language", "0:eng", &track_s],
        Duration::from_secs(10),
    );
    let port = free_port();
    let server = start_server(port, &media, &data, false, false);
    let token = csrf(port);
    let first = api(port, "GET", "/api/system", None, &token);
    let usage = &first["resource_usage"];
    if !matches!(
        usage["cpu"]["source"].as_str(),
        Some("cgroup_v1" | "cgroup_v2")
    ) || usage["cpu"]["quota_cores"].as_f64() != Some(expected_cpus)
        || usage["memory"]["cgroup_limit_bytes"].as_u64() != Some(memory_mib * 1_048_576)
    {
        stop_server(server);
        fail("container resource telemetry does not match configured limits");
    }
    if first["resource_policy"]["conversion_workers"] != 1
        || first["resource_policy"]["ocr_threads"] != 1
        || usage["cpu"]["usage_sample_state"] != "collecting"
    {
        stop_server(server);
        fail("unexpected initial resource policy or sample state");
    }
    let baseline = usage["memory"]["cgroup_current_bytes"]
        .as_u64()
        .unwrap_or(0);
    let child = Command::new(std::env::current_exe().unwrap_or_else(|error| fail(error)))
        .arg("hold-memory")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| fail(error));
    thread::sleep(Duration::from_millis(3200));
    let measured = api(port, "GET", "/api/system", None, &token);
    let resource = &measured["resource_usage"];
    if measured["uptime_seconds"].as_u64().unwrap_or(0)
        <= first["uptime_seconds"].as_u64().unwrap_or(0)
        || resource["cpu"]["usage_sample_state"] != "available"
        || resource["cpu"]["usage_percent"].as_f64().unwrap_or(-1.0) < 0.0
        || resource["cpu"]["effective_cores"]
            .as_f64()
            .unwrap_or(expected_cpus + 1.0)
            > expected_cpus
        || resource["memory"]["cgroup_current_bytes"]
            .as_u64()
            .unwrap_or(0)
            < baseline + 16 * 1_048_576
    {
        let mut child = child;
        let _ = child.kill();
        stop_server(server);
        fail("live cgroup telemetry did not reflect the bounded child process");
    }
    let id = job(port, &token, "inspect", &[source]);
    let result = wait_job(port, &token, &id, Duration::from_secs(10));
    if result["status"] != "completed" || result["summary"]["inspected"] != 1 {
        let mut child = child;
        let _ = child.kill();
        stop_server(server);
        fail("metadata inspection failed in resource smoke test");
    }
    let cache =
        api(port, "GET", "/api/system", None, &token)["resource_usage"]["metadata_cache"].clone();
    if cache["entries"] != 1 || cache["accounted_bytes"].as_u64().unwrap_or(0) == 0 {
        let mut child = child;
        let _ = child.kill();
        stop_server(server);
        fail("metadata telemetry did not advance");
    }
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    stop_server(server);
    println!(
        "resource smoke passed: {expected_cpus} CPU quota, {memory_mib} MiB memory limit, live telemetry and metadata inspection"
    );
}

fn hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let mut file = fs::File::open(path).unwrap_or_else(|error| fail(error));
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).unwrap_or_else(|error| fail(error));
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("{:x}", digest.finalize())
}

fn gpu_smoke(cues: usize) {
    if !(2..=500).contains(&cues) {
        fail("GPU fixture cue count must be between 2 and 500");
    }
    if std::env::var("CONVERTER_ENABLE_GPU").as_deref() != Ok("true") {
        fail("GPU smoke requires the CUDA image and an explicitly enabled GPU");
    }
    let workspace = TempDir::new("subforge-gpu-smoke");
    let media = workspace.0.join("media");
    let data = workspace.0.join("data");
    let generated = workspace.0.join("generated");
    for directory in [&media, &data, &generated] {
        fs::create_dir(directory).unwrap_or_else(|error| fail(error));
    }
    let text = workspace.0.join("gpu-fixture.en.srt");
    let mut fixture = String::new();
    for number in 1..=cues {
        let start = number * 3;
        fixture.push_str(&format!(
            "{number}\n{} --> {}\nGPU OCR FIXTURE {number:04}\n\n",
            timestamp(start),
            timestamp(start + 1)
        ));
    }
    // Subtitle Edit accepts standard SRT timestamps with comma milliseconds.
    write(&text, &fixture);
    let text_s = text.to_string_lossy().into_owned();
    let generated_s = generated.to_string_lossy().into_owned();
    command(
        "seconv",
        &[
            &text_s,
            "bluraysup",
            "--resolution:640x480",
            "--font-name:DejaVu Sans",
            "--font-size:32",
            "--overwrite",
            "--json",
            &format!("--output-folder:{generated_s}"),
        ],
        Duration::from_secs(45),
    );
    let sup = fs::read_dir(&generated)
        .unwrap_or_else(|error| fail(error))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "sup"))
        .unwrap_or_else(|| fail("Subtitle Edit produced no SUP fixture"));
    let expected_hash = hash(&sup);
    let target = media.join("OCR.en.sup");
    fs::copy(&sup, &target).unwrap_or_else(|error| fail(error));
    if hash(&target) != expected_hash {
        fail("copy changed synthetic SUP fixture");
    }
    let mut gpu_paths = vec![target.clone()];
    gpu_paths.extend(secondary_language_fixtures(&workspace.0, &media));
    let port = free_port();
    let server = start_server(port, &media, &data, true, true);
    let token = csrf(port);
    let before = api(port, "GET", "/api/workers", None, &token);
    let system = api(port, "GET", "/api/system", None, &token);
    if before["edition"] != "cuda" || system["edition"] != "cuda" {
        stop_server(server);
        fail("GPU fixture requires CUDA edition");
    }
    let gpu_state = before["detected"]["gpu_verification"].clone();
    let repeated = api(port, "GET", "/api/workers", None, &token);
    if repeated["detected"]["gpu_verification"] != gpu_state
        || !repeated["detected"]["gpu_health"].is_null()
    {
        stop_server(server);
        fail("worker status GET unexpectedly changed or probed GPU state");
    }
    if before["allocation"]
        != json!({"cpu_workers":1,"ocr_workers":1,"gpu_workers":0,"paused":false})
    {
        stop_server(server);
        fail("unexpected generated worker defaults");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let config = data.join("workers.conf");
        if !fs::metadata(&config)
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o777 == 0o600)
        {
            stop_server(server);
            fail("workers.conf permissions are not private");
        }
    }
    api(
        port,
        "PUT",
        "/api/workers",
        Some(&json!({"cpu_workers":1,"ocr_workers":0,"gpu_workers":1,"paused":false})),
        &token,
    );
    let id = job(port, &token, "extract", &gpu_paths);
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut gpu_seen = false;
    let result;
    loop {
        let status = api(port, "GET", "/api/status", None, &token);
        if let Some(item) = status["history"]
            .as_array()
            .and_then(|rows| rows.iter().find(|item| item["id"] == id))
            && matches!(
                item["status"].as_str(),
                Some("completed" | "failed" | "cancelled")
            )
        {
            result = item.clone();
            break;
        }
        let current = api(port, "GET", "/api/workers", None, &token);
        gpu_seen |= current["active"]["gpu_workers"].as_u64().unwrap_or(0) > 0
            || current["workers"]
                .as_array()
                .is_some_and(|workers| workers.iter().any(|w| w["resource"] == "gpu"));
        if Instant::now() >= deadline {
            stop_server(server);
            fail("GPU extraction exceeded five minute deadline");
        }
        thread::sleep(Duration::from_millis(250));
    }
    if result["status"] != "completed" || result["summary"]["failed"].as_u64().unwrap_or(0) != 0 {
        stop_server(server);
        fail(format!(
            "GPU extraction failed: status={}",
            result["status"]
        ));
    }
    let output = media.join("OCR.en.srt");
    let recognized =
        fs::read_to_string(&output).unwrap_or_else(|_| fail("GPU OCR output is missing"));
    let recognized = recognized.to_ascii_lowercase();
    if !recognized.contains("gpu") || !recognized.contains("fixture") {
        stop_server(server);
        fail("GPU OCR output did not contain fixture text");
    }
    assert_language_ocr(&media.join("OCR.fr.srt"), "fr");
    assert_language_ocr(&media.join("OCR.ar.srt"), "ar");
    if hash(&target) != expected_hash {
        stop_server(server);
        fail("OCR changed synthetic SUP source");
    }
    let worker = api(port, "GET", "/api/workers", None, &token);
    let detected = &worker["detected"];
    let health = &detected["gpu_health"];
    if detected["gpu_verification"] != "verified"
        || health["cuda"] != true
        || health["device_count"] != 1
        || !gpu_seen
    {
        stop_server(server);
        fail("GPU execution was not verified/observed");
    }
    let output_hash = hash(&output);
    let repeat_id = job(port, &token, "extract", std::slice::from_ref(&target));
    let repeat = wait_job(port, &token, &repeat_id, Duration::from_secs(30));
    if repeat["status"] != "completed"
        || repeat["summary"]["cached"].as_u64().unwrap_or(0) < 1
            && repeat["summary"]["already_ready"].as_u64().unwrap_or(0) < 1
    {
        stop_server(server);
        fail("repeat extraction did not use OCR cache");
    }
    if hash(&output) != output_hash {
        stop_server(server);
        fail("cached extraction changed SRT output");
    }
    stop_server(server);
    println!(
        "GPU smoke passed: CUDA verified, {cues} synthetic cues converted, source preserved, cached repeat confirmed"
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("hold-memory") => {
            let mut allocation = vec![0u8; 32 * 1024 * 1024];
            for (index, byte) in allocation.iter_mut().step_by(4096).enumerate() {
                *byte = index as u8;
            }
            thread::sleep(Duration::from_secs(10));
            std::hint::black_box(allocation);
        }
        Some("extraction") => extraction(args.any(|arg| arg == "--text-only")),
        Some("resources") => {
            let mut cpus = None;
            let mut memory = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--expected-cpus" => cpus = args.next().and_then(|v| v.parse().ok()),
                    "--expected-memory-mib" => memory = args.next().and_then(|v| v.parse().ok()),
                    _ => fail("unknown resources argument"),
                }
            }
            resources(
                cpus.unwrap_or_else(|| fail("missing --expected-cpus")),
                memory.unwrap_or_else(|| fail("missing --expected-memory-mib")),
            );
        }
        Some("gpu") => {
            let mut cues = 200;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--cues" => {
                        cues = args
                            .next()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or_else(|| fail("invalid cue count"))
                    }
                    _ => fail("unknown GPU argument"),
                }
            }
            gpu_smoke(cues);
        }
        _ => fail(
            "usage: subforge-smoke extraction [--text-only] | resources --expected-cpus N --expected-memory-mib N | gpu [--cues N]",
        ),
    }
}
