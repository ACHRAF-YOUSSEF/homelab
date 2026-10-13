//! Local subtitle discovery, inspection, and sidecar extraction.
//!
//! This module intentionally uses only safe Rust.  Paths are constrained to
//! configured roots, conversion tools receive a scrubbed environment, and
//! outputs are staged beside their destination before no-clobber publication.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf, Prefix};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::Builder;
use walkdir::WalkDir;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mks", "mp4", "m4v", "webm", "mov", "avi", "ts", "m2ts",
];
const SIDECAR_EXTENSIONS: &[&str] = &[
    "ass", "ssa", "vtt", "ttml", "smi", "sami", "sbv", "sup", "sub", "idx",
];
const MAX_PROBE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONVERTER_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SRT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TRACKS: usize = 64;
const MAX_TRACK_LABEL: usize = 32;
// Bump only when a converter fix or new diagnostics should retry cached
// failures immediately. Successful fingerprint caches are independent.
const ERROR_CACHE_REVISION: u64 = 1;
const MAX_CANDIDATES: usize = 20_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Track {
    number: Value,
    native_number: Option<String>,
    stream_index: Option<u64>,
    language: String,
    format: String,
    forced: bool,
    hi: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workload {
    All,
    Text,
    Ocr,
}

type ConversionJob = (
    PathBuf,
    String,
    Option<u64>,
    Option<String>,
    bool,
    bool,
    String,
    String,
);

pub struct Converter {
    pub roots: Vec<PathBuf>,
    pub state: Value,
    state_path: PathBuf,
    ffprobe: PathBuf,
    ffmpeg: PathBuf,
    mkvmerge: PathBuf,
    seconv: PathBuf,
    min_age: Duration,
    timeout: Duration,
    retry_interval: Duration,
    enable_ocr: bool,
    ocr_language: String,
    language_aliases: BTreeMap<String, String>,
    metadata: Mutex<crate::metadata::MetadataCache>,
}

impl Converter {
    pub fn new(roots: Vec<PathBuf>, state_path: PathBuf) -> Result<Self, String> {
        let roots = roots.into_iter().map(|p| absolute_lexical(&p)).collect();
        let state = crate::util::read_json(&state_path, MAX_STATE_BYTES as usize)
            .unwrap_or_else(|_| json!({}));
        let state = if state.is_object() { state } else { json!({}) };
        let min_age = env_seconds("CONVERTER_MIN_AGE", 120, 0, 86_400);
        let timeout = env_seconds("CONVERTER_TIMEOUT", 7_200, 1, 86_400);
        let retry_interval = env_seconds("CONVERTER_RETRY_INTERVAL", 3_600, 1, 604_800);
        let enable_ocr =
            std::env::var("CONVERTER_ENABLE_OCR").is_ok_and(|v| v == "true" || v == "1");
        let ocr_language =
            std::env::var("CONVERTER_OCR_LANGUAGE").unwrap_or_else(|_| "eng".to_owned());
        Ok(Self {
            roots,
            state,
            state_path: state_path.clone(),
            ffprobe: env_tool("CONVERTER_FFPROBE", "ffprobe"),
            ffmpeg: env_tool("CONVERTER_FFMPEG", "ffmpeg"),
            mkvmerge: env_tool("CONVERTER_MKVMERGE", "mkvmerge"),
            seconv: env_tool("CONVERTER_SECONV", "seconv"),
            min_age,
            timeout,
            retry_interval,
            enable_ocr,
            ocr_language,
            language_aliases: language_aliases(),
            metadata: Mutex::new(crate::metadata::MetadataCache::new(
                state_path.with_file_name("metadata.json"),
            )),
        })
    }

    pub fn normalize_languages(languages: &Value) -> Result<Vec<String>, String> {
        let list = languages
            .as_array()
            .ok_or("Choose all languages or at least one language code")?;
        if list.is_empty() || list.len() > 64 {
            return Err("Choose all languages or at least one language code".into());
        }
        if list.len() == 1 && list[0].as_str() == Some("*") {
            return Ok(vec!["*".into()]);
        }
        let aliases = language_aliases();
        let mut result = BTreeSet::new();
        for value in list {
            let code = value
                .as_str()
                .ok_or("Invalid subtitle language selection")?
                .to_ascii_lowercase();
            if !valid_language_code(&code) {
                return Err("Invalid subtitle language selection".into());
            }
            result.insert(short_language(&code, &aliases));
        }
        Ok(result.into_iter().collect())
    }

    pub fn candidates(roots: &[PathBuf]) -> Vec<PathBuf> {
        Self::candidates_with_stop(roots, &AtomicBool::new(false))
    }

    pub fn candidates_with_stop(roots: &[PathBuf], stop: &AtomicBool) -> Vec<PathBuf> {
        let mut found = BTreeSet::new();
        for root in roots {
            if root.is_symlink() || !root.is_dir() {
                continue;
            }
            let canonical_root = match root.canonicalize() {
                Ok(p) => normalize_canonical_local_path(p),
                Err(_) => continue,
            };
            for entry in WalkDir::new(root)
                .follow_links(false)
                .max_depth(64)
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.')
                })
                .filter_map(Result::ok)
            {
                if stop.load(Ordering::Relaxed) || found.len() >= MAX_CANDIDATES {
                    return found.into_iter().collect();
                }
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if path.is_symlink() || !has_supported_extension(path) {
                    continue;
                }
                let canonical = match path.canonicalize() {
                    Ok(p) => normalize_canonical_local_path(p),
                    Err(_) => continue,
                };
                if !canonical.starts_with(&canonical_root) {
                    continue;
                }
                if extension(path).as_deref() == Some("idx") && path.with_extension("sub").is_file()
                {
                    continue;
                }
                found.insert(canonical);
            }
        }
        found.into_iter().collect()
    }

    pub fn safe_path(value: &str, roots: &[PathBuf]) -> Result<PathBuf, String> {
        let raw = Path::new(value);
        for component in raw.components() {
            match component {
                Component::ParentDir => return Err("Path is outside configured libraries".into()),
                Component::Prefix(prefix)
                    if !raw.is_absolute() || !matches!(prefix.kind(), Prefix::Disk(_)) =>
                {
                    return Err("Network and device paths are not allowed".into());
                }
                _ => {}
            }
        }
        let candidate = absolute_lexical(raw);
        for root in roots {
            let root = root
                .canonicalize()
                .map_err(|_| "Configured library is unavailable")?;
            let root = normalize_canonical_local_path(root);
            let canonical = candidate
                .canonicalize()
                .map_err(|_| "Media path is unavailable")?;
            let canonical = normalize_canonical_local_path(canonical);
            if canonical.starts_with(root) && !has_symlink_component(&candidate)? {
                return Ok(canonical);
            }
        }
        Err("Path is outside configured libraries".into())
    }

    pub fn describe(&self, path: &Path) -> Result<Value, String> {
        self.describe_with_stop(path, &AtomicBool::new(false))
    }

    pub fn describe_with_stop(&self, path: &Path, stop: &AtomicBool) -> Result<Value, String> {
        let path = Self::safe_path(&path.to_string_lossy(), &self.roots)?;
        if is_video(&path) {
            return Ok(Value::Array(
                self.tracks(&path, stop)?
                    .into_iter()
                    .map(track_json)
                    .collect(),
            ));
        }
        if matches!(extension(&path).as_deref(), Some("idx" | "sub")) {
            let idx = if extension(&path).as_deref() == Some("idx") {
                path.clone()
            } else {
                path.with_extension("idx")
            };
            if idx.exists() {
                if idx.is_symlink() {
                    return Err("Subtitle index is a symbolic link".into());
                }
                let text = read_index_text(&idx)?;
                let id_re = Regex::new(r"(?mi)^id:\s*([a-zA-Z-]+),\s*index:\s*(\d+).*$")
                    .map_err(|_| "Invalid subtitle parser")?;
                let captures: Vec<_> = id_re.captures_iter(&text).take(MAX_TRACKS + 1).collect();
                if captures.len() > MAX_TRACKS {
                    return Err("VobSub contains too many subtitle tracks to inspect safely".into());
                }
                return Ok(Value::Array(
                    captures
                        .iter()
                        .map(|c| {
                            json!({
                                "number": c[2].parse::<u64>().unwrap_or(0),
                                "language": short_language(&bounded_track_language(&c[1]), &self.language_aliases),
                                "forced": false, "hi": false, "format": "VobSub"
                            })
                        })
                        .collect(),
                ));
            }
        }
        Ok(
            json!([{"number": null, "language": filename_language(&path, &self.language_aliases),
            "forced": path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().contains(".forced"),
            "hi": path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().contains(".hi"),
            "format": extension(&path).unwrap_or_default().to_ascii_uppercase()}]),
        )
    }

    pub fn scan(
        &mut self,
        paths: &[PathBuf],
        languages: &[String],
        retry: bool,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(Value),
    ) -> Result<Value, String> {
        self.scan_workload(
            paths,
            languages,
            retry,
            Workload::All,
            false,
            stop,
            progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn scan_workload(
        &mut self,
        paths: &[PathBuf],
        languages: &[String],
        retry: bool,
        workload: Workload,
        automatic: bool,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(Value),
    ) -> Result<Value, String> {
        if workload == Workload::Ocr && !self.enable_ocr {
            return Err("OCR is disabled by deployment permissions".into());
        }
        let languages = if languages.is_empty() {
            vec!["*".to_owned()]
        } else {
            languages.to_vec()
        };
        // The caller supplies the authorized paths. An empty selection must
        // remain empty; silently expanding it to every configured root would
        // turn a cleared root selection into a whole-library scan.
        let paths = paths.to_vec();
        let mut summary = json!({"converted":0,"cached":0,"pending":0,"failed":0,"planned":0});
        for raw_path in paths {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let path = match Self::safe_path(&raw_path.to_string_lossy(), &self.roots) {
                Ok(path) => path,
                Err(_) => {
                    bump(&mut summary, "failed");
                    continue;
                }
            };
            progress(json!({"status":"processing","path":path,"summary":summary}));
            let before = match fingerprint(&path) {
                Ok(v) => v,
                Err(_) => {
                    bump(&mut summary, "failed");
                    continue;
                }
            };
            if before.iter().any(|(size, modified)| {
                *size == 0 || modified.elapsed().is_ok_and(|age| age < self.min_age)
            }) {
                bump(&mut summary, "pending");
                progress(json!({"status":"pending","path":path,"summary":summary}));
                continue;
            }
            let key = path.to_string_lossy().to_string();
            let previous = self.state.get(&key).cloned().unwrap_or_else(|| json!({}));
            let previous_outputs = safe_outputs(&previous);
            if previous.get("fingerprint") == Some(&fingerprint_json(&before)) {
                let covered = previous
                    .get("languages")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let coverage = covered.iter().any(|v| v.as_str() == Some("*"))
                    || languages
                        .iter()
                        .all(|lang| covered.iter().any(|v| v.as_str() == Some(lang)));
                // Legacy state entries predate this flag and represent prior
                // successful OCR runs. New OCR-disabled entries explicitly
                // record false so enabling OCR schedules image tracks again.
                let ocr_covered =
                    workload == Workload::Text || ocr_coverage(&previous, self.enable_ocr);
                let text_covered = workload == Workload::Ocr
                    || previous["text_complete"].as_bool().unwrap_or(true);
                let outputs_exist = previous_outputs
                    .iter()
                    .all(|name| path.parent().unwrap_or(Path::new("/")).join(name).is_file());
                if previous.get("ok").and_then(Value::as_bool) == Some(true)
                    && coverage
                    && ocr_covered
                    && text_covered
                    && outputs_exist
                {
                    bump(&mut summary, "cached");
                    progress(json!({"status":"cached","path":path,"summary":summary}));
                    continue;
                }
                if unchanged_failure_deferred(&previous, retry, workload, automatic)
                    || failure_retry_delayed(&previous, retry, epoch_seconds(), self.retry_interval)
                {
                    bump(&mut summary, "pending");
                    progress(json!({"status":"pending","path":path,"summary":summary}));
                    continue;
                }
            }
            let existing = sibling_srt_names(&path);
            match self.convert(&path, &languages, workload, stop, progress) {
                Ok((outputs, ocr_pending)) => {
                    let after = match fingerprint(&path) {
                        Ok(v) => v,
                        Err(_) => {
                            bump(&mut summary, "failed");
                            continue;
                        }
                    };
                    if before != after || stop.load(Ordering::Relaxed) {
                        bump(&mut summary, "failed");
                        continue;
                    }
                    let mut output_names: BTreeSet<String> = outputs.into_iter().collect();
                    let mut completed_languages: BTreeSet<String> =
                        languages.iter().cloned().collect();
                    if previous.get("fingerprint") == Some(&fingerprint_json(&before)) {
                        output_names.extend(previous_outputs);
                        completed_languages.extend(
                            previous
                                .get("languages")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned),
                        );
                    }
                    let unchanged = previous.get("fingerprint") == Some(&fingerprint_json(&before));
                    let text_complete = workload != Workload::Ocr
                        || (unchanged
                            && previous["text_complete"]
                                .as_bool()
                                .unwrap_or(previous["ok"] == true));
                    let ocr_complete = (workload != Workload::Text && self.enable_ocr)
                        || (unchanged
                            && previous["ocr"].as_bool().unwrap_or(previous["ok"] == true));
                    self.set_state_entry(&key, json!({"fingerprint":fingerprint_json(&before),"outputs":output_names,"ok":true,"languages":completed_languages,"ocr":ocr_complete,"ocr_pending":ocr_pending && !ocr_complete,"text_complete":text_complete}));
                    if crate::util::write_private(&self.state_path, &self.state).is_err() {
                        bump(&mut summary, "failed");
                        continue;
                    }
                    bump(&mut summary, "converted");
                    let new_outputs: Vec<String> = sibling_srt_names(&path)
                        .difference(&existing)
                        .map(|n| {
                            path.parent()
                                .unwrap_or(Path::new("/"))
                                .join(n)
                                .to_string_lossy()
                                .to_string()
                        })
                        .collect();
                    progress(
                        json!({"status":"converted","path":path,"outputs":new_outputs,"new_outputs":new_outputs,"summary":summary}),
                    );
                }
                Err(message) => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    bump(&mut summary, "failed");
                    let safe = bounded_message(&message);
                    progress(json!({"status":"failed","path":path,"error":safe,"summary":summary}));
                    self.set_state_entry(&key, json!({"fingerprint":fingerprint_json(&before),"ok":false,"attempt":epoch_seconds(),"error":safe,"error_revision":ERROR_CACHE_REVISION,"ocr_pending":workload==Workload::Ocr || previous["ocr_pending"]==true,"text_complete":previous["text_complete"].as_bool().unwrap_or(previous["ok"]==true),"outputs":previous_outputs}));
                    let _ = crate::util::write_private(&self.state_path, &self.state);
                }
            }
        }
        self.persist_metadata()?;
        Ok(summary)
    }

    pub fn persist_metadata(&self) -> Result<(), String> {
        self.metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .persist()
    }

    pub fn cached_description(&self, path: &Path) -> Option<Value> {
        if !is_video(path) {
            return self.describe(path).ok();
        }
        let fingerprint = fingerprint_json(&fingerprint(path).ok()?);
        let cached = self
            .metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&path.to_string_lossy(), &fingerprint)?;
        let tracks: Vec<Track> = serde_json::from_value(cached).ok()?;
        Some(Value::Array(tracks.into_iter().map(track_json).collect()))
    }

    pub fn ocr_allowed(&self) -> bool {
        self.enable_ocr
    }

    pub fn ocr_candidates(&self, roots: &[PathBuf], retry: bool) -> Vec<PathBuf> {
        self.state
            .as_object()
            .into_iter()
            .flat_map(|entries| entries.iter())
            .filter(|(_, entry)| entry["ocr_pending"] == true && (entry["ok"] == true || retry))
            .filter_map(|(path, _)| Self::safe_path(path, roots).ok())
            .collect()
    }

    pub fn compact_cache(&mut self) -> Result<Value, String> {
        let roots = self.roots.clone();
        let mut removed = 0;
        if let Some(entries) = self.state.as_object_mut() {
            entries.retain(|path, _| {
                let keep = Path::new(path).exists() && Self::safe_path(path, &roots).is_ok();
                if !keep {
                    removed += 1;
                }
                keep
            });
        }
        let metadata_removed = self
            .metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .compact(|path| Path::new(path).exists() && Self::safe_path(path, &roots).is_ok());
        crate::util::write_private(&self.state_path, &self.state)?;
        self.persist_metadata()?;
        Ok(json!({"state_entries_removed":removed,"metadata_entries_removed":metadata_removed}))
    }

    fn set_state_entry(&mut self, key: &str, value: Value) {
        if let Some(map) = self.state.as_object_mut() {
            map.insert(key.to_owned(), value);
        }
    }

    fn tracks(&self, path: &Path, stop: &AtomicBool) -> Result<Vec<Track>, String> {
        let before = fingerprint_json(&fingerprint(path)?);
        let key = path.to_string_lossy();
        if let Some(cached) = self
            .metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key, &before)
            && let Ok(tracks) = serde_json::from_value(cached)
        {
            return Ok(tracks);
        }
        let native_data = if matches!(extension(path).as_deref(), Some("mkv" | "mks" | "webm")) {
            let mut command = Command::new(&self.mkvmerge);
            command.args(["-J"]).arg(path);
            // mkvmerge uses exit status 1 when it emitted useful track JSON
            // alongside warnings (for example a damaged subtitle track).
            // Trust only parseable, structurally valid JSON; ffprobe remains
            // the fallback if mkvmerge cannot provide it.
            let (_success, bytes) = crate::util::bounded_process_allow_failure(
                &mut command,
                Duration::from_secs(120),
                stop,
                MAX_PROBE_BYTES,
            )?;
            parse_mkvmerge_metadata(&bytes)
        } else {
            None
        };
        // ffprobe stream indices are what FFmpeg accepts in `-map`. Matroska
        // track IDs come from mkvmerge, so gather both tables and join by the
        // subtitle stream ordinal instead of confusing the two numbering schemes.
        let mut probe = Command::new(&self.ffprobe);
        probe
            .args([
                "-protocol_whitelist",
                "file,pipe",
                "-v",
                "error",
                "-select_streams",
                "s",
                "-show_streams",
                "-of",
                "json",
            ])
            .arg(path);
        let probe_bytes = crate::util::bounded_process(
            &mut probe,
            Duration::from_secs(120),
            stop,
            MAX_PROBE_BYTES,
        )?;
        let data: Value = serde_json::from_slice(&probe_bytes)
            .map_err(|_| "Media stream metadata could not be parsed")?;
        let streams = data
            .get("streams")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if streams.len() > MAX_TRACKS {
            return Err("Media contains too many subtitle tracks to inspect safely".into());
        }
        let native = matching_native_tracks(native_data.as_ref(), streams.len());
        let mut result = Vec::new();
        for (ordinal, stream) in streams.iter().enumerate() {
            let tags = stream.get("tags").cloned().unwrap_or_else(|| json!({}));
            let disposition = stream
                .get("disposition")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let native_track = native.get(ordinal);
            let props = native_track.and_then(|t| t.get("properties"));
            let container_number = props.and_then(|p| p.get("number")).and_then(Value::as_u64);
            let ffmpeg_id = if extension(path).as_deref() == Some("avi") {
                stream.get("index").and_then(Value::as_u64)
            } else {
                numeric_stream_id(stream)
            };
            let native_number = container_number.or(ffmpeg_id).map(|n| n.to_string());
            let number = json!(container_number.or(ffmpeg_id).unwrap_or(ordinal as u64));
            let language = props
                .and_then(|p| p.get("language_ietf"))
                .and_then(Value::as_str)
                .or_else(|| {
                    props
                        .and_then(|p| p.get("language"))
                        .and_then(Value::as_str)
                })
                .or_else(|| tags.get("language").and_then(Value::as_str))
                .unwrap_or("und");
            let language = bounded_track_language(language);
            let format = native_track
                .and_then(|t| t.get("codec"))
                .and_then(Value::as_str)
                .or_else(|| stream.get("codec_name").and_then(Value::as_str))
                .unwrap_or("Unknown");
            let format = bounded_track_label(format);
            result.push(Track {
                number,
                native_number,
                stream_index: stream.get("index").and_then(Value::as_u64),
                language,
                format,
                forced: props
                    .and_then(|p| p.get("forced_track"))
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| {
                        disposition
                            .get("forced")
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                            != 0
                    }),
                hi: props
                    .and_then(|p| p.get("hearing_impaired"))
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| {
                        disposition
                            .get("hearing_impaired")
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                            != 0
                    }),
            });
        }
        if !stop.load(Ordering::Relaxed) && fingerprint_json(&fingerprint(path)?) == before {
            self.metadata
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(&key, before, json!(result));
        }
        Ok(result)
    }

    fn convert(
        &self,
        path: &Path,
        languages: &[String],
        workload: Workload,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(Value),
    ) -> Result<(Vec<String>, bool), String> {
        let before = fingerprint(path)?;
        let parent = path.parent().ok_or("Media path has no parent directory")?;
        let staging = Builder::new()
            .prefix(".subforge-")
            .tempdir_in(parent)
            .map_err(|_| "Could not create a private staging directory")?;
        let tracks = if is_video(path) {
            self.tracks(path, stop)?
        } else {
            Vec::new()
        };
        let mut counts: BTreeMap<(String, bool, bool), usize> = BTreeMap::new();
        for track in &tracks {
            *counts
                .entry((
                    short_language(&track.language, &self.language_aliases),
                    track.forced,
                    track.hi,
                ))
                .or_default() += 1;
        }
        let mut jobs: Vec<ConversionJob> = Vec::new();
        let mut ocr_pending = false;
        if is_video(path) {
            for track in tracks {
                let lang = short_language(&track.language, &self.language_aliases);
                if !wants(&track.language, languages, &self.language_aliases) {
                    continue;
                }
                let mut stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("subtitle")
                    .to_owned();
                if counts
                    .get(&(lang.clone(), track.forced, track.hi))
                    .copied()
                    .unwrap_or(0)
                    > 1
                    || lang == "und"
                {
                    stem.push_str(&format!(
                        ".track{}",
                        track
                            .number
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| track.number.to_string())
                    ));
                }
                stem.push('.');
                stem.push_str(&lang);
                if track.forced {
                    stem.push_str(".forced");
                }
                if track.hi {
                    stem.push_str(".hi");
                }
                let image = is_image_format(&track.format);
                let ocr_lang = self.ocr_for(&track.language);
                if image && (workload == Workload::Text || !self.enable_ocr) {
                    ocr_pending = true;
                    continue;
                }
                if !image && workload == Workload::Ocr {
                    continue;
                }
                let stream_index = track
                    .stream_index
                    .ok_or("Subtitle stream index is unavailable")?;
                jobs.push((
                    path.to_path_buf(),
                    format!("{stem}.srt"),
                    Some(stream_index),
                    track.native_number,
                    image,
                    false,
                    ocr_lang,
                    track.language,
                ));
            }
        } else if matches!(extension(path).as_deref(), Some("idx" | "sub")) {
            let idx = if extension(path).as_deref() == Some("idx") {
                path.to_path_buf()
            } else {
                path.with_extension("idx")
            };
            if idx.exists() {
                let text = read_index_text(&idx)?;
                let re = Regex::new(r"(?mi)^id:\s*([a-zA-Z-]+),\s*index:\s*\d+.*$")
                    .map_err(|_| "Invalid subtitle parser")?;
                let caps: Vec<_> = re.captures_iter(&text).take(MAX_TRACKS + 1).collect();
                if caps.len() > MAX_TRACKS {
                    return Err("VobSub contains too many subtitle tracks to convert safely".into());
                }
                let header = Regex::new(r"(?mi)^langidx:.*$").unwrap();
                let body_re = Regex::new(r"(?mi)^(id:.*index:)\s*\d+").unwrap();
                for (n, c) in caps.iter().enumerate() {
                    let lang = bounded_track_language(&c[1]);
                    if !wants(&lang, languages, &self.language_aliases) {
                        continue;
                    }
                    if workload == Workload::Text || !self.enable_ocr {
                        ocr_pending = true;
                        continue;
                    }
                    let label = short_language(&lang, &self.language_aliases);
                    let mut stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("subtitle")
                        .to_owned();
                    if filename_language(path, &self.language_aliases) != label {
                        stem.push_str(&format!(".{label}"));
                    }
                    let staged_idx = staging.path().join(format!("vobsub{n}.idx"));
                    let payload = idx.with_extension("sub");
                    if payload.is_symlink() || !payload.is_file() {
                        return Err("VobSub payload is missing or unsafe".into());
                    }
                    fs::copy(&payload, staged_idx.with_extension("sub"))
                        .map_err(|_| "VobSub staging failed")?;
                    let section_start = c.get(0).map(|m| m.start()).unwrap_or(0);
                    let section_end = caps
                        .get(n + 1)
                        .and_then(|x| x.get(0))
                        .map(|m| m.start())
                        .unwrap_or(text.len());
                    let header_end = caps
                        .first()
                        .and_then(|x| x.get(0))
                        .map(|m| m.start())
                        .unwrap_or(0);
                    let prefix = header.replace_all(&text[..header_end], "langidx: 0");
                    let body = body_re.replace(&text[section_start..section_end], "$1 0");
                    fs::write(&staged_idx, format!("{prefix}{body}"))
                        .map_err(|_| "VobSub staging failed")?;
                    jobs.push((
                        staged_idx,
                        format!("{stem}.srt"),
                        None,
                        None,
                        true,
                        false,
                        self.ocr_for(&lang),
                        lang,
                    ));
                }
            }
        } else if wants(
            &filename_language(path, &self.language_aliases),
            languages,
            &self.language_aliases,
        ) {
            let fmt = extension(path).unwrap_or_default();
            let image = matches!(fmt.as_str(), "sup" | "sub" | "idx");
            let seconv = matches!(fmt.as_str(), "ttml" | "sbv");
            if image && (workload == Workload::Text || !self.enable_ocr) {
                ocr_pending = true;
            }
            if (image && self.enable_ocr && workload != Workload::Text)
                || (!image && workload != Workload::Ocr)
            {
                jobs.push((
                    path.to_path_buf(),
                    format!(
                        "{}.srt",
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("subtitle")
                    ),
                    None,
                    None,
                    image,
                    seconv,
                    self.ocr_for(&filename_language(path, &self.language_aliases)),
                    filename_language(path, &self.language_aliases),
                ));
            }
        }
        let mut outputs = Vec::new();
        let mut destinations = HashSet::new();
        for (source, name, stream, native_track, image, seconv, ocr_lang, language) in jobs {
            if !destinations.insert(name.clone()) {
                continue;
            }
            let destination = parent.join(&name);
            if destination.exists() || destination.is_symlink() {
                continue;
            }
            progress(
                json!({"status":"extracting","path":path,"output":name,"track":stream,"language":ocr_lang}),
            );
            let staged = staging.path().join(&name);
            if image || seconv {
                let mut command = Command::new(&self.seconv);
                command
                    .arg(&source)
                    .args(["subrip", "--output-folder"])
                    .arg(staging.path())
                    .args([
                        "--output-filename",
                        &name,
                        "--overwrite",
                        "--encoding",
                        "utf-8-no-bom",
                        "--ocr-engine",
                        "tesseract",
                        "--ocr-language",
                        &ocr_lang,
                        "--json",
                    ]);
                if let Some(track_number) = native_track.as_deref() {
                    command.args(["--track-number", track_number]);
                }
                let (_exit_success, report) = crate::util::bounded_process_allow_failure(
                    &mut command,
                    self.timeout,
                    stop,
                    MAX_CONVERTER_OUTPUT,
                )?;
                let report: Value = serde_json::from_slice(&report)
                    .map_err(|_| "OCR converter returned an unreadable report")?;
                if report.get("success").and_then(Value::as_bool) != Some(true) {
                    return Err(classify_ocr_failure(&report));
                }
            } else {
                let mut command = Command::new(&self.ffmpeg);
                command
                    .args([
                        "-nostdin",
                        "-v",
                        "error",
                        "-protocol_whitelist",
                        "file,pipe",
                        "-threads",
                        "1",
                        "-i",
                    ])
                    .arg(&source);
                if let Some(index) = stream {
                    command.args(["-map", &format!("0:{index}")]);
                } else {
                    command.args(["-map", "0:s:0"]);
                }
                command
                    .args(["-c:s", "srt", "-threads", "1", "-f", "srt"])
                    .arg(&staged);
                crate::util::bounded_process(
                    &mut command,
                    self.timeout,
                    stop,
                    MAX_CONVERTER_OUTPUT,
                )?;
            }
            if staged.is_symlink() {
                return Err("Converter created an unsafe symbolic-link output".into());
            }
            remove_empty_cues(&staged)
                .and_then(|_| validate_srt(&staged))
                .map_err(|message| {
                    conversion_error_context(&message, native_track.as_deref(), stream, &language)
                })?;
            outputs.push(name);
        }
        if fingerprint(path)? != before || stop.load(Ordering::Relaxed) {
            return Err("Source changed or conversion was cancelled".into());
        }
        for name in &outputs {
            let staged = staging.path().join(name);
            let destination = parent.join(name);
            publish_noclobber(&staged, &destination)?;
        }
        Ok((outputs, ocr_pending))
    }

    fn ocr_for(&self, language: &str) -> String {
        let code = short_language(language, &self.language_aliases);
        if code == "und" {
            return self.ocr_language.clone();
        }
        if code == "zh" {
            if language.to_ascii_lowercase().contains("hant") {
                "chi_tra".into()
            } else {
                "chi_sim".into()
            }
        } else {
            self.language_aliases.get(&code).cloned().unwrap_or(code)
        }
    }
}

