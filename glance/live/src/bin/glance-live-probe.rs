#![forbid(unsafe_code)]

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::Full;
use hyper::{
    Request, Response, StatusCode, body::Incoming, header::HeaderValue, server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use hyper_util::rt::TokioTimer;
use serde_json::{Map, Value, json};
use std::{
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};

const FIXTURE_ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 9090);
const GATEWAY_ADDRESS: &str = "http://127.0.0.1:8081";
const CLIENTS: usize = 8;
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

type Body = Full<Bytes>;

#[derive(Clone, Default)]
struct Calls {
    qbittorrent: Arc<AtomicUsize>,
    jellyfin: Arc<AtomicUsize>,
}

impl Calls {
    fn snapshot(&self) -> (usize, usize) {
        (
            self.qbittorrent.load(Ordering::SeqCst),
            self.jellyfin.load(Ordering::SeqCst),
        )
    }
}

struct FixtureServer {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), std::io::Error>>,
}

impl FixtureServer {
    async fn start(calls: Calls) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind(FIXTURE_ADDRESS).await?;
        let torrents = fixture_torrents();
        let connection_slots = Arc::new(Semaphore::new(16));
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut stopped = Box::pin(async {
                let _ = stopped.await;
            });
            loop {
                tokio::select! {
                    _ = &mut stopped => return Ok(()),
                    accepted = listener.accept() => {
                        let (socket, _) = accepted?;
                        let Ok(permit) = connection_slots.clone().try_acquire_owned() else {
                            drop(socket);
                            continue;
                        };
                        let calls = calls.clone();
                        let torrents = torrents.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let service = service_fn(move |request| {
                                let calls = calls.clone();
                                let torrents = torrents.clone();
                                async move { Ok::<_, Infallible>(fixture_response(request, calls, torrents)) }
                            });
                            let mut builder = http1::Builder::new();
                            builder
                                .timer(TokioTimer::new())
                                .header_read_timeout(Duration::from_secs(5))
                                .max_headers(64)
                                .max_buf_size(64 * 1024);
                            let _ = builder
                                .serve_connection(TokioIo::new(socket), service)
                                .await;
                        });
                    }
                }
            }
        });
        Ok(Self {
            stop: Some(stop),
            task,
        })
    }

    async fn close(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        timeout(Duration::from_secs(2), self.task).await???;
        Ok(())
    }
}

fn fixture_torrents() -> Bytes {
    let mut torrents = Map::new();
    for index in 0..1000 {
        torrents.insert(
            format!("test-{index:04}"),
            json!({
                "name": format!("Fixture episode {index:04}.mkv"),
                "state": "downloading",
                "progress": 0.5,
                "dlspeed": 100,
                "upspeed": 2,
                "eta": 15
            }),
        );
    }
    Bytes::from(
        json!({
            "rid": 1,
            "full_update": true,
            "server_state": {"dl_info_speed": 100000, "up_info_speed": 2000},
            "torrents": torrents
        })
        .to_string(),
    )
}

fn json_response(payload: impl Into<Bytes>) -> Response<Body> {
    let mut response = Response::new(Full::new(payload.into()));
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    response
}

