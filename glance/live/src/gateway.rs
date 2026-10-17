use crate::{
    Error,
    calendar::{self, Range},
    config::{Config, authority, endpoint},
    relay::{Relay, Topic, http_client},
};
use bytes::Bytes;
use futures_util::{Stream, TryStreamExt};
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Frame, Incoming},
    header::{self, HeaderMap, HeaderName, HeaderValue},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    collections::HashSet,
    convert::Infallible,
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinSet,
    time::{Instant, Sleep, timeout},
};

type Body = UnsyncBoxBody<Bytes, Error>;
const REQUEST_BYTES: usize = 8 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

fn response(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(text.as_bytes()))
            .map_err(|never: Infallible| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    response
}

fn streamed(stream: impl Stream<Item = Result<Bytes, Error>> + Send + 'static) -> Body {
    StreamBody::new(stream.map_ok(Frame::data)).boxed_unsync()
}

fn hop_names(headers: &HeaderMap) -> HashSet<HeaderName> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect()
}

fn hop_header(name: &HeaderName, nominated: &HashSet<HeaderName>) -> bool {
    nominated.contains(name)
        || matches!(
            name.as_str(),
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "trailers"
                | "transfer-encoding"
                | "upgrade"
        )
}

fn same_origin(headers: &HeaderMap, host: &str) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "same-site" | "cross-site"))
    {
        return false;
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Some((scheme, raw)) = origin.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https") {
        return false;
    }
    matches!((authority(raw), authority(host)), (Ok(a), Ok(b)) if a == b)
}

async fn events(request: Request<Incoming>, relay: Arc<Relay>, host: &str) -> Response<Body> {
    let page = url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .find(|(name, _)| name == "page")
        .map(|(_, value)| value.into_owned());
    let (page, topic) = match page.as_deref() {
        Some("media") => ("media", Topic::Jellyfin),
        Some("downloads") => ("downloads", Topic::Qbittorrent),
        _ => return response(StatusCode::BAD_REQUEST, "page must be media or downloads"),
    };
    if !same_origin(request.headers(), host) {
        return response(StatusCode::FORBIDDEN, "forbidden");
    }
    let mut auth = relay.http.get(endpoint(
        &relay.config.glance,
        &format!("/api/pages/{page}/content/"),
    ));
    if let Some(cookie) = request.headers().get(header::COOKIE) {
        auth = auth.header(header::COOKIE, cookie);
    }
    if !matches!(auth.send().await, Ok(response) if response.status() == StatusCode::OK) {
        return response(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let Ok(mut subscription) = relay.subscribe(topic) else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            "live connection limit reached",
        );
    };
    let deadline = Instant::now() + relay.config.sse_lifetime;
    let stream = async_stream::try_stream! {
        let initial = subscription.rx.borrow_and_update().clone();
        yield initial;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                changed = subscription.rx.changed() => {
                    if changed.is_err() { break; }
                    let frame = subscription.rx.borrow_and_update().clone();
                    yield frame;
                }
                _ = tokio::time::sleep(Duration::from_secs(20)) => yield Bytes::from_static(b": keepalive\n\n"),
            }
        }
    };
    let mut output = Response::new(streamed(stream));
    output.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    output.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    output
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    output
}

async fn calendar(request: Request<Incoming>, relay: Arc<Relay>, host: &str) -> Response<Body> {
    if !same_origin(request.headers(), host) {
        return response(StatusCode::FORBIDDEN, "forbidden");
    }
    let Ok(range) = Range::parse(request.uri().query().unwrap_or_default()) else {
        return response(
            StatusCode::BAD_REQUEST,
            "start and exclusive end must be valid dates at most 366 days apart",
        );
    };
    let Ok(_permit) = relay.calendar_slots.try_acquire() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            "calendar request limit reached",
        );
    };
    let mut auth = relay
        .http
        .get(endpoint(&relay.config.glance, "/api/pages/media/content/"));
    if let Some(cookie) = request.headers().get(header::COOKIE) {
        auth = auth.header(header::COOKIE, cookie);
    }
    if !matches!(auth.send().await, Ok(response) if response.status() == StatusCode::OK) {
        return response(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if relay.config.sonarr_key.is_empty() && relay.config.radarr_key.is_empty() {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            "calendar providers are not configured",
        );
    }
    let Ok(bytes) = calendar::load(&relay, range).await else {
        return response(
            StatusCode::BAD_GATEWAY,
            "calendar providers unavailable or calendar limit exceeded",
        );
    };
    let mut output = Response::new(
        Full::new(bytes)
            .map_err(|never: Infallible| match never {})
            .boxed_unsync(),
    );
    output.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    output
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    output
}