fn env_tool(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}
fn env_seconds(key: &str, default: u64, min: u64, max: u64) -> Duration {
    let value = std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v >= min && *v <= max)
        .unwrap_or(default);
    Duration::from_secs(value)
}
fn absolute_lexical(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    }
}

/// Normalize only an internally canonicalized local drive path. User-supplied
/// UNC and device paths are still rejected before canonicalization.
pub fn normalize_canonical_local_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next()
            && let Prefix::VerbatimDisk(drive) = prefix.kind()
            && components.next() == Some(Component::RootDir)
        {
            let mut normalized = PathBuf::from(format!("{}:\\", char::from(drive)));
            normalized.push(components.as_path());
            return normalized;
        }
    }
    path
}
fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
}
fn is_video(path: &Path) -> bool {
    extension(path).is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.as_str()))
}
fn has_supported_extension(path: &Path) -> bool {
    extension(path).is_some_and(|e| {
        VIDEO_EXTENSIONS.contains(&e.as_str()) || SIDECAR_EXTENSIONS.contains(&e.as_str())
    })
}
fn valid_language_code(code: &str) -> bool {
    let mut parts = code.split('-');
    let first = parts.next().unwrap_or("");
    (2..=3).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}
fn language_aliases() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for (a, b) in [
        ("en", "eng"),
        ("fr", "fra"),
        ("fre", "fra"),
        ("ar", "ara"),
        ("ja", "jpn"),
        ("de", "deu"),
        ("ger", "deu"),
        ("es", "spa"),
        ("zh", "zho"),
        ("chi", "zho"),
        ("und", "und"),
    ] {
        m.insert(a.into(), b.into());
    }
    // Debian's iso-codes package supplies the full ISO-639-2 table in the
    // runtime image. Load it only when it is small enough to keep startup
    // memory bounded; the explicit common aliases above remain the fallback.
    let table = Path::new("/usr/share/iso-codes/json/iso_639-2.json");
    if fs::metadata(table).is_ok_and(|meta| meta.len() <= 4 * 1024 * 1024)
        && let Ok(bytes) = fs::read(table)
        && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        && let Some(entries) = value.get("639-2").and_then(Value::as_array)
    {
        for item in entries {
            let Some(alpha3) = item.get("alpha_3").and_then(Value::as_str) else {
                continue;
            };
            m.insert(alpha3.to_owned(), alpha3.to_owned());
            for key in ["alpha_2", "bibliographic"] {
                if let Some(code) = item.get(key).and_then(Value::as_str) {
                    m.insert(code.to_owned(), alpha3.to_owned());
                }
            }
        }
    }
    m
}
fn short_language(language: &str, aliases: &BTreeMap<String, String>) -> String {
    let normalized = language.to_ascii_lowercase();
    let primary = normalized.split('-').next().unwrap_or("und");
    if !valid_language_code(&normalized) {
        return "und".into();
    }
    let canonical = aliases
        .get(&normalized)
        .or_else(|| aliases.get(primary))
        .map(String::as_str)
        .unwrap_or(primary);
    aliases
        .iter()
        .find(|(k, v)| k.len() == 2 && v.as_str() == canonical)
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| canonical.to_owned())
}
fn bounded_track_label(value: &str) -> String {
    let label: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(MAX_TRACK_LABEL)
        .collect();
    if label.is_empty() {
        "Unknown".into()
    } else {
        label
    }
}
fn bounded_track_language(value: &str) -> String {
    let normalized = value.to_ascii_lowercase();
    if normalized.len() <= 35 && valid_language_code(&normalized) {
        normalized
    } else {
        "und".into()
    }
}
fn parse_mkvmerge_metadata(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .filter(|data| data.get("tracks").and_then(Value::as_array).is_some())
}
fn matching_native_tracks(native_data: Option<&Value>, stream_count: usize) -> Vec<Value> {
    let native: Vec<Value> = native_data
        .and_then(|data| data.get("tracks"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| t.get("type").and_then(Value::as_str) == Some("subtitles"))
        .take(MAX_TRACKS + 1)
        .cloned()
        .collect();
    if native.len() == stream_count && native.len() <= MAX_TRACKS {
        native
    } else {
        Vec::new()
    }
}
fn filename_language(path: &Path, aliases: &BTreeMap<String, String>) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut parts: Vec<_> = stem
        .to_ascii_lowercase()
        .split('.')
        .map(str::to_owned)
        .collect();
    while parts
        .last()
        .is_some_and(|s| ["forced", "hi", "sdh", "cc"].contains(&s.as_str()))
    {
        parts.pop();
    }
    if parts.len() > 1 {
        short_language(parts.last().unwrap(), aliases)
    } else {
        "und".into()
    }
}
fn wants(language: &str, selected: &[String], aliases: &BTreeMap<String, String>) -> bool {
    selected.iter().any(|v| {
        v == "*"
            || short_language(language, aliases) == *v
            || (!v.contains('-')
                && short_language(language, aliases).split('-').next() == Some(v.as_str()))
    })
}
fn is_image_format(format: &str) -> bool {
    let f = format.to_ascii_lowercase();
    [
        "pgs",
        "hdmv_pgs_subtitle",
        "dvd_subtitle",
        "vobsub",
        "dvb_subtitle",
        "xsub",
        "sup",
    ]
    .iter()
    .any(|v| f.contains(v))
}
fn track_json(t: Track) -> Value {
    json!({"number":t.number,"language":t.language,"format":t.format,"forced":t.forced,"hi":t.hi})
}

