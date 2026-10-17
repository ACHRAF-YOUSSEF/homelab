use crate::{
    Error,
    config::{Config, endpoint},
    model::{QbState, normalize_sessions},
    subforge::normalize as normalize_subforge,
    subforge_events::Decoder as SubforgeDecoder,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reqwest::{
    Client, Response, StatusCode,
    header::{AUTHORIZATION, COOKIE, HeaderValue, SET_COOKIE},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

#[derive(Clone, Copy)]
pub enum Topic {
    Jellyfin,
    Qbittorrent,
    Subforge,
}

impl Topic {
    pub fn name(self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
            Self::Qbittorrent => "qbittorrent",
            Self::Subforge => "subforge",
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Jellyfin => 0,
            Self::Qbittorrent => 1,
            Self::Subforge => 2,
        }
    }
}

struct Snapshot {
    data: Option<Bytes>,
    updated: Option<u128>,
    source: &'static str,
    failed: bool,
    subscribers: usize,
    task: Option<JoinHandle<()>>,
}

impl Snapshot {
    fn frame(&self, topic: Topic) -> Bytes {
        let status = if self.failed {
            "stale"
        } else if self.updated.is_some() {
            "ok"
        } else {
            "warming"
        };
        let updated = self
            .updated
            .map_or_else(|| "null".to_owned(), |n| n.to_string());
        let data = self.data.as_deref().unwrap_or(b"null");
        let mut frame = Vec::with_capacity(data.len() + 160);
        frame.extend_from_slice(format!("event: {}\ndata: {{\"status\":\"{status}\",\"updatedAt\":{updated},\"source\":\"{}\",\"data\":", topic.name(), self.source).as_bytes());
        frame.extend_from_slice(data);
        frame.extend_from_slice(b"}\n\n");
        frame.into()
    }
}

struct Channel {
    snapshot: Mutex<Snapshot>,
    tx: watch::Sender<Bytes>,
}

pub struct Relay {
    pub config: Arc<Config>,
    pub http: Client,
    pub(crate) calendar_slots: Semaphore,
    topics: [Channel; 3],
    slots: Arc<Semaphore>,
}

pub struct Subscription {
    pub rx: watch::Receiver<Bytes>,
    relay: Arc<Relay>,
    topic: Topic,
    _permit: OwnedSemaphorePermit,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let channel = &self.relay.topics[self.topic.index()];
        let mut snapshot = channel.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        snapshot.subscribers = snapshot.subscribers.saturating_sub(1);
        if snapshot.subscribers == 0
            && let Some(task) = snapshot.task.take()
        {
            task.abort();
        }
    }
}

pub fn http_client() -> Result<Client, Error> {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(2)
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| Error::Upstream)
}

/// Cap before allocation, including responses without Content-Length.
pub async fn bounded_body(mut response: Response, limit: usize) -> Result<Bytes, Error> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(Error::Limit);
    }
    let mut body =
        Vec::with_capacity(response.content_length().unwrap_or(0).min(limit as u64) as usize);
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Upstream)? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(Error::Limit);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.into())
}

/// Stop serialization before a large provider state can allocate an oversized event.
pub(crate) fn bounded_json(data: &impl serde::Serialize, limit: usize) -> Result<Bytes, Error> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Buffer {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            if input.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("snapshot limit exceeded"));
            }
            let needed = self.bytes.len() + input.len();
            if needed > self.bytes.capacity() {
                let capacity = needed
                    .max(self.bytes.capacity().saturating_mul(2))
                    .min(self.limit);
                self.bytes.reserve_exact(capacity - self.bytes.len());
            }
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::with_capacity(limit.min(4096)),
        limit,
    };
    serde_json::to_writer(&mut buffer, data).map_err(|_| Error::Limit)?;
    Ok(buffer.bytes.into())
}

impl Relay {
    pub fn new(config: Config) -> Result<Arc<Self>, Error> {
        let topics = [Topic::Jellyfin, Topic::Qbittorrent, Topic::Subforge].map(|topic| {
            let snapshot = Snapshot {
                data: None,
                updated: None,
                source: "rest",
                failed: false,
                subscribers: 0,
                task: None,
            };
            let (tx, _) = watch::channel(snapshot.frame(topic));
            Channel {
                snapshot: Mutex::new(snapshot),
                tx,
            }
        });
        Ok(Arc::new(Self {
            calendar_slots: Semaphore::new(2),
            slots: Arc::new(Semaphore::new(config.max_connections)),
            config: Arc::new(config),
            http: http_client()?,
            topics,
        }))
    }

