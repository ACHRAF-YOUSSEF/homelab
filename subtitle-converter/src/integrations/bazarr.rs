//! Bazarr owns desired languages and media identities. Its API deliberately
//! omits embedded codecs; those remain unknown until the converter probes them.
//! Sources: upstream api/{movies/movies,series/series,episodes/episodes}.py,
//! api/system/languages_profiles.py, api/swaggerui.py and app/database.py.

use super::*;
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Instant;

const PAGE_SIZE: usize = 200;
const MAX_PROFILES: usize = 512;
const MAX_TARGETS: usize = 64;
const MAX_SUBTITLES: usize = 256;
const MAX_INVENTORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_REQUESTS: usize = 1024;
const FETCH_DEADLINE: Duration = Duration::from_secs(90);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Target {
    language: String,
    forced: bool,
    hi: bool,
}

#[derive(Deserialize)]
struct RawProfile {
    #[serde(rename = "profileId")]
    id: i64,
    items: Vec<RawTarget>,
}
#[derive(Deserialize)]
struct RawTarget {
    language: String,
    #[serde(default)]
    forced: Value,
    #[serde(default)]
    hi: Value,
    #[serde(default)]
    audio_exclude: Value,
    #[serde(default)]
    audio_only_include: Value,
}
#[derive(Deserialize)]
struct RawLanguage {
    code2: Option<String>,
    code3: Option<String>,
}
#[derive(Deserialize)]
struct RawSubtitle {
    code2: Option<String>,
    code3: Option<String>,
    path: Option<String>,
    #[serde(default)]
    forced: Value,
    #[serde(default)]
    hi: Value,
    embedded_track_id: Option<u64>,
    file_size: Option<u64>,
}
#[derive(Deserialize)]
struct RawMedia {
    path: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "radarrId")]
    movie_id: Option<i64>,
    #[serde(rename = "sonarrEpisodeId")]
    episode_id: Option<i64>,
    #[serde(rename = "sonarrSeriesId")]
    series_id: Option<i64>,
    #[serde(rename = "profileId")]
    profile_id: Option<i64>,
    #[serde(default)]
    subtitles: Vec<RawSubtitle>,
    #[serde(default)]
    audio_language: Vec<RawLanguage>,
}
#[derive(Deserialize)]
struct Page<T> {
    data: Vec<T>,
    total: usize,
}
#[derive(Deserialize)]
struct EpisodePage {
    data: Vec<RawMedia>,
}
#[derive(Deserialize)]
struct Series {
    #[serde(rename = "sonarrSeriesId")]
    id: i64,
    #[serde(rename = "profileId")]
    profile_id: Option<i64>,
}

struct FetchBudget {
    requests: usize,
    bytes: usize,
    deadline: Instant,
}
impl FetchBudget {
    fn new() -> Self {
        Self {
            requests: 0,
            bytes: 0,
            deadline: Instant::now() + FETCH_DEADLINE,
        }
    }
    fn get(
        &mut self,
        integration: &Integrations,
        path: &str,
        stop: &AtomicBool,
    ) -> Result<Value, String> {
        if stop.load(Ordering::Relaxed) {
            return Err("Bazarr inventory refresh cancelled".into());
        }
        if self.requests >= MAX_REQUESTS || Instant::now() >= self.deadline {
            return Err("Bazarr inventory exceeds the bounded request or time limit".into());
        }
        self.requests += 1;
        let value = integration
            .api("bazarr", path, "GET", None, None)
            .map_err(|error| error.message("Bazarr inventory failed"))?;
        let size = serde_json::to_vec(&value)
            .map_err(|_| "Bazarr inventory JSON is invalid")?
            .len();
        self.bytes = self
            .bytes
            .checked_add(size)
            .filter(|bytes| *bytes <= MAX_INVENTORY_BYTES)
            .ok_or("Bazarr inventory exceeds the response byte budget")?;
        Ok(value)
    }
}

