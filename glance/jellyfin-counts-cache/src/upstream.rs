use crate::{Error, MAX_UPSTREAM_BYTES, UPSTREAM_TIMEOUT, config::Config};
use bytes::Bytes;
use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
struct Counts {
    movie_count: u64,
    series_count: u64,
    episode_count: u64,
    album_count: u64,
    song_count: u64,
    box_set_count: u64,
}

pub struct Upstream {
    http: Client,
    config: Config,
}

impl Upstream {
    pub fn new(config: Config) -> Result<Self, Error> {
        let http = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .http1_only()
            .pool_max_idle_per_host(0)
            .connect_timeout(Duration::from_secs(10))
            .timeout(UPSTREAM_TIMEOUT)
            .build()
            .map_err(|_| Error::Upstream)?;
        Ok(Self { http, config })
    }

    pub async fn fetch(&self) -> Result<Bytes, Error> {
        // The outer timeout also bounds JSON processing and every streamed chunk.
        tokio::time::timeout(UPSTREAM_TIMEOUT, self.fetch_inner())
            .await
            .map_err(|_| Error::Upstream)?
    }

    async fn fetch_inner(&self) -> Result<Bytes, Error> {
        let mut response = self
            .http
            .get(self.config.endpoint.clone())
            .header(header::AUTHORIZATION, self.config.authorization.clone())
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| Error::Upstream)?;
        if response.status() != StatusCode::OK {
            return Err(Error::Upstream);
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_UPSTREAM_BYTES as u64)
        {
            return Err(Error::Limit);
        }
        let mut body = Vec::with_capacity(MAX_UPSTREAM_BYTES);
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Upstream)? {
            if chunk.len() > MAX_UPSTREAM_BYTES.saturating_sub(body.len()) {
                return Err(Error::Limit);
            }
            body.extend_from_slice(&chunk);
        }
        let counts: Counts = serde_json::from_slice(&body).map_err(|_| Error::InvalidCounts)?;
        // Unknown upstream fields are discarded instead of retained in the cache.
        serde_json::to_vec(&counts)
            .map(Bytes::from)
            .map_err(|_| Error::InvalidCounts)
    }
}
