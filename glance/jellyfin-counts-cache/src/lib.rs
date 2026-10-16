#![forbid(unsafe_code)]

pub mod config;
pub mod server;
pub mod upstream;

use bytes::Bytes;
use std::{fmt, time::Duration};
use tokio::sync::watch;

pub const SUCCESS_INTERVAL: Duration = Duration::from_secs(300);
pub const RETRY_INTERVAL: Duration = Duration::from_secs(30);
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_UPSTREAM_BYTES: usize = 16 * 1024;

/// Display intentionally excludes URLs, tokens, I/O details and provider bodies.
#[derive(Debug)]
pub enum Error {
    Configuration(&'static str),
    Upstream,
    InvalidCounts,
    Limit,
    Io,
    NotReady,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(name) => write!(f, "invalid configuration: {name}"),
            Self::Upstream => f.write_str("upstream request failed"),
            Self::InvalidCounts => f.write_str("invalid library counts"),
            Self::Limit => f.write_str("response size limit exceeded"),
            Self::Io => f.write_str("local I/O failed"),
            Self::NotReady => f.write_str("counts are not ready"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

/// The only persisted application state is a tiny serialized successful response.
pub type Cache = watch::Sender<Option<Bytes>>;

pub fn cache() -> Cache {
    watch::channel(None).0
}

/// A failure never replaces the last good snapshot, including during outages.
pub async fn refresh_once(upstream: &upstream::Upstream, cache: &Cache) -> Result<(), Error> {
    let counts = upstream.fetch().await?;
    cache.send_replace(Some(counts));
    Ok(())
}

pub async fn refresh_forever(upstream: upstream::Upstream, cache: Cache) {
    loop {
        let delay = match refresh_once(&upstream, &cache).await {
            Ok(()) => SUCCESS_INTERVAL,
            Err(error) => {
                eprintln!("Jellyfin counts refresh failed: {error}");
                RETRY_INTERVAL
            }
        };
        tokio::time::sleep(delay).await;
    }
}