fn decoded<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|_| "Bazarr returned unsupported inventory data".into())
}
fn flag(value: &Value) -> Option<bool> {
    match value {
        Value::Null => Some(false),
        Value::Bool(value) => Some(*value),
        Value::String(value) => match value.to_ascii_lowercase().as_str() {
            "true" | "1" | "only" => Some(true),
            "false" | "0" | "never" | "also" => Some(false),
            _ => None,
        },
        _ => None,
    }
}
fn language(raw: &str) -> Option<String> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<BTreeMap<String, Option<String>>>> = OnceLock::new();
    if raw.is_empty() || raw.len() > 32 || raw == "*" {
        return None;
    }
    let normalized = raw.to_ascii_lowercase();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(cached) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&normalized)
    {
        return cached.clone();
    }
    let result = uncached_language(&normalized);
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() < 512 {
        cache.insert(normalized, result.clone());
    }
    result
}
fn uncached_language(raw: &str) -> Option<String> {
    match raw.to_ascii_lowercase().as_str() {
        "pb" | "pob" => return Some("pt-br".into()),
        "zt" | "zht" => return Some("zh-tw".into()),
        "ea" | "spl" => return Some("es-mx".into()),
        _ => {}
    }
    let values = crate::converter::Converter::normalize_languages(&json!([raw])).ok()?;
    let primary = values
        .into_iter()
        .next()
        .filter(|language| !["und", "unknown", "zxx"].contains(&language.as_str()))?;
    Some(primary)
}
fn subtitle_language(code2: Option<&str>, code3: Option<&str>) -> Option<String> {
    code2
        .and_then(language)
        .or_else(|| code3.and_then(language))
}
fn targets(profile: &RawProfile, audio: &[RawLanguage]) -> Result<Vec<Target>, String> {
    if profile.items.len() > MAX_TARGETS {
        return Err("Bazarr language profile exceeds target limit".into());
    }
    let audio: BTreeSet<String> = audio
        .iter()
        .filter_map(|item| subtitle_language(item.code2.as_deref(), item.code3.as_deref()))
        .collect();
    let mut result = BTreeSet::new();
    for item in &profile.items {
        let language =
            language(&item.language).ok_or("Bazarr profile contains an unknown language")?;
        let excluded =
            flag(&item.audio_exclude).ok_or("Bazarr profile has unsupported audio rules")?;
        let included_only =
            flag(&item.audio_only_include).ok_or("Bazarr profile has unsupported audio rules")?;
        if (excluded && audio.contains(&language)) || (included_only && !audio.contains(&language))
        {
            continue;
        }
        result.insert(Target {
            language,
            forced: flag(&item.forced).ok_or("Bazarr profile has unsupported forced rules")?,
            hi: flag(&item.hi).ok_or("Bazarr profile has unsupported HI rules")?,
        });
    }
    Ok(result.into_iter().collect())
}

impl Integrations {
    pub fn bazarr_enabled(&self) -> bool {
        // Authority selection survives credential edits. Missing credentials
        // must fail closed rather than select filesystem-wide conversion.
        enabled(self.conf("bazarr"))
    }

    /// Fetch complete profile-based SRT coverage, never treating embedded or
    /// non-SRT subtitles as an external SRT. An incomplete fetch is an error;
    /// callers must retain previous inventory and must not widen conversion.
    pub fn bazarr_inventory(&self, stop: &AtomicBool) -> Result<Value, String> {
        if !self.bazarr_enabled() {
            return Err("Bazarr integration is disabled".into());
        }
        if get_str(self.conf("bazarr"), "url").is_empty()
            || get_str(self.conf("bazarr"), "api_key").is_empty()
        {
            return Err("Bazarr is enabled but its URL or API key is missing".into());
        }
        let mut budget = FetchBudget::new();
        let profiles: Vec<RawProfile> =
            decoded(budget.get(self, "/api/system/languages/profiles", stop)?)?;
        if profiles.len() > MAX_PROFILES {
            return Err("Bazarr profiles exceed the supported limit".into());
        }
        let mut profile_map = BTreeMap::new();
        for profile in profiles {
            if profile.id <= 0 || profile_map.insert(profile.id, profile).is_some() {
                return Err("Bazarr returned duplicate or invalid profile IDs".into());
            }
        }
        let mut items = Vec::new();
        let mut item_bytes = 0_usize;
        let mut ids = BTreeSet::new();
        self.bazarr_pages("/api/movies", &mut budget, stop, |value| {
            let media: RawMedia = decoded(value)?;
            let row = self.bazarr_row(media, None, &profile_map)?;
            add_row(&mut items, &mut item_bytes, &mut ids, row)
        })?;
        let mut series = BTreeMap::new();
        self.bazarr_pages("/api/series", &mut budget, stop, |value| {
            let item: Series = decoded(value)?;
            if item.id <= 0 || series.insert(item.id, item.profile_id).is_some() {
                return Err("Bazarr returned duplicate or invalid series IDs".into());
            }
            Ok(())
        })?;
        // Upstream episodes has no paging parameters. It accepts repeated
        // seriesid[] query arguments; batching bounds requests while each
        // response and the overall inventory retain explicit byte ceilings.
        let series_ids: Vec<i64> = series.keys().copied().collect();
        for batch in series_ids.chunks(8) {
            let query = batch
                .iter()
                .map(|id| format!("seriesid%5B%5D={id}"))
                .collect::<Vec<_>>()
                .join("&");
            let page: EpisodePage =
                decoded(budget.get(self, &format!("/api/episodes?{query}"), stop)?)?;
            if page.data.len() > MAX_ITEMS {
                return Err("Bazarr episode batch exceeds item limit".into());
            }
            for media in page.data {
                let series_id = media
                    .series_id
                    .ok_or("Bazarr episode has no series identity")?;
                if !batch.contains(&series_id) {
                    return Err("Bazarr episode response escaped its requested series".into());
                }
                let row = self.bazarr_row(
                    media,
                    series.get(&series_id).copied().flatten(),
                    &profile_map,
                )?;
                add_row(&mut items, &mut item_bytes, &mut ids, row)?;
            }
        }
        Ok(
            json!({"source":"bazarr","items":items,"partial":false,"truncated":false,"requests":budget.requests,"external_srt_required":true}),
        )
    }

