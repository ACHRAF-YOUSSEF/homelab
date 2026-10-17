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
use std::sync::{Arc, Mutex, OnceLock};
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
const MAX_TRACKS: usize = crate::metadata::MAX_TRACKS;
const MAX_SELECTED_TRACKS: usize = 64;
const MAX_TRACK_LABEL: usize = 32;
// Bump only when a converter fix or new diagnostics should retry cached
// failures immediately. Successful fingerprint caches are independent.
const ERROR_CACHE_REVISION: u64 = 3;
const MAX_CANDIDATES: usize = 20_000;
const MAX_TRACKED_OUTPUTS: usize = 20_000;
const MAX_COVERAGE_ENTRIES: usize = 4096;
const MAX_COVERAGE_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_COVERAGE_TOTAL_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FileGate {
    pub entered: std::sync::atomic::AtomicUsize,
    pub released_count: std::sync::atomic::AtomicUsize,
    pub released: AtomicBool,
}

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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct SubtitleTarget {
    pub language: String,
    pub forced: bool,
    pub hi: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleSourceHint {
    pub path: PathBuf,
    pub target: SubtitleTarget,
}

impl SubtitleTarget {
    fn matches(&self, available: &Self) -> bool {
        self.forced == available.forced
            && self.hi == available.hi
            && (self.language == available.language
                || (!self.language.contains('-')
                    && available.language.split('-').next() == Some(self.language.as_str())))
    }
}

struct ExistingCoverage {
    targets: Vec<SubtitleTarget>,
    sidecars: Vec<(SubtitleTarget, PathBuf)>,
}

impl ExistingCoverage {
    fn covers(&self, target: &SubtitleTarget) -> bool {
        self.targets.iter().any(|existing| target.matches(existing))
    }
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
    SubtitleTarget,
);

type ScopeResolutionMarker = (Option<String>, Option<bool>);

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
    metadata: Arc<Mutex<crate::metadata::MetadataCache>>,
    persist_state: bool,
    state_dirty: bool,
    selected_tracks: Option<Vec<u64>>,
    targets: Option<Vec<SubtitleTarget>>,
    source_hints: Vec<SubtitleSourceHint>,
    force_covered: bool,
    gpu_ocr: Option<u32>,
    gpu_health: Mutex<Option<crate::gpu::Health>>,
    gpu_failure: Mutex<Option<&'static str>>,
    remote_staging: Option<PathBuf>,
    staged_outputs: Mutex<Vec<String>>,
    published_outputs: Mutex<Vec<String>>,
    #[cfg(test)]
    file_gate: Option<Arc<FileGate>>,
    #[cfg(test)]
    text_fixture: Option<&'static [u8]>,
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
            metadata: Arc::new(Mutex::new(crate::metadata::MetadataCache::new(
                state_path.with_file_name("metadata.json"),
            ))),
            persist_state: true,
            state_dirty: false,
            selected_tracks: None,
            targets: None,
            source_hints: Vec::new(),
            force_covered: false,
            gpu_ocr: None,
            gpu_health: Mutex::new(None),
            gpu_failure: Mutex::new(None),
            remote_staging: None,
            staged_outputs: Mutex::new(Vec::new()),
            published_outputs: Mutex::new(Vec::new()),
            #[cfg(test)]
            file_gate: None,
            #[cfg(test)]
            text_fixture: None,
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
            result.insert(target_language(&code, &aliases));
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
                                "id": c[2].parse::<u64>().ok(),
                                "number": c[2].parse::<u64>().ok(),
                                "language": short_language(&bounded_track_language(&c[1]), &self.language_aliases),
                                "forced": false, "hi": false, "format": "VobSub"
                            })
                        })
                        .collect(),
                ));
            }
        }
        let target = self.source_target(&path).unwrap_or(SubtitleTarget {
            language: "und".into(),
            forced: false,
            hi: false,
        });
        Ok(json!([{"number": null, "language":target.language,
            "forced":target.forced,
            "hi":target.hi,
            "format": extension(&path).unwrap_or_default().to_ascii_uppercase()}]))
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
            #[cfg(test)]
            if let Some(gate) = &self.file_gate {
                let ticket = gate.entered.fetch_add(1, Ordering::Relaxed);
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                while !gate.released.load(Ordering::Relaxed)
                    && gate.released_count.load(Ordering::Relaxed) <= ticket
                    && !stop.load(Ordering::Relaxed)
                {
                    if std::time::Instant::now() >= deadline {
                        return Err("Fixture file gate timed out".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
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
            if !self.force_covered
                && let Some(targets) = &self.targets
            {
                let existing = self.existing_coverage(&path)?;
                if targets.iter().all(|target| existing.covers(target)) {
                    let coverage = json!({"status":"already_covered","reason":if targets.is_empty() {"No subtitle targets are required by the inventory authority"} else {"Valid SRT files already cover every requested language and variant"},"existing_targets":existing.targets,"missing_targets":[]});
                    let mut entry = self.state.get(&key).cloned().unwrap_or_else(|| json!({}));
                    entry["coverage"] = coverage.clone();
                    self.set_state_entry(&key, entry);
                    self.persist_state()?;
                    bump(&mut summary, "cached");
                    progress(
                        json!({"status":"skipped","path":path,"reason":coverage["reason"],"coverage":coverage,"summary":summary}),
                    );
                    continue;
                }
            }
            let selection_coverage = if let Some(ids) = &self.selected_tracks {
                let description = self.describe_with_stop(&path, stop)?;
                validate_track_ids(&description, ids)?;
                let selected = description
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|track| track["number"].as_u64().is_some_and(|id| ids.contains(&id)));
                let mut selected_languages = BTreeSet::new();
                let mut required_text = false;
                let mut required_ocr = false;
                for track in selected {
                    selected_languages.insert(short_language(
                        track["language"].as_str().unwrap_or("und"),
                        &self.language_aliases,
                    ));
                    let image = is_image_format(track["format"].as_str().unwrap_or(""));
                    required_text |= !image && workload != Workload::Ocr;
                    required_ocr |= image && workload != Workload::Text && self.enable_ocr;
                }
                json!({"languages":selected_languages,"text":required_text,"ocr":required_ocr})
            } else {
                json!({"languages":languages.iter().map(|language| if language == "*" { "*".to_owned() } else {short_language(language, &self.language_aliases)}).collect::<BTreeSet<_>>(),
                    "text":workload != Workload::Ocr,"ocr":workload != Workload::Text && self.enable_ocr})
            };
            let scope = self.selection_key();
            let previous = self
                .state
                .get(&key)
                .and_then(|entry| match &scope {
                    Some(scope) => entry.get("track_selections")?.get(scope),
                    None => Some(entry),
                })
                .cloned()
                .unwrap_or_else(|| json!({}));
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
                    && self.targets.is_none()
                    && !self.force_covered
                    && coverage
                    && (!languages.iter().any(|language| language == "und")
                        || previous["unknown_complete"] == true)
                    && ocr_covered
                    && text_covered
                    && outputs_exist
                {
                    if scope.is_none() {
                        let mut cached_entry = previous.clone();
                        cached_entry["selection_coverage"] = selection_coverage.clone();
                        let needs_refresh = cached_entry["track_selections"]
                            .as_object()
                            .is_some_and(|selections| {
                                selections.values().any(|selection| {
                                    selection.get("fingerprint") != cached_entry.get("fingerprint")
                                        || resolved_selection(&cached_entry, selection)
                                })
                            });
                        if needs_refresh {
                            self.set_scoped_state_entry(&key, None, cached_entry)?;
                            self.persist_state()?;
                        }
                    }
                    bump(&mut summary, "cached");
                    progress(json!({"status":"cached","path":path,"summary":summary}));
                    continue;
                }
                if !self.force_covered
                    && (self.targets.is_none() || previous["targets"] == json!(self.targets))
                    && (unchanged_failure_deferred(&previous, retry, workload, automatic)
                        || failure_retry_delayed(
                            &previous,
                            retry,
                            epoch_seconds(),
                            self.retry_interval,
                        ))
                {
                    bump(&mut summary, "pending");
                    progress(json!({"status":"pending","path":path,"summary":summary}));
                    continue;
                }
            }
            let mut skip_reason = None;
            let mut conversion_progress = |event: Value| {
                if event["status"] == "skipped" {
                    skip_reason = event["reason"].as_str().map(str::to_owned);
                }
                progress(event);
            };
            match self.convert(&path, &languages, workload, stop, &mut conversion_progress) {
                Ok((outputs, ocr_pending, published)) => {
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
                    let existing = self.existing_coverage(&path)?;
                    let missing: Vec<_> = self
                        .targets
                        .iter()
                        .flatten()
                        .filter(|target| !existing.covers(target))
                        .cloned()
                        .collect();
                    let coverage = json!({"status":if self.targets.is_some() && missing.is_empty() {"already_covered"} else if published.is_empty() && skip_reason.is_some() {"skipped"} else {"converted"},
                        "reason":skip_reason.unwrap_or_else(||"Requested subtitle conversion completed".into()),"existing_targets":existing.targets,"missing_targets":missing});
                    self.set_scoped_state_entry(&key, scope.as_deref(), json!({"fingerprint":fingerprint_json(&before),"outputs":output_names,"ok":true,"languages":completed_languages,"ocr":ocr_complete,"ocr_pending":ocr_pending && !ocr_complete,"text_complete":text_complete,"selection_coverage":selection_coverage,"coverage":coverage,"targets":self.targets,"unknown_complete":languages.iter().any(|language|language=="und")}))?;
                    if self.persist_state().is_err() {
                        bump(&mut summary, "failed");
                        continue;
                    }
                    bump(&mut summary, "converted");
                    let new_outputs: Vec<String> = published
                        .iter()
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
                    self.set_scoped_state_entry(&key, scope.as_deref(), json!({"fingerprint":fingerprint_json(&before),"ok":false,"attempt":epoch_seconds(),"error":safe,"error_revision":ERROR_CACHE_REVISION,"ocr_pending":workload==Workload::Ocr || previous["ocr_pending"]==true,"text_complete":previous["text_complete"].as_bool().unwrap_or(previous["ok"]==true),"outputs":previous_outputs,"selection_coverage":selection_coverage,"targets":self.targets}))?;
                    let _ = self.persist_state();
                }
            }
        }
        self.persist_metadata()?;
        Ok(summary)
    }

    pub fn persist_metadata(&self) -> Result<(), String> {
        if !self.persist_state {
            return Ok(());
        }
        self.metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .persist()
    }

    fn persist_state(&self) -> Result<(), String> {
        if self.persist_state {
            crate::util::write_private_limited(&self.state_path, &self.state, MAX_STATE_BYTES)
        } else {
            Ok(())
        }
    }

    /// Only immutable tool configuration and one bounded persisted entry are
    /// copied. Metadata is shared behind its short lookup/insert mutex.
    pub fn file_worker(&self, path: &Path) -> Result<Self, String> {
        let key = path.to_string_lossy();
        let mut state = json!({});
        if let Some(previous) = self.state.get(key.as_ref()) {
            state[key.as_ref()] = crate::util::bounded_json_clone(previous, 64 * 1024)?;
        }
        Ok(self.worker_with_state(state))
    }

    pub fn inspection_worker(&self) -> Self {
        self.worker_with_state(json!({}))
    }

    /// IDs identify existing embedded tracks, never command arguments.
    pub fn select_tracks(&mut self, ids: Vec<u64>) -> Result<(), String> {
        if ids.is_empty()
            || ids.len() > MAX_SELECTED_TRACKS
            || ids.iter().copied().collect::<BTreeSet<_>>().len() != ids.len()
        {
            return Err("Choose 1–64 unique subtitle track IDs".into());
        }
        self.selected_tracks = Some(ids);
        Ok(())
    }

    /// Per-file requirements supplied by the inventory authority. An explicit
    /// empty list means no subtitle is needed; it never expands to all tracks.
    pub fn set_targets(&mut self, targets: Vec<SubtitleTarget>) -> Result<(), String> {
        if targets.len() > MAX_SELECTED_TRACKS {
            return Err("Too many subtitle language requirements".into());
        }
        let mut normalized = BTreeSet::new();
        for target in targets {
            if !valid_language_code(&target.language) {
                return Err("Invalid subtitle language requirement".into());
            }
            normalized.insert(SubtitleTarget {
                language: target_language(&target.language, &self.language_aliases),
                ..target
            });
        }
        self.targets = Some(normalized.into_iter().collect());
        Ok(())
    }

    pub fn targets(&self) -> Option<&[SubtitleTarget]> {
        self.targets.as_deref()
    }

    /// Accept explicit inventory metadata only for existing local sidecars.
    /// Language and variant guesses are never inferred from wanted targets.
    pub fn set_source_hints(&mut self, hints: Vec<SubtitleSourceHint>) -> Result<(), String> {
        if hints.len() > MAX_TRACKS {
            return Err("Too many subtitle source metadata hints".into());
        }
        let mut normalized = BTreeMap::new();
        for hint in hints {
            let path = Self::safe_path(&hint.path.to_string_lossy(), &self.roots)?;
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| "Subtitle source metadata path is unavailable")?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || !extension(&path).is_some_and(|extension| {
                    extension == "srt" || SIDECAR_EXTENSIONS.contains(&extension.as_str())
                })
                || !valid_language_code(&hint.target.language)
            {
                return Err(
                    "Subtitle source metadata requires a safe local sidecar and language".into(),
                );
            }
            let target = SubtitleTarget {
                language: target_language(&hint.target.language, &self.language_aliases),
                ..hint.target
            };
            let (_, filename) = subtitle_name(&path, &self.language_aliases)
                .ok_or("Subtitle source filename is unavailable")?;
            if (filename.language != "und"
                && !same_language_family(&target.language, &filename.language))
                || (filename.forced && !target.forced)
                || (filename.hi && !target.hi)
            {
                return Err("Subtitle source metadata conflicts with the explicit filename language or variant".into());
            }
            if normalized
                .get(&path)
                .is_some_and(|previous| previous != &target)
            {
                return Err("Subtitle source metadata contains conflicting entries".into());
            }
            normalized.insert(path, target);
        }
        self.source_hints = normalized
            .into_iter()
            .map(|(path, target)| SubtitleSourceHint { path, target })
            .collect();
        Ok(())
    }

    pub fn source_hints(&self) -> &[SubtitleSourceHint] {
        &self.source_hints
    }

    fn source_target(&self, path: &Path) -> Option<SubtitleTarget> {
        self.source_hints
            .iter()
            .find(|hint| same_local_path(&hint.path, path))
            .map(|hint| hint.target.clone())
            .or_else(|| subtitle_name(path, &self.language_aliases).map(|(_, target)| target))
    }

    fn existing_coverage(&self, path: &Path) -> Result<ExistingCoverage, String> {
        existing_coverage_with_hints(path, &self.language_aliases, &self.source_hints)
    }

    pub fn force_covered_tracks(&mut self, force: bool) -> Result<(), String> {
        if force && self.selected_tracks.is_none() {
            return Err("Forcing covered subtitles requires an explicit track selection".into());
        }
        self.force_covered = force;
        Ok(())
    }

    pub fn force_covered(&self) -> bool {
        self.force_covered
    }

    /// Uses only bounded sibling inspection and metadata supplied by the caller.
    /// No media probe or OCR subprocess is started to calculate this plan.
    pub fn conversion_coverage(
        &self,
        path: &Path,
        description: &Value,
        targets: &[SubtitleTarget],
    ) -> Result<Value, String> {
        let path = Self::safe_path(&path.to_string_lossy(), &self.roots)?;
        let existing = self.existing_coverage(&path)?;
        let mut normalized = BTreeSet::new();
        if targets.len() > MAX_SELECTED_TRACKS {
            return Err("Too many subtitle language requirements".into());
        }
        for target in targets {
            if !valid_language_code(&target.language) {
                return Err("Invalid subtitle language requirement".into());
            }
            normalized.insert(SubtitleTarget {
                language: target_language(&target.language, &self.language_aliases),
                forced: target.forced,
                hi: target.hi,
            });
        }
        let missing: Vec<_> = normalized
            .into_iter()
            .filter(|target| !existing.covers(target))
            .collect();
        let tracks = description
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_TRACKS);
        let mut available: Vec<_> = existing
            .sidecars
            .iter()
            .map(|(target, _)| target.clone())
            .collect();
        let mut paths = BTreeSet::new();
        for (target, source) in &existing.sidecars {
            if missing.iter().any(|wanted| wanted.matches(target)) {
                paths.insert(source.clone());
            }
        }
        let mut unknown = 0usize;
        for track in tracks {
            let language = target_language(
                track["language"].as_str().unwrap_or("und"),
                &self.language_aliases,
            );
            if language == "und" {
                unknown += 1;
                continue;
            }
            let target = SubtitleTarget {
                language,
                forced: track["forced"].as_bool().unwrap_or(false),
                hi: track["hi"].as_bool().unwrap_or(false),
            };
            if missing.iter().any(|wanted| wanted.matches(&target)) {
                paths.insert(path.clone());
            }
            available.push(target);
        }
        let needed: Vec<_> = missing
            .iter()
            .filter(|target| available.iter().any(|candidate| target.matches(candidate)))
            .cloned()
            .collect();
        Ok(
            json!({"status":if missing.is_empty() {"already_covered"} else if needed.is_empty() {"no_available_conversion"} else {"needed"},
            "reason":if missing.is_empty() {"Valid SRT files already cover every requested language and variant"} else if needed.is_empty() {"No matching embedded or sidecar subtitle can supply the missing targets"} else {"Matching subtitles can supply the missing language and variant targets"},
            "existing_targets":existing.targets,"missing_targets":missing,"available_targets":needed,"available_paths":paths,"unknown_tracks_skipped":unknown}),
        )
    }

    fn selection_key(&self) -> Option<String> {
        let mut ids = self.selected_tracks.clone()?;
        ids.sort_unstable();
        Some(ids.iter().map(u64::to_string).collect::<Vec<_>>().join(","))
    }

    fn set_scoped_state_entry(
        &mut self,
        key: &str,
        scope: Option<&str>,
        mut entry: Value,
    ) -> Result<(), String> {
        if let Some(fields) = entry.as_object_mut() {
            fields.remove("inventory_resolved");
        }
        let previous = self.state.get(key);
        let current_fingerprint = entry.get("fingerprint").cloned();
        if let Some(scope) = scope {
            let mut merged = previous.cloned().unwrap_or_else(|| json!({}));
            let mut outputs: BTreeSet<_> = safe_outputs(&merged).into_iter().collect();
            outputs.extend(safe_outputs(&entry));
            merged["outputs"] = json!(outputs);
            if !merged["track_selections"].is_object() {
                merged["track_selections"] = json!({});
            }
            if let Some(selections) = merged["track_selections"].as_object_mut() {
                selections.retain(|_, selection| {
                    selection.get("fingerprint") == current_fingerprint.as_ref()
                });
                // One file remains bounded even after many different manual selections.
                if selections.len() >= MAX_SELECTED_TRACKS
                    && !selections.contains_key(scope)
                    && let Some(oldest) = selections.keys().next().cloned()
                {
                    selections.remove(&oldest);
                }
                selections.insert(scope.to_owned(), entry);
            }
            entry = merged;
        } else if let Some(selections) =
            previous.and_then(|previous| previous.get("track_selections"))
        {
            let retained: serde_json::Map<_, _> = selections
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, selection)| {
                    selection.get("fingerprint") == current_fingerprint.as_ref()
                        && !resolved_selection(&entry, selection)
                })
                .map(|(scope, selection)| (scope.clone(), selection.clone()))
                .collect();
            entry["track_selections"] = json!(retained);
            let mut outputs: BTreeSet<_> = safe_outputs(&entry).into_iter().collect();
            outputs.extend(previous.into_iter().flat_map(safe_outputs));
            entry["outputs"] = json!(outputs);
        }
        if let Some(fingerprint) = current_fingerprint {
            entry["processing_fingerprint"] = fingerprint;
        }
        // Keep each worker snapshot within its existing 64 KiB bound. Evict
        // older selection caches; never erase the legacy whole-file success.
        while crate::util::bounded_json_clone(&entry, 64 * 1024).is_err() {
            let selections = entry["track_selections"]
                .as_object_mut()
                .ok_or("File state exceeds its size limit")?;
            let oldest = selections
                .keys()
                .find(|key| Some(key.as_str()) != scope)
                .cloned()
                .ok_or("Selected file state exceeds its size limit")?;
            selections.remove(&oldest);
        }
        self.set_state_entry(key, entry);
        Ok(())
    }

    /// Snapshot from bounded persisted state only. No filesystem queries or probes.
    pub fn pipeline_counts(&self) -> Value {
        let mut outputs = BTreeSet::new();
        let mut pending = 0_u64;
        let mut failed = 0_u64;
        let mut output_count_available = true;
        for (source, entry) in self.state.as_object().into_iter().flatten() {
            let path = Path::new(source);
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir))
                || !self.roots.iter().any(|root| path.starts_with(root))
            {
                continue;
            }
            for name in entry["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if Path::new(name).file_name().and_then(|value| value.to_str()) == Some(name)
                    && Path::new(name).extension().is_some_and(|ext| ext == "srt")
                    && let Some(parent) = path.parent()
                {
                    if outputs.len() < MAX_TRACKED_OUTPUTS {
                        outputs.insert((parent, name));
                    } else if !outputs.contains(&(parent, name)) {
                        output_count_available = false;
                    }
                }
            }
            let mut file_pending = false;
            let mut file_failed = false;
            for item in current_processing_entries(entry) {
                file_pending |= item["ocr_pending"] == true;
                file_failed |= failed_state(item);
            }
            pending += u64::from(file_pending);
            failed += u64::from(file_failed);
        }
        json!({"generated_srt_count":if output_count_available {Some(outputs.len())}else{None},"pending_ocr_files":pending,"failed_files":failed,"generated_srt_count_available":output_count_available,"generated_srt_count_scope":"saved_publication_receipts","generated_srt_filesystem_verified":false})
    }

    pub fn file_processing(&self, path: &str) -> Value {
        let Some(entry) = self.state.get(path) else {
            return json!({"pending_ocr":false,"failed":false,"generated_srt_count":0,"last_error":null,"coverage":null});
        };
        let mut outputs = BTreeSet::new();
        let mut available = true;
        for name in entry["outputs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|name| {
                Path::new(name).file_name().and_then(|value| value.to_str()) == Some(*name)
                    && Path::new(name).extension().is_some_and(|ext| ext == "srt")
            })
        {
            if outputs.len() < MAX_TRACKED_OUTPUTS {
                outputs.insert(name);
            } else if !outputs.contains(name) {
                available = false;
            }
        }
        let mut pending = false;
        let mut failed = false;
        let mut latest_error: Option<(f64, String)> = None;
        for item in current_processing_entries(entry) {
            pending |= item["ocr_pending"] == true;
            failed |= failed_state(item);
            if failed_state(item)
                && let Some(error) = item["error"].as_str()
            {
                let attempt = item["attempt"]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .unwrap_or(0.0);
                if latest_error
                    .as_ref()
                    .is_none_or(|(previous, _)| attempt >= *previous)
                {
                    latest_error = Some((
                        attempt,
                        error
                            .chars()
                            .filter(|character| !character.is_control())
                            .take(240)
                            .collect(),
                    ));
                }
            }
        }
        let coverage = entry.get("coverage").cloned().unwrap_or(Value::Null);
        json!({"pending_ocr":pending,"failed":failed,"generated_srt_count":if available {Some(outputs.len())}else{None},"last_error":latest_error.map(|(_, error)|error),"coverage":coverage})
    }

    /// Reconcile existing processing records during an explicit inventory job.
    /// Verified external coverage resolves only the same source and exact scope;
    /// it never adds publication receipts or rewrites historical errors.
    pub(crate) fn reconcile_inventory_processing(
        &mut self,
        rows: &[Value],
    ) -> Result<bool, String> {
        // Roll back only bounded boolean annotations if persistence fails;
        // never clone the conversion library or overwrite historical records.
        let mut undo: Vec<(String, Vec<ScopeResolutionMarker>)> = Vec::new();
        for row in rows.iter().take(MAX_CANDIDATES) {
            let Some(source) = row["path"].as_str() else {
                continue;
            };
            if self.state.get(source).is_none() {
                continue;
            }
            let Ok(path) = Self::safe_path(source, &self.roots) else {
                continue;
            };
            let Ok(stamp) = fingerprint(&path) else {
                continue;
            };
            let current = fingerprint_json(&stamp);
            let existing: Vec<SubtitleTarget> = row["coverage"]["existing_targets"]
                .as_array()
                .filter(|targets| targets.len() <= MAX_COVERAGE_ENTRIES)
                .and_then(|targets| serde_json::from_value(json!(targets)).ok())
                .unwrap_or_default();
            let known_coverage = matches!(
                row["coverage"]["status"].as_str(),
                Some("covered" | "already_covered" | "needs_conversion" | "no_convertible_source")
            );
            let Some(entry) = self.state.get_mut(source) else {
                continue;
            };
            // Scope records stay independent: covering a normal English track
            // cannot hide an unresolved French, forced, or hearing-impaired one.
            let tracks = row["tracks"].as_array();
            let mut previous_markers = Vec::new();
            let mut resolve = |item: &mut Value, scope: Option<&str>| {
                let previous = match item.get("inventory_resolved") {
                    Some(Value::Bool(previous)) => Some(*previous),
                    Some(_) => return,
                    None => None,
                };
                let resolved = item["fingerprint"] == current
                    && known_coverage
                    && inventory_covers_scope(
                        item,
                        scope,
                        tracks.map(Vec::as_slice).unwrap_or(&[]),
                        &existing,
                    );
                if previous.unwrap_or(false) == resolved {
                    return;
                }
                previous_markers.push((scope.map(str::to_owned), previous));
                item["inventory_resolved"] = json!(resolved);
            };
            resolve(entry, None);
            if let Some(scopes) = entry["track_selections"].as_object_mut() {
                for (scope, item) in scopes.iter_mut().take(MAX_SELECTED_TRACKS) {
                    resolve(item, Some(scope));
                }
            }
            if !previous_markers.is_empty() {
                undo.push((source.to_owned(), previous_markers));
            }
        }
        let changed = !undo.is_empty();
        if changed {
            let previous_dirty = self.state_dirty;
            self.state_dirty = true;
            if let Err(error) = self.persist_state() {
                for (source, markers) in undo.into_iter().rev() {
                    let Some(entry) = self.state.get_mut(&source) else {
                        continue;
                    };
                    for (scope, previous) in markers {
                        let item = match scope {
                            Some(scope) => entry
                                .get_mut("track_selections")
                                .and_then(|scopes| scopes.get_mut(&scope)),
                            None => Some(&mut *entry),
                        };
                        if let Some(fields) = item.and_then(Value::as_object_mut) {
                            match previous {
                                Some(previous) => {
                                    fields.insert("inventory_resolved".into(), json!(previous));
                                }
                                None => {
                                    fields.remove("inventory_resolved");
                                }
                            }
                        }
                    }
                }
                self.state_dirty = previous_dirty;
                return Err(error);
            }
        }
        Ok(changed)
    }

    fn worker_with_state(&self, state: Value) -> Self {
        Self {
            roots: self.roots.clone(),
            state,
            state_path: self.state_path.clone(),
            ffprobe: self.ffprobe.clone(),
            ffmpeg: self.ffmpeg.clone(),
            mkvmerge: self.mkvmerge.clone(),
            seconv: self.seconv.clone(),
            min_age: self.min_age,
            timeout: self.timeout,
            retry_interval: self.retry_interval,
            enable_ocr: self.enable_ocr,
            ocr_language: self.ocr_language.clone(),
            language_aliases: self.language_aliases.clone(),
            metadata: Arc::clone(&self.metadata),
            persist_state: false,
            state_dirty: false,
            selected_tracks: None,
            targets: None,
            source_hints: Vec::new(),
            force_covered: false,
            gpu_ocr: None,
            gpu_health: Mutex::new(None),
            gpu_failure: Mutex::new(None),
            remote_staging: None,
            staged_outputs: Mutex::new(Vec::new()),
            published_outputs: Mutex::new(Vec::new()),
            #[cfg(test)]
            file_gate: self.file_gate.clone(),
            #[cfg(test)]
            text_fixture: self.text_fixture,
        }
    }

    pub(crate) fn enable_gpu_ocr(&mut self, minimum_free_mib: u32) {
        if crate::edition::CUDA_ENABLED {
            self.gpu_ocr = Some(minimum_free_mib.min(65536));
        }
    }

    pub(crate) fn enable_remote_staging(&mut self, directory: PathBuf) -> Result<(), String> {
        if !directory.is_dir()
            || has_symlink_component(&directory)?
            || !directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".subforge-remote-") && name.len() <= 100)
        {
            return Err("Remote staging directory is unavailable or unsafe".into());
        }
        self.remote_staging = Some(normalize_canonical_local_path(
            directory
                .canonicalize()
                .map_err(|_| "Remote staging directory is unavailable")?,
        ));
        Ok(())
    }

    pub(crate) fn staged_outputs(&self) -> Vec<String> {
        self.staged_outputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Receipts exist only after validated no-clobber publication. Keep them
    /// independent of final track/state success so post-publication errors
    /// cannot hide files from the coordinator's deferred index refresh.
    pub(crate) fn take_published_outputs(&mut self) -> Vec<String> {
        std::mem::take(
            self.published_outputs
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    pub(crate) fn gpu_status(&self) -> (Option<crate::gpu::Health>, Option<&'static str>) {
        (
            *self
                .gpu_health
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            *self
                .gpu_failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    #[cfg(test)]
    pub(crate) fn set_file_gate(&mut self, gate: Arc<FileGate>, missing_tool: PathBuf) {
        self.file_gate = Some(gate);
        self.min_age = Duration::ZERO;
        self.ffmpeg = missing_tool;
    }

    pub fn merge_file_state(&mut self, path: &Path, entry: Option<Value>) -> Result<(), String> {
        if let Some(entry) = entry {
            let key = path.to_string_lossy().into_owned();
            let previous = self
                .state
                .as_object_mut()
                .ok_or("Conversion state is unavailable")?
                .insert(key.clone(), entry);
            if let Err(error) = self.persist_state() {
                if let Some(entries) = self.state.as_object_mut() {
                    entries.remove(&key);
                    if let Some(previous) = previous {
                        entries.insert(key, previous);
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn take_file_state(&mut self, path: &Path) -> Option<Value> {
        if !self.state_dirty {
            return None;
        }
        self.state
            .as_object_mut()?
            .remove(path.to_string_lossy().as_ref())
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

    pub fn metadata_usage(&self) -> Arc<crate::metadata::CacheUsage> {
        self.metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .usage()
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
            self.state_dirty = true;
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
                "-show_entries",
                "stream=index,id,codec_name:stream_tags=language:stream_disposition=forced,hearing_impaired",
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
    ) -> Result<(Vec<String>, bool, Vec<String>), String> {
        let before = fingerprint(path)?;
        let parent = path.parent().ok_or("Media path has no parent directory")?;
        let existing = self.existing_coverage(path)?;
        if !self.force_covered
            && let Some(targets) = &self.targets
            && targets.iter().all(|target| existing.covers(target))
        {
            progress(
                json!({"status":"skipped","path":path,"reason":"Requested subtitle languages and variants are already covered"}),
            );
            return Ok((Vec::new(), false, Vec::new()));
        }
        if let Some(directory) = &self.remote_staging
            && (directory.parent() != Some(parent)
                || has_symlink_component(directory)?
                || !directory.is_dir())
        {
            return Err("Remote staging must be a safe direct sibling of the media file".into());
        }
        self.staged_outputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.published_outputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let staging = Builder::new()
            .prefix(".subforge-")
            .tempdir_in(parent)
            .map_err(|_| "Could not create a private staging directory")?;
        let mut tracks = if is_video(path) {
            self.tracks(path, stop)?
        } else {
            Vec::new()
        };
        if let Some(ids) = &self.selected_tracks
            && is_video(path)
        {
            validate_selected_tracks(&tracks, ids)?;
        }
        if self.selected_tracks.is_none() {
            tracks.sort_by_key(|track| is_image_format(&track.format));
        }
        let mut counts: BTreeMap<(String, bool, bool), usize> = BTreeMap::new();
        for track in &tracks {
            *counts
                .entry((
                    target_language(&track.language, &self.language_aliases),
                    track.forced,
                    track.hi,
                ))
                .or_default() += 1;
        }
        let mut jobs: Vec<ConversionJob> = Vec::new();
        let mut ocr_pending = false;
        let mut planned_targets = BTreeSet::new();
        if is_video(path) {
            for track in tracks {
                let lang = target_language(&track.language, &self.language_aliases);
                if self.selected_tracks.as_ref().map_or_else(
                    || !wants(&track.language, languages, &self.language_aliases),
                    |ids| !track.number.as_u64().is_some_and(|id| ids.contains(&id)),
                ) {
                    continue;
                }
                let target = SubtitleTarget {
                    language: lang.clone(),
                    forced: track.forced,
                    hi: track.hi,
                };
                if let Some(reason) = self.skip_target(&target, languages, &existing) {
                    progress(
                        json!({"status":"skipped","path":path,"track":track.stream_index,"language":lang,"reason":reason}),
                    );
                    continue;
                }
                let planning_target = self.planning_target(&target);
                if self.selected_tracks.is_none() && planned_targets.contains(&planning_target) {
                    progress(
                        json!({"status":"skipped","path":path,"track":track.stream_index,"language":lang,"reason":"Another track already supplies this language and variant"}),
                    );
                    continue;
                }
                let mut stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("subtitle")
                    .to_owned();
                if (self.selected_tracks.is_some()
                    && counts
                        .get(&(lang.clone(), track.forced, track.hi))
                        .copied()
                        .unwrap_or(0)
                        > 1)
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
                planned_targets.insert(planning_target);
                jobs.push((
                    path.to_path_buf(),
                    format!("{stem}.srt"),
                    Some(stream_index),
                    track.native_number,
                    image,
                    false,
                    ocr_lang,
                    track.language,
                    target,
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
                let re = Regex::new(r"(?mi)^id:\s*([a-zA-Z-]+),\s*index:\s*(\d+).*$")
                    .map_err(|_| "Invalid subtitle parser")?;
                let caps: Vec<_> = re.captures_iter(&text).take(MAX_TRACKS + 1).collect();
                if caps.len() > MAX_TRACKS {
                    return Err("VobSub contains too many subtitle tracks to convert safely".into());
                }
                let header = Regex::new(r"(?mi)^langidx:.*$").unwrap();
                let body_re = Regex::new(r"(?mi)^(id:.*index:)\s*\d+").unwrap();
                for (n, c) in caps.iter().enumerate() {
                    let lang = bounded_track_language(&c[1]);
                    let track_id = c[2].parse::<u64>().ok();
                    if self.selected_tracks.as_ref().map_or_else(
                        || !wants(&lang, languages, &self.language_aliases),
                        |ids| !track_id.is_some_and(|id| ids.contains(&id)),
                    ) {
                        continue;
                    }
                    let variant =
                        subtitle_name(path, &self.language_aliases).map(|(_, target)| target);
                    let target = SubtitleTarget {
                        language: target_language(&lang, &self.language_aliases),
                        forced: variant.as_ref().is_some_and(|target| target.forced),
                        hi: variant.as_ref().is_some_and(|target| target.hi),
                    };
                    if let Some(reason) = self.skip_target(&target, languages, &existing) {
                        progress(
                            json!({"status":"skipped","path":path,"track":track_id,"language":target.language,"reason":reason}),
                        );
                        continue;
                    }
                    let planning_target = self.planning_target(&target);
                    if self.selected_tracks.is_none() && planned_targets.contains(&planning_target)
                    {
                        continue;
                    }
                    if workload == Workload::Text || !self.enable_ocr {
                        ocr_pending = true;
                        continue;
                    }
                    let label = target.language.clone();
                    let mut stem = media_stem(path, &self.language_aliases)
                        .unwrap_or_else(|| "subtitle".into());
                    if self.selected_tracks.is_some()
                        && caps
                            .iter()
                            .filter(|capture| {
                                short_language(
                                    &bounded_track_language(&capture[1]),
                                    &self.language_aliases,
                                ) == label
                            })
                            .count()
                            > 1
                    {
                        stem.push_str(&format!(
                            ".track{}",
                            track_id.ok_or("VobSub track ID is invalid")?
                        ));
                    }
                    stem.push_str(&format!(".{label}"));
                    if target.forced {
                        stem.push_str(".forced");
                    }
                    if target.hi {
                        stem.push_str(".hi");
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
                    planned_targets.insert(planning_target);
                    jobs.push((
                        staged_idx,
                        format!("{stem}.srt"),
                        None,
                        None,
                        true,
                        false,
                        self.ocr_for(&lang),
                        lang,
                        target,
                    ));
                }
            }
        } else if wants(
            &self
                .source_target(path)
                .map(|target| target.language)
                .unwrap_or_else(|| "und".into()),
            languages,
            &self.language_aliases,
        ) {
            let fmt = extension(path).unwrap_or_default();
            let target = self.source_target(path).unwrap_or(SubtitleTarget {
                language: "und".into(),
                forced: false,
                hi: false,
            });
            if let Some(reason) = self.skip_target(&target, languages, &existing) {
                progress(
                    json!({"status":"skipped","path":path,"language":target.language,"reason":reason}),
                );
                return Ok((Vec::new(), false, Vec::new()));
            }
            let image = matches!(fmt.as_str(), "sup" | "sub" | "idx");
            let seconv = matches!(fmt.as_str(), "ttml" | "sbv");
            if image && (workload == Workload::Text || !self.enable_ocr) {
                ocr_pending = true;
            }
            if (image && self.enable_ocr && workload != Workload::Text)
                || (!image && workload != Workload::Ocr)
            {
                let name = if self
                    .source_hints
                    .iter()
                    .any(|hint| same_local_path(&hint.path, path))
                {
                    let mut stem = media_stem(path, &self.language_aliases)
                        .ok_or("Subtitle source filename is unavailable")?;
                    stem.push('.');
                    stem.push_str(&target.language);
                    if target.forced {
                        stem.push_str(".forced");
                    }
                    if target.hi {
                        stem.push_str(".hi");
                    }
                    format!("{stem}.srt")
                } else {
                    format!(
                        "{}.srt",
                        path.file_stem()
                            .and_then(|stem| stem.to_str())
                            .unwrap_or("subtitle")
                    )
                };
                jobs.push((
                    path.to_path_buf(),
                    name,
                    None,
                    None,
                    image,
                    seconv,
                    self.ocr_for(&target.language),
                    target.language.clone(),
                    target,
                ));
            }
        }
        let mut outputs = Vec::new();
        let mut destinations = HashSet::new();
        for (source, name, stream, native_track, image, seconv, ocr_lang, language, target) in jobs
        {
            if !destinations.insert(name.clone()) {
                continue;
            }
            let destination = parent.join(&name);
            if destination.exists() || destination.is_symlink() {
                progress(
                    json!({"status":"skipped","path":path,"track":stream,"language":target.language,"reason":"The existing output filename is preserved and will not be overwritten"}),
                );
                continue;
            }
            if !self.force_covered && self.existing_coverage(path)?.covers(&target) {
                progress(
                    json!({"status":"skipped","path":path,"track":stream,"language":target.language,"reason":"A valid SRT already covers this language and variant"}),
                );
                continue;
            }
            let backend = if image && self.gpu_ocr.is_some() {
                "ocr_rs_cuda"
            } else if image {
                "ocr_rs_cpu"
            } else if seconv {
                "seconv"
            } else {
                "ffmpeg"
            };
            progress(
                json!({"status":if image {"ocr"} else {"extracting"},"path":path,
                "output":name,"track":stream,"language":ocr_lang,"backend":backend}),
            );
            let staged = staging.path().join(&name);
            if image || seconv {
                let gpu = image && self.gpu_ocr.is_some();
                if let Some(minimum_free_mib) = self.gpu_ocr.filter(|_| gpu)
                    && self.gpu_status().0.is_none()
                {
                    progress(json!({"status":"checking_gpu","path":path}));
                    match crate::gpu::probe(stop, minimum_free_mib) {
                        Ok(health) => {
                            *self
                                .gpu_health
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(health);
                        }
                        Err(error) => {
                            *self
                                .gpu_failure
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                            return Err(error.into());
                        }
                    }
                }
                // Subtitle Edit's fixed Paddle-compatible protocol delegates
                // bitmap recognition to our native ocr-rs executable on both
                // CPU and CUDA. No Python/PaddlePaddle runtime is involved.
                let engine = "paddle";
                let language = if image {
                    let language = short_language(&language, &self.language_aliases);
                    if language == "und" {
                        short_language(&self.ocr_language, &self.language_aliases)
                    } else {
                        language
                    }
                } else {
                    ocr_lang.clone()
                };
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
                        engine,
                        "--ocr-language",
                        &language,
                        "--json",
                    ]);
                if let Some(track_number) = native_track.as_deref() {
                    command.args(["--track-number", track_number]);
                }
                let (exit_success, report) = if gpu {
                    crate::util::bounded_gpu_process(
                        &mut command,
                        self.timeout,
                        stop,
                        MAX_CONVERTER_OUTPUT,
                        self.gpu_ocr.unwrap_or(2048),
                    )
                } else {
                    crate::util::bounded_process_allow_failure(
                        &mut command,
                        self.timeout,
                        stop,
                        MAX_CONVERTER_OUTPUT,
                    )
                }
                .map_err(|message| {
                    conversion_error_context(&message, native_track.as_deref(), stream, &language)
                })?;
                let report = parse_conversion_report(&report).map_err(|message| {
                    conversion_error_context(message, native_track.as_deref(), stream, &language)
                })?;
                if !exit_success || report.get("success").and_then(Value::as_bool) != Some(true) {
                    return Err(conversion_error_context(
                        &classify_ocr_failure(&report, gpu),
                        native_track.as_deref(),
                        stream,
                        &language,
                    ));
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
                self.extract_text(&mut command, &staged, stop)
                    .map_err(|message| {
                        conversion_error_context(
                            &message,
                            native_track.as_deref(),
                            stream,
                            &language,
                        )
                    })?;
            }
            if staged.is_symlink() {
                return Err("Converter created an unsafe symbolic-link output".into());
            }
            progress(json!({"status":"validating","path":path,"output":name,
                "track":stream,"language":language,"backend":backend}));
            remove_empty_cues(&staged)
                .and_then(|_| validate_srt(&staged))
                .map_err(|message| {
                    conversion_error_context(&message, native_track.as_deref(), stream, &language)
                })?;
            progress(json!({"status":"track_complete","path":path,"output":name,
                "track":stream,"language":language,"backend":backend}));
            outputs.push(name);
        }
        if fingerprint(path)? != before || stop.load(Ordering::Relaxed) {
            return Err("Source changed or conversion was cancelled".into());
        }
        let mut published = Vec::new();
        let mut retained = Vec::new();
        for name in &outputs {
            let staged = staging.path().join(name);
            if !self.force_covered
                && let Some((_, target)) = subtitle_name(Path::new(name), &self.language_aliases)
                && self.existing_coverage(path)?.covers(&target)
            {
                progress(
                    json!({"status":"skipped","path":path,"language":target.language,"reason":"A valid SRT appeared before publication and now covers this target"}),
                );
                continue;
            }
            let destination = self.remote_staging.as_deref().unwrap_or(parent).join(name);
            if publish_noclobber(&staged, &destination)? {
                retained.push(name.clone());
                published.push(name.clone());
                if self.remote_staging.is_some() {
                    self.staged_outputs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(name.clone());
                } else {
                    let mut receipts = self
                        .published_outputs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if receipts.len() < MAX_TRACKED_OUTPUTS {
                        receipts.push(destination.to_string_lossy().into_owned());
                    }
                }
            }
        }
        Ok((retained, ocr_pending, published))
    }

    fn extract_text(
        &self,
        command: &mut Command,
        staged: &Path,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        #[cfg(test)]
        if let Some(bytes) = self.text_fixture {
            fs::write(staged, bytes).map_err(|_| "Cannot write fixture subtitle")?;
            return normalize_ffmpeg_srt(staged);
        }
        crate::util::bounded_process(command, self.timeout, stop, MAX_CONVERTER_OUTPUT)?;
        normalize_ffmpeg_srt(staged)
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

    fn skip_target(
        &self,
        target: &SubtitleTarget,
        languages: &[String],
        existing: &ExistingCoverage,
    ) -> Option<&'static str> {
        if self.force_covered {
            return None;
        }
        if target.language == "und"
            && self.selected_tracks.is_none()
            && !languages.iter().any(|language| language == "und")
            && !self
                .targets
                .as_ref()
                .is_some_and(|targets| targets.iter().any(|target| target.language == "und"))
        {
            return Some(
                "Unknown-language tracks require an explicit track or und language selection",
            );
        }
        if self
            .targets
            .as_ref()
            .is_some_and(|targets| !targets.iter().any(|wanted| wanted.matches(target)))
        {
            return Some("This language and variant is not required by the inventory authority");
        }
        if existing.covers(target) {
            return Some("A valid SRT already covers this language and variant");
        }
        None
    }

    fn planning_target(&self, target: &SubtitleTarget) -> SubtitleTarget {
        self.targets
            .as_ref()
            .and_then(|targets| targets.iter().find(|wanted| wanted.matches(target)))
            .unwrap_or(target)
            .clone()
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
    static ALIASES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    ALIASES.get_or_init(load_language_aliases).clone()
}

fn load_language_aliases() -> BTreeMap<String, String> {
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
        ("pt", "por"),
        ("it", "ita"),
        ("ru", "rus"),
        ("ko", "kor"),
        ("nl", "nld"),
        ("dut", "nld"),
        ("pl", "pol"),
        ("sv", "swe"),
        ("da", "dan"),
        ("fi", "fin"),
        ("no", "nor"),
        ("tr", "tur"),
        ("hi", "hin"),
        ("uk", "ukr"),
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
fn target_language(language: &str, aliases: &BTreeMap<String, String>) -> String {
    let normalized = language.to_ascii_lowercase();
    match normalized.as_str() {
        "pb" | "pob" => return "pt-br".into(),
        "zt" | "zht" => return "zh-tw".into(),
        "ea" | "spl" => return "es-mx".into(),
        _ => {}
    }
    if !valid_language_code(&normalized) {
        return "und".into();
    }
    let (primary, suffix) = normalized.split_once('-').unwrap_or((&normalized, ""));
    let primary = short_language(primary, aliases);
    if suffix.is_empty() {
        primary
    } else {
        format!("{primary}-{suffix}")
    }
}

fn subtitle_name(
    path: &Path,
    aliases: &BTreeMap<String, String>,
) -> Option<(String, SubtitleTarget)> {
    let stem = path.file_stem()?.to_str()?;
    let mut parts: Vec<_> = stem.split('.').collect();
    let mut forced = false;
    let mut hi = false;
    while let Some(part) = parts.last() {
        if part.eq_ignore_ascii_case("forced") {
            forced = true;
        } else if ["sdh", "cc"]
            .iter()
            .any(|flag| part.eq_ignore_ascii_case(flag))
            || part.eq_ignore_ascii_case("hi")
                && parts.len() > 2
                && parts[..parts.len() - 1]
                    .iter()
                    .rev()
                    .find(|prior| {
                        !["forced", "sdh", "cc"]
                            .iter()
                            .any(|flag| prior.eq_ignore_ascii_case(flag))
                    })
                    .is_some_and(|prior| valid_language_code(prior))
        {
            hi = true;
        } else {
            break;
        }
        parts.pop();
    }
    let mut language = "und".to_owned();
    if parts.len() > 1
        && let Some(part) = parts.last()
        && valid_language_code(part)
    {
        language = target_language(part, aliases);
        parts.pop();
        if parts.last().is_some_and(|part| {
            part.get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("track"))
                && part.get(5..).is_some_and(|id| {
                    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
                })
        }) {
            parts.pop();
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some((
        parts.join("."),
        SubtitleTarget {
            language,
            forced,
            hi,
        },
    ))
}

fn media_stem(path: &Path, aliases: &BTreeMap<String, String>) -> Option<String> {
    if is_video(path) {
        Some(path.file_stem()?.to_str()?.to_owned())
    } else {
        subtitle_name(path, aliases).map(|(stem, _)| stem)
    }
}

fn same_language_family(left: &str, right: &str) -> bool {
    left == right
        || (!left.contains('-') && right.split('-').next() == Some(left))
        || (!right.contains('-') && left.split('-').next() == Some(right))
}

fn same_local_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
fn existing_coverage(
    path: &Path,
    aliases: &BTreeMap<String, String>,
) -> Result<ExistingCoverage, String> {
    existing_coverage_with_hints(path, aliases, &[])
}

fn existing_coverage_with_hints(
    path: &Path,
    aliases: &BTreeMap<String, String>,
    hints: &[SubtitleSourceHint],
) -> Result<ExistingCoverage, String> {
    let parent = path.parent().ok_or("Media path has no parent directory")?;
    let stem = media_stem(path, aliases).ok_or("Media filename is unavailable")?;
    let mut targets = BTreeSet::new();
    let mut sidecars = BTreeSet::new();
    let mut bytes = 0u64;
    for (ordinal, entry) in fs::read_dir(parent)
        .map_err(|_| "Subtitle coverage directory is unavailable")?
        .enumerate()
    {
        if ordinal >= MAX_COVERAGE_ENTRIES {
            return Err("Subtitle coverage directory exceeds the safe entry limit".into());
        }
        let entry = entry.map_err(|_| "Subtitle coverage directory could not be inspected")?;
        let file_type = entry
            .file_type()
            .map_err(|_| "Subtitle coverage file could not be inspected")?;
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let candidate = entry.path();
        let Some(extension) = extension(&candidate) else {
            continue;
        };
        if extension != "srt" && !SIDECAR_EXTENSIONS.contains(&extension.as_str()) {
            continue;
        }
        let Some((candidate_stem, mut target)) = subtitle_name(&candidate, aliases) else {
            continue;
        };
        if let Some(hint) = hints
            .iter()
            .find(|hint| same_local_path(&hint.path, &candidate))
        {
            target = hint.target.clone();
        }
        #[cfg(windows)]
        let matching = candidate_stem.eq_ignore_ascii_case(&stem);
        #[cfg(not(windows))]
        let matching = candidate_stem == stem;
        if !matching {
            continue;
        }
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|_| "Subtitle coverage file is unavailable")?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() == 0 {
            continue;
        }
        if extension != "srt" {
            sidecars.insert((target, candidate));
            if sidecars.len() > MAX_TRACKS {
                return Err("Subtitle coverage contains too many matching sidecars".into());
            }
            continue;
        }
        if metadata.len() > MAX_COVERAGE_FILE_BYTES {
            continue;
        }
        bytes = bytes.saturating_add(metadata.len());
        if bytes > MAX_COVERAGE_TOTAL_BYTES {
            return Err("Subtitle coverage inspection exceeds the safe byte limit".into());
        }
        if validate_srt(&candidate).is_ok()
            && fs::symlink_metadata(&candidate).is_ok_and(|after| {
                after.is_file()
                    && !after.file_type().is_symlink()
                    && after.len() == metadata.len()
                    && after.modified().ok() == metadata.modified().ok()
            })
        {
            targets.insert(target);
            if targets.len() > MAX_TRACKS {
                return Err("Subtitle coverage contains too many language variants".into());
            }
        }
    }
    Ok(ExistingCoverage {
        targets: targets.into_iter().collect(),
        sidecars: sidecars.into_iter().collect(),
    })
}

fn wants(language: &str, selected: &[String], aliases: &BTreeMap<String, String>) -> bool {
    let actual = target_language(language, aliases);
    selected.iter().any(|v| {
        if v == "*" {
            return true;
        }
        let requested = target_language(v, aliases);
        actual == requested
            || (!requested.contains('-') && actual.split('-').next() == Some(requested.as_str()))
    })
}
pub(crate) fn is_image_format(format: &str) -> bool {
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
    json!({"id":t.number,"number":t.number,"language":t.language,"format":t.format,"forced":t.forced,"hi":t.hi})
}

fn validate_selected_tracks(tracks: &[Track], ids: &[u64]) -> Result<(), String> {
    if ids.iter().any(|id| {
        tracks
            .iter()
            .filter(|track| track.number.as_u64() == Some(*id))
            .count()
            != 1
    }) {
        return Err(
            "Selected subtitle track is unknown or ambiguous; refresh the inventory".into(),
        );
    }
    Ok(())
}

pub fn validate_track_ids(description: &Value, ids: &[u64]) -> Result<(), String> {
    if ids.is_empty()
        || ids.len() > MAX_SELECTED_TRACKS
        || ids.iter().copied().collect::<BTreeSet<_>>().len() != ids.len()
    {
        return Err("Choose 1–64 unique subtitle track IDs".into());
    }
    if ids.iter().any(|id| {
        description
            .as_array()
            .into_iter()
            .flatten()
            .filter(|track| track["number"].as_u64() == Some(*id))
            .count()
            != 1
    }) {
        return Err(
            "Selected subtitle track is unknown or ambiguous; refresh the inventory".into(),
        );
    }
    Ok(())
}

fn failed_state(entry: &Value) -> bool {
    entry["ok"] == false
        && (entry["error"].is_string()
            || (entry["ocr_pending"] != true && entry["text_complete"] != true))
}

fn current_processing_entries(entry: &Value) -> impl Iterator<Item = &Value> {
    let fingerprint = entry
        .get("processing_fingerprint")
        .or_else(|| entry.get("fingerprint"));
    let scopes = entry["track_selections"]
        .as_object()
        .into_iter()
        .flat_map(|scopes| scopes.values());
    std::iter::once(entry).chain(scopes).filter(move |item| {
        fingerprint.is_none_or(|fingerprint| item.get("fingerprint") == Some(fingerprint))
            && item["inventory_resolved"] != true
    })
}

fn inventory_covers_scope(
    item: &Value,
    scope: Option<&str>,
    tracks: &[Value],
    existing: &[SubtitleTarget],
) -> bool {
    let covered =
        |wanted: &SubtitleTarget| existing.iter().any(|available| wanted.matches(available));
    if let Ok(targets) = serde_json::from_value::<Vec<SubtitleTarget>>(item["targets"].clone()) {
        return !targets.is_empty()
            && targets.len() <= MAX_SELECTED_TRACKS
            && targets.iter().all(covered);
    }
    let languages = item["selection_coverage"]["languages"]
        .as_array()
        .or_else(|| item["languages"].as_array());
    let ids = scope.map(|scope| {
        scope
            .split(',')
            .take(MAX_SELECTED_TRACKS + 1)
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
    });
    if matches!(ids, Some(Err(_))) || (scope.is_none() && languages.is_none()) {
        return false;
    }
    if ids.as_ref().is_some_and(|ids| {
        ids.as_ref()
            .is_ok_and(|ids| ids.len() > MAX_SELECTED_TRACKS)
    }) {
        return false;
    }
    if scope.is_none()
        && !languages.is_some_and(|languages| {
            !languages.is_empty()
                && languages.iter().all(|language| {
                    language == "*"
                        || tracks
                            .iter()
                            .take(MAX_TRACKS)
                            .any(|track| track["language"] == *language)
                })
        })
    {
        return false;
    }
    let mut matched = 0_usize;
    for track in tracks.iter().take(MAX_TRACKS) {
        if let Some(Ok(ids)) = &ids {
            if !track["number"].as_u64().is_some_and(|id| ids.contains(&id)) {
                continue;
            }
        } else if !languages.is_some_and(|languages| {
            languages
                .iter()
                .any(|language| language == "*" || language == &track["language"])
        }) {
            continue;
        }
        let Some(language) = track["language"]
            .as_str()
            .filter(|language| !matches!(*language, "und" | "unknown" | "zxx"))
        else {
            return false;
        };
        let (Some(forced), Some(hi)) = (track["forced"].as_bool(), track["hi"].as_bool()) else {
            return false;
        };
        if !covered(&SubtitleTarget {
            language: language.into(),
            forced,
            hi,
        }) {
            return false;
        }
        matched += 1;
    }
    matched > 0
        && ids
            .as_ref()
            .is_none_or(|ids| ids.as_ref().is_ok_and(|ids| matched == ids.len()))
}

fn resolved_selection(whole: &Value, selection: &Value) -> bool {
    if whole["ok"] != true || (!failed_state(selection) && selection["ocr_pending"] != true) {
        return false;
    }
    let whole_coverage = &whole["selection_coverage"];
    let completed_languages = whole_coverage
        .get("languages")
        .unwrap_or(&whole["languages"]);
    let all_languages = completed_languages
        .as_array()
        .is_some_and(|languages| languages.iter().any(|language| language == "*"));
    let coverage = &selection["selection_coverage"];
    let languages_covered = all_languages
        || coverage["languages"].as_array().is_some_and(|languages| {
            !languages.is_empty()
                && languages.iter().all(|language| {
                    completed_languages
                        .as_array()
                        .is_some_and(|completed| completed.contains(language))
                })
        });
    // Legacy scope entries lack precise pass coverage, so only a complete
    // whole-file text/OCR pass may resolve their failures.
    let text_needed = coverage["text"].as_bool().unwrap_or(true);
    let ocr_needed = coverage["ocr"].as_bool().unwrap_or(true) || selection["ocr_pending"] == true;
    let text_covered = whole_coverage["text"]
        .as_bool()
        .unwrap_or(whole["text_complete"] == true);
    let ocr_covered = whole_coverage["ocr"]
        .as_bool()
        .unwrap_or(whole["ocr"] == true);
    languages_covered
        && (!text_needed || text_covered)
        && (!ocr_needed || (ocr_covered && whole["ocr_pending"] != true))
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
fn parse_conversion_report(bytes: &[u8]) -> Result<Value, &'static str> {
    // SeConv 5.2's container loaders can print diagnostic lines even under
    // --json. Accept only the final, complete report envelope, never a nested
    // success property or truncated JSON. Diagnostic text remains private.
    let valid = |value: &Value| {
        value["success"].is_boolean()
            && value["totalFiles"].is_u64()
            && value["files"].is_array()
            && value["errors"].is_array()
    };
    if let Ok(value) = serde_json::from_slice::<Value>(bytes)
        && valid(&value)
    {
        return Ok(value);
    }
    // Only a top-level object at column zero can start a SeConv report.
    // Limit parser retries as well as the subprocess's overall byte limit.
    for (offset, _) in bytes
        .iter()
        .enumerate()
        .rev()
        .filter(|(offset, byte)| {
            **byte == b'{' && (*offset == 0 || bytes.get(offset - 1) == Some(&b'\n'))
        })
        .take(32)
    {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes[offset..])
            && valid(&value)
        {
            return Ok(value);
        }
    }
    Err(
        "OCR converter returned no complete conversion report; check runtime memory and converter availability",
    )
}

fn classify_ocr_failure(report: &Value, gpu: bool) -> String {
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
    if text.contains("out of memory")
        || text.contains("cannot allocate")
        || text.contains("memory limit")
    {
        "OCR exceeded available memory. Increase the worker memory limit or reduce concurrent OCR workers.".into()
    } else if gpu || text.contains("cuda") || text.contains("cudnn") || text.contains("gpu") {
        "GPU OCR failed. Check the granted CUDA runtime, available GPU memory, and packaged OCR models.".into()
    } else if text.contains("model") || text.contains("language") {
        "OCR failed. Check that the selected ocr-rs language model is packaged and supported."
            .into()
    } else {
        "OCR converter could not convert this track. Check the image subtitle format and logs."
            .into()
    }
}

fn normalize_ffmpeg_srt(path: &Path) -> Result<(), String> {
    if fs::metadata(path)
        .map_err(|_| "Subtitle output could not be read")?
        .len()
        > MAX_SRT_BYTES
    {
        return Err("Subtitle output exceeds the safe size limit".into());
    }
    let bytes = fs::read(path).map_err(|_| "Subtitle output could not be read")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "Subtitle output is not valid UTF-8")?;
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .trim()
        .lines()
        .peekable();
    let mut output = String::with_capacity(text.len());
    let mut ordinal = 0u64;
    while let Some(number) = lines.next() {
        ordinal += 1;
        if number.trim().parse::<u64>().ok() != Some(ordinal) {
            return Err(format!(
                "FFmpeg produced malformed SRT cue {ordinal}: invalid cue number"
            ));
        }
        let timing = lines.next().and_then(canonical_srt_timing).ok_or_else(|| {
            format!("FFmpeg produced malformed SRT cue {ordinal}: invalid timing line")
        })?;
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("{ordinal}\n{timing}\n"));
        let mut gap = false;
        while let Some(line) = lines.peek().copied() {
            if line.trim().is_empty() {
                lines.next();
                gap = true;
                continue;
            }
            if gap
                && line.trim().parse::<u64>().is_ok()
                && lines
                    .clone()
                    .nth(1)
                    .and_then(canonical_srt_timing)
                    .is_some()
            {
                break;
            }
            if canonical_srt_timing(line).is_some() {
                return Err(format!(
                    "FFmpeg produced malformed SRT cue {ordinal}: missing cue number"
                ));
            }
            // FFmpeg's ASS/SubRip encoder preserves repeated ASS line breaks
            // inside a cue. Blank body lines are illegal SRT separators; fold
            // those gaps while retaining every nonblank text line. This repair
            // applies only to fresh FFmpeg output, never arbitrary OCR files.
            output.push_str(line);
            output.push('\n');
            lines.next();
            gap = false;
        }
    }
    if output.len() as u64 > MAX_SRT_BYTES {
        return Err("Subtitle output exceeds the safe size limit".into());
    }
    if output.as_bytes() != bytes {
        fs::write(path, output).map_err(|_| "Could not normalize staged subtitle cues")?;
    }
    Ok(())
}

pub(crate) fn validate_srt(path: &Path) -> Result<(), String> {
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

    const COVERAGE_SRT: &[u8] = b"1\n00:00:00,000 --> 00:00:01,000\nDisposable subtitle fixture\n";

    fn target(language: &str, forced: bool, hi: bool) -> SubtitleTarget {
        SubtitleTarget {
            language: language.into(),
            forced,
            hi,
        }
    }

    fn coverage_fixture(directory: &Path, tracks: Vec<Track>) -> (Converter, PathBuf) {
        let media = directory.join("movie.mkv");
        fs::write(&media, b"disposable media").unwrap();
        let mut converter =
            Converter::new(vec![directory.to_path_buf()], directory.join("state.json")).unwrap();
        converter.min_age = Duration::ZERO;
        converter.enable_ocr = true;
        converter.ffprobe = directory.join("must-not-probe");
        converter.mkvmerge = directory.join("must-not-identify");
        converter.seconv = directory.join("must-not-ocr");
        converter.text_fixture = Some(COVERAGE_SRT);
        let media = Converter::safe_path(&media.to_string_lossy(), &converter.roots).unwrap();
        converter.metadata.lock().unwrap().insert(
            &media.to_string_lossy(),
            fingerprint_json(&fingerprint(&media).unwrap()),
            json!(tracks),
        );
        (converter, media)
    }

    fn fixture_track(number: u64, language: &str, image: bool, forced: bool, hi: bool) -> Track {
        Track {
            number: json!(number),
            native_number: Some(number.to_string()),
            stream_index: Some(number),
            language: language.into(),
            format: if image { "VobSub".into() } else { "ASS".into() },
            forced,
            hi,
        }
    }

    #[test]
    fn validated_publication_receipts_survive_a_later_state_write_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (mut converter, media) = coverage_fixture(
            directory.path(),
            vec![fixture_track(1, "en", false, false, false)],
        );
        let state_directory = directory.path().join("state-destination-is-a-directory");
        fs::create_dir(&state_directory).unwrap();
        converter.state_path = state_directory;
        let summary = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["en".into()],
                true,
                Workload::Text,
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(
            summary["failed"], 1,
            "final state cannot replace a directory"
        );
        let output = media.with_extension("en.srt");
        validate_srt(&output).unwrap();
        assert_eq!(
            converter.take_published_outputs(),
            vec![output.to_string_lossy().into_owned()]
        );
        assert!(
            converter.take_published_outputs().is_empty(),
            "receipts are consumed once"
        );
        let rejected = tempfile::tempdir().unwrap();
        let (mut converter, media) = coverage_fixture(
            rejected.path(),
            vec![fixture_track(1, "en", false, false, false)],
        );
        converter.text_fixture = Some(b"invalid subtitle fixture");
        let summary = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["en".into()],
                true,
                Workload::Text,
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(summary["failed"], 1);
        assert!(
            converter.take_published_outputs().is_empty(),
            "unvalidated staging files are never publication receipts"
        );
        assert!(!media.with_extension("en.srt").exists());
    }

    #[test]
    fn existing_en_fr_ar_and_unknown_outputs_skip_authoritative_targets_before_probe() {
        let dir = tempfile::tempdir().unwrap();
        let (mut converter, media) = coverage_fixture(dir.path(), Vec::new());
        for language in ["en", "fr", "ar"] {
            fs::write(
                dir.path().join(format!("movie.{language}.srt")),
                COVERAGE_SRT,
            )
            .unwrap();
        }
        for number in 1..=30 {
            fs::write(
                dir.path().join(format!("movie.track{number}.und.srt")),
                COVERAGE_SRT,
            )
            .unwrap();
        }
        converter.metadata.lock().unwrap().compact(|_| false);
        converter
            .set_targets(vec![
                target("eng", false, false),
                target("fre", false, false),
                target("ara", false, false),
            ])
            .unwrap();
        let mut events = Vec::new();
        let result = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                Workload::All,
                true,
                &AtomicBool::new(false),
                &mut |event| events.push(event),
            )
            .unwrap();
        assert_eq!(result["cached"], 1);
        assert_eq!(result["failed"], 0);
        assert!(events.iter().any(|event| event["status"] == "skipped"));
        assert!(
            !events
                .iter()
                .any(|event| event["status"] == "ocr" || event["status"] == "extracting")
        );
        assert_eq!(
            converter.file_processing(&media.to_string_lossy())["coverage"]["status"],
            "already_covered"
        );
        assert_eq!(converter.pipeline_counts()["generated_srt_count"], 0);
        assert_eq!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| extension(&entry.path()).as_deref() == Some("srt"))
                .count(),
            33
        );
        converter.set_targets(Vec::new()).unwrap();
        assert_eq!(
            converter
                .scan(
                    std::slice::from_ref(&media),
                    &["*".into()],
                    false,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .unwrap()["cached"],
            1
        );
    }

    #[test]
    fn coverage_is_variant_region_alias_and_movie_specific() {
        let dir = tempfile::tempdir().unwrap();
        let (converter, media) = coverage_fixture(dir.path(), Vec::new());
        for name in [
            "movie.ENG-US.SRT",
            "movie.fre.forced.srt",
            "movie.ar.SDH.srt",
            "movie.pt-PT.srt",
            "movie2.de.srt",
            "movie.track30.und.srt",
        ] {
            fs::write(dir.path().join(name), COVERAGE_SRT).unwrap();
        }
        fs::write(dir.path().join("movie.ja.srt"), b"").unwrap();
        fs::write(dir.path().join("movie.de.srt"), b"invalid subtitle").unwrap();
        let requested = vec![
            target("en", false, false),
            target("fr", false, false),
            target("fr", true, false),
            target("ar", false, false),
            target("ar", false, true),
            target("por-BR", false, false),
            target("de", false, false),
            target("ja", false, false),
        ];
        let plan = converter
            .conversion_coverage(&media, &json!([]), &requested)
            .unwrap();
        let missing: Vec<SubtitleTarget> =
            serde_json::from_value(plan["missing_targets"].clone()).unwrap();
        assert!(!missing.contains(&target("en", false, false)));
        assert!(!missing.contains(&target("fr", true, false)));
        assert!(!missing.contains(&target("ar", false, true)));
        for expected in [
            target("fr", false, false),
            target("ar", false, false),
            target("pt-br", false, false),
            target("de", false, false),
            target("ja", false, false),
        ] {
            assert!(missing.contains(&expected));
        }
        assert_eq!(plan["status"], "no_available_conversion");
    }

    #[test]
    fn wildcard_unknown_tracks_are_skipped_but_explicit_unknown_language_remains_available() {
        let dir = tempfile::tempdir().unwrap();
        let tracks = (1..=30)
            .map(|number| fixture_track(number, "und", false, false, false))
            .collect();
        let (mut converter, media) = coverage_fixture(dir.path(), tracks);
        let mut events = Vec::new();
        let result = converter
            .scan(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                &AtomicBool::new(false),
                &mut |event| events.push(event),
            )
            .unwrap();
        assert_eq!(result["failed"], 0);
        assert!(!events.iter().any(|event| event["status"] == "extracting"));
        assert_eq!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| extension(&entry.path()).as_deref() == Some("srt"))
                .count(),
            0
        );
        let result = converter
            .scan(
                std::slice::from_ref(&media),
                &["und".into()],
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(result["converted"], 1);
        assert!(dir.path().join("movie.track1.und.srt").is_file());
        assert!(!dir.path().join("movie.track2.und.srt").exists());
    }

    #[test]
    fn missing_language_uses_one_text_track_and_preserves_forced_variant() {
        let dir = tempfile::tempdir().unwrap();
        let tracks = vec![
            fixture_track(1, "eng", true, false, false),
            fixture_track(2, "eng", false, false, false),
            fixture_track(3, "eng", false, false, false),
            fixture_track(4, "eng", false, true, false),
        ];
        let (mut converter, media) = coverage_fixture(dir.path(), tracks);
        converter
            .set_targets(vec![target("en", false, false), target("en", true, false)])
            .unwrap();
        let mut events = Vec::new();
        let result = converter
            .scan(
                std::slice::from_ref(&media),
                &["en".into()],
                false,
                &AtomicBool::new(false),
                &mut |event| events.push(event),
            )
            .unwrap();
        assert_eq!(result["failed"], 0);
        assert!(dir.path().join("movie.en.srt").is_file());
        assert!(dir.path().join("movie.en.forced.srt").is_file());
        assert!(!dir.path().join("movie.track2.en.srt").exists());
        assert_eq!(
            events
                .iter()
                .filter(|event| event["status"] == "extracting")
                .count(),
            2
        );
        assert!(!events.iter().any(|event| event["status"] == "ocr"));
    }

    #[test]
    fn hindi_language_is_distinct_from_the_hearing_impaired_suffix() {
        let aliases = language_aliases();
        for (name, language, hi, forced) in [
            ("movie.hi.srt", "hi", false, false),
            ("movie.hi.forced.srt", "hi", false, true),
            ("movie.hi.hi.srt", "hi", true, false),
            ("movie.en.hi.srt", "en", true, false),
            ("movie.en.forced.hi.srt", "en", true, true),
        ] {
            let (_, parsed) = subtitle_name(Path::new(name), &aliases).unwrap();
            assert_eq!(parsed, target(language, forced, hi), "{name}");
        }
        let dir = tempfile::tempdir().unwrap();
        let (converter, media) = coverage_fixture(dir.path(), Vec::new());
        fs::write(dir.path().join("movie.hi.ass"), "Hindi source").unwrap();
        let plan = converter
            .conversion_coverage(&media, &json!([]), &[target("hi", false, false)])
            .unwrap();
        assert_eq!(plan["status"], "needed");
        fs::write(dir.path().join("movie.hi.srt"), COVERAGE_SRT).unwrap();
        let plan = converter
            .conversion_coverage(&media, &json!([]), &[target("hi", false, false)])
            .unwrap();
        assert_eq!(plan["status"], "already_covered");
    }

    #[test]
    fn bazarr_regional_filename_aliases_supply_and_cover_profile_targets() {
        for (alias, language) in [
            ("pb", "pt-br"),
            ("pob", "pt-br"),
            ("zt", "zh-tw"),
            ("zht", "zh-tw"),
            ("ea", "es-mx"),
            ("spl", "es-mx"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (converter, media) = coverage_fixture(dir.path(), Vec::new());
            fs::write(
                dir.path().join(format!("movie.{alias}.ass")),
                "Regional source",
            )
            .unwrap();
            let wanted = [target(language, false, false)];
            let plan = converter
                .conversion_coverage(&media, &json!([]), &wanted)
                .unwrap();
            assert_eq!(plan["status"], "needed", "{alias}");
            fs::write(dir.path().join(format!("movie.{alias}.srt")), COVERAGE_SRT).unwrap();
            let plan = converter
                .conversion_coverage(&media, &json!([]), &wanted)
                .unwrap();
            assert_eq!(plan["status"], "already_covered", "{alias}");
        }
    }

    #[test]
    fn explicit_inventory_metadata_converts_an_untagged_sidecar_once() {
        let dir = tempfile::tempdir().unwrap();
        let (mut converter, media) = coverage_fixture(dir.path(), Vec::new());
        let source = dir.path().join("movie.ass");
        fs::write(&source, b"Disposable source fixture").unwrap();
        let wanted = vec![target("fr", false, false)];
        assert_eq!(
            converter
                .conversion_coverage(&media, &json!([]), &wanted)
                .unwrap()["status"],
            "no_available_conversion"
        );
        converter
            .set_source_hints(vec![SubtitleSourceHint {
                path: source.clone(),
                target: wanted[0].clone(),
            }])
            .unwrap();
        assert_eq!(
            converter
                .conversion_coverage(&media, &json!([]), &wanted)
                .unwrap()["status"],
            "needed"
        );
        let mut worker = converter.file_worker(&source).unwrap();
        assert!(
            worker.source_hints().is_empty(),
            "hints do not leak between file workers"
        );
        worker
            .set_source_hints(converter.source_hints().to_vec())
            .unwrap();
        worker.set_targets(wanted.clone()).unwrap();
        let result = worker
            .scan(
                std::slice::from_ref(&source),
                &["*".into()],
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(result["converted"], 1);
        assert!(dir.path().join("movie.fr.srt").is_file());
        assert!(!dir.path().join("movie.srt").exists());
        assert!(!dir.path().join("movie.und.srt").exists());
        assert_eq!(
            converter
                .conversion_coverage(&media, &json!([]), &wanted)
                .unwrap()["status"],
            "already_covered"
        );
        assert!(
            converter
                .set_source_hints(vec![
                    SubtitleSourceHint {
                        path: source.clone(),
                        target: target("fr", false, false)
                    },
                    SubtitleSourceHint {
                        path: source,
                        target: target("en", false, false)
                    }
                ])
                .is_err()
        );
    }

    #[test]
    fn source_metadata_cannot_relabel_a_conflicting_filename_or_another_movie() {
        let dir = tempfile::tempdir().unwrap();
        let (mut converter, media) = coverage_fixture(dir.path(), Vec::new());
        let named = dir.path().join("movie.en.ass");
        fs::write(&named, "Disposable source").unwrap();
        assert!(
            converter
                .set_source_hints(vec![SubtitleSourceHint {
                    path: named,
                    target: target("fr", false, false)
                }])
                .is_err()
        );
        let other = dir.path().join("other.ass");
        fs::write(&other, "Unrelated source").unwrap();
        converter
            .set_source_hints(vec![SubtitleSourceHint {
                path: other,
                target: target("fr", false, false),
            }])
            .unwrap();
        assert_eq!(
            converter
                .conversion_coverage(&media, &json!([]), &[target("fr", false, false)])
                .unwrap()["status"],
            "no_available_conversion"
        );
        assert_eq!(
            Converter::normalize_languages(&json!(["por-BR", "pob", "zht", "spl"])).unwrap(),
            vec!["es-mx", "pt-br", "zh-tw"]
        );
    }

    #[test]
    fn sidecar_coverage_and_available_paths_use_matching_family() {
        let dir = tempfile::tempdir().unwrap();
        let (mut converter, media) = coverage_fixture(dir.path(), Vec::new());
        let source = dir.path().join("movie.fre.ASS");
        fs::write(&source, b"disposable ASS source").unwrap();
        fs::write(dir.path().join("othermovie.en.ass"), b"unrelated").unwrap();
        let plan = converter
            .conversion_coverage(&media, &json!([]), &[target("fr", false, false)])
            .unwrap();
        assert_eq!(plan["status"], "needed");
        assert_eq!(plan["available_paths"].as_array().unwrap().len(), 1);
        fs::write(dir.path().join("movie.fr.srt"), COVERAGE_SRT).unwrap();
        converter
            .set_targets(vec![target("fr", false, false)])
            .unwrap();
        let result = converter
            .scan(
                &[source],
                &["fr".into()],
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(result["cached"], 1);
        assert!(!dir.path().join("movie.fre.srt").exists());
    }

    #[test]
    fn coverage_rechecks_before_ocr_and_publication() {
        let dir = tempfile::tempdir().unwrap();
        let tracks = vec![
            fixture_track(1, "eng", false, false, false),
            fixture_track(2, "fra", true, false, false),
        ];
        let (mut converter, media) = coverage_fixture(dir.path(), tracks);
        converter
            .set_targets(vec![target("en", false, false), target("fr", false, false)])
            .unwrap();
        let mut events = Vec::new();
        let result = converter
            .scan(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                &AtomicBool::new(false),
                &mut |event| {
                    if event["status"] == "track_complete" {
                        fs::write(dir.path().join("movie.eng.srt"), COVERAGE_SRT).unwrap();
                        fs::write(dir.path().join("movie.fra.srt"), COVERAGE_SRT).unwrap();
                    }
                    events.push(event);
                },
            )
            .unwrap();
        assert_eq!(result["failed"], 0);
        assert!(!events.iter().any(|event| event["status"] == "ocr"));
        assert!(!dir.path().join("movie.en.srt").exists());
        assert!(!dir.path().join("movie.fr.srt").exists());
        assert_eq!(converter.pipeline_counts()["generated_srt_count"], 0);
    }

    #[test]
    fn force_requires_explicit_tracks_and_file_workers_do_not_inherit_targets() {
        let dir = tempfile::tempdir().unwrap();
        let (mut converter, media) = coverage_fixture(
            dir.path(),
            vec![fixture_track(1, "eng", false, false, false)],
        );
        assert!(converter.force_covered_tracks(true).is_err());
        converter
            .set_targets(vec![target("por-BR", false, false)])
            .unwrap();
        assert_eq!(
            converter.targets().unwrap(),
            &[target("pt-br", false, false)]
        );
        let worker = converter.file_worker(&media).unwrap();
        assert!(worker.targets().is_none());
        assert!(!worker.force_covered());
    }

    #[test]
    fn coverage_rejects_symlinks_and_enforces_directory_and_byte_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let (converter, media) = coverage_fixture(dir.path(), Vec::new());
        let other = dir.path().join("other.en.srt");
        fs::write(&other, COVERAGE_SRT).unwrap();
        let link = dir.path().join("movie.en.srt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, &link).unwrap();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&other, &link).is_ok();
        #[cfg(unix)]
        let linked = true;
        if linked {
            assert!(
                !existing_coverage(&media, &converter.language_aliases)
                    .unwrap()
                    .covers(&target("en", false, false))
            );
        }
        let huge = dir.path().join("movie.fr.srt");
        fs::File::create(&huge)
            .unwrap()
            .set_len(MAX_COVERAGE_FILE_BYTES + 1)
            .unwrap();
        assert!(
            !existing_coverage(&media, &converter.language_aliases)
                .unwrap()
                .covers(&target("fr", false, false))
        );
        for number in 0..MAX_COVERAGE_ENTRIES {
            fs::write(dir.path().join(format!("unrelated-{number}.fixture")), b"").unwrap();
        }
        assert!(
            existing_coverage(&media, &converter.language_aliases)
                .err()
                .unwrap()
                .contains("entry limit")
        );
    }

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
    fn ffmpeg_srt_body_gaps_preserve_text_and_strict_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ass.srt");
        fs::write(&path, "1\n00:00:00,000 --> 00:00:01,000\nFirst line\n\nSecond line\n\n2026\n\n2\n00:00:01,000 --> 00:00:02,000\nNext cue\n").unwrap();
        assert!(validate_srt(&path).is_err());
        normalize_ffmpeg_srt(&path).unwrap();
        assert!(validate_srt(&path).is_ok());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "1\n00:00:00,000 --> 00:00:01,000\nFirst line\nSecond line\n2026\n\n2\n00:00:01,000 --> 00:00:02,000\nNext cue\n"
        );
        for broken in [
            "bad\n00:00:00,000 --> 00:00:01,000\nText\n",
            "1\n00:00:00,000 --> 00:00:01,000\nText\n\nwrong\n00:00:01,000 --> 00:00:02,000\nText\n",
            "1\n00:00:00,000 --> 00:00:01,000\nText\n\n3\n00:00:01,000 --> 00:00:02,000\nText\n",
        ] {
            fs::write(&path, broken).unwrap();
            assert!(normalize_ffmpeg_srt(&path).is_err());
        }
    }

    #[test]
    fn ffmpeg_ass_repeated_line_breaks_are_repaired_after_real_conversion() {
        // Local, disposable text fixture; no deployment media or OCR required.
        let Ok(available) = Command::new("ffmpeg").arg("-version").output() else {
            return;
        };
        if !available.status.success() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.ass");
        let output = dir.path().join("fixture.srt");
        fs::write(&source, "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,1,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,First\\N\\NSecond\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Next\n").unwrap();
        let mut command = Command::new("ffmpeg");
        command
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(&source)
            .args(["-c:s", "srt", "-f", "srt"])
            .arg(&output);
        crate::util::bounded_process(
            &mut command,
            Duration::from_secs(15),
            &AtomicBool::new(false),
            4096,
        )
        .unwrap();
        assert!(validate_srt(&output).is_err());
        normalize_ffmpeg_srt(&output).unwrap();
        validate_srt(&output).unwrap();
        let repaired = fs::read_to_string(output).unwrap();
        assert!(repaired.contains("First") && repaired.contains("Second"));
        assert!(repaired.contains("\n\n2\n"));
        assert!(!repaired.contains("\n\nSecond"));
    }

    #[test]
    fn converter_report_accepts_diagnostic_prefix_but_not_partial_or_nested_success() {
        let report = br#"{"success":true,"totalFiles":1,"files":[{"success":true}],"errors":[]}"#;
        assert_eq!(parse_conversion_report(report).unwrap()["success"], true);
        let mut prefixed = b"Warning: VobSub OCR diagnostic\n".to_vec();
        prefixed.extend_from_slice(report);
        assert_eq!(parse_conversion_report(&prefixed).unwrap()["totalFiles"], 1);
        for invalid in [
            br#"{"success":true}"#.as_slice(),
            br#"{"success":false,"totalFiles":1,"files":[{"success":true}],"errors":[]"#,
            br#"{"unrelated":{"success":true,"totalFiles":1,"files":[],"errors":[]}}"#,
            br#"{"success":true,"totalFiles":1,"files":[],"errors":[]} trailing diagnostics"#,
            b"Killed",
        ] {
            assert!(parse_conversion_report(invalid).is_err());
        }
    }

    #[test]
    fn remote_worker_stages_without_publishing_beside_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.en.ass");
        fs::write(&source, b"disposable fixture source").unwrap();
        let remote = dir.path().join(".subforge-remote-fixture");
        fs::create_dir(&remote).unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.text_fixture = Some(b"1\n00:00:00,000 --> 00:00:01,000\nFixture\n");
        converter.enable_remote_staging(remote.clone()).unwrap();
        let source = Converter::safe_path(&source.to_string_lossy(), &converter.roots).unwrap();
        converter
            .convert(
                &source,
                &["*".into()],
                Workload::Text,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert!(!dir.path().join("fixture.en.srt").exists());
        assert_eq!(converter.staged_outputs(), vec!["fixture.en.srt"]);
        validate_srt(&remote.join("fixture.en.srt")).unwrap();
        let nested = remote.join(".subforge-remote-nested");
        fs::create_dir(&nested).unwrap();
        converter.enable_remote_staging(nested).unwrap();
        assert!(
            converter
                .convert(
                    &source,
                    &["*".into()],
                    Workload::Text,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .is_err()
        );
    }

    #[test]
    fn generated_totals_count_distinct_saved_receipts_without_claiming_file_existence() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("media");
        fs::create_dir(&root).unwrap();
        let first = root.join("first.mkv").to_string_lossy().into_owned();
        let second = root.join("second.mkv").to_string_lossy().into_owned();
        let mut converter =
            Converter::new(vec![root.clone()], fixture.path().join("state.json")).unwrap();
        converter.state = json!({first.clone():{"outputs":["old.en.srt","old.fr.srt"]},second.clone():{"outputs":["old.en.srt"]}});
        assert!(!root.join("old.en.srt").exists());
        let pipeline = converter.pipeline_counts();
        assert_eq!(pipeline["generated_srt_count"], 2);
        assert_eq!(
            pipeline["generated_srt_count_scope"],
            "saved_publication_receipts"
        );
        assert_eq!(pipeline["generated_srt_filesystem_verified"], false);
        assert_eq!(converter.file_processing(&first)["generated_srt_count"], 2);
        assert_eq!(converter.file_processing(&second)["generated_srt_count"], 1);
    }

    #[test]
    fn processing_exposes_latest_current_failure_with_bounded_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.state = json!({"fixture": {"fingerprint":[1],"ok":false,"error":"Old failure","attempt":1,
            "track_selections":{"2":{"fingerprint":[1],"ok":false,"error":"Latest failure","attempt":3},
                "3":{"fingerprint":[0],"ok":false,"error":"Stale failure","attempt":10}}}});
        assert_eq!(
            converter.file_processing("fixture")["last_error"],
            "Latest failure"
        );
        assert!(converter.file_processing("missing")["last_error"].is_null());
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
    fn selected_same_language_track_extracts_only_its_stream_and_has_independent_cache() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("selected.mkv");
        fs::write(&media, b"disposable media fixture").unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.min_age = Duration::ZERO;
        converter.text_fixture = Some(b"1\n00:00:00,000 --> 00:00:01,000\nFixture subtitle\n");
        let media = Converter::safe_path(&media.to_string_lossy(), &converter.roots).unwrap();
        let tracks: Vec<_> = [(3, 7), (4, 11)]
            .into_iter()
            .map(|(number, stream)| Track {
                number: json!(number),
                native_number: Some(number.to_string()),
                stream_index: Some(stream),
                language: "eng".into(),
                format: "SubRip".into(),
                forced: false,
                hi: false,
            })
            .collect();
        converter.metadata.lock().unwrap().insert(
            &media.to_string_lossy(),
            fingerprint_json(&fingerprint(&media).unwrap()),
            json!(tracks),
        );
        converter.set_state_entry(&media.to_string_lossy(), json!({"ok":true,"fingerprint":fingerprint_json(&fingerprint(&media).unwrap()),"languages":["*"],"outputs":[]}));
        converter.select_tracks(vec![4]).unwrap();
        let mut events = Vec::new();
        let first = converter
            .scan(
                std::slice::from_ref(&media),
                &["fr".into()],
                false,
                &AtomicBool::new(false),
                &mut |event| events.push(event),
            )
            .unwrap();
        assert_eq!(first["converted"], 1);
        let extracting: Vec<_> = events
            .iter()
            .filter(|event| event["status"] == "extracting")
            .collect();
        assert_eq!(extracting.len(), 1);
        assert_eq!(extracting[0]["track"], 11);
        assert!(dir.path().join("selected.track4.en.srt").is_file());
        assert!(!dir.path().join("selected.track3.en.srt").exists());
        let second = converter
            .scan(
                std::slice::from_ref(&media),
                &["fr".into()],
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(second["cached"], 1);
        converter.select_tracks(vec![3]).unwrap();
        converter.force_covered_tracks(true).unwrap();
        let third = converter
            .scan(
                std::slice::from_ref(&media),
                &["en".into()],
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(third["converted"], 1);
        assert!(dir.path().join("selected.track3.en.srt").is_file());
        assert_eq!(converter.pipeline_counts()["generated_srt_count"], 2);
        converter.selected_tracks = None;
        converter.force_covered_tracks(false).unwrap();
        let legacy = converter
            .scan(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(legacy["cached"], 1);
        converter.select_tracks(vec![99]).unwrap();
        assert!(
            converter
                .scan(
                    &[media],
                    &["*".into()],
                    false,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .is_err()
        );
    }

    #[test]
    fn vobsub_selection_uses_section_ids_even_with_duplicate_languages() {
        let dir = tempfile::tempdir().unwrap();
        let idx = dir.path().join("selected.idx");
        fs::write(&idx, "langidx: 0\nid: en, index: 2\ntimestamp: 00:00:00:000, filepos: 000000\nid: en, index: 7\ntimestamp: 00:00:01:000, filepos: 000010\n").unwrap();
        fs::write(idx.with_extension("sub"), b"disposable payload").unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.select_tracks(vec![7]).unwrap();
        converter.enable_ocr = true;
        converter.seconv = dir.path().join("missing-tool");
        let description = converter.describe(&idx).unwrap();
        assert_eq!(description[1]["id"], 7);
        validate_track_ids(&description, &[7]).unwrap();
        assert!(validate_track_ids(&description, &[1]).is_err());
        let mut events = Vec::new();
        assert!(
            converter
                .convert(
                    &idx,
                    &["fr".into()],
                    Workload::All,
                    &AtomicBool::new(false),
                    &mut |event| events.push(event)
                )
                .is_err()
        );
        let extracting: Vec<_> = events
            .iter()
            .filter(|event| event["status"] == "ocr")
            .collect();
        assert_eq!(extracting.len(), 1);
        assert_eq!(extracting[0]["output"], "selected.track7.en.srt");
    }

    #[test]
    fn successful_whole_file_retry_resolves_same_fingerprint_selected_failure() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("retry.mkv");
        fs::write(&media, b"disposable source").unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.min_age = Duration::ZERO;
        converter.enable_ocr = false;
        converter.ffmpeg = dir.path().join("missing-fixture-tool");
        let media = Converter::safe_path(&media.to_string_lossy(), &converter.roots).unwrap();
        let track = Track {
            number: json!(3),
            native_number: Some("3".into()),
            stream_index: Some(7),
            language: "eng".into(),
            format: "SubRip".into(),
            forced: false,
            hi: false,
        };
        converter.metadata.lock().unwrap().insert(
            &media.to_string_lossy(),
            fingerprint_json(&fingerprint(&media).unwrap()),
            json!([track]),
        );
        converter.select_tracks(vec![3]).unwrap();
        let failed = converter
            .scan(
                std::slice::from_ref(&media),
                &["fr".into()],
                true,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(failed["failed"], 1);
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
        assert_eq!(
            converter.file_processing(&media.to_string_lossy())["failed"],
            true
        );
        assert_eq!(
            converter.state[media.to_string_lossy().as_ref()]["track_selections"]["3"]["selection_coverage"]
                ["languages"],
            json!(["en"])
        );
        converter.selected_tracks = None;
        converter.text_fixture = Some(b"1\n00:00:00,000 --> 00:00:01,000\nFixture subtitle\n");
        let retried = converter
            .scan(
                std::slice::from_ref(&media),
                &["en".into()],
                true,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(retried["converted"], 1);
        assert_eq!(converter.pipeline_counts()["failed_files"], 0);
        assert_eq!(
            converter.file_processing(&media.to_string_lossy())["failed"],
            false
        );
        assert!(
            converter.state[media.to_string_lossy().as_ref()]["track_selections"]
                .get("3")
                .is_none()
        );
        let restarted = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        assert_eq!(restarted.pipeline_counts()["failed_files"], 0);
    }

    #[test]
    fn valid_whole_file_cache_hit_also_resolves_covered_selected_failure() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("cached-retry.mkv");
        fs::write(&media, b"disposable source").unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        converter.min_age = Duration::ZERO;
        converter.enable_ocr = false;
        let media = Converter::safe_path(&media.to_string_lossy(), &converter.roots).unwrap();
        let fp = fingerprint_json(&fingerprint(&media).unwrap());
        converter.set_state_entry(&media.to_string_lossy(),json!({"fingerprint":fp,"ok":true,"languages":["*"],"text_complete":true,"ocr":false,"outputs":[],"track_selections":{
            "3":{"fingerprint":fp,"ok":false,"error":"Retry fixture","selection_coverage":{"languages":["en"],"text":true,"ocr":false}}
        }}));
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
        let result = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["en".into()],
                true,
                Workload::Text,
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(result["cached"], 1);
        assert_eq!(converter.pipeline_counts()["failed_files"], 0);
        assert_eq!(
            converter.file_processing(&media.to_string_lossy())["failed"],
            false
        );
        let restarted = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        assert_eq!(restarted.pipeline_counts()["failed_files"], 0);
    }

    #[test]
    fn source_change_excludes_stale_scopes_and_successful_whole_pass_prunes_them() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir
            .path()
            .join("changed.mkv")
            .to_string_lossy()
            .into_owned();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let old = json!([[10, 1000]]);
        let current = json!([[20, 2000]]);
        converter.set_state_entry(
            &source,
            json!({"fingerprint":current,"ok":true,"outputs":["changed.en.srt"],"track_selections":{
                "3":{"fingerprint":old,"ok":false,"error":"Old failure"},
                "4":{"fingerprint":old,"ok":true,"ocr_pending":true}
            }}),
        );
        assert_eq!(converter.pipeline_counts()["failed_files"], 0);
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 0);
        assert_eq!(converter.file_processing(&source)["failed"], false);
        assert_eq!(converter.file_processing(&source)["pending_ocr"], false);
        converter.set_scoped_state_entry(&source,None,json!({"fingerprint":current,"ok":true,"languages":["*"],"text_complete":true,"ocr":true,"outputs":["changed.en.srt"]})).unwrap();
        assert!(
            converter.state[&source]["track_selections"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        // A new selected pass also hides a prior whole-file failure, without
        // replacing the old whole-file cache fingerprint or tracked outputs.
        converter.set_state_entry(&source,json!({"fingerprint":old,"ok":false,"error":"Old whole-file failure","outputs":["changed.en.srt"]}));
        converter
            .set_scoped_state_entry(
                &source,
                Some("5"),
                json!({"fingerprint":current,"ok":true,"outputs":[]}),
            )
            .unwrap();
        assert_eq!(converter.state[&source]["fingerprint"], old);
        assert_eq!(converter.pipeline_counts()["failed_files"], 0);
        assert_eq!(converter.pipeline_counts()["generated_srt_count"], 1);
    }

    #[test]
    fn whole_file_coverage_retains_unresolved_ocr_other_languages_and_success_caches() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir
            .path()
            .join("coverage.mkv")
            .to_string_lossy()
            .into_owned();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let fingerprint = json!([[10, 1000]]);
        converter.set_state_entry(&source,json!({"fingerprint":fingerprint,"outputs":[],"track_selections":{
            "3":{"fingerprint":fingerprint,"ok":true,"ocr_pending":true,"selection_coverage":{"languages":["en"],"text":false,"ocr":false}},
            "4":{"fingerprint":fingerprint,"ok":false,"error":"French OCR failure","selection_coverage":{"languages":["fr"],"text":false,"ocr":true}},
            "5":{"fingerprint":fingerprint,"ok":true,"outputs":[],"languages":["en"]}
        }}));
        converter.set_scoped_state_entry(&source,None,json!({"fingerprint":fingerprint,"ok":true,"languages":["en","fr"],"text_complete":true,"ocr":true,"outputs":[],"selection_coverage":{"languages":["en"],"text":true,"ocr":false}})).unwrap();
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 1);
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
        assert_eq!(
            converter.state[&source]["track_selections"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
        converter.set_scoped_state_entry(&source,None,json!({"fingerprint":fingerprint,"ok":true,"languages":["en"],"text_complete":true,"ocr":true,"outputs":[]})).unwrap();
        assert!(
            converter.state[&source]["track_selections"]
                .get("3")
                .is_none()
        );
        assert!(
            converter.state[&source]["track_selections"]
                .get("4")
                .is_some()
        );
        assert!(
            converter.state[&source]["track_selections"]
                .get("5")
                .is_some()
        );
        converter.set_scoped_state_entry(&source,None,json!({"fingerprint":fingerprint,"ok":true,"languages":["*"],"text_complete":true,"ocr":true,"outputs":[]})).unwrap();
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 0);
        assert_eq!(converter.pipeline_counts()["failed_files"], 0);
        assert!(
            converter.state[&source]["track_selections"]
                .get("5")
                .is_some()
        );
    }

    #[test]
    fn track_ids_reject_empty_duplicate_unknown_and_ambiguous_selections() {
        let tracks = json!([{"number":3},{"number":4},{"number":null}]);
        for ids in [vec![], vec![3, 3], vec![0], vec![99], vec![3; 65]] {
            assert!(validate_track_ids(&tracks, &ids).is_err());
        }
        assert!(validate_track_ids(&json!([{"number":3},{"number":3}]), &[3]).is_err());
        validate_track_ids(&tracks, &[3, 4]).unwrap();
    }

    #[test]
    fn inventory_coverage_resolves_only_matching_processing_and_preserves_receipts_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("inventory.mkv");
        fs::write(&source, b"disposable media").unwrap();
        let key = source.to_string_lossy().into_owned();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let stamp = fingerprint_json(&fingerprint(&source).unwrap());
        let target = json!({"language":"en","forced":false,"hi":false});
        converter.set_state_entry(&key, json!({"fingerprint":stamp,"ok":false,"error":"Preserved conversion failure","ocr_pending":true,"targets":[target],"outputs":["inventory.en.srt"],"track_selections":{
            "3":{"fingerprint":stamp,"ok":false,"error":"Preserved selected failure","ocr_pending":true,"selection_coverage":{"languages":["en"]}}
        }}));
        let mut row = json!({"path":source,"tracks":[{"number":3,"language":"en","forced":false,"hi":false}],"coverage":{"status":"covered","existing_targets":[target]}});
        assert!(
            converter
                .reconcile_inventory_processing(std::slice::from_ref(&row))
                .unwrap()
        );
        assert_eq!(converter.pipeline_counts()["failed_files"], 0);
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 0);
        assert_eq!(converter.pipeline_counts()["generated_srt_count"], 1);
        assert_eq!(
            converter.state[&key]["error"],
            "Preserved conversion failure"
        );
        assert_eq!(
            converter.state[&key]["track_selections"]["3"]["error"],
            "Preserved selected failure"
        );
        assert_eq!(converter.file_processing(&key)["failed"], false);
        assert!(converter.file_processing(&key)["last_error"].is_null());
        assert!(
            !converter
                .reconcile_inventory_processing(std::slice::from_ref(&row))
                .unwrap()
        );
        let restarted = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        assert_eq!(restarted.pipeline_counts()["failed_files"], 0);
        row["coverage"] = json!({"status":"needs_conversion","existing_targets":[]});
        converter.reconcile_inventory_processing(&[row]).unwrap();
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 1);
    }

    #[test]
    fn inventory_coverage_keeps_unknown_languages_variants_and_other_scopes_visible() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("scope.mkv");
        fs::write(&source, b"disposable media").unwrap();
        let key = source.to_string_lossy().into_owned();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let stamp = fingerprint_json(&fingerprint(&source).unwrap());
        converter.set_state_entry(&key, json!({"fingerprint":stamp,"ok":false,"error":"Unresolved language","languages":["en","fr"],"track_selections":{
            "3":{"fingerprint":stamp,"ok":false,"error":"Covered English"},
            "4":{"fingerprint":stamp,"ok":false,"error":"French forced","ocr_pending":true},
            "5":{"fingerprint":stamp,"ok":false,"error":"Unknown language"},
            "99":{"fingerprint":stamp,"ok":false,"error":"Missing metadata"}
        }}));
        let row = json!({"path":source,"tracks":[{"number":3,"language":"en","forced":false,"hi":false},{"number":4,"language":"fr","forced":true,"hi":false},{"number":5,"language":"und","forced":false,"hi":false}],"coverage":{"status":"covered","existing_targets":[{"language":"en","forced":false,"hi":false},{"language":"fr","forced":false,"hi":false}]}});
        converter.reconcile_inventory_processing(&[row]).unwrap();
        assert_eq!(
            converter.state[&key]["track_selections"]["3"]["inventory_resolved"],
            true
        );
        for scope in ["4", "5", "99"] {
            assert_ne!(
                converter.state[&key]["track_selections"][scope]["inventory_resolved"],
                true
            );
        }
        assert_ne!(converter.state[&key]["inventory_resolved"], true);
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 1);
        // An unversioned legacy error is ambiguous and must remain visible.
        converter.set_state_entry(&key, json!({"ok":false,"error":"Legacy unknown failure"}));
        converter
            .reconcile_inventory_processing(&[
                json!({"path":source,"coverage":{"status":"covered","existing_targets":[]}}),
            ])
            .unwrap();
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
    }

    #[test]
    fn inventory_observation_never_hides_changed_source_or_unrelated_coverage_failures() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("changed.mkv");
        fs::write(&source, b"original disposable media").unwrap();
        let key = source.to_string_lossy().into_owned();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let stamp = fingerprint_json(&fingerprint(&source).unwrap());
        converter.set_state_entry(&key,json!({"fingerprint":stamp,"processing_fingerprint":stamp,"ok":false,"error":"French OCR failure","ocr_pending":true,"targets":[{"language":"fr","forced":false,"hi":false}],"coverage":{"status":"already_covered","existing_targets":[{"language":"en","forced":false,"hi":false}]}}));
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
        assert_eq!(converter.file_processing(&key)["failed"], true);
        fs::write(&source, b"changed source with different length").unwrap();
        assert!(
            !converter
                .reconcile_inventory_processing(&[
                    json!({"path":source,"coverage":{"status":"unknown","existing_targets":[]}})
                ])
                .unwrap()
        );
        assert_eq!(converter.state[&key]["processing_fingerprint"], stamp);
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 1);
    }

    #[test]
    fn inventory_resolution_rolls_back_on_persist_failure_and_retries_later() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("retry.mkv");
        fs::write(&source, b"disposable media").unwrap();
        let key = source.to_string_lossy().into_owned();
        let state_path = dir.path().join("state.json");
        let mut converter =
            Converter::new(vec![dir.path().to_path_buf()], state_path.clone()).unwrap();
        let stamp = fingerprint_json(&fingerprint(&source).unwrap());
        let target = json!({"language":"en","forced":false,"hi":false});
        converter.set_state_entry(&key,json!({"fingerprint":stamp,"ok":false,"error":"Preserved failure","ocr_pending":true,"targets":[target],"track_selections":{"3":{"fingerprint":stamp,"ok":false,"error":"Preserved selected failure","targets":[target]}}}));
        let before = converter.state.clone();
        fs::create_dir(&state_path).unwrap();
        let rows =
            [json!({"path":source,"coverage":{"status":"covered","existing_targets":[target]}})];
        assert!(converter.reconcile_inventory_processing(&rows).is_err());
        assert_eq!(converter.state, before);
        assert_eq!(converter.pipeline_counts()["failed_files"], 1);
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 1);
        fs::remove_dir(&state_path).unwrap();
        assert!(converter.reconcile_inventory_processing(&rows).unwrap());
        assert_eq!(converter.pipeline_counts()["failed_files"], 0);
        assert_eq!(converter.pipeline_counts()["pending_ocr_files"], 0);
    }

    #[test]
    fn tracked_output_counts_become_unavailable_at_the_explicit_allocation_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut converter = Converter::new(
            vec![dir.path().to_path_buf()],
            dir.path().join("state.json"),
        )
        .unwrap();
        let source = dir
            .path()
            .join("fixture.mkv")
            .to_string_lossy()
            .into_owned();
        let outputs: Vec<_> = (0..=MAX_TRACKED_OUTPUTS)
            .map(|index| format!("fixture.{index}.srt"))
            .collect();
        converter.set_state_entry(&source, json!({"ok":true,"outputs":outputs}));
        let summary = converter.pipeline_counts();
        assert!(summary["generated_srt_count"].is_null());
        assert_eq!(summary["generated_srt_count_available"], false);
        assert!(converter.file_processing(&source)["generated_srt_count"].is_null());
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
    fn gpu_health_is_not_probed_for_cached_or_noop_files() {
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
        let media = Converter::safe_path(&media.to_string_lossy(), &converter.roots).unwrap();
        cache_image_track(&converter, &media);
        fs::write(dir.path().join("image.en.srt"), b"existing output").unwrap();
        converter.enable_gpu_ocr(2048);
        let first = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                Workload::All,
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(first["converted"], 1);
        assert!(converter.gpu_status().0.is_none());
        assert!(converter.gpu_status().1.is_none());
        let second = converter
            .scan_workload(
                std::slice::from_ref(&media),
                &["*".into()],
                false,
                Workload::All,
                false,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(second["cached"], 1);
        assert!(converter.gpu_status().0.is_none());
        assert!(converter.gpu_status().1.is_none());
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
        fs::write(
            &idx,
            (0..100)
                .map(|index| format!("id: en, index: {index}\n"))
                .collect::<String>(),
        )
        .unwrap();
        assert_eq!(
            converter
                .describe_with_stop(&idx, &stop)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            100
        );
    }
}
