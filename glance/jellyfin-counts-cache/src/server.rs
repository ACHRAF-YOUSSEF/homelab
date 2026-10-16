use crate::{Cache, Error};
use bytes::Bytes;
use std::{future::Future, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
    task::JoinSet,
    time::timeout,
};

pub const MAX_CONNECTIONS: usize = 8;
pub const MAX_REQUEST_BYTES: usize = 8192;
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const WARMING_UP: &[u8] = b"{\"error\":\"warming up\"}";

pub fn bind(address: SocketAddr) -> Result<TcpListener, Error> {
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    socket.bind(address)?;
    // Also bound the kernel's pending connection queue.
    Ok(socket.listen(16)?)
}

async fn respond(socket: &mut TcpStream, status: &str, body: &[u8]) -> Result<(), Error> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.shutdown().await?;
    Ok(())
}

async fn connection(mut socket: TcpStream, cache: Cache) -> Result<(), Error> {
    let mut buffer = [0_u8; MAX_REQUEST_BYTES];
    let mut received = 0;
    loop {
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(buffer.get(..received).ok_or(Error::Limit)?) {
            Ok(httparse::Status::Complete(_)) => {
                let has_body = request.headers.iter().any(|header| {
                    header.name.eq_ignore_ascii_case("transfer-encoding")
                        || (header.name.eq_ignore_ascii_case("content-length")
                            && header.value != b"0")
                });
                if has_body || request.version != Some(1) && request.version != Some(0) {
                    return respond(
                        &mut socket,
                        "400 Bad Request",
                        b"{\"error\":\"invalid request\"}",
                    )
                    .await;
                }
                if request.method != Some("GET") {
                    return respond(
                        &mut socket,
                        "405 Method Not Allowed",
                        b"{\"error\":\"method not allowed\"}",
                    )
                    .await;
                }
                if request.path != Some("/counts") {
                    return respond(&mut socket, "404 Not Found", b"{\"error\":\"not found\"}")
                        .await;
                }
                let body: Option<Bytes> = cache.borrow().clone();
                return match body {
                    Some(body) => respond(&mut socket, "200 OK", &body).await,
                    None => respond(&mut socket, "503 Service Unavailable", WARMING_UP).await,
                };
            }
            Ok(httparse::Status::Partial) => {}
            Err(_) => {
                return respond(
                    &mut socket,
                    "400 Bad Request",
                    b"{\"error\":\"invalid request\"}",
                )
                .await;
            }
        }
        if received == MAX_REQUEST_BYTES {
            return respond(
                &mut socket,
                "431 Request Header Fields Too Large",
                b"{\"error\":\"request too large\"}",
            )
            .await;
        }
        let count = socket
            .read(buffer.get_mut(received..).ok_or(Error::Limit)?)
            .await?;
        if count == 0 {
            return Ok(());
        }
        received += count;
    }
}

/// One task per accepted connection, with a hard concurrency and lifetime bound.
pub async fn serve(
    listener: TcpListener,
    cache: Cache,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Error> {
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept(), if tasks.len() < MAX_CONNECTIONS => {
                let (socket, _) = accepted?;
                let cache = cache.clone();
                tasks.spawn(async move {
                    let _ = timeout(CONNECTION_TIMEOUT, connection(socket, cache)).await;
                });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

pub async fn healthcheck(mut address: SocketAddr) -> Result<(), Error> {
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    timeout(Duration::from_secs(3), async {
        let mut socket = TcpStream::connect(address).await?;
        socket
            .write_all(b"GET /counts HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut status = [0_u8; 17];
        socket.read_exact(&mut status).await?;
        if status == *b"HTTP/1.1 200 OK\r\n" {
            Ok(())
        } else {
            Err(Error::NotReady)
        }
    })
    .await
    .map_err(|_| Error::NotReady)?
}