    pub(super) fn bazarr_scan_paths(
        &self,
        paths: &[String],
        stop: &AtomicBool,
        progress: &mut dyn FnMut(Value),
    ) -> Result<usize, String> {
        let remotes: Vec<NormPath> = paths
            .iter()
            .filter_map(|path| {
                self.remote_path("bazarr", path)
                    .and_then(|path| norm_path(&path))
            })
            .collect();
        let mut budget = FetchBudget::new();
        let mut series_matches: Vec<Option<(usize, i64)>> = vec![None; remotes.len()];
        let mut movie_ids = BTreeSet::new();
        self.bazarr_pages("/api/series", &mut budget, stop, |row| {
            let id = row["sonarrSeriesId"]
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or("Bazarr series identity is invalid")?;
            let root = row["path"]
                .as_str()
                .and_then(norm_path)
                .ok_or("Bazarr series path is invalid")?;
            for (path, matched) in remotes.iter().zip(&mut series_matches) {
                if under(path, &root) && matched.is_none_or(|(length, _)| root.value.len() > length)
                {
                    *matched = Some((root.value.len(), id));
                }
            }
            Ok(())
        })?;
        self.bazarr_pages("/api/movies", &mut budget, stop, |row| {
            let id = row["radarrId"]
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or("Bazarr movie identity is invalid")?;
            let media = row["path"]
                .as_str()
                .and_then(norm_path)
                .ok_or("Bazarr movie path is invalid")?;
            if remotes.iter().any(|path| {
                path.flavor == media.flavor
                    && path_case_eq(
                        &path_stem(&path.value),
                        &path_stem(&media.value),
                        media.flavor,
                    )
                    && parent(path)
                        .zip(parent(&media))
                        .is_some_and(|(left, right)| path_eq(&left, &right))
            }) {
                movie_ids.insert(id);
            }
            Ok(())
        })?;
        let mut count = 0;
        let series_ids = series_matches
            .into_iter()
            .flatten()
            .map(|(_, id)| id)
            .collect::<BTreeSet<_>>();
        for (kind, id) in series_ids
            .into_iter()
            .map(|id| ("series", id))
            .chain(movie_ids.into_iter().map(|id| ("movie", id)))
        {
            if stop.load(Ordering::Relaxed) {
                return Ok(count);
            }
            if count >= 1024 || budget.requests >= MAX_REQUESTS || Instant::now() >= budget.deadline
            {
                return Err("Bazarr notification exceeds its scoped request or time limit".into());
            }
            budget.requests += 1;
            progress(json!({"status":"refreshing","path":format!("Bazarr · {kind} {id}")}));
            let (endpoint, field) = if kind == "series" {
                ("/api/series", "seriesid")
            } else {
                ("/api/movies", "radarrid")
            };
            // sync may update manager state; search-* may download subtitles.
            // Only this documented action reindexes already published files.
            let fields = [(field, id.to_string()), ("action", "scan-disk".to_owned())];
            self.api("bazarr", endpoint, "PATCH", None, Some(&fields))
                .map_err(|error| error.message("Bazarr scoped subtitle rescan failed"))?;
            count += 1;
        }
        Ok(count)
    }

