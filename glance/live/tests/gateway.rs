use bytes::Bytes;
use futures_util::{SinkExt, Stream, StreamExt};
use glance_live::{
    config::{Config, base},
    gateway,
    relay::{Relay, Topic},
};
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Frame, Incoming},
    header::{HeaderValue, SET_COOKIE},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    convert::Infallible,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, handshake::server::create_response, protocol::Role},
};

type Handler = Arc<
    dyn Fn(Request<Incoming>) -> Pin<Box<dyn Future<Output = Response<TestBody>> + Send>>
        + Send
        + Sync,
>;

type TestBody = UnsyncBoxBody<Bytes, Infallible>;

fn stream_body<S>(stream: S) -> TestBody
where
    S: Stream<Item = Result<Frame<Bytes>, Infallible>> + Send + 'static,
{
    BodyExt::boxed_unsync(StreamBody::new(stream))
}

async fn http_server(handler: Handler) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let response = handler(request);
                    async move { Ok::<_, Infallible>(response.await) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    });
    (addr, task)
}

fn response(status: StatusCode, body: impl Into<Bytes>) -> Response<TestBody> {
    let mut response = Response::new(BodyExt::boxed_unsync(Full::new(body.into())));
    *response.status_mut() = status;
    response
}

fn json_response(value: Value) -> Response<TestBody> {
    let mut response = response(StatusCode::OK, Bytes::from(value.to_string()));
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    response
}

fn event_json(frame: &[u8]) -> Value {
    let text = std::str::from_utf8(frame).unwrap();
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    serde_json::from_str(data).unwrap()
}

type UpstreamFrame = Result<Frame<Bytes>, Infallible>;
type UpstreamSender = mpsc::UnboundedSender<UpstreamFrame>;

fn subforge_event(widget: Value) -> Bytes {
    Bytes::from(format!("event: widget\ndata: {}\n\n", widget))
}

fn subforge_stream_response(
    receiver: mpsc::UnboundedReceiver<UpstreamFrame>,
) -> Response<TestBody> {
    let body = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|frame| (frame, receiver))
    });
    let mut response = Response::new(stream_body(body));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

