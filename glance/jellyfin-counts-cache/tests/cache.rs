#![forbid(unsafe_code)]

use jellyfin_counts_cache::{
    Error, MAX_UPSTREAM_BYTES, cache, config::Config, refresh_once, server, upstream::Upstream,
};
use std::{future::pending, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::{Instant, timeout},
};

const COUNTS: &str = r#"{"MovieCount":10,"SeriesCount":20,"EpisodeCount":30,"AlbumCount":40,"SongCount":50,"BoxSetCount":60}"#;

async fn mock(responses: Vec<Vec<u8>>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/base/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0_u8; 8192];
            let mut received = 0;
            while !bytes
                .get(..received)
                .unwrap()
                .windows(4)
                .any(|v| v == b"\r\n\r\n")
            {
                let n = socket
                    .read(bytes.get_mut(received..).unwrap())
                    .await
                    .unwrap();
                assert!(n > 0);
                received += n;
            }
            requests.push(String::from_utf8(bytes.get(..received).unwrap().to_vec()).unwrap());
            // Oversized responses may be closed by the client before all bytes arrive.
            let _ = socket.write_all(&response).await;
        }
        requests
    });
    (url, task)
}

fn response(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[tokio::test(flavor = "current_thread")]
async fn authenticates_fetches_only_counts_and_keeps_last_success() {
    let extra = COUNTS.replacen('{', "{\"Unrelated\":\"discarded\",", 1);
    let (url, task) = mock(vec![
        response(&extra),
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
        response("{invalid}"),
    ])
    .await;
    let upstream = Upstream::new(Config::new(&url, "fixture-token").unwrap()).unwrap();
    let cache = cache();
    assert!(cache.borrow().is_none());
    refresh_once(&upstream, &cache).await.unwrap();
    assert_eq!(cache.borrow().as_ref().unwrap().as_ref(), COUNTS.as_bytes());
    assert!(matches!(
        refresh_once(&upstream, &cache).await,
        Err(Error::Upstream)
    ));
    assert!(matches!(
        refresh_once(&upstream, &cache).await,
        Err(Error::InvalidCounts)
    ));
    assert_eq!(cache.borrow().as_ref().unwrap().as_ref(), COUNTS.as_bytes());
    for request in task.await.unwrap() {
        assert!(request.starts_with("GET /base/Items/Counts HTTP/1.1\r\n"));
        assert!(request.to_ascii_lowercase().contains("authorization: mediabrowser client=\"glance\", device=\"glance\", deviceid=\"glance-dashboard\", version=\"1.0\", token=\"fixture-token\"\r\n"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_missing_negative_and_non_integer_counts() {
    for body in [
        "{}".to_owned(),
        COUNTS.replace("10", "-1"),
        COUNTS.replace("10", "1.5"),
        COUNTS.replace("10", "null"),
    ] {
        let (url, task) = mock(vec![response(&body)]).await;
        let upstream = Upstream::new(Config::new(&url, "fixture").unwrap()).unwrap();
        assert!(matches!(upstream.fetch().await, Err(Error::InvalidCounts)));
        task.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_declared_and_chunked_oversize_and_redirects() {
    let large = "x".repeat(MAX_UPSTREAM_BYTES + 1);
    for bytes in [
        response(&large),
        format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{large}\r\n0\r\n\r\n", large.len()).into_bytes(),
    ] {
        let (url, task) = mock(vec![bytes]).await;
        let upstream = Upstream::new(Config::new(&url, "fixture").unwrap()).unwrap();
        assert!(matches!(upstream.fetch().await, Err(Error::Limit)));
        task.await.unwrap();
    }
    // A redirect back to the fixture would deadlock if silently followed.
    let (url, task) = mock(vec![b"HTTP/1.1 302 Found\r\nLocation: /redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()]).await;
    let upstream = Upstream::new(Config::new(&url, "fixture").unwrap()).unwrap();
    assert!(matches!(upstream.fetch().await, Err(Error::Upstream)));
    task.await.unwrap();
}

async fn request(address: std::net::SocketAddr, input: &[u8]) -> Vec<u8> {
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket.write_all(input).await.unwrap();
    let mut buffer = Vec::new();
    socket.read_to_end(&mut buffer).await.unwrap();
    buffer
}

#[tokio::test(flavor = "current_thread")]
async fn serves_warmup_cached_counts_strict_endpoints_and_health() {
    let listener = server::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = listener.local_addr().unwrap();
    let cache = cache();
    let serving = tokio::spawn(server::serve(listener, cache.clone(), pending()));
    let bytes = request(address, b"GET /counts HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(bytes.starts_with(b"HTTP/1.1 503 Service Unavailable\r\n"));
    assert!(bytes.ends_with(b"{\"error\":\"warming up\"}"));
    assert!(server::healthcheck(address).await.is_err());
    cache.send_replace(Some(bytes::Bytes::from_static(COUNTS.as_bytes())));
    assert!(server::healthcheck(address).await.is_ok());
    let bytes = request(address, b"GET /counts HTTP/1.0\r\n\r\n").await;
    assert!(bytes.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(bytes.ends_with(COUNTS.as_bytes()));
    for path in [
        "/",
        "/counts?query",
        "/Counts",
        "/counts/",
        "/../counts",
        "http://localhost/counts",
    ] {
        let bytes = request(
            address,
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
        )
        .await;
        assert!(bytes.starts_with(b"HTTP/1.1 404 Not Found\r\n"), "{path}");
    }
    let bytes = request(address, b"POST /counts HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(bytes.starts_with(b"HTTP/1.1 405 Method Not Allowed\r\n"));
    let bytes = request(
        address,
        b"GET /counts HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\n\r\nx",
    )
    .await;
    assert!(bytes.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
    let bytes = request(address, "x".repeat(server::MAX_REQUEST_BYTES).as_bytes()).await;
    assert!(bytes.starts_with(b"HTTP/1.1 431 Request Header Fields Too Large\r\n"));
    serving.abort();
    let _ = serving.await;
}

#[tokio::test(flavor = "current_thread")]
async fn caps_active_connections_and_expires_stalled_clients() {
    let listener = server::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(server::serve(listener, cache(), pending()));
    let started = Instant::now();
    let mut stalled = Vec::with_capacity(server::MAX_CONNECTIONS);
    for _ in 0..server::MAX_CONNECTIONS {
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket
            .write_all(b"GET /counts HTTP/1.1\r\nX-Stalled: ")
            .await
            .unwrap();
        stalled.push(socket);
    }

    // This complete request must stay in the bounded listen queue until an
    // active slot is released. An unbounded per-client spawn would answer now.
    let mut queued = TcpStream::connect(address).await.unwrap();
    queued
        .write_all(b"GET /counts HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut first_byte = [0_u8; 1];
    assert!(
        timeout(Duration::from_millis(100), queued.read(&mut first_byte))
            .await
            .is_err(),
        "a request beyond the active connection cap was processed"
    );

    // The server must release stalled clients by its fixed five-second
    // deadline and serve the queued request without requiring client progress.
    let mut response = Vec::new();
    timeout(
        server::CONNECTION_TIMEOUT + Duration::from_secs(1),
        queued.read_to_end(&mut response),
    )
    .await
    .expect("stalled clients did not release connection slots")
    .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 503 Service Unavailable\r\n"));
    assert!(started.elapsed() >= server::CONNECTION_TIMEOUT);
    for mut socket in stalled {
        assert_eq!(
            timeout(Duration::from_secs(1), socket.read(&mut first_byte))
                .await
                .expect("stalled client remained open beyond the deadline")
                .unwrap(),
            0
        );
    }
    serving.abort();
    let _ = serving.await;
}