    pub fn subscribe(self: &Arc<Self>, topic: Topic) -> Result<Subscription, Error> {
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Limit)?;
        let channel = &self.topics[topic.index()];
        let mut snapshot = channel.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.subscribers == 0 {
            snapshot.failed = snapshot.updated.is_some();
            channel.tx.send_replace(snapshot.frame(topic));
            let relay = self.clone();
            snapshot.task = Some(tokio::spawn(async move {
                match topic {
                    Topic::Qbittorrent => relay.collect_qb().await,
                    Topic::Jellyfin => relay.collect_jellyfin().await,
                    Topic::Subforge => relay.collect_subforge().await,
                }
            }));
        }
        snapshot.subscribers += 1;
        Ok(Subscription {
            rx: channel.tx.subscribe(),
            relay: self.clone(),
            topic,
            _permit: permit,
        })
    }

    pub fn shutdown(&self) {
        for topic in &self.topics {
            if let Some(task) = topic
                .snapshot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .task
                .take()
            {
                task.abort();
            }
        }
    }

    fn publish(&self, topic: Topic, data: Value, source: &'static str) -> Result<(), Error> {
        let bytes = bounded_json(&data, self.config.snapshot_bytes)?;
        let channel = &self.topics[topic.index()];
        let mut snapshot = channel.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        snapshot.data = Some(bytes);
        snapshot.updated = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        );
        snapshot.failed = false;
        snapshot.source = source;
        channel.tx.send_replace(snapshot.frame(topic));
        Ok(())
    }

    fn fail(&self, topic: Topic) {
        let channel = &self.topics[topic.index()];
        let mut snapshot = channel.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        snapshot.failed = true;
        channel.tx.send_replace(snapshot.frame(topic));
    }

    async fn stream_subforge(&self, http: &Client) -> Result<(), Error> {
        let mut response = http
            .get(endpoint(&self.config.subforge, "/api/widget/events"))
            .bearer_auth(&self.config.subforge_token)
            .header("accept", "text/event-stream")
            .send()
            .await
            .map_err(|_| Error::Upstream)?;
        if response.status() != StatusCode::OK
            || !response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .eq_ignore_ascii_case("text/event-stream")
                })
        {
            return Err(Error::Upstream);
        }
        let mut decoder = SubforgeDecoder::new(self.config.provider_bytes);
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Upstream)? {
            for frame in decoder.feed(&chunk)? {
                self.publish(Topic::Subforge, normalize_subforge(&frame)?, "sse")?;
            }
        }
        Err(Error::Upstream)
    }

    async fn collect_subforge(&self) {
        if self.config.subforge_token.is_empty() {
            self.fail(Topic::Subforge);
            return;
        }
        // Streaming uses its own idle/lifetime bounds rather than the REST client's 15s total timeout.
        let Ok(http) = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(35))
            .timeout(Duration::from_secs(925))
            .pool_max_idle_per_host(1)
            .build()
        else {
            self.fail(Topic::Subforge);
            return;
        };
        let mut delay = Duration::from_millis(500);
        loop {
            let started = Instant::now();
            let _ = self.stream_subforge(&http).await;
            self.fail(Topic::Subforge);
            if started.elapsed() >= Duration::from_secs(30) {
                delay = Duration::from_millis(500);
            }
            sleep(delay).await;
            delay = delay.saturating_mul(2).min(Duration::from_secs(30));
        }
    }

    async fn qb_login(&self) -> Result<Option<HeaderValue>, Error> {
        let config = &self.config;
        if config.qb_username.is_empty() || config.qb_password.is_empty() {
            return Err(Error::InvalidData);
        }
        let response = self
            .http
            .post(endpoint(&config.qbittorrent, "/api/v2/auth/login"))
            .form(&[
                ("username", &config.qb_username),
                ("password", &config.qb_password),
            ])
            .send()
            .await
            .map_err(|_| Error::Upstream)?;
        let status = response.status();
        let sid = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| v.split(';').next())
            .find(|v| v.starts_with("SID=") && v.len() > 4 && v.len() <= 4096)
            .map(HeaderValue::from_str)
            .transpose()
            .map_err(|_| Error::InvalidData)?;
        let body = bounded_body(response, 4096).await?;
        if status != StatusCode::NO_CONTENT
            && !(status == StatusCode::OK && body.as_ref() == b"Ok.")
        {
            return Err(Error::Upstream);
        }
        Ok(sid)
    }

    async fn qb_sync(
        &self,
        state: &mut QbState,
        cookie: Option<&HeaderValue>,
    ) -> Result<StatusCode, Error> {
        let mut request = self
            .http
            .get(endpoint(&self.config.qbittorrent, "/api/v2/sync/maindata"))
            .query(&[("rid", state.rid)]);
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        let response = request.send().await.map_err(|_| Error::Upstream)?;
        let status = response.status();
        if status != StatusCode::OK {
            return Ok(status);
        }
        let bytes = bounded_body(response, self.config.provider_bytes).await?;
        let data = serde_json::from_slice(&bytes).map_err(|_| Error::InvalidData)?;
        let data = state.apply(data, self.config.max_torrents)?;
        self.publish(Topic::Qbittorrent, data, "rest")?;
        Ok(status)
    }

    async fn collect_qb(&self) {
        let auth = !self.config.qb_username.is_empty() || !self.config.qb_password.is_empty();
        let mut authenticated = !auth;
        let mut cookie = None;
        let mut state = QbState::default();
        let delay = self
            .config
            .qb_interval
            .clamp(Duration::from_millis(500), Duration::from_secs(30));
        loop {
            if !authenticated {
                match self.qb_login().await {
                    Ok(sid) => {
                        cookie = sid;
                        authenticated = true;
                        state = QbState::default();
                    }
                    Err(_) => {
                        self.fail(Topic::Qbittorrent);
                        sleep(delay.min(Duration::from_secs(5))).await;
                        continue;
                    }
                }
            }
            match self.qb_sync(&mut state, cookie.as_ref()).await {
                Ok(StatusCode::OK) => {}
                Ok(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) if auth => {
                    authenticated = false;
                    cookie = None;
                    state = QbState::default();
                    self.fail(Topic::Qbittorrent);
                }
                Ok(_) => self.fail(Topic::Qbittorrent),
                Err(_) => {
                    state = QbState::default();
                    self.fail(Topic::Qbittorrent);
                }
            }
            sleep(delay).await;
        }
    }

    fn jellyfin_authorization(&self) -> Result<HeaderValue, Error> {
        HeaderValue::from_str(&format!("MediaBrowser Client=\"Glance Live\", Device=\"Server\", DeviceId=\"glance-live\", Version=\"1.0\", Token=\"{}\"", self.config.jellyfin_key))
            .map_err(|_| Error::InvalidData)
    }

    async fn reconcile_jellyfin(&self, auth: &HeaderValue) -> Result<(), Error> {
        let response = self
            .http
            .get(endpoint(&self.config.jellyfin, "/Sessions"))
            .header(AUTHORIZATION, auth)
            .send()
            .await
            .map_err(|_| Error::Upstream)?;
        if response.status() != StatusCode::OK {
            return Err(Error::Upstream);
        }
        let body = bounded_body(response, self.config.provider_bytes).await?;
        let payload = serde_json::from_slice(&body).map_err(|_| Error::InvalidData)?;
        self.publish(
            Topic::Jellyfin,
            normalize_sessions(&payload, self.config.max_sessions)?,
            "rest",
        )
    }

    async fn jellyfin_socket(&self, auth: &HeaderValue) -> Result<(), Error> {
        let mut url = self.config.jellyfin.clone();
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme).map_err(|_| Error::InvalidData)?;
        url.set_path(&format!("{}/socket", url.path().trim_end_matches('/')));
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| Error::InvalidData)?;
        request.headers_mut().insert(AUTHORIZATION, auth.clone());
        let ws_config = WebSocketConfig::default()
            .read_buffer_size(4096)
            .max_message_size(Some(self.config.provider_bytes))
            .max_frame_size(Some(self.config.provider_bytes))
            .write_buffer_size(4096)
            .max_write_buffer_size(16384);
        // tungstenite does not follow redirects; credentials stay on this explicit endpoint.
        let (mut socket, _) = timeout(
            Duration::from_secs(10),
            connect_async_with_config(request, Some(ws_config), false),
        )
        .await
        .map_err(|_| Error::Upstream)?
        .map_err(|_| Error::Upstream)?;
        timeout(
            Duration::from_secs(10),
            socket.send(Message::Text(
                json!({"MessageType":"SessionsStart","Data":"0,2000"})
                    .to_string()
                    .into(),
            )),
        )
        .await
        .map_err(|_| Error::Upstream)?
        .map_err(|_| Error::Upstream)?;
        let interval = self
            .config
            .jellyfin_interval
            .clamp(Duration::from_secs(1), Duration::from_secs(60));
        let mut reconcile_at = Instant::now() + interval;
        let mut heartbeat = tokio::time::interval_at(
            Instant::now() + Duration::from_secs(25),
            Duration::from_secs(25),
        );
        let mut ping_at = None;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(reconcile_at) => {
                    if self.reconcile_jellyfin(auth).await.is_err() { self.fail(Topic::Jellyfin); }
                    reconcile_at = Instant::now() + interval;
                }
                _ = heartbeat.tick() => {
                    if ping_at.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(25)) { return Err(Error::Upstream); }
                    timeout(Duration::from_secs(10), socket.send(Message::Ping(Bytes::new()))).await.map_err(|_| Error::Upstream)?.map_err(|_| Error::Upstream)?;
                    ping_at = Some(Instant::now());
                }
                message = socket.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(payload) = serde_json::from_str::<Value>(&text) else { continue; };
                            if payload.get("MessageType").and_then(Value::as_str) == Some("Sessions") && let Some(data) = payload.get("Data").filter(|v| v.is_array()) {
                                self.publish(Topic::Jellyfin, normalize_sessions(data, self.config.max_sessions)?, "websocket")?;
                            }
                        }
                        Some(Ok(Message::Pong(_))) => ping_at = None,
                        Some(Ok(Message::Ping(_))) => {
                            timeout(Duration::from_secs(10), socket.flush()).await.map_err(|_| Error::Upstream)?.map_err(|_| Error::Upstream)?;
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Err(Error::Upstream),
                        _ => {}
                    }
                }
            }
        }
    }

    async fn collect_jellyfin(&self) {
        if self.config.jellyfin_key.is_empty() {
            self.fail(Topic::Jellyfin);
            return;
        }
        let Ok(auth) = self.jellyfin_authorization() else {
            self.fail(Topic::Jellyfin);
            return;
        };
        let delay = self
            .config
            .jellyfin_interval
            .clamp(Duration::from_secs(1), Duration::from_secs(60));
        loop {
            if self.reconcile_jellyfin(&auth).await.is_err() {
                self.fail(Topic::Jellyfin);
            }
            if self.config.jellyfin_ws {
                // An unavailable optional WS never invalidates a successful REST snapshot.
                let _ = self.jellyfin_socket(&auth).await;
            }
            sleep(delay).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_serialization_stops_at_the_byte_cap() {
        let payload = json!({"value": "x".repeat(8192)});
        assert!(matches!(bounded_json(&payload, 128), Err(Error::Limit)));
        assert_eq!(
            bounded_json(&json!({"ok": true}), 11).unwrap().as_ref(),
            b"{\"ok\":true}"
        );
    }

    #[test]
    fn event_contract_preserves_last_good_data_on_failure() {
        let mut snapshot = Snapshot {
            data: None,
            updated: None,
            source: "rest",
            failed: false,
            subscribers: 0,
            task: None,
        };
        let frame = snapshot.frame(Topic::Jellyfin);
        assert!(String::from_utf8_lossy(&frame).contains(
            "{\"status\":\"warming\",\"updatedAt\":null,\"source\":\"rest\",\"data\":null}"
        ));
        snapshot.data = Some(Bytes::from_static(b"{\"sessions\":[]}"));
        snapshot.updated = Some(123);
        snapshot.failed = true;
        let frame = snapshot.frame(Topic::Jellyfin);
        assert!(String::from_utf8_lossy(&frame).contains("\"status\":\"stale\",\"updatedAt\":123"));
        assert!(String::from_utf8_lossy(&frame).contains("\"data\":{\"sessions\":[]}"));
    }
}
