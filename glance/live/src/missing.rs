//! Bounded, read-only missing-media projection; provider paths and credentials stay server-side.
use crate::{
    Error,
    config::endpoint,
    relay::{Relay, bounded_body, bounded_json},
};
use bytes::Bytes;
use serde::{
    Deserialize, Serialize,
    de::{self, SeqAccess, Visitor},
};
use std::{collections::HashSet, fmt, marker::PhantomData};

const PAGE_SIZE: usize = 250;
const MAX_ITEMS: usize = 10_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    kind: &'static str,
    id: u64,
    series_id: Option<u64>,
    title: String,
    subtitle: String,
    slug: String,
    season: Option<u32>,
    episode: Option<u32>,
    year: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Series {
    title: String,
    title_slug: String,
    monitored: Option<bool>,
    seasons: Option<List<Season, MAX_ITEMS>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Season {
    season_number: u32,
    monitored: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Episode {
    id: u64,
    series_id: u64,
    #[serde(default)]
    title: String,
    series: Series,
    season_number: u32,
    episode_number: u32,
    has_file: Option<bool>,
    monitored: Option<bool>,
}

impl Episode {
    fn is_monitored(&self) -> bool {
        // The monitored-only endpoint guarantees episode and series monitoring. Reject
        // contrary flags defensively and check season flags, which its query omits.
        self.monitored != Some(false)
            && self.series.monitored != Some(false)
            && !self.series.seasons.as_ref().is_some_and(|seasons| {
                seasons.0.iter().any(|season| {
                    season.season_number == self.season_number && season.monitored == Some(false)
                })
            })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Movie {
    id: u64,
    title: String,
    title_slug: String,
    year: Option<u32>,
    has_file: Option<bool>,
    movie_file_id: Option<u64>,
    monitored: Option<bool>,
}

impl Movie {
    fn into_item(self) -> Result<Option<Item>, Error> {
        // Current Radarr uses movieFileId; older versions also expose hasFile.
        // A missing field alone is never evidence that a file is missing.
        let missing = self.has_file == Some(false)
            || (self.has_file.is_none() && self.movie_file_id == Some(0));
        if self.monitored != Some(true) || !missing || self.movie_file_id.is_some_and(|id| id != 0)
        {
            return Ok(None);
        }
        if self.year.is_some_and(|year| year > 9999) {
            return Err(Error::InvalidData);
        }
        Ok(Some(Item {
            kind: "movie",
            id: self.id,
            series_id: None,
            title: text(self.title)?,
            subtitle: String::new(),
            slug: slug(self.title_slug)?,
            season: None,
            episode: None,
            year: self.year,
        }))
    }
}

// Keep only projected missing movies instead of retaining a second copy of the whole library.
struct Movies(Vec<Item>);
impl<'de> Deserialize<'de> for Movies {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MoviesVisitor;
        impl<'de> Visitor<'de> for MoviesVisitor {
            type Value = Movies;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded movie list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut scanned = 0usize;
                let mut ids = HashSet::new();
                let mut items = Vec::new();
                while let Some(movie) = seq.next_element::<Movie>()? {
                    if scanned >= MAX_ITEMS {
                        return Err(de::Error::custom("movie entry limit exceeded"));
                    }
                    scanned += 1;
                    if let Some(item) = movie.into_item().map_err(de::Error::custom)?
                        && ids.insert(item.id)
                    {
                        items.push(item);
                    }
                }
                Ok(Movies(items))
            }
        }
        deserializer.deserialize_seq(MoviesVisitor)
    }
}

// The byte ceiling also bounds strings and ignored provider data. This independent entry
// ceiling rejects excess records while deserializing, before growing an unbounded list.
struct List<T, const LIMIT: usize>(Vec<T>);
impl<'de, T: Deserialize<'de>, const LIMIT: usize> Deserialize<'de> for List<T, LIMIT> {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListVisitor<T, const LIMIT: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const LIMIT: usize> Visitor<'de> for ListVisitor<T, LIMIT> {
            type Value = List<T, LIMIT>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded media list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(LIMIT));
                while let Some(item) = seq.next_element()? {
                    if items.len() >= LIMIT {
                        return Err(de::Error::custom("media entry limit exceeded"));
                    }
                    items.push(item);
                }
                Ok(List(items))
            }
        }
        deserializer.deserialize_seq(ListVisitor::<T, LIMIT>(PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    page: usize,
    page_size: usize,
    total_records: usize,
    records: List<Episode, PAGE_SIZE>,
}

fn text(raw: String) -> Result<String, Error> {
    if raw.len() > 1024 || raw.chars().any(char::is_control) {
        return Err(Error::InvalidData);
    }
    Ok(raw)
}

fn slug(raw: String) -> Result<String, Error> {
    if raw.is_empty()
        || raw.len() > 256
        || !raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(Error::InvalidData);
    }
    Ok(raw)
}