async fn proxy(
    request: Request<Incoming>,
    relay: Arc<Relay>,
    host: &str,
    remote: SocketAddr,
) -> Response<Body> {
    if request
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    {
        return response(
            StatusCode::NOT_IMPLEMENTED,
            "WebSocket upgrades are not supported by this gateway",
        );
    }
    if !matches!(
        request.method().as_str(),
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    ) {
        let mut denied = response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        denied.headers_mut().insert(
            header::ALLOW,
            HeaderValue::from_static("GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"),
        );
        return denied;
    }
    if request.uri().path().starts_with("/live/") {
        return response(StatusCode::NOT_FOUND, "not found");
    }
    if request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > REQUEST_BYTES as u64)
    {
        return response(StatusCode::PAYLOAD_TOO_LARGE, "request body limit exceeded");
    }
    let path = request.uri().path_and_query().map_or("/", |pq| pq.as_str());
    // Refuse URL normalization/traversal that could escape a configured base path.
    let lower_path = request.uri().path().to_ascii_lowercase();
    if !path.starts_with('/')
        || path.starts_with("//")
        || ["%2e", "%2f", "%5c"]
            .iter()
            .any(|encoded| lower_path.contains(encoded))
        || request
            .uri()
            .path()
            .split('/')
            .any(|part| matches!(part, "." | ".."))
        || request.uri().path().contains('\\')
    {
        return response(StatusCode::BAD_REQUEST, "invalid request path");
    }
    let url = endpoint(&relay.config.glance, path);
    let nominated = hop_names(request.headers());
    let mut headers = HeaderMap::new();
    for (name, value) in request.headers() {
        if !hop_header(name, &nominated)
            && !matches!(
                name.as_str(),
                "host"
                    | "content-length"
                    | "forwarded"
                    | "x-forwarded-for"
                    | "x-forwarded-host"
                    | "x-forwarded-proto"
                    | "x-forwarded-uri"
            )
        {
            headers.append(name, value.clone());
        }
    }
    let Ok(forwarded_host) = HeaderValue::from_str(host) else {
        return response(StatusCode::BAD_REQUEST, "invalid host");
    };
    let Ok(forwarded_uri) = HeaderValue::from_str(path) else {
        return response(StatusCode::BAD_REQUEST, "invalid request path");
    };
    headers.insert("x-forwarded-host", forwarded_host);
    headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
    if let Ok(value) = HeaderValue::from_str(&remote.ip().to_string()) {
        headers.insert("x-forwarded-for", value);
    }
    headers.insert("x-forwarded-uri", forwarded_uri);
    if let Some(length) = request.headers().get(header::CONTENT_LENGTH) {
        headers.insert(header::CONTENT_LENGTH, length.clone());
    }
    let method = request.method().clone();
    let has_body = request.headers().contains_key(header::CONTENT_LENGTH)
        || request.headers().contains_key(header::TRANSFER_ENCODING);
    let mut upstream = relay.http.request(method, url).headers(headers);
    if has_body {
        let mut incoming = request.into_body();
        let stream = async_stream::try_stream! {
            let mut received = 0usize;
            while let Some(frame) = timeout(IO_TIMEOUT, incoming.frame()).await.map_err(|_| Error::Upstream)? {
                let frame = frame.map_err(|_| Error::Upstream)?;
                if let Ok(data) = frame.into_data() {
                    received = received.checked_add(data.len()).ok_or(Error::Limit)?;
                    if received > REQUEST_BYTES { Err(Error::Limit)?; }
                    yield data;
                }
            }
        };
        // Explicit error type prevents inference through reqwest's erased body error.
        let stream: Pin<Box<dyn Stream<Item = Result<Bytes, Error>> + Send>> = Box::pin(stream);
        upstream = upstream.body(reqwest::Body::wrap_stream(stream));
    }
    let Ok(upstream) = upstream.send().await else {
        return response(StatusCode::BAD_GATEWAY, "Glance upstream unavailable");
    };
    let nominated = hop_names(upstream.headers());
    let mut headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if !hop_header(name, &nominated) && name != header::CONTENT_LENGTH {
            headers.append(name, value.clone());
        }
    }
    let status = upstream.status();
    let stream = upstream.bytes_stream().map_err(|_| Error::Upstream);
    let mut output = Response::new(streamed(stream));
    *output.status_mut() = status;
    *output.headers_mut() = headers;
    output
}