fn fixture_response(request: Request<Incoming>, calls: Calls, torrents: Bytes) -> Response<Body> {
    if request.method() != hyper::Method::GET {
        let mut denied = Response::new(Full::new(Bytes::from_static(b"method not allowed")));
        *denied.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
        return denied;
    }
    if request.headers().contains_key("transfer-encoding")
        || request
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|length| length > 0)
    {
        let mut denied =
            Response::new(Full::new(Bytes::from_static(b"request body not supported")));
        *denied.status_mut() = StatusCode::BAD_REQUEST;
        return denied;
    }
    let path = request.uri().path();
    match path {
        path if path.starts_with("/api/pages/") => {
            if request
                .headers()
                .get("cookie")
                .and_then(|v| v.to_str().ok())
                != Some("auth=valid")
            {
                let mut denied = Response::new(Full::new(Bytes::from_static(b"unauthorized")));
                *denied.status_mut() = StatusCode::UNAUTHORIZED;
                return denied;
            }
            json_response("fixture page")
        }
        "/api/v2/sync/maindata" => {
            calls.qbittorrent.fetch_add(1, Ordering::SeqCst);
            let is_full = request
                .uri()
                .query()
                .unwrap_or_default()
                .split('&')
                .any(|pair| pair == "rid=0");
            if is_full {
                json_response(torrents)
            } else {
                json_response(Bytes::from_static(
                    br#"{"rid":1,"full_update":false,"server_state":{"dl_info_speed":100000,"up_info_speed":2000},"torrents":{}}"#,
                ))
            }
        }
        "/Sessions" => {
            if !request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .contains("Token=\"test-key\"")
            {
                let mut denied = Response::new(Full::new(Bytes::from_static(b"unauthorized")));
                *denied.status_mut() = StatusCode::UNAUTHORIZED;
                return denied;
            }
            calls.jellyfin.fetch_add(1, Ordering::SeqCst);
            json_response(Bytes::from_static(
                br#"[{"Id":"test-session","UserName":"test-user","NowPlayingItem":{"Name":"Fixture film","Type":"Movie","RunTimeTicks":1000},"PlayState":{"PositionTicks":100,"PlayMethod":"DirectStream"}}]"#,
            ))
        }
        "/fixture" => Response::new(Full::new(Bytes::from_static(b"proxy fixture"))),
        _ => {
            let mut missing = Response::new(Full::new(Bytes::from_static(b"not found")));
            *missing.status_mut() = StatusCode::NOT_FOUND;
            missing
        }
    }
}

#[derive(Clone, Copy)]
enum Topic {
    Qbittorrent,
    Jellyfin,
}

impl Topic {
    fn page(self) -> &'static str {
        match self {
            Self::Qbittorrent => "downloads",
            Self::Jellyfin => "media",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Qbittorrent => "qbittorrent",
            Self::Jellyfin => "jellyfin",
        }
    }
}

async fn subscribe(
    client: reqwest::Client,
    index: usize,
    deadline: Instant,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let topic = if index < CLIENTS / 2 {
        Topic::Qbittorrent
    } else {
        Topic::Jellyfin
    };
    let url = format!("{GATEWAY_ADDRESS}/live/events?page={}", topic.page());
    let response = timeout(
        Duration::from_secs(4),
        client
            .get(url)
            .header("cookie", "auth=valid")
            .header("origin", GATEWAY_ADDRESS)
            .send(),
    )
    .await??;
    if response.status() != StatusCode::OK {
        return Err(format!("{} SSE returned {}", topic.name(), response.status()).into());
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type.starts_with("text/event-stream") {
        return Err(format!("{} SSE had an unexpected content type", topic.name()).into());
    }

    let mut stream = response.bytes_stream();
    let mut buffer = Vec::with_capacity(16 * 1024);
    let mut successes = 0;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let chunk = match timeout(remaining.min(Duration::from_secs(4)), stream.next()).await {
            Ok(Some(chunk)) => chunk?,
            Ok(None) => return Err("SSE stream closed before the probe deadline".into()),
            Err(_) if Instant::now() >= deadline => break,
            Err(error) => return Err(error.into()),
        };
        if chunk.len() > MAX_FRAME_BYTES.saturating_sub(buffer.len()) {
            return Err("SSE frame exceeded the probe's 2 MiB buffer limit".into());
        }
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.windows(2).position(|window| window == b"\n\n") {
            let frame: Vec<_> = buffer.drain(..end + 2).collect();
            let Some(data) = std::str::from_utf8(&frame)?
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
            else {
                continue;
            };
            let event: Value = serde_json::from_str(data)?;
            if event["status"] != "ok" {
                continue;
            }
            let items = match topic {
                Topic::Qbittorrent => event["data"]["torrents"].as_array(),
                Topic::Jellyfin => event["data"]["sessions"].as_array(),
            }
            .ok_or_else(|| format!("{} snapshot did not contain an array", topic.name()))?;
            let expected = match topic {
                Topic::Qbittorrent => 1000,
                Topic::Jellyfin => 1,
            };
            if items.len() != expected {
                return Err(format!(
                    "{} snapshot had {} items, expected {expected}",
                    topic.name(),
                    items.len()
                )
                .into());
            }
            successes += 1;
        }
    }
    if successes == 0 {
        return Err(format!("no successful {} snapshot", topic.name()).into());
    }
    Ok(successes)
}

