//! Read-only calendar projection. Provider credentials and unrelated provider fields never
//! cross this boundary; HTTP redirects remain disabled by the shared HTTP client.
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
use std::{fmt, marker::PhantomData};

const MAX_EVENTS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Date {
    year: u32,
    month: u32,
    day: u32,
}

impl Date {
    fn parse(raw: &str) -> Result<Self, Error> {
        if raw.len() != 10
            || raw.as_bytes().get(4) != Some(&b'-')
            || raw.as_bytes().get(7) != Some(&b'-')
            || !raw
                .bytes()
                .enumerate()
                .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
        {
            return Err(Error::InvalidData);
        }
        let number = |start, end| {
            raw.get(start..end)
                .and_then(|v| v.parse().ok())
                .ok_or(Error::InvalidData)
        };
        let date = Self {
            year: number(0, 4)?,
            month: number(5, 7)?,
            day: number(8, 10)?,
        };
        if !(1..=9999).contains(&date.year)
            || !(1..=12).contains(&date.month)
            || !(1..=date.month_days()).contains(&date.day)
        {
            return Err(Error::InvalidData);
        }
        Ok(date)
    }

    fn month_days(self) -> u32 {
        match self.month {
            2 if self.year.is_multiple_of(400)
                || (self.year.is_multiple_of(4) && !self.year.is_multiple_of(100)) =>
            {
                29
            }
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }

    fn ordinal(self) -> u32 {
        let previous = self.year - 1;
        let mut days = previous * 365 + previous / 4 - previous / 100 + previous / 400 + self.day;
        for month in 1..self.month {
            days += Self { month, ..self }.month_days();
        }
        days
    }

    fn previous(mut self) -> Self {
        if self.day > 1 {
            self.day -= 1;
        } else {
            if self.month > 1 {
                self.month -= 1;
            } else {
                self.year -= 1;
                self.month = 12;
            }
            self.day = self.month_days();
        }
        self
    }

    fn next(mut self) -> Self {
        if self.day < self.month_days() {
            self.day += 1;
        } else {
            self.day = 1;
            if self.month < 12 {
                self.month += 1;
            } else {
                self.year += 1;
                self.month = 1;
            }
        }
        self
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

pub(crate) struct Range {
    start: Date,
    end: Date,
}

impl Range {
    pub(crate) fn parse(query: &str) -> Result<Self, Error> {
        if query.len() > 80 {
            return Err(Error::InvalidData);
        }
        let (mut start, mut end) = (None, None);
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let target = match name.as_ref() {
                "start" => &mut start,
                "end" => &mut end,
                _ => return Err(Error::InvalidData),
            };
            if target.is_some() {
                return Err(Error::InvalidData);
            }
            *target = Some(Date::parse(&value)?);
        }
        let (start, end) = (
            start.ok_or(Error::InvalidData)?,
            end.ok_or(Error::InvalidData)?,
        );
        // Reserve one day at each edge for browser timezone conversion.
        if start.year < 2
            || end.year > 9998
            || end <= start
            || end.ordinal() - start.ordinal() > 366
        {
            return Err(Error::InvalidData);
        }
        Ok(Self { start, end })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    kind: &'static str,
    id: u64,
    series_id: Option<u64>,
    title: String,
    subtitle: String,
    date: String,
    slug: String,
    season: Option<u32>,
    episode: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Series {
    title: String,
    title_slug: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Episode {
    id: u64,
    series_id: u64,
    #[serde(default)]
    title: String,
    air_date_utc: Option<String>,
    series: Option<Series>,
    season_number: u32,
    episode_number: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Movie {
    id: u64,
    title: String,
    title_slug: String,
    release_date: Option<String>,
    digital_release: Option<String>,
    physical_release: Option<String>,
    in_cinemas: Option<String>,
}

// Refuse excess entries during deserialization rather than after allocating a Vec.
struct LimitedList<T>(Vec<T>);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for LimitedList<T> {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for ListVisitor<T> {
            type Value = LimitedList<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded calendar list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_EVENTS));
                while let Some(item) = seq.next_element()? {
                    if items.len() >= MAX_EVENTS {
                        return Err(de::Error::custom("calendar entry limit exceeded"));
                    }
                    items.push(item);
                }
                Ok(LimitedList(items))
            }
        }
        deserializer.deserialize_seq(ListVisitor(PhantomData))
    }
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

fn event_date(raw: String, range: &Range) -> Result<Option<String>, Error> {
    let date = Date::parse(raw.get(..10).ok_or(Error::InvalidData)?)?;
    if raw.len() > 40
        || (raw.len() > 10 && raw.as_bytes().get(10) != Some(&b'T'))
        || !raw
            .bytes()
            .all(|b| b.is_ascii_digit() || b"-T:.+Z".contains(&b))
    {
        return Err(Error::InvalidData);
    }
    // Padding is intentional: the browser selects the final local calendar day.
    if date < range.start.previous() || date > range.end {
        return Ok(None);
    }
    Ok(Some(raw))
}

async fn provider<T: for<'de> Deserialize<'de>>(
    relay: &Relay,
    base: &url::Url,
    key: &str,
    range: &Range,
    include_series: bool,
) -> Result<Vec<T>, Error> {
    let mut request = relay
        .http
        .get(endpoint(base, "/api/v3/calendar"))
        .header("X-Api-Key", key)
        .query(&[
            ("start", range.start.previous().to_string()),
            ("end", range.end.next().to_string()),
        ]);
    if include_series {
        request = request.query(&[("includeSeries", "true")]);
    }
    let response = request.send().await.map_err(|_| Error::Upstream)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error::Upstream);
    }
    let body = bounded_body(response, relay.config.provider_bytes).await?;
    serde_json::from_slice::<LimitedList<T>>(&body)
        .map(|items| items.0)
        .map_err(|_| Error::InvalidData)
}

pub(crate) async fn load(relay: &Relay, range: Range) -> Result<Bytes, Error> {
    let mut events = Vec::new();
    let config = &relay.config;
    if !config.sonarr_key.is_empty() {
        for episode in
            provider::<Episode>(relay, &config.sonarr, &config.sonarr_key, &range, true).await?
        {
            let (Some(series), Some(date)) = (episode.series, episode.air_date_utc) else {
                continue;
            };
            let Some(date) = event_date(date, &range)? else {
                continue;
            };
            events.push(Event {
                kind: "episode",
                id: episode.id,
                series_id: Some(episode.series_id),
                title: text(series.title)?,
                subtitle: text(episode.title)?,
                date,
                slug: slug(series.title_slug)?,
                season: Some(episode.season_number),
                episode: Some(episode.episode_number),
            });
        }
    }
    if !config.radarr_key.is_empty() {
        for movie in
            provider::<Movie>(relay, &config.radarr, &config.radarr_key, &range, false).await?
        {
            let Some(date) = movie
                .release_date
                .or(movie.digital_release)
                .or(movie.physical_release)
                .or(movie.in_cinemas)
            else {
                continue;
            };
            let Some(date) = event_date(date, &range)? else {
                continue;
            };
            if events.len() >= MAX_EVENTS {
                return Err(Error::Limit);
            }
            events.push(Event {
                kind: "movie",
                id: movie.id,
                series_id: None,
                title: text(movie.title)?,
                subtitle: String::new(),
                date,
                slug: slug(movie.title_slug)?,
                season: None,
                episode: None,
            });
        }
    }
    events.sort_unstable_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(a.title.cmp(&b.title))
            .then(a.season.cmp(&b.season))
            .then(a.episode.cmp(&b.episode))
    });
    #[derive(Serialize)]
    struct Calendar {
        start: String,
        end: String,
        events: Vec<Event>,
    }
    bounded_json(
        &Calendar {
            start: range.start.to_string(),
            end: range.end.to_string(),
            events,
        },
        config.snapshot_bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_require_real_dates_and_at_most_one_year() {
        assert!(Range::parse("start=2024-01-01&end=2025-01-01").is_ok());
        for query in [
            "",
            "start=2026-02-29&end=2026-03-01",
            "start=2026-10-20&end=2026-10-20",
            "start=2024-01-01&end=2025-01-02",
            "start=2026-01-01&end=2026-02-01&start=2026-01-01",
            "start=2026-01-01&end=2026-02-01&url=http://evil",
            "start=0001-01-01&end=0001-01-02",
        ] {
            assert!(Range::parse(query).is_err(), "{query}");
        }
        assert_eq!(
            Date::parse("2024-03-01").unwrap().previous().to_string(),
            "2024-02-29"
        );
        assert_eq!(
            Date::parse("2026-12-31").unwrap().next().to_string(),
            "2027-01-01"
        );
    }

    #[test]
    fn provider_lists_and_link_paths_are_bounded() {
        let payload = format!("[{}]", vec!["0"; MAX_EVENTS + 1].join(","));
        assert!(serde_json::from_str::<LimitedList<u8>>(&payload).is_err());
        for raw in [
            "../private",
            "%2fprivate",
            "https://evil",
            "",
            "x?key=value",
        ] {
            assert!(slug(raw.into()).is_err());
        }
        assert!(slug("cyberpunk-edgerunners".into()).is_ok());
        assert!(text("x".repeat(1025)).is_err());
    }
}