async fn provider<T: for<'de> Deserialize<'de>>(
    relay: &Relay,
    base: &url::Url,
    key: &str,
    path: &str,
    query: &[(&str, String)],
) -> Result<T, Error> {
    let response = relay
        .http
        .get(endpoint(base, path))
        .header("X-Api-Key", key)
        .query(query)
        .send()
        .await
        .map_err(|_| Error::Upstream)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error::Upstream);
    }
    let body = bounded_body(response, relay.config.missing_provider_bytes).await?;
    serde_json::from_slice(&body).map_err(|_| Error::InvalidData)
}

async fn episodes(relay: &Relay, items: &mut Vec<Item>) -> Result<(), Error> {
    let config = &relay.config;
    let mut page_number = 1usize;
    let mut expected_total = None;
    let mut scanned = 0usize;
    let mut page_ids = HashSet::new();
    // Sonarr's monitored query includes specials and excludes episodes not finished airing.
    loop {
        let page: Page = provider(
            relay,
            &config.sonarr,
            &config.sonarr_key,
            "/api/v3/wanted/missing",
            &[
                ("includeSeries", "true".into()),
                ("monitored", "true".into()),
                ("page", page_number.to_string()),
                ("pageSize", PAGE_SIZE.to_string()),
                ("sortKey", "episodes.airDateUtc".into()),
                ("sortDirection", "ascending".into()),
            ],
        )
        .await?;
        if page.page != page_number
            || page.page_size != PAGE_SIZE
            || page.total_records > MAX_ITEMS
            || expected_total.is_some_and(|total| total != page.total_records)
            || page.records.0.len() != PAGE_SIZE.min(page.total_records.saturating_sub(scanned))
        {
            return Err(Error::InvalidData);
        }
        expected_total = Some(page.total_records);
        scanned += page.records.0.len();
        for episode in page.records.0 {
            // Repeated pages or unstable page boundaries could otherwise silently omit media.
            if !page_ids.insert(episode.id) {
                return Err(Error::InvalidData);
            }
            if episode.has_file == Some(true) || !episode.is_monitored() {
                continue;
            }
            items.push(Item {
                kind: "episode",
                id: episode.id,
                series_id: Some(episode.series_id),
                title: text(episode.series.title)?,
                subtitle: text(episode.title)?,
                slug: slug(episode.series.title_slug)?,
                season: Some(episode.season_number),
                episode: Some(episode.episode_number),
                year: None,
            });
        }
        if scanned == page.total_records {
            break;
        }
        page_number += 1;
    }
    Ok(())
}

pub(crate) async fn load(relay: &Relay) -> Result<Bytes, Error> {
    let config = &relay.config;
    let mut items = Vec::new();
    if !config.sonarr_key.is_empty() {
        episodes(relay, &mut items).await?;
    }
    if !config.radarr_key.is_empty() {
        let movies: Movies = provider(
            relay,
            &config.radarr,
            &config.radarr_key,
            "/api/v3/movie",
            &[],
        )
        .await?;
        if movies.0.len() > MAX_ITEMS.saturating_sub(items.len()) {
            return Err(Error::Limit);
        }
        items.extend(movies.0);
    }
    items.sort_unstable_by(|a, b| {
        a.title
            .cmp(&b.title)
            .then(a.kind.cmp(b.kind))
            .then(a.series_id.cmp(&b.series_id))
            .then(a.season.cmp(&b.season))
            .then(a.episode.cmp(&b.episode))
            .then(a.id.cmp(&b.id))
    });
    #[derive(Serialize)]
    struct Providers {
        sonarr: bool,
        radarr: bool,
    }
    #[derive(Serialize)]
    struct Missing {
        items: Vec<Item>,
        providers: Providers,
    }
    bounded_json(
        &Missing {
            items,
            providers: Providers {
                sonarr: !config.sonarr_key.is_empty(),
                radarr: !config.radarr_key.is_empty(),
            },
        },
        config.missing_snapshot_bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_text_and_link_paths_are_bounded() {
        assert!(serde_json::from_str::<List<u8, 2>>("[0,1]").is_ok());
        assert!(serde_json::from_str::<List<u8, 2>>("[0,1,2]").is_err());
        assert!(text("x".repeat(1025)).is_err());
        assert!(text("title\nprivate".into()).is_err());
        for raw in [
            "../private",
            "%2fprivate",
            "https://evil",
            "",
            "x?key=value",
        ] {
            assert!(slug(raw.into()).is_err());
        }
    }
}
