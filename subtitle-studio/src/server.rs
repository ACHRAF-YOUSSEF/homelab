use crate::{App, View, config, media, pipeline, valid_id};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_HEADER: usize = 16384;
const MAX_BODY: usize = 16384;

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn acquire(count: &Arc<AtomicUsize>, limit: usize) -> Option<Permit> {
    let mut current = count.load(Ordering::Acquire);
    while current < limit {
        match count.compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return Some(Permit(count.clone())),
            Err(next) => current = next,
        }
    }
    None
}

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let read = |stream: &mut TcpStream, buffer: &mut [u8]| -> Result<usize, String> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or("Request timed out")?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| "Could not set request timeout")?;
        stream
            .read(buffer)
            .map_err(|_| "Could not read request before deadline".into())
    };
    let mut data = Vec::with_capacity(4096);
    let header_end;
    loop {
        if let Some(end) = data.windows(4).position(|b| b == b"\r\n\r\n") {
            header_end = end + 4;
            break;
        }
        if data.len() >= MAX_HEADER {
            return Err("Request headers are too large".into());
        }
        let mut buffer = [0; 2048];
        let length = read(stream, &mut buffer)?;
        if length == 0 {
            return Err("Incomplete request".into());
        }
        data.extend_from_slice(&buffer[..length]);
    }
    if header_end > MAX_HEADER {
        return Err("Request headers are too large".into());
    }
    let mut slots = [httparse::EMPTY_HEADER; 48];
    let mut parsed = httparse::Request::new(&mut slots);
    if !parsed
        .parse(&data[..header_end])
        .map_err(|_| "Invalid HTTP request")?
        .is_complete()
    {
        return Err("Incomplete HTTP request".into());
    }
    let method = parsed.method.ok_or("Missing method")?.to_string();
    let target = parsed.path.ok_or("Missing target")?.to_string();
    if !target.starts_with('/') || target.starts_with("//") {
        return Err("Invalid target".into());
    }
    let mut headers = Vec::new();
    let mut body_length = None;
    let mut hosts = 0;
    for header in parsed.headers.iter() {
        let value = std::str::from_utf8(header.value)
            .map_err(|_| "Invalid header")?
            .trim()
            .to_string();
        if header.name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("Transfer encoding is unsupported".into());
        }
        if header.name.eq_ignore_ascii_case("content-length") {
            if body_length.is_some()
                || value.is_empty()
                || !value.bytes().all(|b| b.is_ascii_digit())
            {
                return Err("Invalid Content-Length".into());
            }
            body_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "Invalid Content-Length")?,
            );
        }
        if header.name.eq_ignore_ascii_case("host") {
            hosts += 1;
        }
        headers.push((header.name.into(), value));
    }
    if hosts != 1 {
        return Err("One Host header is required".into());
    }
    let length = body_length.unwrap_or(0);
    if length > MAX_BODY {
        return Err("Invalid request size".into());
    }
    while data.len() < header_end + length {
        let mut buffer = [0; 2048];
        let remaining = (header_end + length - data.len()).min(buffer.len());
        let count = read(stream, &mut buffer[..remaining])?;
        if count == 0 {
            return Err("Incomplete request body".into());
        }
        data.extend_from_slice(&buffer[..count]);
    }
    Ok(Request {
        method,
        target,
        headers,
        body: data[header_end..header_end + length].to_vec(),
    })
}

fn response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    download: Option<&str>,
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    write!(stream,"HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'\r\nConnection: close\r\n",body.len()).map_err(|_|"Disconnected client")?;
    if let Some(name) = download {
        let encoded: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect();
        write!(
            stream,
            "Content-Disposition: attachment; filename*=UTF-8''{}\r\n",
            encoded.replace('+', "%20")
        )
        .map_err(|_| "Disconnected client")?;
    }
    stream
        .write_all(b"\r\n")
        .and_then(|()| stream.write_all(body))
        .map_err(|_| "Disconnected client".into())
}
fn json_response(stream: &mut TcpStream, status: u16, value: &Value) -> Result<(), String> {
    response(
        stream,
        status,
        "application/json; charset=utf-8",
        &serde_json::to_vec(value).map_err(|_| "Could not encode response")?,
        None,
    )
}