fn subforge_bytes_response(status: StatusCode, body: impl Into<Bytes>) -> Response<TestBody> {
    let mut response = response(status, body);
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

async fn wait_for_senders(senders: &Arc<Mutex<Vec<UpstreamSender>>>, count: usize) {
    timeout(Duration::from_secs(3), async {
        loop {
            if senders.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Subforge event stream did not connect");
}

async fn wait_for_subforge_snapshot(
    rx: &mut tokio::sync::watch::Receiver<Bytes>,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    timeout(Duration::from_secs(4), async {
        loop {
            rx.changed().await.unwrap();
            let snapshot = event_json(&rx.borrow_and_update());
            if predicate(&snapshot) {
                return snapshot;
            }
        }
    })
    .await
    .expect("expected Subforge snapshot was not published")
}

struct Fixture {
    client: reqwest::Client,
    relay: Arc<Relay>,
    gateway_addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    gateway_task: JoinHandle<()>,
    upstream_task: JoinHandle<()>,
}

impl Fixture {
    async fn start(handler: Handler, configure: impl FnOnce(&mut Config)) -> Self {
        let (upstream, upstream_task) = http_server(handler).await;
        let upstream_url = format!("http://{upstream}/");
        let mut config = Config {
            glance: base(&upstream_url, "GLANCE_UPSTREAM_URL").unwrap(),
            qbittorrent: base(&upstream_url, "QBITTORRENT_INTERNAL_URL").unwrap(),
            jellyfin: base(&upstream_url, "JELLYFIN_INTERNAL_URL").unwrap(),
            sonarr: base(&format!("http://{upstream}/sonarr"), "SONARR_INTERNAL_URL").unwrap(),
            radarr: base(&format!("http://{upstream}/radarr"), "RADARR_INTERNAL_URL").unwrap(),
            subforge: base(
                &format!("http://{upstream}/subforge"),
                "GLANCE_SUBFORGE_INTERNAL_URL",
            )
            .unwrap(),
            allowed_hosts: HashSet::from(["localhost".to_owned(), "127.0.0.1".to_owned()]),
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            qb_interval: Duration::from_millis(500),
            jellyfin_interval: Duration::from_secs(1),
            jellyfin_ws: false,
            max_connections: 8,
            max_http_connections: 16,
            sse_lifetime: Duration::from_secs(2),
            provider_bytes: 1024 * 1024,
            snapshot_bytes: 256 * 1024,
            max_torrents: 100,
            max_sessions: 100,
            qb_username: String::new(),
            qb_password: String::new(),
            jellyfin_key: "fixture-key".into(),
            sonarr_key: "fixture-sonarr-key".into(),
            radarr_key: "fixture-radarr-key".into(),
            subforge_token: "fixture-widget-token".into(),
        };
        configure(&mut config);
        let relay = Relay::new(config).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let gateway_addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let gateway_relay = relay.clone();
        let gateway_task = tokio::spawn(async move {
            let _ = gateway::serve(listener, gateway_relay, async {
                let _ = stopped.await;
            })
            .await;
        });
        Self {
            client: reqwest::Client::new(),
            relay,
            gateway_addr,
            stop: Some(stop),
            gateway_task,
            upstream_task,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.gateway_addr, path)
    }

    async fn close(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = timeout(Duration::from_secs(2), self.gateway_task).await;
        self.upstream_task.abort();
        self.relay.shutdown();
    }
}

#[allow(clippy::too_many_arguments)]
fn fixture_upstream(
    qb_calls: Arc<AtomicUsize>,
    login_calls: Arc<AtomicUsize>,
    jelly_calls: Arc<AtomicUsize>,
    leak_calls: Arc<AtomicUsize>,
    login_failures: Arc<AtomicUsize>,
    require_qb_auth: Arc<AtomicBool>,
    expire_sid_once: Arc<AtomicBool>,
    bad_login: Arc<AtomicBool>,
    redirect_login: Arc<AtomicBool>,
    login_204: Arc<AtomicBool>,
    websocket_attempts: Arc<AtomicUsize>,
    websocket_connected: Arc<AtomicBool>,
    redirect_websocket: Arc<AtomicBool>,
) -> Handler {
    Arc::new(move |mut request| {
        let qb_calls = qb_calls.clone();
        let login_calls = login_calls.clone();
        let jelly_calls = jelly_calls.clone();
        let leak_calls = leak_calls.clone();
        let login_failures = login_failures.clone();
        let require_qb_auth = require_qb_auth.clone();
        let expire_sid_once = expire_sid_once.clone();
        let bad_login = bad_login.clone();
        let redirect_login = redirect_login.clone();
        let login_204 = login_204.clone();
        let websocket_attempts = websocket_attempts.clone();
        let websocket_connected = websocket_connected.clone();
        let redirect_websocket = redirect_websocket.clone();
        Box::pin(async move {
            let path = request.uri().path();
            match path {
                "/api/pages/downloads/content/" | "/api/pages/media/content/" => {
                    if request
                        .headers()
                        .get("cookie")
                        .and_then(|v| v.to_str().ok())
                        == Some("auth=valid")
                    {
                        response(StatusCode::OK, "page")
                    } else {
                        response(StatusCode::UNAUTHORIZED, "denied")
                    }
                }
                "/api/v2/auth/login" => {
                    login_calls.fetch_add(1, Ordering::SeqCst);
                    if redirect_login.load(Ordering::SeqCst) {
                        let mut output = response(StatusCode::TEMPORARY_REDIRECT, "");
                        output
                            .headers_mut()
                            .insert("location", HeaderValue::from_static("/leak"));
                        return output;
                    }
                    if login_failures
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                        .is_ok()
                    {
                        return response(StatusCode::OK, "Fails.");
                    }
                    if bad_login.load(Ordering::SeqCst) {
                        return response(StatusCode::OK, "unexpected");
                    }
                    if login_204.load(Ordering::SeqCst) {
                        let mut output = response(StatusCode::NO_CONTENT, "");
                        output
                            .headers_mut()
                            .append(SET_COOKIE, HeaderValue::from_static("SID=valid; Path=/"));
                        return output;
                    }
                    let mut output = response(StatusCode::OK, "Ok.");
                    output
                        .headers_mut()
                        .append(SET_COOKIE, HeaderValue::from_static("SID=valid; Path=/"));
                    output
                }
                "/api/v2/sync/maindata" => {
                    qb_calls.fetch_add(1, Ordering::SeqCst);
                    if require_qb_auth.load(Ordering::SeqCst) {
                        if request
                            .headers()
                            .get("cookie")
                            .and_then(|v| v.to_str().ok())
                            != Some("SID=valid")
                        {
                            return response(StatusCode::UNAUTHORIZED, "unauthorized");
                        }
                        if expire_sid_once.swap(false, Ordering::SeqCst) {
                            return response(StatusCode::UNAUTHORIZED, "expired");
                        }
                    }
                    let rid = request.uri().query().unwrap_or_default();
                    let value = if rid.ends_with("rid=0") {
                        json!({"rid":1,"full_update":true,"server_state":{"dl_info_speed":123,"up_info_speed":7},"torrents":{"hash-a":{"name":"episode.mkv","state":"downloading","progress":0.5,"dlspeed":100,"upspeed":2,"eta":15},"hash-seed":{"name":"seed.mkv","state":"uploading","progress":1}}})
                    } else {
                        json!({"rid":2,"server_state":{"up_info_speed":9},"torrents":{"hash-a":{"state":"pausedDL"}},"torrents_removed":["hash-seed"]})
                    };
                    json_response(value)
                }
                "/Sessions" => {
                    jelly_calls.fetch_add(1, Ordering::SeqCst);
                    if request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .contains("Token=\"fixture-key\"")
                    {
                        json_response(
                            json!([{"Id":"idle","UserName":"listener"},{"Id":"rest","UserName":"fixture","NowPlayingItem":{"Name":"Rest title","Type":"Episode","RunTimeTicks":1000},"PlayState":{"PositionTicks":20,"PlayMethod":"DirectStream"}}]),
                        )
                    } else {
                        response(StatusCode::UNAUTHORIZED, "unauthorized")
                    }
                }
                "/socket" => {
                    websocket_attempts.fetch_add(1, Ordering::SeqCst);
                    if request.uri().query().is_some() {
                        return response(
                            StatusCode::BAD_REQUEST,
                            "query must not contain the API key",
                        );
                    }
                    if !request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .contains("Token=\"fixture-key\"")
                    {
                        return response(StatusCode::UNAUTHORIZED, "missing authorization");
                    }
                    if redirect_websocket.load(Ordering::SeqCst) {
                        let mut output = response(StatusCode::FOUND, "");
                        output
                            .headers_mut()
                            .insert("location", HeaderValue::from_static("/leak"));
                        return output;
                    }
                    let mut handshake = Request::builder()
                        .method(request.method())
                        .uri(request.uri())
                        .version(request.version())
                        .body(())
                        .unwrap();
                    *handshake.headers_mut() = request.headers().clone();
                    let Ok(handshake_response) = create_response(&handshake) else {
                        return response(StatusCode::BAD_REQUEST, "invalid websocket request");
                    };
                    let mut output = response(StatusCode::SWITCHING_PROTOCOLS, "");
                    for (name, value) in handshake_response.headers() {
                        output.headers_mut().insert(name.clone(), value.clone());
                    }
                    let on_upgrade = hyper::upgrade::on(&mut request);
                    let connected = websocket_connected.clone();
                    tokio::spawn(async move {
                        let Ok(upgraded) = on_upgrade.await else {
                            return;
                        };
                        let io = TokioIo::new(upgraded);
                        let mut socket =
                            WebSocketStream::from_raw_socket(io, Role::Server, None).await;
                        connected.store(true, Ordering::SeqCst);
                        while let Some(Ok(message)) = socket.next().await {
                            if let Message::Text(text) = message
                                && serde_json::from_str::<Value>(&text)
                                    .ok()
                                    .and_then(|v| v.get("MessageType").cloned())
                                    .and_then(|v| v.as_str().map(str::to_owned))
                                    .as_deref()
                                    == Some("SessionsStart")
                            {
                                let data = json!([{"Id":"ws","UserName":"listener","NowPlayingItem":{"Name":"Film","Type":"Movie","RunTimeTicks":1000},"PlayState":{"PositionTicks":100,"IsPaused":false,"PlayMethod":"DirectStream"}}]);
                                let payload = json!({"MessageType":"Sessions","Data":data});
                                if socket
                                    .send(Message::Text(payload.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    });
                    output
                }
                "/plain" => {
                    let mut output = response(StatusCode::OK, "proxy-body");
                    output
                        .headers_mut()
                        .append(SET_COOKIE, HeaderValue::from_static("one=1; Path=/"));
                    output
                        .headers_mut()
                        .append(SET_COOKIE, HeaderValue::from_static("two=2; Path=/"));
                    output
                        .headers_mut()
                        .insert("x-origin", HeaderValue::from_static("fixture"));
                    output
                        .headers_mut()
                        .insert("x-hop", HeaderValue::from_static("private"));
                    output
                        .headers_mut()
                        .insert("connection", HeaderValue::from_static("x-hop"));
                    output
                }
                "/leak" => {
                    leak_calls.fetch_add(1, Ordering::SeqCst);
                    response(StatusCode::OK, "leaked")
                }
                _ => response(StatusCode::NOT_FOUND, "missing"),
            }
        })
    })
}

#[allow(clippy::type_complexity)]
fn counters() -> (
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    Handler,
) {
    let qb = Arc::new(AtomicUsize::new(0));
    let login = Arc::new(AtomicUsize::new(0));
    let jelly = Arc::new(AtomicUsize::new(0));
    let leak = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(AtomicUsize::new(0));
    let require_auth = Arc::new(AtomicBool::new(false));
    let expire_sid = Arc::new(AtomicBool::new(false));
    let bad_login = Arc::new(AtomicBool::new(false));
    let redirect_login = Arc::new(AtomicBool::new(false));
    let login_204 = Arc::new(AtomicBool::new(false));
    let ws_attempts = Arc::new(AtomicUsize::new(0));
    let ws_connected = Arc::new(AtomicBool::new(false));
    let redirect_ws = Arc::new(AtomicBool::new(false));
    let handler = fixture_upstream(
        qb.clone(),
        login.clone(),
        jelly.clone(),
        leak.clone(),
        failures.clone(),
        require_auth.clone(),
        expire_sid.clone(),
        bad_login.clone(),
        redirect_login.clone(),
        login_204.clone(),
        ws_attempts.clone(),
        ws_connected.clone(),
        redirect_ws.clone(),
    );
    (
        qb,
        login,
        jelly,
        leak,
        failures,
        require_auth,
        expire_sid,
        bad_login,
        redirect_login,
        login_204,
        ws_attempts,
        ws_connected,
        redirect_ws,
        handler,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn proxy_preserves_body_and_cookies_while_filtering_hop_headers() {
    let (_, _, _, _, _, _, _, _, _, _, _, _, _, handler) = counters();
    let fixture = Fixture::start(handler, |_| {}).await;
    let response = fixture
        .client
        .get(fixture.url("/plain"))
        .header("host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "proxy-body");
    let response = fixture
        .client
        .get(fixture.url("/plain"))
        .header("host", "localhost")
        .send()
        .await
        .unwrap();
    let cookies: Vec<_> = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(cookies, ["one=1; Path=/", "two=2; Path=/"]);
    assert!(response.headers().get("x-hop").is_none());
    assert_eq!(response.headers().get("x-origin").unwrap(), "fixture");
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn proxy_forwards_request_body_and_query_while_replacing_untrusted_forwarding_headers() {
    let seen = Arc::new(Mutex::new(None));
    let captured = seen.clone();
    let handler: Handler = Arc::new(move |request| {
        let captured = captured.clone();
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = body.collect().await.unwrap().to_bytes();
            *captured.lock().unwrap() = Some((parts.method, parts.uri, parts.headers, body));
            response(StatusCode::OK, "forwarded")
        })
    });
    let fixture = Fixture::start(handler, |_| {}).await;
    let result = fixture
        .client
        .post(fixture.url("/forwarded?item=1"))
        .header("host", "localhost")
        .header("content-type", "text/plain")
        .header("x-request-id", "fixture-request")
        .header(
            "forwarded",
            "for=203.0.113.9;host=attacker.example;proto=https",
        )
        .header("x-forwarded-for", "203.0.113.9")
        .header("x-forwarded-host", "attacker.example")
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-uri", "/forged")
        .body("request-payload")
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::OK);
    let (method, uri, headers, body) = seen.lock().unwrap().take().unwrap();
    assert_eq!(method, hyper::Method::POST);
    assert_eq!(uri.path_and_query().unwrap().as_str(), "/forwarded?item=1");
    assert_eq!(body, "request-payload");
    assert_eq!(headers.get("x-request-id").unwrap(), "fixture-request");
    assert_eq!(headers.get("x-forwarded-host").unwrap(), "localhost");
    assert_eq!(headers.get("x-forwarded-proto").unwrap(), "http");
    assert_eq!(headers.get("x-forwarded-uri").unwrap(), "/forwarded?item=1");
    assert_eq!(headers.get("x-forwarded-for").unwrap(), "127.0.0.1");
    assert!(headers.get("forwarded").is_none());
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn sse_checks_host_origin_and_page_auth_then_enforces_connection_and_lifetime_limits() {
    let (_, _, _, _, _, _, _, _, _, _, _, _, _, handler) = counters();
    let fixture = Fixture::start(handler, |config| {
        config.max_connections = 1;
        config.sse_lifetime = Duration::from_millis(700);
    })
    .await;
    let health = fixture
        .client
        .get(fixture.url("/live/health"))
        .header("host", "attacker.example")
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::MISDIRECTED_REQUEST);
    let denied = fixture
        .client
        .get(fixture.url("/live/events?page=downloads"))
        .header("host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let cross = fixture
        .client
        .get(fixture.url("/live/events?page=downloads"))
        .header("host", "localhost")
        .header("cookie", "auth=valid")
        .header("origin", "https://elsewhere.example")
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);
    let stream = fixture
        .client
        .get(fixture.url("/live/events?page=downloads"))
        .header("host", "localhost")
        .header("cookie", "auth=valid")
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert_eq!(
        stream.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let mut chunks = stream.bytes_stream();
    let first = timeout(
        Duration::from_secs(2),
        futures_util::StreamExt::next(&mut chunks),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let text = String::from_utf8_lossy(&first);
    assert!(
        text.contains("event: qbittorrent\ndata: {\"status\":\"warming\""),
        "{text}"
    );
    let capped = fixture
        .client
        .get(fixture.url("/live/events?page=downloads"))
        .header("host", "localhost")
        .header("cookie", "auth=valid")
        .send()
        .await
        .unwrap();
    assert_eq!(capped.status(), StatusCode::SERVICE_UNAVAILABLE);
    let ended = timeout(Duration::from_secs(2), async {
        while futures_util::StreamExt::next(&mut chunks).await.is_some() {}
    })
    .await;
    assert!(
        ended.is_ok(),
        "SSE stream should end at its configured lifetime"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn shared_collectors_publish_qb_deltas_and_jellyfin_rest_snapshots_then_stop() {
    let (qb_calls, _, jelly_calls, _, _, _, _, _, _, _, ws_attempts, _, _, handler) = counters();
    let fixture = Fixture::start(handler, |_| {}).await;
    let mut first = fixture.relay.subscribe(Topic::Qbittorrent).unwrap();
    let mut second = fixture.relay.subscribe(Topic::Qbittorrent).unwrap();
    timeout(Duration::from_secs(2), first.rx.changed())
        .await
        .unwrap()
        .unwrap();
    let first_event = event_json(&first.rx.borrow_and_update());
    assert_eq!(first_event["status"], "ok");
    assert_eq!(first_event["data"]["downloadSpeed"], 123);
    timeout(Duration::from_secs(2), second.rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        qb_calls.load(Ordering::SeqCst),
        1,
        "two subscribers should share one poller"
    );
    timeout(Duration::from_secs(2), first.rx.changed())
        .await
        .unwrap()
        .unwrap();
    let delta = event_json(&first.rx.borrow_and_update());
    assert_eq!(delta["data"]["torrents"][0]["name"], "episode.mkv");
    assert_eq!(delta["data"]["torrents"][0]["state"], "pausedDL");
    assert_eq!(delta["data"]["seedingCount"], 0);
    assert_eq!(delta["data"]["uploadSpeed"], 9);
    drop(first);
    drop(second);
    let mut restarted = fixture.relay.subscribe(Topic::Qbittorrent).unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            restarted.rx.changed().await.unwrap();
            if event_json(&restarted.rx.borrow_and_update())["status"] == "ok" {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        qb_calls.load(Ordering::SeqCst) >= 3,
        "collector should restart after its last subscriber leaves"
    );
    drop(restarted);

    let mut jellyfin = fixture.relay.subscribe(Topic::Jellyfin).unwrap();
    timeout(Duration::from_secs(2), jellyfin.rx.changed())
        .await
        .unwrap()
        .unwrap();
    let event = event_json(&jellyfin.rx.borrow_and_update());
    assert_eq!(event["source"], "rest");
    assert_eq!(event["data"]["sessions"][0]["id"], "rest");
    assert_eq!(event["data"]["sessions"][0]["playMethod"], "DirectStream");
    timeout(Duration::from_secs(2), jellyfin.rx.changed())
        .await
        .unwrap()
        .unwrap();
    let next_poll = event_json(&jellyfin.rx.borrow_and_update());
    assert_eq!(next_poll["source"], "rest");
    assert!(jelly_calls.load(Ordering::SeqCst) >= 2);
    assert_eq!(ws_attempts.load(Ordering::SeqCst), 0);
    drop(jellyfin);
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn qb_upstream_failure_and_oversize_keep_the_last_good_snapshot_stale() {
    let calls = Arc::new(AtomicUsize::new(0));
    let upstream_calls = calls.clone();
    let handler: Handler = Arc::new(move |_request| {
        let call = upstream_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match call {
                0 => json_response(
                    json!({"rid":1,"full_update":true,"server_state":{"dl_info_speed":42},"torrents":{"one":{"name":"kept.mkv","state":"downloading"}}}),
                ),
                1 => response(StatusCode::SERVICE_UNAVAILABLE, "temporarily unavailable"),
                _ => response(StatusCode::OK, Bytes::from(vec![b'x'; 512])),
            }
        })
    });
    let fixture = Fixture::start(handler, |config| config.provider_bytes = 256).await;
    let mut subscription = fixture.relay.subscribe(Topic::Qbittorrent).unwrap();
    timeout(Duration::from_secs(2), subscription.rx.changed())
        .await
        .unwrap()
        .unwrap();
    let good = event_json(&subscription.rx.borrow_and_update());
    assert_eq!(good["status"], "ok");
    assert_eq!(good["data"]["torrents"][0]["name"], "kept.mkv");
    let updated = good["updatedAt"].clone();

    timeout(Duration::from_secs(2), subscription.rx.changed())
        .await
        .unwrap()
        .unwrap();
    let failed = event_json(&subscription.rx.borrow_and_update());
    assert_eq!(failed["status"], "stale");
    assert_eq!(failed["updatedAt"], updated);
    assert_eq!(failed["data"]["torrents"][0]["name"], "kept.mkv");

    timeout(Duration::from_secs(2), subscription.rx.changed())
        .await
        .unwrap()
        .unwrap();
    let oversized = event_json(&subscription.rx.borrow_and_update());
    assert_eq!(oversized["status"], "stale");
    assert_eq!(oversized["updatedAt"], updated);
    assert_eq!(oversized["data"]["torrents"][0]["name"], "kept.mkv");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    drop(subscription);
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn jellyfin_websocket_stays_quiet_while_rest_reconciles_and_redirects_are_not_followed() {
    let (
        _,
        _,
        jelly_calls,
        _leak_calls,
        _,
        _,
        _,
        _,
        _,
        _,
        _ws_attempts,
        ws_connected,
        _redirect_ws,
        handler,
    ) = counters();
    let fixture = Fixture::start(handler, |config| {
        config.jellyfin_ws = true;
    })
    .await;
    let mut subscription = fixture.relay.subscribe(Topic::Jellyfin).unwrap();
    let mut sources = Vec::new();
    for _ in 0..3 {
        timeout(Duration::from_secs(3), subscription.rx.changed())
            .await
            .unwrap()
            .unwrap();
        let event = event_json(&subscription.rx.borrow_and_update());
        sources.push(event["source"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        sources,
        ["rest", "websocket", "rest"],
        "attempts={}, connected={}",
        _ws_attempts.load(Ordering::SeqCst),
        ws_connected.load(Ordering::SeqCst)
    );
    assert!(ws_connected.load(Ordering::SeqCst));
    assert!(
        jelly_calls.load(Ordering::SeqCst) >= 2,
        "REST should reconcile a quiet WebSocket"
    );
    drop(subscription);
    fixture.close().await;

    let (
        _,
        _,
        jelly_calls,
        leak_calls,
        _,
        _,
        _,
        _,
        _,
        _,
        ws_attempts,
        ws_connected,
        redirect_ws,
        handler,
    ) = counters();
    redirect_ws.store(true, Ordering::SeqCst);
    let fixture = Fixture::start(handler, |config| {
        config.jellyfin_ws = true;
    })
    .await;
    let mut subscription = fixture.relay.subscribe(Topic::Jellyfin).unwrap();
    timeout(Duration::from_secs(2), subscription.rx.changed())
        .await
        .unwrap()
        .unwrap();
    let mut last = event_json(&subscription.rx.borrow_and_update());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline && jelly_calls.load(Ordering::SeqCst) < 2 {
        timeout(Duration::from_secs(2), subscription.rx.changed())
            .await
            .unwrap()
            .unwrap();
        last = event_json(&subscription.rx.borrow_and_update());
    }
    assert!(ws_attempts.load(Ordering::SeqCst) >= 1);
    assert!(!ws_connected.load(Ordering::SeqCst));
    assert_eq!(leak_calls.load(Ordering::SeqCst), 0);
    assert_eq!(last["status"], "ok");
    assert_eq!(last["source"], "rest");
    drop(subscription);
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn authenticated_qb_login_retries_and_refreshes_an_expired_sid() {
    let (
        qb_calls,
        login_calls,
        _,
        _,
        login_failures,
        require_auth,
        expire_sid,
        _,
        _,
        login_204,
        _,
        _,
        _,
        handler,
    ) = counters();
    login_failures.store(1, Ordering::SeqCst);
    require_auth.store(true, Ordering::SeqCst);
    expire_sid.store(true, Ordering::SeqCst);
    login_204.store(true, Ordering::SeqCst);
    let fixture = Fixture::start(handler, |config| {
        config.qb_username = "fixture-user".into();
        config.qb_password = "fixture-pass".into();
    })
    .await;
    let mut subscription = fixture.relay.subscribe(Topic::Qbittorrent).unwrap();
    let event = timeout(Duration::from_secs(3), async {
        loop {
            subscription.rx.changed().await.unwrap();
            let event = event_json(&subscription.rx.borrow_and_update());
            if event["status"] == "ok" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(event["data"]["downloadSpeed"], 123);
    assert_eq!(
        login_calls.load(Ordering::SeqCst),
        3,
        "first login fails, then SID expires and must be refreshed"
    );
    assert!(qb_calls.load(Ordering::SeqCst) >= 2);
    drop(subscription);
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn authenticated_qb_login_accepts_ok_200_response() {
    let (qb_calls, login_calls, _, _, _, require_auth, _, _, _, _, _, _, _, handler) = counters();
    require_auth.store(true, Ordering::SeqCst);
    let fixture = Fixture::start(handler, |config| {
        config.qb_username = "fixture-user".into();
        config.qb_password = "fixture-pass".into();
    })
    .await;
    let mut subscription = fixture.relay.subscribe(Topic::Qbittorrent).unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            subscription.rx.changed().await.unwrap();
            if event_json(&subscription.rx.borrow_and_update())["status"] == "ok" {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(login_calls.load(Ordering::SeqCst), 1);
    assert_eq!(qb_calls.load(Ordering::SeqCst), 1);
    drop(subscription);
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_or_redirected_qb_login_never_fetches_data_or_follows_redirects() {
    for redirect in [false, true] {
        let (qb_calls, _, _, leak_calls, _, _, _, bad_login, redirect_login, _, _, _, _, handler) =
            counters();
        bad_login.store(!redirect, Ordering::SeqCst);
        redirect_login.store(redirect, Ordering::SeqCst);
        let fixture = Fixture::start(handler, |config| {
            config.qb_username = "fixture-user".into();
            config.qb_password = "fixture-pass".into();
        })
        .await;
        let mut subscription = fixture.relay.subscribe(Topic::Qbittorrent).unwrap();
        timeout(Duration::from_secs(2), subscription.rx.changed())
            .await
            .unwrap()
            .unwrap();
        let event = event_json(&subscription.rx.borrow_and_update());
        assert_eq!(event["status"], "stale");
        assert!(event["data"].is_null());
        assert_eq!(qb_calls.load(Ordering::SeqCst), 0);
        assert_eq!(leak_calls.load(Ordering::SeqCst), 0);
        drop(subscription);
        fixture.close().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn calendar_projects_provider_fields_and_pads_date_queries_without_exposing_keys() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let handler: Handler = Arc::new(move |request| {
        let captured = captured.clone();
        Box::pin(async move {
            let path = request.uri().path();
            if path == "/api/pages/media/content/" {
                assert_eq!(request.headers().get("cookie").unwrap(), "auth=valid");
                assert!(request.headers().get("x-api-key").is_none());
                return response(StatusCode::OK, "authorized");
            }
            assert!(request.headers().get("cookie").is_none());
            captured.lock().unwrap().push((
                request.uri().to_string(),
                request
                    .headers()
                    .get("x-api-key")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ));
            if path == "/sonarr/api/v3/calendar" {
                json_response(json!([
                    {"id": 1, "seriesId": 20, "title":"First", "airDateUtc":"2026-10-19T23:30:00Z", "series":{"title":"Cyberpunk", "titleSlug":"cyberpunk"}, "seasonNumber":2,"episodeNumber":1,"path":"private-provider-path", "apiKey":"private-provider-key"},
                    {"id": 2, "seriesId": 20, "title":"Second", "airDateUtc":"2026-10-20T00:00:00Z", "series":{"title":"Cyberpunk", "titleSlug":"cyberpunk"}, "seasonNumber":2,"episodeNumber":2},
                    {"id": 3, "seriesId": 20, "title":"Outside", "airDateUtc":"2027-01-20T00:00:00Z", "series":{"title":"Cyberpunk", "titleSlug":"cyberpunk"}, "seasonNumber":2,"episodeNumber":3}
                ]))
            } else {
                assert_eq!(path, "/radarr/api/v3/calendar");
                json_response(
                    json!([{"id": 30,"title":"Movie","titleSlug":"12345","releaseDate":"2026-10-20T00:00:00Z","digitalRelease":"2026-11-20T00:00:00Z","path":"private-movie-path"}]),
                )
            }
        })
    });
    let fixture = Fixture::start(handler, |_| {}).await;
    let result = fixture
        .client
        .get(fixture.url("/live/calendar?start=2026-10-20&end=2026-10-21"))
        .header("host", "localhost")
        .header("cookie", "auth=valid")
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::OK);
    assert_eq!(result.headers().get("cache-control").unwrap(), "no-store");
    let body = result.text().await.unwrap();
    assert!(!body.contains("private-"));
    assert!(!body.contains("fixture-"));
    let calendar: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(calendar["start"], "2026-10-20");
    assert_eq!(calendar["end"], "2026-10-21");
    let events = calendar["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["seriesId"], 20);
    assert_eq!(events[0]["date"], "2026-10-19T23:30:00Z");
    assert_eq!(events[1]["episode"], 2);
    assert_eq!(events[2]["kind"], "movie");
    assert_eq!(events[2]["date"], "2026-10-20T00:00:00Z");
    {
        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].0,
            "/sonarr/api/v3/calendar?start=2026-10-19&end=2026-10-22&includeSeries=true"
        );
        assert_eq!(calls[0].1, "fixture-sonarr-key");
        assert_eq!(
            calls[1].0,
            "/radarr/api/v3/calendar?start=2026-10-19&end=2026-10-22"
        );
        assert_eq!(calls[1].1, "fixture-radarr-key");
    }
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn calendar_rejects_bad_ranges_origins_methods_and_unauthorized_requests_before_provider_access()
 {
    let providers = Arc::new(AtomicUsize::new(0));
    let captured = providers.clone();
    let handler: Handler = Arc::new(move |request| {
        let captured = captured.clone();
        Box::pin(async move {
            if request.uri().path() == "/api/pages/media/content/" {
                return response(StatusCode::UNAUTHORIZED, "auth required");
            }
            captured.fetch_add(1, Ordering::SeqCst);
            json_response(json!([]))
        })
    });
    let fixture = Fixture::start(handler, |_| {}).await;
    for (query, origin, expected) in [
        (
            "start=2026-02-29&end=2026-03-01",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            "start=2026-10-20&end=2026-10-21",
            Some("https://elsewhere.example"),
            StatusCode::FORBIDDEN,
        ),
        (
            "start=2026-10-20&end=2026-10-21",
            None,
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        let mut request = fixture
            .client
            .get(fixture.url(&format!("/live/calendar?{query}")))
            .header("host", "localhost");
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        assert_eq!(request.send().await.unwrap().status(), expected);
    }
    let denied = fixture
        .client
        .post(fixture.url("/live/calendar?start=2026-10-20&end=2026-10-21"))
        .header("host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(denied.headers().get("allow").unwrap(), "GET");
    assert_eq!(providers.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn calendar_rejects_provider_redirects_oversize_and_unsafe_link_paths() {
    for mode in ["redirect", "oversize", "slug", "output"] {
        let leak = Arc::new(AtomicUsize::new(0));
        let captured = leak.clone();
        let handler: Handler = Arc::new(move |request| {
            let captured = captured.clone();
            Box::pin(async move {
                if request.uri().path() == "/api/pages/media/content/" {
                    return response(StatusCode::OK, "authorized");
                }
                if request.uri().path() == "/leak" {
                    captured.fetch_add(1, Ordering::SeqCst);
                    return json_response(json!([]));
                }
                match mode {
                    "redirect" => {
                        let mut result = response(StatusCode::FOUND, "redirect");
                        result
                            .headers_mut()
                            .insert("location", HeaderValue::from_static("/leak"));
                        result
                    }
                    "oversize" => response(StatusCode::OK, Bytes::from(vec![b'x'; 2048])),
                    _ => json_response(
                        json!([{"id":1,"title":"Movie","titleSlug":if mode == "slug" {"../private"} else {"123"},"releaseDate":"2026-10-20T00:00:00Z"}]),
                    ),
                }
            })
        });
        let fixture = Fixture::start(handler, |config| {
            config.sonarr_key.clear();
            config.provider_bytes = 1024;
            if mode == "output" {
                config.snapshot_bytes = 32;
            }
        })
        .await;
        let result = fixture
            .client
            .get(fixture.url("/live/calendar?start=2026-10-20&end=2026-10-21"))
            .header("host", "localhost")
            .send()
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::BAD_GATEWAY, "{mode}");
        assert_eq!(leak.load(Ordering::SeqCst), 0);
        fixture.close().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn calendar_bounds_concurrent_provider_requests() {
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let captured_calls = calls.clone();
    let captured_gate = gate.clone();
    let handler: Handler = Arc::new(move |request| {
        let calls = captured_calls.clone();
        let gate = captured_gate.clone();
        Box::pin(async move {
            if request.uri().path() == "/api/pages/media/content/" {
                return response(StatusCode::OK, "authorized");
            }
            calls.fetch_add(1, Ordering::SeqCst);
            gate.acquire().await.unwrap().forget();
            json_response(json!([]))
        })
    });
    let fixture = Fixture::start(handler, |config| config.sonarr_key.clear()).await;
    let mut pending = Vec::new();
    for _ in 0..2 {
        let client = fixture.client.clone();
        let url = fixture.url("/live/calendar?start=2026-10-20&end=2026-10-21");
        pending.push(tokio::spawn(async move {
            client
                .get(url)
                .header("host", "localhost")
                .send()
                .await
                .unwrap()
        }));
    }
    timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let denied = fixture
        .client
        .get(fixture.url("/live/calendar?start=2026-10-20&end=2026-10-21"))
        .header("host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    gate.add_permits(2);
    for task in pending {
        assert_eq!(task.await.unwrap().status(), StatusCode::OK);
    }
    fixture.close().await;
}

fn subforge_widget() -> Value {
    json!({"converted_count":5,"queued_count":2,"configured_root_count":1,"recent_failures_count":0,"selected_languages":["eng"],"active_job_count":2,"active_jobs":[
        {"id":"0123456789abcdef","kind":"extract","status":"running","phase":"extracting","processed":1,"total":3,"progress_percent":33,"elapsed_seconds":10,"eta_seconds":20,"liveness":"running","wait_reason":null},
        {"id":"fedcba9876543210","kind":"ocr","status":"running","phase":"ocr_progress","processed":0,"total":1,"progress_percent":0,"elapsed_seconds":12,"eta_seconds":null,"liveness":"running","wait_reason":null}
    ]})
}

#[tokio::test(flavor = "current_thread")]
async fn subforge_shared_collector_projects_multiple_jobs_and_stops_without_subscribers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let captured_calls = calls.clone();
    let senders = Arc::new(Mutex::new(Vec::<UpstreamSender>::new()));
    let captured_senders = senders.clone();
    let handler: Handler = Arc::new(move |request| {
        let calls = captured_calls.clone();
        let senders = captured_senders.clone();
        Box::pin(async move {
            assert_eq!(request.uri().path(), "/subforge/api/widget/events");
            assert_eq!(
                request.headers().get("authorization").unwrap(),
                "Bearer fixture-widget-token"
            );
            assert!(request.headers().get("cookie").is_none());
            assert!(
                request
                    .headers()
                    .get("accept")
                    .is_some_and(|value| value.to_str().unwrap().contains("text/event-stream"))
            );
            calls.fetch_add(1, Ordering::SeqCst);
            let (sender, receiver) = mpsc::unbounded_channel();
            senders.lock().unwrap().push(sender);
            subforge_stream_response(receiver)
        })
    });
    let fixture = Fixture::start(handler, |_| {}).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut first = fixture.relay.subscribe(Topic::Subforge).unwrap();
    let mut second = fixture.relay.subscribe(Topic::Subforge).unwrap();
    wait_for_senders(&senders, 1).await;
    let sender = senders.lock().unwrap()[0].clone();
    let mut widget = subforge_widget();
    widget["active_jobs"][0]["current"] =
        json!({"path":"/private/movie.mkv","error":"private-diagnostic"});
    sender
        .send(Ok(Frame::data(subforge_event(widget))))
        .unwrap();
    let snapshot =
        wait_for_subforge_snapshot(&mut first.rx, |snapshot| snapshot["status"] == "ok").await;
    let shared_snapshot =
        wait_for_subforge_snapshot(&mut second.rx, |snapshot| snapshot["status"] == "ok").await;
    assert_eq!(snapshot["status"], "ok");
    assert_eq!(shared_snapshot["data"], snapshot["data"]);
    assert_eq!(snapshot["data"]["activeJobCount"], 2);
    assert_eq!(snapshot["data"]["jobs"][0]["etaSeconds"], 20);
    assert!(snapshot["data"]["jobs"][1]["etaSeconds"].is_null());
    assert!(!snapshot.to_string().contains("private"));
    assert!(!snapshot.to_string().contains("fixture-widget-token"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "subscribers must share one upstream event stream, without REST polling"
    );
    let mut update = subforge_widget();
    update["converted_count"] = json!(6);
    sender
        .send(Ok(Frame::data(subforge_event(update))))
        .unwrap();
    let updated = wait_for_subforge_snapshot(&mut first.rx, |snapshot| {
        snapshot["data"]["convertedCount"] == 6
    })
    .await;
    let shared_update = wait_for_subforge_snapshot(&mut second.rx, |snapshot| {
        snapshot["data"]["convertedCount"] == 6
    })
    .await;
    assert_eq!(updated["data"]["convertedCount"], 6);
    assert_eq!(shared_update["data"], updated["data"]);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "updates must arrive on the open stream"
    );
    drop(first);
    drop(second);
    timeout(Duration::from_secs(2), async {
        while !sender.is_closed() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the upstream stream should stop when its last subscriber leaves");
    let mut restarted = fixture.relay.subscribe(Topic::Subforge).unwrap();
    wait_for_senders(&senders, 2).await;
    let restart_sender = senders.lock().unwrap()[1].clone();
    restart_sender
        .send(Ok(Frame::data(subforge_event(subforge_widget()))))
        .unwrap();
    let restarted_snapshot =
        wait_for_subforge_snapshot(&mut restarted.rx, |snapshot| snapshot["status"] == "ok").await;
    assert_eq!(restarted_snapshot["status"], "ok",);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(restarted);
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn subforge_failures_oversize_bad_shapes_and_redirects_retain_the_last_good_snapshot() {
    #[derive(Clone, Copy)]
    enum Failure {
        Unauthorized,
        Oversized,
        InvalidFrame,
        MalformedJson,
        BadShape,
        Redirect,
    }
    let failures = [
        Failure::Unauthorized,
        Failure::Oversized,
        Failure::InvalidFrame,
        Failure::MalformedJson,
        Failure::BadShape,
        Failure::Redirect,
    ];
    for failure in failures {
        let calls = Arc::new(AtomicUsize::new(0));
        let leaks = Arc::new(AtomicUsize::new(0));
        let senders = Arc::new(Mutex::new(Vec::<UpstreamSender>::new()));
        let captured_calls = calls.clone();
        let captured_leaks = leaks.clone();
        let captured_senders = senders.clone();
        let handler: Handler = Arc::new(move |request| {
            let calls = captured_calls.clone();
            let leaks = captured_leaks.clone();
            let senders = captured_senders.clone();
            Box::pin(async move {
                if request.uri().path() == "/leak" {
                    leaks.fetch_add(1, Ordering::SeqCst);
                    return json_response(json!({}));
                }
                assert_eq!(request.uri().path(), "/subforge/api/widget/events");
                assert_eq!(
                    request.headers().get("authorization").unwrap(),
                    "Bearer fixture-widget-token"
                );
                assert!(request.headers().get("cookie").is_none());
                match calls.fetch_add(1, Ordering::SeqCst) {
                    0 => {
                        let (sender, receiver) = mpsc::unbounded_channel();
                        senders.lock().unwrap().push(sender);
                        subforge_stream_response(receiver)
                    }
                    _ => match failure {
                        Failure::Unauthorized => {
                            subforge_bytes_response(StatusCode::UNAUTHORIZED, "not authorized")
                        }
                        Failure::Oversized => {
                            subforge_bytes_response(StatusCode::OK, Bytes::from(vec![b'x'; 2048]))
                        }
                        Failure::InvalidFrame => subforge_bytes_response(
                            StatusCode::OK,
                            Bytes::from_static(b"event: widget\ndata: \xff\n\n"),
                        ),
                        Failure::MalformedJson => subforge_bytes_response(
                            StatusCode::OK,
                            Bytes::from_static(b"event: widget\ndata: {invalid json}\n\n"),
                        ),
                        Failure::BadShape => subforge_bytes_response(
                            StatusCode::OK,
                            Bytes::from(format!(
                                "event: widget\ndata: {}\n\n",
                                json!({"active_jobs":"invalid","selected_languages":[]})
                            )),
                        ),
                        Failure::Redirect => {
                            let mut result = response(StatusCode::FOUND, "redirect");
                            result
                                .headers_mut()
                                .insert("location", HeaderValue::from_static("/leak"));
                            result
                        }
                    },
                }
            })
        });
        let fixture = Fixture::start(handler, |config| config.provider_bytes = 1024).await;
        let mut subscription = fixture.relay.subscribe(Topic::Subforge).unwrap();
        wait_for_senders(&senders, 1).await;
        let sender = senders.lock().unwrap()[0].clone();
        sender
            .send(Ok(Frame::data(subforge_event(subforge_widget()))))
            .unwrap();
        let first =
            wait_for_subforge_snapshot(&mut subscription.rx, |snapshot| snapshot["status"] == "ok")
                .await;
        assert_eq!(first["data"]["activeJobCount"], 2);
        drop(sender); // EOF is also a failure and must preserve the last good value.
        senders.lock().unwrap().clear();
        let eof = wait_for_subforge_snapshot(&mut subscription.rx, |snapshot| {
            snapshot["status"] == "stale"
        })
        .await;
        assert_eq!(eof["data"], first["data"]);
        assert_eq!(eof["updatedAt"], first["updatedAt"]);

        timeout(Duration::from_secs(3), async {
            while calls.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("collector did not reconnect after stream EOF");
        let failed = timeout(Duration::from_secs(3), async {
            subscription.rx.changed().await.unwrap();
            event_json(&subscription.rx.borrow_and_update())
        })
        .await
        .expect("failure did not update the stale snapshot");
        assert_eq!(failed["status"], "stale");
        assert_eq!(failed["data"], first["data"]);
        assert_eq!(failed["updatedAt"], first["updatedAt"]);
        assert!(!failed.to_string().contains("private"));
        assert_eq!(
            leaks.load(Ordering::SeqCst),
            0,
            "redirects must not be followed"
        );
        drop(subscription);
        fixture.close().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn subforge_blank_tokens_do_not_make_requests_and_snapshot_limits_fail_closed() {
    for blank_token in [true, false] {
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = calls.clone();
        let senders = Arc::new(Mutex::new(Vec::<UpstreamSender>::new()));
        let captured_senders = senders.clone();
        let handler: Handler = Arc::new(move |request| {
            assert_eq!(request.uri().path(), "/subforge/api/widget/events");
            captured.fetch_add(1, Ordering::SeqCst);
            let senders = captured_senders.clone();
            Box::pin(async move {
                let (sender, receiver) = mpsc::unbounded_channel();
                senders.lock().unwrap().push(sender);
                subforge_stream_response(receiver)
            })
        });
        let fixture = Fixture::start(handler, |config| {
            if blank_token {
                config.subforge_token.clear();
            } else {
                config.snapshot_bytes = 64;
            }
        })
        .await;
        let mut subscription = fixture.relay.subscribe(Topic::Subforge).unwrap();
        let snapshot = if blank_token {
            wait_for_subforge_snapshot(&mut subscription.rx, |snapshot| {
                snapshot["status"] == "stale"
            })
            .await
        } else {
            wait_for_senders(&senders, 1).await;
            senders.lock().unwrap()[0]
                .send(Ok(Frame::data(subforge_event(subforge_widget()))))
                .unwrap();
            wait_for_subforge_snapshot(&mut subscription.rx, |snapshot| {
                snapshot["status"] == "stale"
            })
            .await
        };
        assert_eq!(snapshot["status"], "stale");
        assert!(snapshot["data"].is_null());
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(!blank_token));
        drop(subscription);
        fixture.close().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn subforge_sse_requires_media_authorization_and_allows_only_matching_page_topics() {
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let handler: Handler = Arc::new(move |request| {
        let calls = captured.clone();
        Box::pin(async move {
            if request.uri().path() == "/api/pages/media/content/" {
                assert!(request.headers().get("authorization").is_none());
                return response(
                    if request
                        .headers()
                        .get("cookie")
                        .is_some_and(|value| value == "auth=valid")
                    {
                        StatusCode::OK
                    } else {
                        StatusCode::UNAUTHORIZED
                    },
                    "page auth",
                );
            }
            assert_eq!(request.uri().path(), "/subforge/api/widget/events");
            assert_eq!(
                request.headers().get("authorization").unwrap(),
                "Bearer fixture-widget-token"
            );
            assert!(request.headers().get("cookie").is_none());
            calls.fetch_add(1, Ordering::SeqCst);
            let (sender, receiver) = mpsc::unbounded_channel();
            sender
                .send(Ok(Frame::data(subforge_event(subforge_widget()))))
                .unwrap();
            subforge_stream_response(receiver)
        })
    });
    let fixture = Fixture::start(handler, |_| {}).await;
    let unauthorized_page = fixture
        .client
        .get(fixture.url("/api/pages/media/content/"))
        .header("host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized_page.status(), StatusCode::UNAUTHORIZED);
    let authorized_page = fixture
        .client
        .get(fixture.url("/api/pages/media/content/"))
        .header("host", "localhost")
        .header("cookie", "auth=valid")
        .send()
        .await
        .unwrap();
    assert_eq!(authorized_page.status(), StatusCode::OK);
    for query in [
        "page=downloads&topic=subforge",
        "page=media&topic=qbittorrent",
        "page=media&topic=subforge&topic=jellyfin",
        "page=media&topic=subforge&page=downloads",
    ] {
        let denied = fixture
            .client
            .get(fixture.url(&format!("/live/events?{query}")))
            .header("host", "localhost")
            .header("cookie", "auth=valid")
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    }
    for (cookie, origin, expected) in [
        (None, None, StatusCode::UNAUTHORIZED),
        (
            Some("auth=valid"),
            Some("https://elsewhere.example"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = fixture
            .client
            .get(fixture.url("/live/events?page=media&topic=subforge"))
            .header("host", "localhost");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        assert_eq!(request.send().await.unwrap().status(), expected);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = fixture
        .client
        .get(fixture.url("/live/events?page=media&topic=subforge"))
        .header("host", "localhost")
        .header("cookie", "auth=valid")
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::OK);
    let mut chunks = result.bytes_stream();
    timeout(Duration::from_secs(2), async {
        loop {
            let chunk = chunks.next().await.unwrap().unwrap();
            let text = String::from_utf8_lossy(&chunk);
            assert!(text.starts_with("event: subforge\n"));
            if text.contains("\"status\":\"ok\"") {
                assert!(text.contains("\"activeJobCount\":2"));
                break;
            }
        }
    })
    .await
    .unwrap();
    drop(chunks);
    fixture.close().await;
}