async fn handle(
    request: Request<Incoming>,
    relay: Arc<Relay>,
    remote: SocketAddr,
) -> Result<Response<Body>, Infallible> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let (hostname, _) = match authority(&host) {
        Ok(host) => host,
        Err(_) => return Ok(response(StatusCode::BAD_REQUEST, "invalid host")),
    };
    if !relay.config.allowed_hosts.contains(&hostname) {
        return Ok(response(
            StatusCode::MISDIRECTED_REQUEST,
            "host is not allowed",
        ));
    }
    if request.uri().authority().is_some() || request.uri().scheme().is_some() {
        return Ok(response(
            StatusCode::BAD_REQUEST,
            "absolute request targets are unsupported",
        ));
    }
    let output = match (request.method().as_str(), request.uri().path()) {
        ("GET" | "HEAD", "/live/health") => {
            let mut result = response(StatusCode::OK, "{\"status\":\"ok\"}");
            result.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            result
        }
        ("GET", "/live/events") => events(request, relay, &host).await,
        ("GET", "/live/calendar") => calendar(request, relay, &host).await,
        (_, "/live/calendar") => {
            let mut denied = response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            denied
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET"));
            denied
        }
        _ => proxy(request, relay, &host, remote).await,
    };
    Ok(output)
}

/// Close a downstream socket after ten seconds without write progress.
/// Connection lifetime also bounds clients that make only intermittent progress.
/// Pinning only the timer keeps all first-party I/O safe Rust.
struct TimedIo {
    socket: TcpStream,
    write_deadline: Option<Pin<Box<Sleep>>>,
}

impl TimedIo {
    fn pending_write(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let deadline = self
            .write_deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(IO_TIMEOUT)));
        if deadline.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "downstream write timeout",
            )))
        } else {
            Poll::Pending
        }
    }
}

impl AsyncRead for TimedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_read(cx, buf)
    }
}

impl AsyncWrite for TimedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.socket).poll_write(cx, buf) {
            Poll::Ready(result) => {
                self.write_deadline = None;
                Poll::Ready(result)
            }
            Poll::Pending => self.pending_write(cx),
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}

/// HTTP/1 only, one runtime thread, no task created until a connection slot exists.
pub async fn serve(
    listener: TcpListener,
    relay: Arc<Relay>,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Error> {
    let slots = Arc::new(Semaphore::new(relay.config.max_http_connections));
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            accepted = listener.accept() => {
                let (socket, remote) = accepted?;
                let Ok(permit) = slots.clone().try_acquire_owned() else { drop(socket); continue; };
                let relay = relay.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let lifetime = relay.config.sse_lifetime.max(Duration::from_secs(15)) + IO_TIMEOUT;
                    let service = service_fn(move |request| handle(request, relay.clone(), remote));
                    let mut builder = http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(IO_TIMEOUT).max_headers(100).max_buf_size(65536);
                    let io = TokioIo::new(TimedIo { socket, write_deadline: None });
                    let _ = timeout(lifetime, builder.serve_connection(io, service)).await;
                });
            }
        }
    }
    relay.shutdown();
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

pub async fn healthcheck(config: &Config) -> Result<(), Error> {
    let mut address = config.bind;
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    let host = config
        .allowed_hosts
        .iter()
        .min()
        .ok_or(Error::InvalidData)?;
    let result = http_client()?
        .get(format!("http://{address}/live/health"))
        .header(header::HOST, host)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|_| Error::Upstream)?;
    if result.status() == StatusCode::OK {
        Ok(())
    } else {
        Err(Error::Upstream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_and_fetch_metadata_reject_cross_origin_requests() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:8081"),
        );
        assert!(same_origin(&headers, "localhost:8081"));
        for origin in [
            "null",
            "http://localhost:8081/",
            "http://@localhost:8081",
            "http://localhost",
            "https://other.example",
        ] {
            headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
            assert!(!same_origin(&headers, "localhost:8081"));
        }
        headers.remove(header::ORIGIN);
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-site"));
        assert!(!same_origin(&headers, "localhost:8081"));
    }

    #[test]
    fn filters_connection_nominated_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("X-Private, keep-alive"),
        );
        let names = hop_names(&headers);
        assert!(hop_header(&HeaderName::from_static("x-private"), &names));
        assert!(hop_header(&header::TRANSFER_ENCODING, &names));
        assert!(!hop_header(&header::SET_COOKIE, &names));
    }
}