fn mutation_allowed(request: &Request) -> bool {
    if request.header("sec-fetch-site") == Some("cross-site") {
        return false;
    }
    let host = request.header("host").unwrap_or("");
    if let Some(origin) = request.header("origin") {
        let Ok(origin) = url::Url::parse(origin) else {
            return false;
        };
        let Ok(expected) = url::Url::parse(&format!("http://{host}")) else {
            return false;
        };
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host_str() != expected.host_str()
            || origin.port_or_known_default() != expected.port_or_known_default()
        {
            return false;
        }
    }
    request.header("content-type").is_some_and(|s| {
        s.split(';')
            .next()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
    })
}

fn trusted_host(config: &config::Config, request: &Request) -> bool {
    let Some(authority) = request.header("host") else {
        return false;
    };
    let Ok(url) = url::Url::parse(&format!("http://{authority}")) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || authority.contains(['/', '\\', '@'])
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    config.allowed_hosts.contains(&host)
        || config
            .bind_address
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| !ip.is_unspecified() && ip.to_string() == host)
}

fn handle(
    app: &App,
    stream: &mut TcpStream,
    request: &Request,
    streams: &Arc<AtomicUsize>,
) -> Result<(), String> {
    if !trusted_host(&app.config, request) {
        return json_response(
            stream,
            403,
            &json!({"error":"Untrusted Host header; configure SUBTITLE_ALLOWED_HOSTS for this address"}),
        );
    }
    let url = url::Url::parse(&format!("http://localhost{}", request.target))
        .map_err(|_| "Invalid request target")?;
    let path = url.path();
    let query = url.query_pairs().into_owned().collect::<Vec<_>>();
    if request.method == "GET" {
        match path {
            "/" => {
                return response(
                    stream,
                    200,
                    "text/html; charset=utf-8",
                    include_bytes!("../web/index.html"),
                    None,
                );
            }
            "/app.js" => {
                return response(
                    stream,
                    200,
                    "application/javascript; charset=utf-8",
                    include_bytes!("../web/app.js"),
                    None,
                );
            }
            "/app.css" => {
                return response(
                    stream,
                    200,
                    "text/css; charset=utf-8",
                    include_bytes!("../web/app.css"),
                    None,
                );
            }
            "/favicon.svg" => {
                return response(
                    stream,
                    200,
                    "image/svg+xml",
                    include_bytes!("../web/favicon.svg"),
                    None,
                );
            }
            "/api/health" => {
                return json_response(stream, 200, &json!({"status":"ok","runtime":"rust"}));
            }
            "/api/resources" => return json_response(stream, 200, &resources(app)),
            "/api/config" => {
                let mut value = crate::llm::config_models(&app.config);
                value["languages"] = config::languages();
                value["whisper_model"] = json!(app.config.whisper_model);
                value["whisper_models"] = media::models(&app.config);
                value["whisper_local_only"] = json!(app.config.local_only);
                value["runtime"] = json!("rust");
                value["resources"] = resources(app);
                return json_response(stream, 200, &value);
            }
            "/api/library" => {
                return json_response(
                    stream,
                    200,
                    &media::browse(&app.config, param(&query, "path", "")?)?,
                );
            }
            "/api/media" => {
                let relative = param(&query, "path", "")?;
                let file = media::resolve_media(&app.config, relative, true)?;
                let mut info = media::probe(&app.config, &file)?;
                info["path"] = json!(relative);
                return json_response(stream, 200, &info);
            }
            "/api/jobs" => {
                return json_response(stream, 200, &app.snapshot(View::parse(&query)?.as_ref()));
            }
            "/api/jobs/events" => {
                let view = View::parse(&query)?;
                let Some(_permit) = acquire(streams, app.config.max_streams) else {
                    return json_response(
                        stream,
                        429,
                        &json!({"error":"Event stream limit reached"}),
                    );
                };
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\nretry: 3000\n\n").map_err(|_|"Disconnected client")?;
                let mut revision = app.state.lock().unwrap().revision;
                loop {
                    let snapshot = app.snapshot(view.as_ref());
                    write!(
                        stream,
                        "id: {}:{revision}\nevent: jobs\ndata: {}\n\n",
                        app.epoch, snapshot
                    )
                    .and_then(|()| stream.flush())
                    .map_err(|_| "Disconnected client")?;
                    loop {
                        let next = app.wait_revision(revision);
                        if next != revision {
                            revision = next;
                            break;
                        }
                        stream
                            .write_all(b": heartbeat\n\n")
                            .and_then(|()| stream.flush())
                            .map_err(|_| "Disconnected client")?;
                    }
                }
            }
            _ => {}
        }
        let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
        if parts.len() >= 3 && parts[..2] == ["api", "jobs"] && valid_id(parts[2]) {
            let job = app.job(parts[2])?;
            if parts.len() == 3 {
                return json_response(stream, 200, &app.public_job(job));
            }
            if parts.len() == 4 && parts[3] == "record" {
                let mut record = app.public_job(job);
                if let Some(object) = record.as_object_mut() {
                    for field in ["path", "media_fingerprint", "generation_token"] {
                        object.remove(field);
                    }
                }
                for task in ["review_task", "step_task"] {
                    if let Some(object) = record[task].as_object_mut() {
                        object.remove("token");
                    }
                }
                if let Some(outputs) = record["outputs"].as_object_mut() {
                    for output in outputs.values_mut() {
                        if let Some(object) = output.as_object_mut() {
                            object.remove("path");
                            object.remove("export_path");
                        }
                    }
                }
                return response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&record).map_err(|_| "Could not encode saved job")?,
                    Some(&format!("subtitle-job-{}.json", parts[2])),
                );
            }
            if parts.len() == 5
                && parts[3] == "files"
                && (2..=3).contains(&parts[4].len())
                && parts[4].bytes().all(|b| b.is_ascii_lowercase())
            {
                let file = pipeline::output_path(&app.config, &job, parts[4])?;
                let metadata = std::fs::metadata(&file).map_err(|_| "Subtitle file missing")?;
                if metadata.len() > app.config.max_json_bytes {
                    return Err("Subtitle exceeds configured size limit".into());
                }
                let body = std::fs::read(file).map_err(|_| "Subtitle file missing")?;
                return response(
                    stream,
                    200,
                    "application/x-subrip; charset=utf-8",
                    &body,
                    Some(text_output_name(&job, parts[4])),
                );
            }
        }
        return json_response(stream, 404, &json!({"error":"Not found"}));
    }
    if request.method != "POST" {
        return json_response(stream, 405, &json!({"error":"Method not allowed"}));
    }
    if !mutation_allowed(request) {
        return json_response(
            stream,
            403,
            &json!({"error":"Use a same-origin JSON request"}),
        );
    }
    if request.body.is_empty() {
        return Err("Expected a JSON object".into());
    }
    let payload: Value = serde_json::from_slice(&request.body).map_err(|_| "Invalid JSON")?;
    if !payload.is_object() {
        return Err("Expected a JSON object".into());
    }
    let result = if path == "/api/jobs" {
        app.create_job(&payload)?
    } else if path == "/api/jobs/archive-inactive" {
        app.archive_jobs(None)?
    } else if path == "/api/reviews/missing" {
        if payload.get("force").is_some_and(|v| v != false)
            || payload.get("languages").is_some_and(|v| !v.is_null())
        {
            return Err("Bulk review only accepts a model and thinking setting".into());
        }
        // Validate once, even when the history is empty.
        if payload.get("review_model").is_some_and(|v| {
            v.as_str()
                .is_none_or(|s| s.len() > 300 || s.to_lowercase().contains("translategemma"))
        }) || payload
            .get("review_thinking")
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err("Invalid review options".into());
        }
        let ids = app
            .state
            .lock()
            .unwrap()
            .jobs
            .iter()
            .filter_map(|j| j["id"].as_str().map(str::to_string))
            .collect::<Vec<_>>();
        let mut queued = Vec::new();
        let mut skipped = Vec::new();
        for id in ids {
            match app.queue_review(&id, &payload) {
                Ok(_) => queued.push(id),
                Err(error) => skipped.push(json!({"id":id,"reason":error})),
            }
        }
        json!({"queued":queued,"skipped":skipped})
    } else {
        let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
        if parts.len() < 4 || parts[..2] != ["api", "jobs"] || !valid_id(parts[2]) {
            return json_response(stream, 404, &json!({"error":"Not found"}));
        }
        let job = match &parts[3..] {
            ["archive"] => return json_response(stream, 200, &app.archive_jobs(Some(parts[2]))?),
            ["restore"] => return json_response(stream, 200, &app.restore_job(parts[2])?),
            ["cancel"] => app.cancel(parts[2], None)?,
            ["review"] => app.queue_review(parts[2], &payload)?,
            ["steps", "cancel"] => app.cancel(
                parts[2],
                Some(payload["step"].as_str().ok_or("Select a processing step")?),
            )?,
            ["steps", "retry"] => app.retry_step(
                parts[2],
                payload["step"].as_str().ok_or("Select a processing step")?,
            )?,
            _ => return json_response(stream, 404, &json!({"error":"Not found"})),
        };
        app.public_job(job)
    };
    json_response(stream, 202, &result)
}
fn text_output_name<'a>(job: &'a Value, language: &str) -> &'a str {
    job["outputs"][language]["name"]
        .as_str()
        .unwrap_or("subtitle.srt")
}
fn param<'a>(
    query: &'a [(String, String)],
    key: &str,
    default: &'a str,
) -> Result<&'a str, String> {
    let mut matches = query.iter().filter(|(name, _)| name == key);
    let result = matches.next().map_or(default, |(_, value)| value);
    if matches.next().is_some() {
        return Err("Duplicate query parameter".into());
    }
    Ok(result)
}