    fn bazarr_pages(
        &self,
        endpoint: &str,
        budget: &mut FetchBudget,
        stop: &AtomicBool,
        mut consume: impl FnMut(Value) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut start = 0;
        let mut expected_total = None;
        loop {
            let page: Page<Value> = decoded(budget.get(
                self,
                &format!("{endpoint}?start={start}&length={PAGE_SIZE}"),
                stop,
            )?)?;
            if page.total > MAX_ITEMS
                || page.data.len() > PAGE_SIZE
                || expected_total.is_some_and(|total| total != page.total)
            {
                return Err(
                    "Bazarr paginated inventory changed or exceeded item limits; refresh again"
                        .into(),
                );
            }
            expected_total = Some(page.total);
            let amount = page.data.len();
            if (amount == 0 && start < page.total) || start.saturating_add(amount) > page.total {
                return Err("Bazarr returned an incomplete inventory page".into());
            }
            for row in page.data {
                consume(row)?;
            }
            start = start
                .checked_add(amount)
                .ok_or("Bazarr inventory offset overflow")?;
            if start >= page.total {
                break;
            }
            if amount < PAGE_SIZE {
                return Err("Bazarr returned a short inventory page".into());
            }
        }
        Ok(())
    }

    fn bazarr_row(
        &self,
        media: RawMedia,
        parent_profile: Option<i64>,
        profiles: &BTreeMap<i64, RawProfile>,
    ) -> Result<Value, String> {
        if media.path.len() > 4096
            || norm_path(&media.path).is_none()
            || media.subtitles.len() > MAX_SUBTITLES
            || media.audio_language.len() > 64
        {
            return Err(
                "Bazarr media paths or subtitle counts exceed their supported limits".into(),
            );
        }
        let (kind, id) = if let Some(id) = media.movie_id {
            ("movie", id)
        } else {
            (
                "episode",
                media.episode_id.ok_or("Bazarr media identity is missing")?,
            )
        };
        if id <= 0 {
            return Err("Bazarr media identity is invalid".into());
        }
        let profile_id = media.profile_id.or(parent_profile);
        let desired = profile_id.map(|id| {
            profiles
                .get(&id)
                .ok_or("Bazarr media profile was not returned")
                .and_then(|profile| {
                    targets(profile, &media.audio_language)
                        .map_err(|_| "Bazarr profile requirements are unsupported")
                })
        });
        let mut coverage = BTreeSet::new();
        let mut sources = Vec::with_capacity(media.subtitles.len());
        let mut unknown_source = false;
        for subtitle in &media.subtitles {
            let lang = subtitle_language(subtitle.code2.as_deref(), subtitle.code3.as_deref());
            let forced = (!subtitle.forced.is_null())
                .then(|| flag(&subtitle.forced))
                .flatten();
            let hi = (!subtitle.hi.is_null())
                .then(|| flag(&subtitle.hi))
                .flatten();
            let external = subtitle
                .path
                .as_deref()
                .is_some_and(|path| !path.is_empty());
            if subtitle.path.as_ref().is_some_and(|path| {
                path.len() > 4096 || (!path.is_empty() && norm_path(path).is_none())
            }) {
                return Err("Bazarr returned an invalid subtitle path".into());
            }
            let codec = if external {
                subtitle
                    .path
                    .as_deref()
                    .and_then(|path| path.rsplit('.').next())
                    .unwrap_or("unknown")
                    .to_ascii_lowercase()
            } else {
                "unknown".into()
            };
            if external
                && codec == "srt"
                && subtitle.file_size != Some(0)
                && let (Some(language), Some(forced), Some(hi)) = (lang.clone(), forced, hi)
            {
                coverage.insert(Target {
                    language,
                    forced,
                    hi,
                });
            }
            unknown_source |= lang.is_none() || forced.is_none() || hi.is_none();
            let convertible = !external
                || [
                    "ass", "ssa", "vtt", "ttml", "smi", "sami", "sbv", "sup", "idx", "sub",
                ]
                .contains(&codec.as_str());
            sources.push(json!({"language":lang,"forced":forced,"hi":hi,"source":if external {"sidecar"}else{"embedded"},"codec":codec,"path":subtitle.path.as_deref().filter(|path|!path.is_empty()).and_then(|path|self.map_path("bazarr",path).or_else(||Some(path.to_owned()))),"remote_path":subtitle.path,"embedded_track_id":subtitle.embedded_track_id,"file_size":subtitle.file_size,"convertible":convertible}));
        }
        let (wanted, missing, status, reason) = match desired {
            None => (
                Vec::new(),
                Vec::new(),
                "unconfigured",
                "No Bazarr language profile is assigned",
            ),
            Some(Err(_)) => (
                Vec::new(),
                Vec::new(),
                "unknown",
                "Bazarr profile requirements are unavailable or unsupported",
            ),
            Some(Ok(wanted)) => {
                let missing: Vec<_> = wanted
                    .iter()
                    .filter(|target| !coverage.contains(*target))
                    .cloned()
                    .collect();
                let status = if missing.is_empty() {
                    "covered"
                } else if unknown_source {
                    "unknown"
                } else {
                    "missing"
                };
                let reason = if status == "covered" {
                    "Bazarr reports an external SRT for every wanted variant"
                } else if status == "unknown" {
                    "Unknown subtitle language or flags require inspection"
                } else {
                    "Wanted language variants lack external SRT subtitles"
                };
                (wanted, missing, status, reason)
            }
        };
        let convertible = missing.iter().any(|target| {
            sources.iter().any(|source| {
                source["convertible"] == true
                    && source["language"].as_str() == Some(target.language.as_str())
                    && source["forced"].as_bool() == Some(target.forced)
                    && source["hi"].as_bool() == Some(target.hi)
            })
        });
        Ok(
            json!({"path":self.map_path("bazarr",&media.path).unwrap_or_else(||media.path.clone()),"remote_path":media.path,"kind":kind,"id":id,"series_id":media.series_id,"profile_id":profile_id,"title":media.title.chars().take(240).collect::<String>(),"wanted_targets":wanted,"missing_targets":missing,"missing_srt":!missing.is_empty(),"has_convertible_source":convertible,"subtitles":sources,"coverage":{"status":status,"reason":reason}}),
        )
    }
}