fn numeric_stream_id(stream: &Value) -> Option<u64> {
    stream.get("id").and_then(|value| {
        value.as_u64().or_else(|| {
            let text = value.as_str()?;
            if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16).ok()
            } else {
                text.parse::<u64>().ok()
            }
        })
    })
}
fn has_symlink_component(path: &Path) -> Result<bool, String> {
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        if fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn fingerprint(path: &Path) -> Result<Vec<(u64, SystemTime)>, String> {
    if has_symlink_component(path)? {
        return Err("Subtitle source became a symbolic link".into());
    }
    let mut paths = vec![path.to_path_buf()];
    if matches!(extension(path).as_deref(), Some("sub" | "idx")) {
        let companion = if extension(path).as_deref() == Some("sub") {
            path.with_extension("idx")
        } else {
            path.with_extension("sub")
        };
        if companion.exists() {
            if companion.is_symlink() {
                return Err("VobSub companion is a symbolic link".into());
            }
            paths.push(companion)
        }
    }
    paths
        .into_iter()
        .map(|p| {
            let m =
                fs::symlink_metadata(p).map_err(|_| "Subtitle source could not be inspected")?;
            if m.file_type().is_symlink() || !m.is_file() {
                return Err("Subtitle source became a symbolic link".into());
            }
            Ok((m.len(), m.modified().unwrap_or(UNIX_EPOCH)))
        })
        .collect()
}

fn read_index_text(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|_| "Subtitle index could not be read")?;
    let mut bounded = file.take(4 * 1024 * 1024 + 1);
    let mut text = String::new();
    bounded
        .read_to_string(&mut text)
        .map_err(|_| "Subtitle index is invalid or could not be read")?;
    if text.len() > 4 * 1024 * 1024 {
        return Err("Subtitle index exceeds the safe size limit".into());
    }
    Ok(text)
}
fn fingerprint_json(v: &[(u64, SystemTime)]) -> Value {
    Value::Array(
        v.iter()
            .map(|(n, t)| {
                json!([
                    n,
                    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
                ])
            })
            .collect(),
    )
}
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn safe_outputs(previous: &Value) -> Vec<String> {
    previous
        .get("outputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| Path::new(s).file_name().and_then(|v| v.to_str()) == Some(*s))
        .map(str::to_owned)
        .collect()
}

fn ocr_coverage(previous: &Value, enabled: bool) -> bool {
    !enabled || previous.get("ocr").and_then(Value::as_bool).unwrap_or(true)
}
fn sibling_srt_names(path: &Path) -> BTreeSet<String> {
    fs::read_dir(path.parent().unwrap_or(Path::new("/")))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let p = e.path();
            (extension(&p).as_deref() == Some("srt"))
                .then(|| p.file_name()?.to_str().map(str::to_owned))
                .flatten()
        })
        .collect()
}
fn bump(summary: &mut Value, key: &str) {
    if let Some(entry) = summary.as_object_mut().and_then(|map| map.get_mut(key)) {
        let next = entry.as_u64().unwrap_or(0).saturating_add(1);
        *entry = json!(next);
    }
}
fn bounded_message(s: &str) -> String {
    s.chars().take(240).collect()
}