fn resources(app: &App) -> Value {
    static CPU_SAMPLE: OnceLock<Mutex<Option<(Instant, f64)>>> = OnceLock::new();
    let read_number = |path: &str| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    };
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u64>().ok())
                .map(|k| k * 1024)
        });
    let (queue_length, history_count) = {
        let state = app.state.lock().unwrap();
        (state.queue.len(), state.jobs.len())
    };
    let cpu_usage_seconds = std::fs::read_to_string("/sys/fs/cgroup/cpu.stat")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("usage_usec ")
                    .and_then(|value| value.parse::<u64>().ok())
            })
        })
        .map(|us| us as f64 / 1_000_000.0);
    let cpu_percent = cpu_usage_seconds.and_then(|seconds| {
        let now = Instant::now();
        let mut previous = CPU_SAMPLE.get_or_init(|| Mutex::new(None)).lock().ok()?;
        let percent = previous.and_then(|(at, usage)| {
            let elapsed = now.duration_since(at).as_secs_f64();
            (elapsed >= 0.1 && seconds >= usage).then_some((seconds - usage) * 100.0 / elapsed)
        });
        *previous = Some((now, seconds));
        percent
    });
    let gpu = if app.config.whisper_device == "cuda" {
        crate::process::run("nvidia-smi",&["--query-gpu=utilization.gpu,memory.used,memory.total".into(),"--format=csv,noheader,nounits".into()],3,4096,||Ok(())).ok().and_then(|bytes|String::from_utf8(bytes).ok()).map(|text|text.lines().take(16).filter_map(|line|{let fields=line.split(',').map(str::trim).map(str::parse::<u64>).collect::<Result<Vec<_>,_>>().ok()?;if fields.len()!=3{return None;}Some(json!({"utilization_percent":fields[0],"memory_used_mib":fields[1],"memory_total_mib":fields[2]}))}).collect::<Vec<_>>())
    } else {
        None
    };
    json!({"cpu_threads":app.config.cpu_threads,"whisper_device":app.config.whisper_device,"audio_chunk_seconds":app.config.audio_chunk_seconds,"model_residency":"per audio chunk; unloaded before translation","queue_length":queue_length,"queue_limit":app.config.max_queue,"history_count":history_count,"history_limit":app.config.max_jobs,"server_rss_bytes":rss,"container_memory_bytes":read_number("/sys/fs/cgroup/memory.current"),"container_memory_peak_bytes":read_number("/sys/fs/cgroup/memory.peak"),"container_memory_limit_bytes":read_number("/sys/fs/cgroup/memory.max"),"container_cpu_seconds":cpu_usage_seconds,"container_cpu_percent":cpu_percent,"gpu":gpu,"gpu_usage_note":"GPU metrics are whole-device usage when nvidia-smi is available. LM Studio controls its own allocation."})
}