fn add_row(
    items: &mut Vec<Value>,
    bytes: &mut usize,
    ids: &mut BTreeSet<(String, i64)>,
    row: Value,
) -> Result<(), String> {
    if items.len() >= MAX_ITEMS {
        return Err("Bazarr inventory exceeds its media item limit".into());
    }
    let key = (
        row["kind"].as_str().unwrap_or_default().to_owned(),
        row["id"].as_i64().unwrap_or_default(),
    );
    if !ids.insert(key) {
        return Err("Bazarr inventory repeated a media identity".into());
    }
    let normalized = row["path"]
        .as_str()
        .and_then(norm_path)
        .ok_or("Bazarr mapped media path is invalid")?;
    let normalized = if normalized.flavor == Flavor::Windows {
        normalized.value.to_ascii_lowercase()
    } else {
        normalized.value
    };
    if !ids.insert((format!("path:{normalized}"), 0)) {
        return Err("Bazarr inventory assigned multiple identities to one media path".into());
    }
    let size = serde_json::to_vec(&row)
        .map_err(|_| "Bazarr inventory cannot be encoded")?
        .len();
    *bytes = bytes
        .checked_add(size)
        .filter(|total| *total <= MAX_INVENTORY_BYTES)
        .ok_or("Bazarr inventory exceeds its memory budget")?;
    items.push(row);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    fn integration() -> (tempfile::TempDir, Integrations) {
        let temporary = tempfile::tempdir().unwrap();
        let integrations = Integrations::new(temporary.path().join("settings.json")).unwrap();
        (temporary, integrations)
    }
    fn target(language: &str) -> Value {
        json!({"language":language,"forced":"False","hi":"False","audio_exclude":"False","audio_only_include":"False"})
    }
    fn media(subtitles: Value) -> Value {
        json!({"path":"/movies/Film.mkv","radarrId":1,"profileId":1,"title":"Fixture","subtitles":subtitles,"audio_language":[]})
    }
    fn subtitle(language: &str, path: Option<&str>) -> Value {
        json!({"code2":language,"code3":null,"path":path,"forced":false,"hi":false,"embedded_track_id":if path.is_none(){Some(2)}else{None},"file_size":128})
    }
    fn row(value: Value, items: Value) -> Value {
        let (_temporary, integration) = integration();
        let profile: RawProfile = decoded(json!({"profileId":1,"items":items})).unwrap();
        integration
            .bazarr_row(
                decoded(value).unwrap(),
                None,
                &BTreeMap::from([(1, profile)]),
            )
            .unwrap()
    }
    fn fixture(replies: Vec<(String, Value)>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut requests = Vec::new();
            for (expected, body) in replies {
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error)
                            if error.kind() == ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => {
                            panic!("Bazarr fixture request did not arrive within its deadline")
                        }
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut expected_size = None;
                while bytes.len() < 65536 {
                    let mut chunk = [0_u8; 1024];
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        expected_size = Some(end + 4 + length);
                    }
                    if expected_size.is_some_and(|size| bytes.len() >= size) {
                        break;
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                assert!(
                    request.starts_with(&expected),
                    "Unexpected Bazarr fixture API route"
                );
                requests.push(request);
                let body = body.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
            requests
        });
        (endpoint, worker)
    }

    #[test]
    fn mixed_subtitles_require_external_srt_for_every_profile_language() {
        let item = row(
            media(json!([
                subtitle("en", Some("/movies/Film.en.srt")),
                subtitle("fr", Some("/movies/Film.fr.ass")),
                subtitle("ar", None)
            ])),
            json!([target("en"), target("fr"), target("ar")]),
        );
        assert_eq!(
            item["missing_targets"],
            json!([{"language":"ar","forced":false,"hi":false},{"language":"fr","forced":false,"hi":false}])
        );
        assert_eq!(item["coverage"]["status"], "missing");
        assert_eq!(item["has_convertible_source"], true);
        assert_eq!(item["subtitles"][2]["codec"], "unknown");
        assert_eq!(item["subtitles"][2]["source"], "embedded");
        let item = row(media(json!([subtitle("en", None)])), json!([target("en")]));
        assert_eq!(
            item["missing_srt"], true,
            "embedded SRT is not external coverage"
        );
        let item = row(
            media(json!([subtitle("en", Some("/movies/Film.en.SRT"))])),
            json!([target("en")]),
        );
        assert_eq!(item["coverage"]["status"], "covered");
        assert_eq!(item["missing_targets"], json!([]));
    }

    #[test]
    fn regional_forced_and_hi_targets_remain_distinct() {
        let mut forced = target("en");
        forced["forced"] = json!("True");
        let mut hi = target("fr");
        hi["hi"] = json!("True");
        let item = row(
            media(json!([
                subtitle("pt", Some("/movies/Film.pt.srt")),
                subtitle("en", Some("/movies/Film.en.srt")),
                subtitle("fr", Some("/movies/Film.fr.srt"))
            ])),
            json!([target("pt-BR"), forced, hi]),
        );
        assert_eq!(item["missing_targets"].as_array().unwrap().len(), 3);
        assert!(
            item["missing_targets"]
                .as_array()
                .unwrap()
                .iter()
                .any(|target| target["language"] == "pt-br")
        );
        let item = row(
            media(json!([subtitle("pob", Some("/movies/Film.pb.srt"))])),
            json!([target("pb")]),
        );
        assert_eq!(item["coverage"]["status"], "covered");
        assert_eq!(language("por-BR"), Some("pt-br".into()));
    }

    #[test]
    fn unknown_metadata_empty_srt_and_missing_profiles_do_not_claim_coverage() {
        let mut empty = subtitle("en", Some("/movies/Film.en.srt"));
        empty["file_size"] = json!(0);
        let item = row(
            media(json!([empty, subtitle("und", None)])),
            json!([target("en")]),
        );
        assert_eq!(item["coverage"]["status"], "unknown");
        assert_eq!(item["missing_srt"], true);
        let mut unknown = media(json!([]));
        unknown["profileId"] = json!(9);
        let item = row(unknown, json!([target("en")]));
        assert_eq!(item["coverage"]["status"], "unknown");
        assert_eq!(item["missing_targets"], json!([]));
        let mut unconfigured = media(json!([]));
        unconfigured["profileId"] = Value::Null;
        assert_eq!(
            row(unconfigured, json!([target("en")]))["coverage"]["status"],
            "unconfigured"
        );
    }

    #[test]
    fn audio_exclusion_rules_and_profile_string_flags_are_honored() {
        let mut english = target("en");
        english["audio_exclude"] = json!("True");
        let mut french = target("fr");
        french["audio_only_include"] = json!("True");
        let mut value = media(json!([]));
        value["audio_language"] = json!([{"code2":"en","code3":"eng"}]);
        let item = row(value, json!([english, french, target("ar")]));
        assert_eq!(
            item["wanted_targets"],
            json!([{"language":"ar","forced":false,"hi":false}])
        );
    }

    #[test]
    fn paginated_inventory_inherits_series_profiles_and_maps_paths() {
        let (_temporary, mut integration) = integration();
        let first:Vec<Value>=(1..=200).map(|id|json!({"radarrId":id,"path":format!("/remote/movies/{id}.mkv"),"profileId":1,"subtitles":[]})).collect();
        let (endpoint, server) = fixture(vec![
            (
                "GET /api/system/languages/profiles HTTP/1.1".into(),
                json!([{"profileId":1,"items":[target("en")]}]),
            ),
            (
                "GET /api/movies?start=0&length=200 HTTP/1.1".into(),
                json!({"data":first,"total":201}),
            ),
            (
                "GET /api/movies?start=200&length=200 HTTP/1.1".into(),
                json!({"data":[{"radarrId":201,"path":"/remote/movies/201.mkv","profileId":1,"subtitles":[]}],"total":201}),
            ),
            (
                "GET /api/series?start=0&length=200 HTTP/1.1".into(),
                json!({"data":[{"sonarrSeriesId":10,"profileId":1}],"total":1}),
            ),
            (
                "GET /api/episodes?seriesid%5B%5D=10 HTTP/1.1".into(),
                json!({"data":[{"sonarrSeriesId":10,"sonarrEpisodeId":20,"path":"/remote/tv/Show/episode.mkv","subtitles":[subtitle("en",None)]}]}),
            ),
        ]);
        integration.update(&json!({"bazarr":{"enabled":true,"url":endpoint,"api_key":"fixture-key"},"path_mappings":[{"provider":"bazarr","remote":"/remote","local":"/media"}]})).unwrap();
        let inventory = integration
            .bazarr_inventory(&AtomicBool::new(false))
            .unwrap();
        assert_eq!(inventory["items"].as_array().unwrap().len(), 202);
        let episode = &inventory["items"][201];
        assert_eq!(episode["path"], "/media/tv/Show/episode.mkv");
        assert_eq!(episode["profile_id"], 1);
        assert_eq!(
            episode["missing_targets"],
            json!([{"language":"en","forced":false,"hi":false}])
        );
        let requests = server.join().unwrap();
        assert!(requests.iter().all(|request| {
            request
                .to_ascii_lowercase()
                .contains("x-api-key: fixture-key")
        }));
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
    }

    #[test]
    fn incomplete_pages_and_foreign_episode_results_fail_closed() {
        let (_temporary, mut integration) = integration();
        let (endpoint, server) = fixture(vec![
            ("GET /api/system/languages/profiles ".into(), json!([])),
            (
                "GET /api/movies?start=0&length=200 ".into(),
                json!({"data":[],"total":1}),
            ),
        ]);
        integration
            .update(&json!({"bazarr":{"enabled":true,"url":endpoint,"api_key":"fixture-key"}}))
            .unwrap();
        assert!(
            integration
                .bazarr_inventory(&AtomicBool::new(false))
                .unwrap_err()
                .contains("incomplete")
        );
        server.join().unwrap();
        let (endpoint, server) = fixture(vec![
            ("GET /api/system/languages/profiles ".into(), json!([])),
            (
                "GET /api/movies?start=0&length=200 ".into(),
                json!({"data":[],"total":0}),
            ),
            (
                "GET /api/series?start=0&length=200 ".into(),
                json!({"data":[{"sonarrSeriesId":1,"profileId":null}],"total":1}),
            ),
            (
                "GET /api/episodes?seriesid%5B%5D=1 ".into(),
                json!({"data":[{"sonarrSeriesId":2,"sonarrEpisodeId":2,"path":"/tv/foreign.mkv"}]}),
            ),
        ]);
        integration
            .update(&json!({"bazarr":{"enabled":true,"url":endpoint,"api_key":"fixture-key"}}))
            .unwrap();
        assert!(
            integration
                .bazarr_inventory(&AtomicBool::new(false))
                .unwrap_err()
                .contains("escaped")
        );
        server.join().unwrap();
    }

    #[test]
    fn windows_case_aliases_are_collisions_while_posix_case_is_distinct() {
        let mut rows = Vec::new();
        let mut bytes = 0;
        let mut ids = BTreeSet::new();
        add_row(
            &mut rows,
            &mut bytes,
            &mut ids,
            json!({"kind":"movie","id":1,"path":"C:\\Media\\Film.mkv"}),
        )
        .unwrap();
        assert!(
            add_row(
                &mut rows,
                &mut bytes,
                &mut ids,
                json!({"kind":"movie","id":2,"path":"c:\\media\\film.mkv"})
            )
            .is_err()
        );
        let mut rows = Vec::new();
        let mut bytes = 0;
        let mut ids = BTreeSet::new();
        add_row(
            &mut rows,
            &mut bytes,
            &mut ids,
            json!({"kind":"movie","id":1,"path":"/media/Film.mkv"}),
        )
        .unwrap();
        add_row(
            &mut rows,
            &mut bytes,
            &mut ids,
            json!({"kind":"movie","id":2,"path":"/media/film.mkv"}),
        )
        .unwrap();
    }

    #[test]
    fn selected_authority_survives_missing_credentials() {
        let (_temporary, mut integration) = integration();
        integration.update(&json!({"bazarr":{"enabled":true,"url":"http://127.0.0.1:1","api_key":"fixture-key"}})).unwrap();
        integration
            .update(&json!({"bazarr":{"clear_api_key":true}}))
            .unwrap();
        assert!(integration.bazarr_enabled());
        assert!(
            integration
                .bazarr_inventory(&AtomicBool::new(false))
                .unwrap_err()
                .contains("missing")
        );
        integration
            .update(&json!({"bazarr":{"api_key":"fixture-key"}}))
            .unwrap();
        integration
            .update(&json!({"bazarr":{"url":"http://127.0.0.1:2"}}))
            .unwrap();
        assert!(integration.bazarr_enabled());
        assert!(
            integration
                .bazarr_inventory(&AtomicBool::new(false))
                .unwrap_err()
                .contains("missing")
        );
    }

    #[test]
    fn publication_notifies_only_the_most_specific_series_root() {
        let (_temporary, mut integration) = integration();
        let (endpoint, server) = fixture(vec![
            (
                "GET /api/series?start=0&length=200 ".into(),
                json!({"data":[{"sonarrSeriesId":1,"path":"/tv/Show"},{"sonarrSeriesId":2,"path":"/tv/Show/Extras"}],"total":2}),
            ),
            (
                "GET /api/movies?start=0&length=200 ".into(),
                json!({"data":[],"total":0}),
            ),
            ("PATCH /api/series ".into(), json!({})),
        ]);
        integration
            .update(&json!({"bazarr":{"enabled":true,"url":endpoint,"api_key":"fixture-key"}}))
            .unwrap();
        let triggered = integration
            .bazarr_scan_paths(
                &["/tv/Show/Extras/Episode.en.srt".into()],
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(triggered, 1);
        let requests = server.join().unwrap();
        assert!(
            requests
                .last()
                .unwrap()
                .ends_with("seriesid=2&action=scan-disk")
        );
    }

    #[test]
    fn explicit_bazarr_refresh_reads_inventory_without_starting_tasks_or_searches() {
        let (_temporary, mut integration) = integration();
        let (endpoint, server) = fixture(vec![
            ("GET /api/system/languages/profiles ".into(), json!([])),
            (
                "GET /api/movies?start=0&length=200 ".into(),
                json!({"data":[],"total":0}),
            ),
            (
                "GET /api/series?start=0&length=200 ".into(),
                json!({"data":[],"total":0}),
            ),
        ]);
        integration
            .update(&json!({"bazarr":{"enabled":true,"url":endpoint,"api_key":"fixture-key"}}))
            .unwrap();
        let result = integration.refresh("bazarr", &AtomicBool::new(false), &mut |_| {});
        assert_eq!(result["ok"], true);
        assert_eq!(result["inventory_count"], 0);
        assert!(
            server
                .join()
                .unwrap()
                .iter()
                .all(|request| request.starts_with("GET "))
        );
        assert!(!allowed_route("GET", "/api/system/tasks"));
        assert!(!allowed_route("POST", "/api/system/tasks"));
    }
}