fn duration_arg() -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let mut arguments = std::env::args().skip(1);
    let Some(argument) = arguments.next() else {
        return Ok(10);
    };
    if argument == "--help" || argument == "-h" {
        println!(
            "Usage: glance-live-probe [--seconds 2..60]\n\nRuns an offline fixture on 127.0.0.1:9090 and probes Glance Live at 127.0.0.1:8081. Configure the gateway's three upstream URLs to http://127.0.0.1:9090, set JELLYFIN_API_KEY=test-key, and disable Jellyfin WebSockets."
        );
        std::process::exit(0);
    }
    if argument != "--seconds" {
        return Err("expected --seconds followed by an integer from 2 to 60".into());
    }
    let raw = arguments.next().ok_or("missing --seconds value")?;
    if arguments.next().is_some() {
        return Err("unexpected trailing argument".into());
    }
    let seconds = raw.parse::<u64>()?;
    if !(2..=60).contains(&seconds) {
        return Err("--seconds must be between 2 and 60".into());
    }
    Ok(seconds)
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let seconds = duration_arg()?;
    let calls = Calls::default();
    let fixture = FixtureServer::start(calls.clone()).await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .read_timeout(Duration::from_secs(4))
        .pool_max_idle_per_host(2)
        .build()?;

    let proxy = timeout(
        Duration::from_secs(4),
        client.get(format!("{GATEWAY_ADDRESS}/fixture")).send(),
    )
    .await??;
    if proxy.status() != StatusCode::OK
        || timeout(
            Duration::from_secs(4),
            glance_live::relay::bounded_body(proxy, 64),
        )
        .await??
        .as_ref()
            != b"proxy fixture"
    {
        return Err("gateway proxy fixture check failed".into());
    }

    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let mut clients = tokio::task::JoinSet::new();
    for index in 0..CLIENTS {
        clients.spawn(subscribe(client.clone(), index, deadline));
    }
    let mut snapshots = 0usize;
    while let Some(result) = clients.join_next().await {
        snapshots += result??;
    }

    // Wait for disconnected SSE requests to be observed before checking that
    // each shared collector has stopped polling its fixture upstream.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let settled = calls.snapshot();
    tokio::time::sleep(Duration::from_secs(2)).await;
    if calls.snapshot() != settled {
        return Err(format!(
            "collectors kept polling after clients disconnected: {settled:?} -> {:?}",
            calls.snapshot()
        )
        .into());
    }
    let maximum_calls = seconds + 5;
    if settled.0 > maximum_calls as usize || settled.1 > maximum_calls as usize {
        return Err(format!(
            "unexpected duplicate collectors, upstream calls: qBittorrent={}, Jellyfin={}",
            settled.0, settled.1
        )
        .into());
    }

    fixture.close().await?;
    println!(
        "{}",
        json!({
            "clients": CLIENTS,
            "torrents": 1000,
            "snapshots": snapshots,
            "seconds": seconds,
            "upstream_calls": {"qbittorrent": settled.0, "jellyfin": settled.1},
            "collectors_stopped": true
        })
    );
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("glance-live-probe: {error}");
        std::process::exit(1);
    }
}