pub fn serve(app: Arc<App>) -> Result<(), String> {
    let listener = TcpListener::bind((app.config.bind_address.as_str(), app.config.port))
        .map_err(|_| "Could not bind listener")?;
    let worker = app.clone();
    thread::Builder::new()
        .name("subtitle-pipeline".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || worker.worker())
        .map_err(|_| "Could not start processing queue")?;
    let clients = Arc::new(AtomicUsize::new(0));
    let streams = Arc::new(AtomicUsize::new(0));
    eprintln!("Subtitle Studio Rust listening on port {}", app.config.port);
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
        let Some(permit) = acquire(&clients, app.config.max_clients) else {
            let _ = json_response(&mut stream, 503, &json!({"error":"Client limit reached"}));
            continue;
        };
        let app = app.clone();
        let streams = streams.clone();
        let _ = thread::Builder::new()
            .name("subtitle-http".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                let _permit = permit;
                match read_request(&mut stream) {
                    Ok(request) => {
                        if let Err(error) = handle(&app, &mut stream, &request, &streams)
                            && !error.starts_with("Disconnected")
                        {
                            let status = if error == "Job not found"
                                || error.contains("subtitle file is missing")
                            {
                                404
                            } else if error.starts_with("Conflict:") {
                                409
                            } else if error.contains("queue is full") {
                                429
                            } else {
                                400
                            };
                            let _ = json_response(
                                &mut stream,
                                status,
                                &json!({"error":error.trim_start_matches("Conflict: ")}),
                            );
                        }
                    }
                    Err(error) => {
                        let _ = json_response(&mut stream, 400, &json!({"error":error}));
                    }
                }
            });
    }
    Ok(())
}