fn failure_retry_delayed(previous: &Value, retry: bool, now: f64, interval: Duration) -> bool {
    !retry
        && previous.get("ok").and_then(Value::as_bool) != Some(true)
        && previous.get("error_revision").and_then(Value::as_u64) == Some(ERROR_CACHE_REVISION)
        && now
            - previous
                .get("attempt")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
            < interval.as_secs_f64()
}

fn unchanged_failure_deferred(
    previous: &Value,
    retry: bool,
    workload: Workload,
    automatic: bool,
) -> bool {
    !retry && previous["ok"] != true && (automatic || workload == Workload::Ocr)
}

fn conversion_error_context(
    message: &str,
    native_track: Option<&str>,
    stream: Option<u64>,
    language: &str,
) -> String {
    let track = native_track
        .and_then(|value| value.parse::<u64>().ok())
        .map(|number| format!("Track {number}"))
        .or_else(|| stream.map(|number| format!("Stream {number}")))
        .unwrap_or_else(|| "Sidecar".into());
    bounded_message(&format!(
        "{track}, language {}: {message}",
        bounded_track_language(language)
    ))
}

fn publish_noclobber(staged: &Path, destination: &Path) -> Result<bool, String> {
    if destination.exists() || destination.is_symlink() {
        return Ok(false);
    }
    match fs::hard_link(staged, destination) {
        Ok(()) => {
            let _ = fs::remove_file(staged);
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(_) => Err("Could not publish SRT without replacing an existing file".into()),
    }
}
fn classify_ocr_failure(report: &Value) -> String {
    let mut messages = report
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(files) = report.get("files").and_then(Value::as_array) {
        messages.extend(
            files
                .iter()
                .filter_map(|file| file.get("error").and_then(Value::as_str).map(str::to_owned)),
        );
    }
    let text = messages.join(" ").to_ascii_lowercase();
    if text.contains("tesseract") || text.contains("traineddata") || text.contains("ocr") {
        "OCR failed. Check that the selected language data is installed and supported.".into()
    } else {
        "OCR converter could not convert this track. Check the image subtitle format and logs."
            .into()
    }
}
fn validate_srt(path: &Path) -> Result<(), String> {
    if fs::metadata(path)
        .map_err(|_| "Subtitle output could not be read")?
        .len()
        > MAX_SRT_BYTES
    {
        return Err("Subtitle output exceeds the safe size limit".into());
    }
    let content = fs::read(path).map_err(|_| "Subtitle output could not be read")?;
    let text = String::from_utf8(content).map_err(|_| "Subtitle output is not valid UTF-8")?;
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Err("Converter produced an empty subtitle file".into());
    }
    let timing = Regex::new(r"^\d{2,}:\d{2}:\d{2},\d{3} --> \d{2,}:\d{2}:\d{2},\d{3}(?: .*)?$")
        .map_err(|_| "Invalid subtitle validator")?;
    for (ordinal, block) in Regex::new(r"\r?\n[ \t]*\r?\n(?:[ \t]*\r?\n)*")
        .unwrap()
        .split(text)
        .enumerate()
    {
        let cue = ordinal + 1;
        let mut lines = block.lines();
        if lines.next().unwrap_or("").trim().parse::<u64>().is_err() {
            return Err(format!(
                "Converter produced malformed SRT cue {cue}: invalid cue number"
            ));
        }
        let timing_line = lines.next().ok_or_else(|| {
            format!("Converter produced malformed SRT cue {cue}: missing timing line")
        })?;
        if !timing.is_match(timing_line.trim()) {
            return Err(format!(
                "Converter produced malformed SRT cue {cue}: invalid timing line"
            ));
        }
        if !lines.any(|line| !line.trim().is_empty()) {
            return Err(format!(
                "Converter produced empty SRT cue {cue}: missing text"
            ));
        }
    }
    Ok(())
}
fn canonical_srt_timing(value: &str) -> Option<String> {
    static TIMING: OnceLock<Regex> = OnceLock::new();
    let re = TIMING.get_or_init(|| {
        Regex::new(r"^(\d{1,}):(\d{2}):(\d{2})[,.](\d{3})\s*-->\s*(\d{1,}):(\d{2}):(\d{2})[,.](\d{3})(?:\s+(.*))?$")
            .expect("static SRT timing expression is valid")
    });
    let captures = re.captures(value.trim())?;
    let start_hour = captures[1].parse::<u64>().ok()?;
    let start_minute = captures[2].parse::<u8>().ok()?;
    let start_second = captures[3].parse::<u8>().ok()?;
    let end_hour = captures[5].parse::<u64>().ok()?;
    let end_minute = captures[6].parse::<u8>().ok()?;
    let end_second = captures[7].parse::<u8>().ok()?;
    if start_minute >= 60 || start_second >= 60 || end_minute >= 60 || end_second >= 60 {
        return None;
    }
    let mut timing = format!(
        "{start_hour:02}:{start_minute:02}:{start_second:02},{} --> {end_hour:02}:{end_minute:02}:{end_second:02},{}",
        &captures[4], &captures[8]
    );
    if let Some(settings) = captures.get(9) {
        timing.push(' ');
        timing.push_str(settings.as_str().trim());
    }
    Some(timing)
}
fn remove_empty_cues(path: &Path) -> Result<usize, String> {
    if fs::metadata(path)
        .map_err(|_| "Subtitle output could not be read")?
        .len()
        > MAX_SRT_BYTES
    {
        return Err("Subtitle output exceeds the safe size limit".into());
    }
    let bytes = fs::read(path).map_err(|_| "Subtitle output could not be read")?;
    let text = String::from_utf8(bytes).map_err(|_| "Subtitle output is not valid UTF-8")?;
    let bom = if text.starts_with('\u{feff}') {
        "\u{feff}"
    } else {
        ""
    };
    let timing =
        Regex::new(r"^\d{2,}:\d{2}:\d{2},\d{3} --> \d{2,}:\d{2}:\d{2},\d{3}(?: .*)?$").unwrap();
    let blocks = Regex::new(r"\r?\n[ \t]*\r?\n(?:[ \t]*\r?\n)*").unwrap();
    let mut kept = Vec::new();
    let mut removed = 0usize;
    let mut modified = false;
    for block in blocks.split(text.trim_start_matches('\u{feff}').trim()) {
        let mut lines: Vec<String> = block.lines().map(str::to_owned).collect();
        if let Some(timing) = lines.get_mut(1)
            && let Some(canonical) = canonical_srt_timing(timing)
        {
            modified |= *timing != canonical;
            *timing = canonical;
        }
        let framed = lines.len() >= 2
            && lines[0].trim().parse::<u64>().is_ok()
            && timing.is_match(lines[1].trim());
        if framed && !lines[2..].iter().any(|l| !l.trim().is_empty()) {
            removed += 1;
            modified = true;
            continue;
        }
        kept.push(lines)
    }
    if modified {
        let mut n = 1;
        for lines in &mut kept {
            if lines.len() >= 2
                && lines[0].trim().parse::<u64>().is_ok()
                && timing.is_match(lines[1].trim())
            {
                lines[0] = n.to_string();
                n += 1
            }
        }
        let output = format!(
            "{bom}{}\n",
            kept.iter()
                .map(|v| v.join("\n"))
                .collect::<Vec<_>>()
                .join("\n\n")
        );
        fs::write(path, output).map_err(|_| "Could not clean staged subtitle cues")?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_normalization_handles_all_aliases_and_rejects_invalid_codes() {
        assert_eq!(
            Converter::normalize_languages(&json!(["*"])).unwrap(),
            vec!["*"]
        );
        assert_eq!(
            Converter::normalize_languages(&json!(["eng", "fr"])).unwrap(),
            vec!["en", "fr"]
        );
        assert!(Converter::normalize_languages(&json!(["x/../../etc"])).is_err());
    }

    #[test]
    fn candidates_skip_symlinks_and_idx_companions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("film.mkv"), b"x").unwrap();
        fs::write(dir.path().join("lang.idx"), b"x").unwrap();
        fs::write(dir.path().join("lang.sub"), b"x").unwrap();
        assert_eq!(Converter::candidates(&[dir.path().to_path_buf()]).len(), 2);
    }

    #[test]
    fn valid_srt_removes_empty_cues_and_preserves_malformed_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.srt");
        fs::write(&path,"1\n00:00:00,000 --> 00:00:01,000\nHello\n\n2\n00:00:01,000 --> 00:00:02,000\n  \n\n3\n00:00:02,000 --> 00:00:03,000\nWorld\n").unwrap();
        assert_eq!(remove_empty_cues(&path).unwrap(), 1);
        assert!(validate_srt(&path).is_ok());
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("2\n00:00:02,000")
        );
        fs::write(
            &path,
            "1\n00:00:00,000 --> 00:00:01,000\nHello\n\nwrong\n00:00:01,000 --> 00:00:02,000\n\n",
        )
        .unwrap();
        let _ = remove_empty_cues(&path);
        assert!(validate_srt(&path).is_err());
    }

    #[test]
    fn srt_normalizes_period_milliseconds_and_single_digit_hours() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("period.srt");
        fs::write(&path, "1\n1:02:03.004-->1:02:04.005\nValid text\n").unwrap();
        assert_eq!(remove_empty_cues(&path).unwrap(), 0);
        assert!(validate_srt(&path).is_ok());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "1\n01:02:03,004 --> 01:02:04,005\nValid text\n"
        );
        assert!(canonical_srt_timing("00:61:00,000 --> 00:61:01,000").is_none());
    }

    #[test]
    fn srt_cleanup_never_hides_broken_framing_after_valid_or_empty_cues() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.srt");
        for broken in [
            "orphan text without a cue header",
            "not-a-number\n00:00:03,000 --> 00:00:04,000\nText",
            "3\nmissing timing\nText",
        ] {
            fs::write(
                &path,
                format!("1\n00:00:01,000 --> 00:00:02,000\nValid\n\n2\n00:00:02,000 --> 00:00:03,000\n \n\n{broken}\n"),
            )
            .unwrap();
            remove_empty_cues(&path).unwrap();
            assert!(validate_srt(&path).is_err());
        }
    }

    #[test]
    fn srt_failures_identify_numeric_cue_and_reason_without_subtitle_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.srt");
        fs::write(
            &path,
            "1\n00:00:01,000 --> 00:00:02,000\nPrivate fixture text\n\n2\ninvalid timing containing private fixture text\nPrivate fixture text\n",
        )
        .unwrap();
        let error = validate_srt(&path).unwrap_err();
        assert_eq!(
            error,
            "Converter produced malformed SRT cue 2: invalid timing line"
        );
        let context = conversion_error_context(&error, Some("4"), Some(3), "eng");
        assert!(context.starts_with("Track 4, language eng:"));
        assert!(!context.contains("Private fixture text"));
        assert!(context.len() <= 240);
        assert!(
            conversion_error_context(&error, None, Some(3), "en")
                .starts_with("Stream 3, language en:")
        );
        let hostile = conversion_error_context(
            &"x".repeat(1000),
            Some("untrusted track text"),
            None,
            &"x".repeat(1000),
        );
        assert!(hostile.starts_with("Sidecar, language und:"));
        assert_eq!(hostile.len(), 240);
    }

    #[test]
    fn legacy_failure_entries_retry_but_current_failures_keep_backoff() {
        let interval = Duration::from_secs(3600);
        let legacy = json!({"ok":false,"attempt":1000.0});
        assert!(!failure_retry_delayed(&legacy, false, 1001.0, interval));
        let current = json!({"ok":false,"attempt":1000.0,"error_revision":ERROR_CACHE_REVISION});
        assert!(failure_retry_delayed(&current, false, 1001.0, interval));
        assert!(!failure_retry_delayed(&current, true, 1001.0, interval));
        assert!(!failure_retry_delayed(&current, false, 4601.0, interval));
    }

    #[test]
    fn error_cache_revision_does_not_reprocess_successful_unchanged_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        let output = dir.path().join("movie.en.srt");
        fs::write(&media, b"fixture source").unwrap();
        fs::write(&output, b"existing subtitle must survive").unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.min_age = Duration::ZERO;
        let media = Converter::safe_path(&media.to_string_lossy(), &converter.roots).unwrap();
        converter.set_state_entry(
            &media.to_string_lossy(),
            json!({
                "ok":true,"fingerprint":fingerprint_json(&fingerprint(&media).unwrap()),
                "languages":["*"],"outputs":["movie.en.srt"]
            }),
        );
        let summary = converter
            .scan(
                &[media],
                &["*".into()],
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(summary["cached"], 1);
        assert_eq!(summary["converted"], 0);
        assert_eq!(fs::read(output).unwrap(), b"existing subtitle must survive");
    }

    fn cache_image_track(converter: &Converter, media: &Path) {
        let track = Track {
            number: json!(4),
            native_number: Some("4".into()),
            stream_index: Some(3),
            language: "eng".into(),
            format: "HDMVPGS".into(),
            forced: false,
            hi: false,
        };
        converter.metadata.lock().unwrap().insert(
            &media.to_string_lossy(),
            fingerprint_json(&fingerprint(media).unwrap()),
            json!([track]),
        );
    }

    #[test]
    fn text_discovery_defers_images_and_daily_ocr_excludes_unchanged_failures() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("image.mkv");
        fs::write(&media, b"fixture media").unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.enable_ocr = true;
        converter.min_age = Duration::ZERO;
        converter.seconv = dir.path().join("must-not-be-invoked");
        let media = Converter::safe_path(&media.to_string_lossy(), &converter.roots).unwrap();
        cache_image_track(&converter, &media);
        let summary = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                Workload::Text,
                true,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(summary["converted"], 1);
        assert_eq!(
            converter.ocr_candidates(&converter.roots, false),
            vec![media.clone()]
        );
        let summary = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                Workload::Text,
                true,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(summary["cached"], 1);
        let key = media.to_string_lossy();
        converter.state[&*key]["ok"] = json!(false);
        converter.state[&*key]["attempt"] = json!(0);
        assert!(converter.ocr_candidates(&converter.roots, false).is_empty());
        assert_eq!(
            converter.ocr_candidates(&converter.roots, true),
            vec![media.clone()]
        );
        let summary = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                Workload::Ocr,
                true,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(summary["pending"], 1);
        assert_eq!(summary["failed"], 0);
        fs::write(&media, b"fixture media changed").unwrap();
        cache_image_track(&converter, &media);
        converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                Workload::Text,
                true,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(
            converter.ocr_candidates(&converter.roots, false),
            vec![media]
        );
    }

    #[test]
    fn metadata_survives_restart_and_unchanged_inspection_does_not_spawn_tools() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("cached.mkv");
        fs::write(&media, b"fixture").unwrap();
        let converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let media = Converter::safe_path(&media.to_string_lossy(), &converter.roots).unwrap();
        cache_image_track(&converter, &media);
        converter.persist_metadata().unwrap();
        let mut restarted = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        restarted.ffprobe = dir.path().join("must-not-be-invoked");
        restarted.mkvmerge = dir.path().join("must-not-be-invoked");
        assert_eq!(
            restarted
                .describe(&media)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        fs::write(&media, b"changed fixture").unwrap();
        assert!(restarted.describe(&media).is_err());
    }

    #[test]
    fn ocr_workload_cannot_bypass_deployment_permission() {
        let dir = tempfile::tempdir().unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.enable_ocr = false;
        assert!(
            converter
                .scan_workload(
                    &[],
                    &["*".into()],
                    true,
                    Workload::Ocr,
                    false,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .unwrap_err()
                .contains("disabled by deployment permissions")
        );
    }

    #[test]
    fn safe_path_rejects_parent_escape() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Converter::safe_path("../secret", &[dir.path().to_path_buf()]).is_err());
    }

    #[test]
    fn safe_path_accepts_existing_file_inside_configured_root() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("film.mkv");
        fs::write(&media, b"fixture").unwrap();
        assert_eq!(
            Converter::safe_path(&media.to_string_lossy(), &[dir.path().to_path_buf()]).unwrap(),
            normalize_canonical_local_path(media.canonicalize().unwrap())
        );
    }

    #[test]
    fn safe_path_rejects_existing_file_outside_configured_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        fs::create_dir(&root).unwrap();
        let outside = dir.path().join("private.mkv");
        fs::write(&outside, b"fixture").unwrap();
        assert!(Converter::safe_path(&outside.to_string_lossy(), &[root]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn safe_path_accepts_absolute_drive_path_when_root_contains_it() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("film.mkv");
        fs::write(&media, b"fixture").unwrap();
        let drive_path = media.to_string_lossy();
        assert!(Path::new(&*drive_path).is_absolute());
        assert_eq!(
            Converter::safe_path(&drive_path, &[dir.path().to_path_buf()]).unwrap(),
            normalize_canonical_local_path(media.canonicalize().unwrap())
        );
    }

    #[cfg(windows)]
    #[test]
    fn candidates_return_ordinary_drive_paths_and_preserve_user_device_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("film.mkv");
        fs::write(&media, b"fixture").unwrap();
        let canonical = media.canonicalize().unwrap();
        let discovered = Converter::candidates(&[dir.path().to_path_buf()]);
        assert_eq!(
            discovered,
            vec![normalize_canonical_local_path(canonical.clone())]
        );
        assert!(
            Converter::safe_path(
                &discovered[0].to_string_lossy(),
                &[dir.path().to_path_buf()]
            )
            .is_ok()
        );
        assert!(
            Converter::safe_path(&canonical.to_string_lossy(), &[dir.path().to_path_buf()])
                .is_err()
        );
        let unc = PathBuf::from(r"\\?\UNC\server\share\film.mkv");
        assert_eq!(normalize_canonical_local_path(unc.clone()), unc);
    }

    #[test]
    fn empty_path_selection_does_not_expand_to_all_roots() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("film.mkv"), b"fixture").unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let stop = AtomicBool::new(false);
        let mut events = Vec::new();
        let summary = converter
            .scan(&[], &["*".into()], false, &stop, &mut |e| events.push(e))
            .unwrap();
        assert_eq!(summary["converted"], 0);
        assert_eq!(summary["failed"], 0);
        assert!(events.is_empty());
    }

    #[test]
    fn enabling_ocr_invalidates_state_written_with_ocr_disabled() {
        assert!(!ocr_coverage(&json!({"ocr":false}), true));
        assert!(ocr_coverage(&json!({"ocr":true}), true));
        assert!(ocr_coverage(&json!({}), true)); // legacy state remains valid
        assert!(ocr_coverage(&json!({"ocr":false}), false));
    }

    #[test]
    fn summary_values_are_bounded_counts() {
        let mut v = json!({"failed":0});
        bump(&mut v, "failed");
        assert_eq!(v["failed"], 1);
    }

    #[test]
    fn publication_never_replaces_existing_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.srt");
        let existing = dir.path().join("movie.en.srt");
        fs::write(&staged, b"new subtitle").unwrap();
        fs::write(&existing, b"keep this subtitle").unwrap();
        assert!(!publish_noclobber(&staged, &existing).unwrap());
        assert_eq!(fs::read(existing).unwrap(), b"keep this subtitle");
        assert!(staged.exists());
    }

    #[test]
    fn ffmpeg_hexadecimal_stream_ids_are_parsed_without_shell_interpolation() {
        assert_eq!(numeric_stream_id(&json!({"id":"0x2a"})), Some(42));
        assert_eq!(numeric_stream_id(&json!({"id":"not-an-id"})), None);
    }

    #[test]
    fn mkvmerge_metadata_parser_accepts_valid_json_independent_of_exit_status() {
        let metadata = br#"{"tracks":[{"type":"subtitles","properties":{"number":3}}]}"#;
        assert!(parse_mkvmerge_metadata(metadata).is_some());
        assert!(parse_mkvmerge_metadata(b"warning: invalid UTF-8 \xff").is_none());
        assert!(parse_mkvmerge_metadata(br#"{"files":[]}"#).is_none());
    }

    #[test]
    fn mkvmerge_track_count_mismatch_falls_back_to_ffprobe_ids() {
        let native = json!({"tracks": [{"type":"video"}]});
        assert!(matching_native_tracks(Some(&native), 25).is_empty());
        let matching = json!({"tracks": [
            {"type":"subtitles","properties":{"number":3}},
            {"type":"subtitles","properties":{"number":4}}
        ]});
        assert_eq!(matching_native_tracks(Some(&matching), 2).len(), 2);
    }

    #[test]
    fn untrusted_track_metadata_is_normalized_and_bounded() {
        let aliases = language_aliases();
        let hostile = "x".repeat(100_000);
        assert_eq!(short_language(&hostile, &aliases), "und");
        assert_eq!(short_language("ENG", &aliases), "en");
        assert_eq!(bounded_track_language("zh-Hant"), "zh-hant");
        assert_eq!(
            short_language(&bounded_track_language("zh-Hant"), &aliases),
            "zh"
        );
        assert_eq!(bounded_track_language(&hostile), "und");
        assert_eq!(bounded_track_label(&hostile).len(), MAX_TRACK_LABEL);
        assert_eq!(bounded_track_label("codec <script>"), "codecscript");
    }

    #[test]
    fn vobsub_track_inventory_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let idx = dir.path().join("movie.idx");
        let content = (0..=MAX_TRACKS)
            .map(|index| format!("id: en, index: {index}\n"))
            .collect::<String>();
        fs::write(&idx, content).unwrap();
        let converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let stop = AtomicBool::new(false);
        assert!(converter.describe_with_stop(&idx, &stop).is_err());
    }
}