pub fn healthcheck(config: &config::Config) -> Result<(), String> {
    let host = if config.bind_address == "0.0.0.0" || config.bind_address == "::" {
        "127.0.0.1"
    } else {
        &config.bind_address
    };
    let mut stream = TcpStream::connect((host, config.port)).map_err(|_| "Server unavailable")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "Could not set timeout")?;
    write!(
        stream,
        "GET /api/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| "Server unavailable")?;
    let mut body = String::new();
    stream
        .take(4096)
        .read_to_string(&mut body)
        .map_err(|_| "Server unavailable")?;
    if body.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err("Server unhealthy".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_and_json_guard() {
        let mut request = Request {
            method: "POST".into(),
            target: "/api/jobs".into(),
            headers: vec![
                ("Host".into(), "localhost:8099".into()),
                ("Content-Type".into(), "application/json".into()),
                ("Origin".into(), "http://localhost:8099".into()),
            ],
            body: vec![],
        };
        assert!(mutation_allowed(&request));
        request.headers[2].1 = "http://evil.example".into();
        assert!(!mutation_allowed(&request));
    }
    #[test]
    fn client_permits_are_bounded_and_released() {
        let count = Arc::new(AtomicUsize::new(0));
        let permit = acquire(&count, 1).unwrap();
        assert!(acquire(&count, 1).is_none());
        drop(permit);
        assert!(acquire(&count, 1).is_some());
    }
}
