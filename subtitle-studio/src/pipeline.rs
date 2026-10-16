//! Resumable, sequential subtitle processing. Transcript bodies live only in
//! bounded disk caches; job records contain paths, progress and compact reports.
use crate::config::Config;
use crate::llm::{self, REVIEW_BATCH_SIZE, REVIEW_METHOD, Rating};
use crate::storage;
use crate::subtitle::{self, Cue};
use crate::{App, Work};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const CANCELLED: &str = "Cancelled by user";

struct Source {
    key: String,
    language: String,
    cues: Vec<Cue>,
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn valid_language(language: &str) -> bool {
    (2..=3).contains(&language.len()) && language.bytes().all(|byte| byte.is_ascii_lowercase())
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn valid_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Reject traversal and every existing symlink component before accessing a
/// configured writable root. Roots remain an administrator trust boundary.
fn contained(root: &Path, destination: &Path) -> Result<PathBuf, String> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| "Subtitle path is outside the configured output folders")?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Invalid subtitle path".into());
    }
    let mut checked = root.to_path_buf();
    if fs::symlink_metadata(&checked).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Subtitle roots must not be symlinks".into());
    }
    for part in relative.components() {
        checked.push(part);
        match fs::symlink_metadata(&checked) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Subtitle paths must not contain symlinks".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Subtitle path cannot be inspected".into()),
        }
    }
    Ok(checked)
}

fn relative_media(job: &Value) -> Result<PathBuf, String> {
    let value = string(job, "path");
    if value.is_empty() || value.contains(['\\', '\0', ':']) || value.starts_with('/') {
        return Err("Invalid media path".into());
    }
    if value.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err("Media path must stay inside the library".into());
    }
    let relative = Path::new(value);
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Media path must stay inside the library".into());
    }
    Ok(relative.to_owned())
}

fn export_path(config: &Config, job: &Value, language: &str) -> Result<PathBuf, String> {
    if !valid_language(language) {
        return Err("Invalid subtitle language".into());
    }
    let relative = relative_media(job)?;
    let name = relative
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("Invalid media filename")?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    contained(
        &config.output_root,
        &config
            .output_root
            .join(parent)
            .join(format!("{name}.{language}.srt")),
    )
}

/// Prefer immutable per-job snapshots. Legacy exports remain downloadable when
/// the recorded path or snapshot was removed, provided containment still holds.
pub fn output_path(config: &Config, job: &Value, language: &str) -> Result<PathBuf, String> {
    if !valid_language(language) {
        return Err("Invalid subtitle language".into());
    }
    let output = job
        .get("outputs")
        .and_then(|outputs| outputs.get(language))
        .ok_or("Saved subtitle file is missing")?;
    let destination = export_path(config, job, language)?;
    let id = string(job, "id");
    if crate::valid_id(id) {
        let root = config.data_root.join("job_outputs");
        let name = destination.file_name().ok_or("Missing subtitle filename")?;
        let snapshot = contained(&root, &root.join(id).join(name))?;
        if snapshot.is_file() {
            return Ok(snapshot);
        }
    }
    for key in ["path", "export_path"] {
        if let Some(path) = output.get(key).and_then(Value::as_str) {
            let path = Path::new(path);
            for root in [&config.data_root.join("job_outputs"), &config.output_root] {
                if let Ok(path) = contained(root, path)
                    && path.is_file()
                {
                    return Ok(path);
                }
            }
        }
    }
    let path = export_path(config, job, language)?;
    if path.is_file() {
        Ok(path)
    } else {
        Err("Saved subtitle file is missing".into())
    }
}

fn read_srt(config: &Config, job: &Value, language: &str) -> Result<Vec<Cue>, String> {
    let path = output_path(config, job, language)?;
    let file = fs::File::open(path).map_err(|_| "Saved subtitle file cannot be opened")?;
    let limit = config
        .max_json_bytes
        .min(subtitle::MAX_SUBTITLE_BYTES as u64);
    if file
        .metadata()
        .map_err(|_| "Saved subtitle file cannot be inspected")?
        .len()
        > limit
    {
        return Err("Saved subtitle exceeds the configured size limit".into());
    }
    let mut content = String::new();
    file.take(limit.saturating_add(1))
        .read_to_string(&mut content)
        .map_err(|_| "Saved subtitles are not valid UTF-8")?;
    if content.len() as u64 > limit {
        return Err("Saved subtitle exceeds the configured size limit".into());
    }
    subtitle::parse_srt(&content)
}

fn write_output(app: &App, id: &str, language: &str, cues: &[Cue]) -> Result<(), String> {
    subtitle::validate(cues)?;
    let job = app.job(id)?;
    let destination = export_path(&app.config, &job, language)?;
    let filename = destination.file_name().ok_or("Missing subtitle filename")?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("Invalid job ID".into());
    }
    let snapshots = app.config.data_root.join("job_outputs");
    let snapshot = contained(&snapshots, &snapshots.join(id).join(filename))?;
    let content = subtitle::render_srt(cues);
    let step = if string(&job, "detected_language") == language {
        "transcription".to_owned()
    } else {
        format!("translation:{language}")
    };
    let output = json!({"path":snapshot,"export_path":destination,"name":filename.to_string_lossy(),"url":format!("/api/jobs/{id}/files/{language}")});
    app.update_checked(id, &step, |job| {
        storage::write_bytes(&destination, content.as_bytes())?;
        storage::write_bytes(&snapshot, content.as_bytes())?;
        if !job.get("outputs").is_some_and(Value::is_object) {
            job["outputs"] = json!({});
        }
        job["outputs"][language] = output;
        if !job["steps"].is_object() {
            job["steps"] = json!({});
        }
        if !job["steps"][&step].is_object() {
            job["steps"][&step] = json!({"label":step});
        }
        job["steps"][&step]["status"] = json!("completed");
        job["steps"][&step]["progress"] = json!(100);
        job["steps"][&step]["error"] = Value::Null;
        Ok(())
    })
}

fn cache_path(config: &Config, kind: &str, key: &str) -> Result<PathBuf, String> {
    if !valid_key(key) || !matches!(kind, "transcripts" | "translations" | "quality") {
        return Err("Invalid cache identity".into());
    }
    let root = config.data_root.join("cache");
    contained(&root, &root.join(kind).join(format!("{key}.json")))
}

fn read_cache(config: &Config, kind: &str, key: &str) -> Option<Value> {
    let path = cache_path(config, kind, key).ok()?;
    let value = storage::read_json(&path, config.max_json_bytes).ok()?;
    (value.get("key").and_then(Value::as_str) == Some(key)).then_some(value)
}

fn write_cache<T: Serialize>(
    config: &Config,
    kind: &str,
    key: &str,
    value: &T,
) -> Result<(), String> {
    #[derive(Serialize)]
    struct Envelope<'a, T> {
        key: &'a str,
        #[serde(flatten)]
        payload: &'a T,
    }
    struct BoundedWriter<W> {
        writer: W,
        remaining: u64,
    }
    impl<W: std::io::Write> std::io::Write for BoundedWriter<W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() as u64 > self.remaining {
                return Err(std::io::Error::other("Cache exceeds configured size limit"));
            }
            let written = self.writer.write(bytes)?;
            self.remaining -= written as u64;
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.writer.flush()
        }
    }
    let path = cache_path(config, kind, key)?;
    let parent = path.parent().ok_or("Missing cache directory")?;
    fs::create_dir_all(parent).map_err(|_| "Could not create cache directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "Could not create cache file")?;
    {
        let writer = std::io::BufWriter::with_capacity(16 * 1024, temporary.as_file_mut());
        let mut bounded = BoundedWriter {
            writer,
            remaining: config.max_json_bytes,
        };
        serde_json::to_writer(
            &mut bounded,
            &Envelope {
                key,
                payload: value,
            },
        )
        .map_err(|_| "Could not encode cache within configured size limit")?;
        std::io::Write::flush(&mut bounded).map_err(|_| "Could not write cache")?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| "Could not save cache")?;
    temporary
        .persist(path)
        .map_err(|_| "Could not publish cache")?;
    Ok(())
}

// Alphabetical field declaration order retains Python's sorted-key cache
// identities while streaming directly into SHA-256, without a deep JSON copy.
fn digest<T: Serialize>(value: &T) -> String {
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).expect("JSON-compatible cache identity serializes");
    format!("{:x}", writer.0.finalize())
}

#[derive(Serialize)]
struct TranscriptCache<'a> {
    language: &'a str,
    cues: &'a [Cue],
}
#[derive(Serialize)]
struct TranslationCache<'a> {
    texts: &'a [String],
}
#[derive(Serialize)]
struct QualityCache<'a> {
    method: &'a str,
    ratings: &'a [Rating],
}

fn load_transcript(config: &Config, key: &str) -> Option<Source> {
    let mut value = read_cache(config, "transcripts", key)?;
    let language = value.get("language")?.as_str()?.to_owned();
    if !valid_language(&language) {
        return None;
    }
    let cues: Vec<Cue> = serde_json::from_value(value.get_mut("cues")?.take()).ok()?;
    subtitle::validate(&cues).ok()?;
    Some(Source {
        key: key.to_owned(),
        language,
        cues,
    })
}

fn transcript_key(
    job: &Value,
    fingerprint: &Value,
    rust_engine: bool,
    legacy_options: bool,
) -> String {
    let model = match string(job, "whisper_model") {
        "" => "medium",
        value => value,
    };
    let mut payload = json!({"kind":"transcript","version":1,"media":fingerprint,
        "audio_stream_index":job.get("audio_stream_index"),"audio_offset":job.get("audio_offset"),
        "source_language":job.get("source_language"),"whisper_model":model,
        "whisper_options":{"beam_size":5,"vad_filter":true,"word_timestamps":true}});
    if !legacy_options {
        payload["whisper_options"]["condition_on_previous_text"] = json!(false);
    }
    if rust_engine {
        payload["engine"] = json!("whisper.cpp-cli-v1");
        // Rust CLI segmentation intentionally has a separate transcription identity.
        // Retain the legacy json-full identity: ordinary JSON omits unused token
        // metadata but preserves the same inference, segment text and offsets.
        payload["whisper_options"] = json!({"output":"json-full","split_on_word":true,"max_len":72,
            "chunk_seconds":job.get("audio_chunk_seconds").cloned().unwrap_or(json!(300))});
    }
    storage::digest(&payload)
}

fn source_keys(job: &Value) -> Vec<String> {
    let mut keys = Vec::with_capacity(4);
    let recorded = string(job, "transcript_key");
    if valid_key(recorded) {
        keys.push(recorded.to_owned());
    }
    if let Some(fingerprint) = job
        .get("media_fingerprint")
        .filter(|value| value.is_object())
    {
        for (rust, legacy) in [(true, false), (false, false), (false, true)] {
            let key = transcript_key(job, fingerprint, rust, legacy);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

fn canonical(language: &str, cues: &[Cue]) -> Source {
    let cues: Vec<_> = cues
        .iter()
        .map(|cue| Cue {
            start: (cue.start * 1000.0).round() / 1000.0,
            end: (cue.end * 1000.0).round() / 1000.0,
            text: subtitle::normalize_text(&cue.text),
        })
        .collect();
    #[derive(Serialize)]
    struct Identity<'a> {
        cues: &'a [Cue],
        kind: &'a str,
        source_language: &'a str,
    }
    let key = digest(&Identity {
        cues: &cues,
        kind: "saved-transcript",
        source_language: language,
    });
    Source {
        key,
        language: language.to_owned(),
        cues,
    }
}

fn source_for_review(config: &Config, job: &Value) -> Result<Source, String> {
    let outputs = job.get("outputs").and_then(Value::as_object);
    let mut language = [
        string(job, "detected_language"),
        string(job, "source_language"),
    ]
    .into_iter()
    .find(|value| valid_language(value))
    .map(str::to_owned);
    if language.is_none() {
        let targets = strings(job.get("targets"));
        let possible: Vec<_> = outputs
            .into_iter()
            .flat_map(|outputs| outputs.keys())
            .filter(|language| valid_language(language) && !targets.contains(language))
            .take(2)
            .cloned()
            .collect();
        if possible.len() == 1 {
            language = possible.into_iter().next();
        }
    }
    if let Some(language) = language.as_deref()
        && let Ok(cues) = read_srt(config, job, language)
    {
        let saved = canonical(language, &cues);
        // Keep the original cache identity and precision only when its text
        // and millisecond timings match the authoritative saved subtitle.
        for key in source_keys(job) {
            if let Some(cached) = load_transcript(config, &key)
                && cached.language == language
                && canonical(language, &cached.cues).cues == saved.cues
            {
                return Ok(cached);
            }
        }
        return Ok(saved);
    }
    for key in source_keys(job) {
        if let Some(cached) = load_transcript(config, &key)
            && language
                .as_ref()
                .is_none_or(|language| *language == cached.language)
        {
            return Ok(cached);
        }
    }
    Err(
        "Source transcript is missing. A saved source SRT or matching transcript cache is needed"
            .into(),
    )
}

pub fn source_available(config: &Config, job: &Value) -> bool {
    source_for_review(config, job).is_ok()
}

fn saved_translation(
    config: &Config,
    job: &Value,
    source: &Source,
    language: &str,
) -> Result<Vec<String>, String> {
    let target = read_srt(config, job, language)?;
    if target.len() != source.cues.len()
        || target.iter().zip(&source.cues).any(|(left, right)| {
            (left.start - right.start).abs() > 0.0021 || (left.end - right.end).abs() > 0.0021
        })
    {
        return Err("Saved source and translation cue counts or timestamps do not match".into());
    }
    Ok(target.into_iter().map(|cue| cue.text).collect())
}

/// Check existence without parsing transcript bodies for every event subscriber.
pub fn source_available_hint(config: &Config, job: &Value) -> bool {
    for code in [
        string(job, "detected_language"),
        string(job, "source_language"),
    ] {
        if valid_language(code) && output_path(config, job, code).is_ok() {
            return true;
        }
    }
    source_keys(job)
        .iter()
        .any(|key| cache_path(config, "transcripts", key).is_ok_and(|path| path.is_file()))
}

pub fn usable_review_languages(config: &Config, job: &Value) -> Vec<String> {
    let Ok(source) = source_for_review(config, job) else {
        return Vec::new();
    };
    job.get("outputs")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|outputs| outputs.keys())
        .filter(|language| {
            **language != source.language
                && crate::config::languages().get(language.as_str()).is_some()
        })
        .filter(|language| saved_translation(config, job, &source, language).is_ok())
        .cloned()
        .collect()
}

fn progress(app: &App, id: &str, kind: &str, stage: &str, percent: u64) -> Result<(), String> {
    let current = app.job(id)?;
    let step = if kind == "step" {
        string(&current["step_task"], "step")
    } else {
        ""
    };
    app.update_checked(id, step, |job| {
        let target = match kind {
            "review" => &mut job["review_task"],
            "step" => &mut job["step_task"],
            _ => job,
        };
        if !target.is_object() {
            *target = json!({});
        }
        target["status"] = json!("running");
        target["stage"] = json!(stage);
        target["progress"] = json!(percent.min(100));
        Ok(())
    })
}

fn finish(
    app: &App,
    id: &str,
    kind: &str,
    status: &str,
    stage: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let current = app.job(id)?;
    let step = if kind == "step" {
        string(&current["step_task"], "step")
    } else {
        ""
    };
    app.update_checked(id, step, |job| {
        let target = match kind {
            "review" => &mut job["review_task"],
            "step" => &mut job["step_task"],
            _ => job,
        };
        if !target.is_object() {
            *target = json!({});
        }
        target["status"] = json!(status);
        target["stage"] = json!(stage);
        target["progress"] = json!(100);
        target["error"] = json!(error);
        Ok(())
    })
}

fn reuse(app: &App, id: &str, label: &str) -> Result<(), String> {
    app.update(id, |job| {
        let mut reused = strings(job.get("reused"));
        if !reused.iter().any(|value| value == label) {
            reused.push(label.to_owned());
        }
        job["reused"] = json!(reused);
    })
}

fn prepare_source(
    app: &App,
    id: &str,
    kind: &str,
    extraction_only: bool,
) -> Result<Option<Source>, String> {
    let job = app.job(id)?;
    app.check_cancel(id, "extraction")?;
    let media = crate::media::resolve_media(&app.config, string(&job, "path"), true)?;
    let fingerprint = crate::media::fingerprint(&media, string(&job, "path"))?;
    if job
        .get("media_fingerprint")
        .is_some_and(|old| !old.is_null() && *old != fingerprint)
    {
        return Err("The media file changed after this job was queued. Submit it again.".into());
    }
    let mut rust_job = job.clone();
    // Chunk boundaries influence the native engine's cue segmentation.
    rust_job["audio_chunk_seconds"] = json!(app.config.audio_chunk_seconds);
    let key = transcript_key(&rust_job, &fingerprint, true, false);
    app.update(id, |job| {
        job["transcript_key"] = json!(key);
        job["media_fingerprint"] = fingerprint;
        job["audio_chunk_seconds"] = json!(app.config.audio_chunk_seconds);
    })?;
    if let Some(cached) = load_transcript(&app.config, &key) {
        app.step(id, "extraction", "completed", 100, None)?;
        if !extraction_only {
            app.check_cancel(id, "transcription")?;
            app.step(id, "transcription", "completed", 100, None)?;
        }
        reuse(app, id, "transcription")?;
        progress(app, id, kind, "Reusing cached transcription", 64)?;
        app.update(id, |job| {
            job["detected_language"] = json!(cached.language);
        })?;
        return Ok((!extraction_only).then_some(cached));
    }
    progress(app, id, kind, "Extracting audio", 2)?;
    let (language, cues) = crate::media::transcribe(app, id, &job, &key, extraction_only)?;
    if extraction_only {
        return Ok(None);
    }
    app.check_cancel(id, "transcription")?;
    subtitle::validate(&cues)?;
    if !valid_language(&language) {
        return Err(
            "Whisper could not detect the audio language. Select it explicitly and retry.".into(),
        );
    }
    write_cache(
        &app.config,
        "transcripts",
        &key,
        &TranscriptCache {
            language: &language,
            cues: &cues,
        },
    )?;
    app.update(id, |job| {
        job["detected_language"] = json!(language);
    })?;
    app.step(id, "transcription", "completed", 100, None)?;
    Ok(Some(Source {
        key,
        language,
        cues,
    }))
}

fn translation_key(source: &Source, target: &str, model: &str, thinking: Option<bool>) -> String {
    #[derive(Serialize)]
    struct Identity<'a> {
        adapter: &'a str,
        kind: &'a str,
        model: &'a str,
        source_cues: &'a [Cue],
        source_language: &'a str,
        target_language: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking: Option<bool>,
        transcript_key: &'a str,
        version: u8,
    }
    digest(&Identity {
        adapter: if llm::is_translategemma(model) {
            "translategemma-raw-v1"
        } else {
            "chat-json-v1"
        },
        kind: "translation",
        model,
        source_cues: &source.cues,
        source_language: &source.language,
        target_language: target,
        thinking,
        transcript_key: &source.key,
        version: 1,
    })
}

fn quality_key(
    source: &Source,
    target: &str,
    texts: &[String],
    model: &str,
    thinking: Option<bool>,
) -> String {
    #[derive(Serialize)]
    struct Identity<'a> {
        batch_size: usize,
        kind: &'a str,
        method: &'a str,
        model: &'a str,
        source_cues: &'a [Cue],
        source_language: &'a str,
        target_language: &'a str,
        target_texts: &'a [String],
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking: Option<bool>,
        transcript_key: &'a str,
    }
    digest(&Identity {
        batch_size: REVIEW_BATCH_SIZE,
        kind: "quality",
        method: REVIEW_METHOD,
        model,
        source_cues: &source.cues,
        source_language: &source.language,
        target_language: target,
        target_texts: texts,
        thinking,
        transcript_key: &source.key,
    })
}

fn load_texts(config: &Config, key: &str, count: usize) -> Vec<String> {
    let Some(value) = read_cache(config, "translations", key) else {
        return Vec::new();
    };
    let Some(texts) = value
        .get("texts")
        .and_then(Value::as_array)
        .filter(|texts| texts.len() <= count)
    else {
        return Vec::new();
    };
    let mut result = Vec::with_capacity(texts.len());
    for text in texts {
        let Some(text) = text
            .as_str()
            .filter(|text| !text.trim().is_empty() && text.len() <= subtitle::MAX_CUE_BYTES)
        else {
            return Vec::new();
        };
        result.push(text.to_owned());
    }
    result
}

fn load_ratings(config: &Config, key: &str, count: usize) -> Vec<Rating> {
    let Some(value) = read_cache(config, "quality", key) else {
        return Vec::new();
    };
    let Some(rows) = value.get("ratings").and_then(Value::as_array) else {
        return Vec::new();
    };
    if rows.len() > count || (rows.len() != count && !rows.len().is_multiple_of(REVIEW_BATCH_SIZE))
    {
        return Vec::new();
    }
    let ids: Vec<_> = (1..=rows.len()).collect();
    llm::validate_ratings(&json!(rows), &ids).unwrap_or_default()
}

fn block_review(app: &App, id: &str, language: &str, reason: &str) -> Result<(), String> {
    app.step(
        id,
        &format!("review:{language}"),
        "blocked",
        0,
        Some(reason),
    )
}

fn perform_translation(
    app: &App,
    id: &str,
    kind: &str,
    source: &Source,
    language: &str,
    model: &str,
) -> Result<(), String> {
    let step = format!("translation:{language}");
    let result: Result<(), String> = (|| {
        app.check_cancel(id, &step)?;
        app.step(id, &step, "running", 0, None)?;
        let job = app.job(id)?;
        let thinking = job.get("translation_thinking").and_then(Value::as_bool);
        let key = translation_key(source, language, model, thinking);
        let mut texts = load_texts(&app.config, &key, source.cues.len());
        let batch_size = if llm::is_translategemma(model) {
            1
        } else {
            REVIEW_BATCH_SIZE
        };
        if !texts.is_empty() {
            let label = if texts.len() == source.cues.len() {
                language.to_owned()
            } else {
                format!("{language} ({} cues)", texts.len())
            };
            reuse(app, id, &label)?;
        }
        loop {
            let percent = (texts.len() * 100 / source.cues.len()) as u64;
            app.step(id, &step, "running", percent, None)?;
            progress(
                app,
                id,
                kind,
                &format!(
                    "Translating to {language} ({}/{} cues)",
                    texts.len(),
                    source.cues.len()
                ),
                if kind == "generation" {
                    64 + percent * 24 / 100
                } else {
                    percent
                },
            )?;
            app.check_cancel(id, &step)?;
            if texts.len() == source.cues.len() {
                break;
            }
            let end = texts
                .len()
                .saturating_add(batch_size)
                .min(source.cues.len());
            let batch = source
                .cues
                .get(texts.len()..end)
                .ok_or("Invalid translation cache offset")?;
            let translated =
                llm::translate(app, id, model, &source.language, language, batch, thinking)?;
            texts.extend(translated);
            write_cache(
                &app.config,
                "translations",
                &key,
                &TranslationCache { texts: &texts },
            )?;
        }
        // Borrow the source timeline and retain only one target language at once.
        let translated: Vec<_> = source
            .cues
            .iter()
            .zip(texts)
            .map(|(cue, text)| Cue {
                start: cue.start,
                end: cue.end,
                text,
            })
            .collect();
        app.check_cancel(id, &step)?;
        write_output(app, id, language, &translated)?;
        Ok(())
    })();
    if let Err(error) = &result {
        app.step(
            id,
            &step,
            if error == CANCELLED {
                "cancelled"
            } else {
                "failed"
            },
            0,
            Some(error),
        )?;
        block_review(
            app,
            id,
            language,
            if error == CANCELLED {
                "Translation was cancelled"
            } else {
                "Translation failed"
            },
        )?;
    }
    result
}

fn set_quality(app: &App, id: &str, language: &str, report: Value) -> Result<(), String> {
    let checked = matches!(report["status"].as_str(), Some("running" | "completed"));
    let step = format!("review:{language}");
    let change = |job: &mut Value| {
        let complete = report["status"] == "completed";
        if !job.get("quality").is_some_and(Value::is_object) {
            job["quality"] = json!({});
        }
        job["quality"][language] = report;
        if complete {
            if !job["steps"].is_object() {
                job["steps"] = json!({});
            }
            if !job["steps"][&step].is_object() {
                job["steps"][&step] = json!({"label":step});
            }
            job["steps"][&step]["status"] = json!("completed");
            job["steps"][&step]["progress"] = json!(100);
            job["steps"][&step]["error"] = Value::Null;
        }
    };
    if checked {
        app.update_checked(id, &step, |job| {
            change(job);
            Ok(())
        })
    } else {
        app.update(id, change)
    }
}

#[allow(clippy::too_many_arguments)]
fn review_language(
    app: &App,
    id: &str,
    kind: &str,
    source: &Source,
    language: &str,
    model: &str,
    texts: &[String],
    thinking: Option<bool>,
    force: bool,
) -> Result<(), String> {
    let job = app.job(id)?;
    let translation_model = string(&job, "model");
    let keep_previous = job
        .get("quality")
        .and_then(|quality| quality.get(language))
        .and_then(|report| report.get("status"))
        .and_then(Value::as_str)
        == Some("completed");
    let step = format!("review:{language}");
    let key = quality_key(source, language, texts, model, thinking);
    let canonical_source = canonical(&source.language, &source.cues);
    let canonical_key = quality_key(&canonical_source, language, texts, model, thinking);
    let mut ratings = Vec::new();
    let result: Result<(), String> = (|| {
        app.check_cancel(id, &step)?;
        app.step(id, &step, "running", 0, None)?;
        if force {
            for cache_key in [&key, &canonical_key] {
                write_cache(
                    &app.config,
                    "quality",
                    cache_key,
                    &QualityCache {
                        method: REVIEW_METHOD,
                        ratings: &[],
                    },
                )?;
            }
        } else {
            ratings = load_ratings(&app.config, &key, source.cues.len());
            let alternate = load_ratings(&app.config, &canonical_key, source.cues.len());
            if alternate.len() > ratings.len() {
                ratings = alternate;
            }
        }
        if ratings.len() == source.cues.len() {
            reuse(app, id, &format!("{language} review"))?;
        }
        loop {
            let percent = (ratings.len() * 100 / source.cues.len()) as u64;
            app.step(id, &step, "running", percent, None)?;
            progress(
                app,
                id,
                kind,
                &format!(
                    "Reviewing {language} ({}/{} cues)",
                    ratings.len(),
                    source.cues.len()
                ),
                if kind == "generation" {
                    88 + percent * 11 / 100
                } else {
                    percent.min(99)
                },
            )?;
            if !keep_previous {
                set_quality(
                    app,
                    id,
                    language,
                    llm::quality_report(
                        "running",
                        model,
                        translation_model,
                        &source.cues,
                        texts,
                        &ratings,
                        None,
                        thinking,
                    )?,
                )?;
            }
            app.check_cancel(id, &step)?;
            if ratings.len() == source.cues.len() {
                break;
            }
            ratings.extend(llm::review(
                app,
                id,
                model,
                &source.language,
                language,
                &source.cues,
                texts,
                ratings.len(),
                thinking,
            )?);
            for cache_key in [&key, &canonical_key] {
                write_cache(
                    &app.config,
                    "quality",
                    cache_key,
                    &QualityCache {
                        method: REVIEW_METHOD,
                        ratings: &ratings,
                    },
                )?;
            }
        }
        for cache_key in [&key, &canonical_key] {
            write_cache(
                &app.config,
                "quality",
                cache_key,
                &QualityCache {
                    method: REVIEW_METHOD,
                    ratings: &ratings,
                },
            )?;
        }
        app.check_cancel(id, &step)?;
        set_quality(
            app,
            id,
            language,
            llm::quality_report(
                "completed",
                model,
                translation_model,
                &source.cues,
                texts,
                &ratings,
                None,
                thinking,
            )?,
        )?;
        Ok(())
    })();
    if let Err(error) = &result {
        if !keep_previous {
            set_quality(
                app,
                id,
                language,
                llm::quality_report(
                    "unavailable",
                    model,
                    translation_model,
                    &source.cues,
                    texts,
                    &ratings,
                    Some(error),
                    thinking,
                )?,
            )?;
        }
        app.step(
            id,
            &step,
            if error == CANCELLED {
                "cancelled"
            } else {
                "failed"
            },
            (ratings.len() * 100 / source.cues.len()) as u64,
            Some(error),
        )?;
    }
    result
}

fn run_reviews(
    app: &App,
    id: &str,
    kind: &str,
    source: &Source,
    languages: &[String],
) -> Result<bool, String> {
    let job = app.job(id)?;
    let task = if kind == "review" {
        &job["review_task"]
    } else {
        &job
    };
    let preferred = if kind == "review" {
        string(task, "model")
    } else {
        string(task, "review_model")
    };
    let thinking = task
        .get(if kind == "review" {
            "thinking"
        } else {
            "review_thinking"
        })
        .and_then(Value::as_bool);
    let force = kind == "review" && task.get("force").and_then(Value::as_bool) == Some(true);
    let selected = llm::review_model_for_job(&app.config, preferred, string(&job, "model"));
    if let Ok(model) = &selected {
        app.update(id, |job| {
            if kind == "review" {
                job["review_task"]["model"] = json!(model);
            } else {
                job["review_model"] = json!(model);
            }
        })?;
    }
    let mut errors = Vec::new();
    let mut cancelled = false;
    for language in languages {
        app.check_cancel(id, "")?;
        let step = format!("review:{language}");
        let result = (|| {
            app.check_cancel(id, &step)?;
            let texts = saved_translation(&app.config, &job, source, language)?;
            let model = selected.as_ref().map_err(Clone::clone)?;
            review_language(
                app, id, kind, source, language, model, &texts, thinking, force,
            )
        })();
        if let Err(error) = result {
            app.check_cancel(id, "")?;
            app.step(
                id,
                &step,
                if error == CANCELLED {
                    "cancelled"
                } else {
                    "failed"
                },
                0,
                Some(&error),
            )?;
            if error == CANCELLED {
                cancelled = true;
            } else {
                errors.push(format!("{language}: {error}"));
            }
            if selected.is_err() {
                let keep_previous = job
                    .get("quality")
                    .and_then(|quality| quality.get(language))
                    .and_then(|report| report.get("status"))
                    .and_then(Value::as_str)
                    == Some("completed");
                if !keep_previous {
                    set_quality(
                        app,
                        id,
                        language,
                        json!({"status":"unavailable","model":preferred,"method":REVIEW_METHOD,"reviewed_cues":0,"total_cues":source.cues.len(),"flagged_cues":0,"issues":[],"thinking":thinking,"error":error}),
                    )?;
                }
            }
        }
    }
    if kind == "review" {
        let error = (!errors.is_empty()).then(|| errors.join("; "));
        finish(
            app,
            id,
            kind,
            if error.is_some() {
                "failed"
            } else if cancelled {
                "cancelled"
            } else {
                "completed"
            },
            if error.is_some() {
                "Translation review finished with errors"
            } else if cancelled {
                "Review finished with cancelled steps"
            } else {
                "Translation review complete"
            },
            error.as_deref(),
        )?;
    }
    Ok(!errors.is_empty() || cancelled)
}

fn run_generation(app: &App, work: &Work) -> Result<(), String> {
    let id = &work.id;
    progress(app, id, "generation", "Checking cached results", 1)?;
    let source =
        prepare_source(app, id, "generation", false)?.ok_or("Source transcription is missing")?;
    let job = app.job(id)?;
    let targets = strings(job.get("targets"));
    if job.get("transcript").and_then(Value::as_bool) == Some(true)
        || targets.contains(&source.language)
    {
        write_output(app, id, &source.language, &source.cues)?;
    }
    let targets: Vec<_> = targets
        .into_iter()
        .filter(|language| *language != source.language)
        .collect();
    let mut completed = Vec::new();
    let mut cancelled = false;
    if !targets.is_empty() {
        let model = llm::model_for_job(&app.config, string(&job, "model"))?;
        app.update(id, |job| {
            job["model"] = json!(model);
        })?;
        for language in targets {
            match perform_translation(app, id, "generation", &source, &language, &model) {
                Ok(()) => completed.push(language),
                Err(error) if error == CANCELLED => {
                    app.check_cancel(id, "")?;
                    cancelled = true;
                }
                Err(error) => return Err(error),
            }
        }
    }
    let warnings = if job.get("review_enabled").and_then(Value::as_bool) != Some(false)
        && !completed.is_empty()
    {
        run_reviews(app, id, "generation", &source, &completed)?
    } else {
        false
    };
    app.check_cancel(id, "")?;
    finish(
        app,
        id,
        "generation",
        "completed",
        if cancelled {
            "Complete with cancelled steps"
        } else if warnings {
            "Complete with review warnings"
        } else {
            "Complete"
        },
        None,
    )?;
    app.update(id, |job| {
        job["position"] = job.get("duration").cloned().unwrap_or(json!(0));
    })
}

pub fn run(app: &App, work: &Work) -> Result<(), String> {
    app.check_cancel(&work.id, "")?;
    match work.kind.as_str() {
        "generation" => run_generation(app, work),
        "review" => {
            progress(app, &work.id, "review", "Loading saved subtitles", 1)?;
            let job = app.job(&work.id)?;
            let source = source_for_review(&app.config, &job)?;
            let languages = strings(
                job.get("review_task")
                    .and_then(|task| task.get("languages")),
            );
            if languages.is_empty() {
                return Err("No saved translated subtitles selected".into());
            }
            run_reviews(app, &work.id, "review", &source, &languages).map(|_| ())
        }
        "step" => {
            let step = work.step.as_deref().ok_or("Missing processing step")?;
            app.check_cancel(&work.id, step)?;
            progress(app, &work.id, "step", &format!("Retrying {step}"), 1)?;
            if matches!(step, "extraction" | "transcription") {
                let source = prepare_source(app, &work.id, "step", step == "extraction")?;
                if let Some(source) = source {
                    let job = app.job(&work.id)?;
                    if job.get("transcript").and_then(Value::as_bool) == Some(true)
                        || strings(job.get("targets")).contains(&source.language)
                    {
                        write_output(app, &work.id, &source.language, &source.cues)?;
                    }
                }
            } else if let Some(language) = step.strip_prefix("translation:") {
                let job = app.job(&work.id)?;
                let source = source_for_review(&app.config, &job)?;
                let model = llm::model_for_job(&app.config, string(&job, "model"))?;
                app.update(&work.id, |job| {
                    job["model"] = json!(model);
                })?;
                perform_translation(app, &work.id, "step", &source, language, &model)?;
            } else {
                return Err("Unsupported independent processing step".into());
            }
            app.check_cancel(&work.id, step)?;
            finish(
                app,
                &work.id,
                "step",
                "completed",
                "Processing step complete",
                None,
            )
        }
        _ => Err("Unsupported queued work type".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    fn fixture() -> (tempfile::TempDir, App, Source) {
        let directory = tempfile::tempdir().expect("fixture directory");
        let root = directory.path();
        let config = Config {
            library_root: root.join("library"),
            output_root: root.join("output"),
            data_root: root.join("data"),
            model_root: root.join("models"),
            bind_address: "127.0.0.1".into(),
            port: 8099,
            whisper_model: "small-q5_1".into(),
            whisper_device: "cpu".into(),
            whisper_cli: "whisper-cli".into(),
            model_download_timeout: 3600,
            local_only: true,
            cpu_threads: 2,
            audio_chunk_seconds: 300,
            process_timeout: 5,
            llm_base: "http://127.0.0.1:1/v1".into(),
            llm_model: "translator".into(),
            review_model: "reviewer".into(),
            llm_timeout: 3,
            max_json_bytes: 1024 * 1024,
            max_jobs: 10,
            max_queue: 4,
            max_clients: 4,
            max_streams: 2,
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into()],
        };
        let app = App::open(config).expect("fixture app");
        let cues: Vec<_> = (0..9)
            .map(|index| Cue {
                start: index as f64,
                end: index as f64 + 0.8,
                text: format!("Source {index}"),
            })
            .collect();
        let source = canonical("ja", &cues);
        let job = json!({"id":"abcdef123456","path":"folder/video.mkv","source_language":"ja","detected_language":"ja","targets":["en"],"outputs":{},"quality":{},"reused":[],"status":"failed","stage":"Original generation failed","progress":73,"model":"translator","steps":{},"cancelled_steps":[],"cancel_requested":false,"step_task":{"status":"queued","step":"translation:en"}});
        app.state.lock().expect("fixture state").jobs.push(job);
        write_output(&app, "abcdef123456", "ja", &cues).expect("source snapshot");
        (directory, app, source)
    }

    fn model_server(reply: Value) -> (String, std::thread::JoinHandle<Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture model endpoint");
        let endpoint = format!(
            "http://{}/v1",
            listener.local_addr().expect("fixture address")
        );
        listener.set_nonblocking(true).expect("fixture nonblocking");
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && started.elapsed() < Duration::from_secs(5) =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("fixture blocking stream");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("fixture timeout");
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            let mut length = None;
            loop {
                line.clear();
                assert!(reader.read_line(&mut line).expect("fixture headers") > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().ok();
                }
            }
            let length = length
                .filter(|length| *length < 64 * 1024)
                .expect("bounded fixture request");
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("fixture JSON body");
            let payload = serde_json::from_slice(&body).expect("request JSON");
            let response = json!({"choices":[{"finish_reason":"stop","message":{"content":reply.to_string()}}]}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).expect("fixture response");
            payload
        });
        (endpoint, handle)
    }

    #[test]
    fn translation_step_resumes_cached_batch_and_preserves_parent_result() {
        let (_directory, mut app, source) = fixture();
        let texts: Vec<_> = (0..8).map(|index| format!("Translation {index}")).collect();
        let key = translation_key(&source, "en", "translator", None);
        write_cache(
            &app.config,
            "translations",
            &key,
            &TranslationCache { texts: &texts },
        )
        .expect("partial cache");
        let (endpoint, server) = model_server(json!([{"id":1,"text":"Translation 8"}]));
        app.config.llm_base = endpoint;
        let work = Work {
            id: "abcdef123456".into(),
            kind: "step".into(),
            step: Some("translation:en".into()),
            token: String::new(),
        };
        run(&app, &work).expect("resumed translation");
        let payload = server.join().expect("model fixture finished");
        assert!(
            payload["messages"][1]["content"]
                .as_str()
                .expect("translation prompt")
                .contains("Source 8")
        );
        assert!(
            !payload["messages"][1]["content"]
                .as_str()
                .expect("translation prompt")
                .contains("Source 0")
        );
        let job = app.job(&work.id).expect("saved job");
        assert_eq!(job["status"], "failed");
        assert_eq!(job["progress"], 73);
        assert_eq!(job["step_task"]["status"], "completed");
        assert_eq!(job["steps"]["translation:en"]["status"], "completed");
        assert_eq!(load_texts(&app.config, &key, 9).len(), 9);
        assert_eq!(
            read_srt(&app.config, &job, "en").expect("target SRT").len(),
            9
        );
        // Complete cache reuse succeeds after the fixture endpoint has closed.
        run(&app, &work).expect("complete translation cache reuse");
        let current = app.job(&work.id).expect("job");
        let snapshot = PathBuf::from(
            current["outputs"]["en"]["path"]
                .as_str()
                .expect("snapshot path"),
        );
        fs::remove_file(snapshot).expect("remove fixture snapshot");
        assert!(
            output_path(&app.config, &current, "en")
                .expect("legacy export fallback")
                .starts_with(&app.config.output_root)
        );
    }

    #[test]
    fn saved_review_resumes_global_ids_and_preserves_valid_translation() {
        let (_directory, mut app, source) = fixture();
        let translated: Vec<_> = source
            .cues
            .iter()
            .map(|cue| Cue {
                start: cue.start,
                end: cue.end,
                text: cue.text.replace("Source", "Translation"),
            })
            .collect();
        let texts: Vec<_> = translated.iter().map(|cue| cue.text.clone()).collect();
        write_output(&app, "abcdef123456", "en", &translated).expect("target snapshot");
        app.update("abcdef123456", |job| {
            job["step_task"]["status"] = json!("completed");
            job["review_task"] =
                json!({"status":"queued","languages":["en"],"model":"reviewer","force":false});
        })
        .expect("review task");
        let rows = json!(
            (1..=8)
                .map(|id| json!({"id":id,"fidelity":90,"fluency":95,"issue":""}))
                .collect::<Vec<_>>()
        );
        let ratings = llm::validate_ratings(&rows, &(1..=8).collect::<Vec<_>>()).expect("ratings");
        let key = quality_key(&source, "en", &texts, "reviewer", None);
        write_cache(
            &app.config,
            "quality",
            &key,
            &QualityCache {
                method: REVIEW_METHOD,
                ratings: &ratings,
            },
        )
        .expect("partial ratings");
        let (endpoint, server) =
            model_server(json!([{"id":9,"fidelity":45,"fluency":60,"issue":"Meaning reversed"}]));
        app.config.llm_base = endpoint;
        let work = Work {
            id: "abcdef123456".into(),
            kind: "review".into(),
            step: None,
            token: String::new(),
        };
        run(&app, &work).expect("review resume");
        let payload = server.join().expect("review fixture finished");
        let content: Value = serde_json::from_str(
            payload["messages"][1]["content"]
                .as_str()
                .expect("review content"),
        )
        .expect("review pairs");
        assert_eq!(content["pairs"][0]["id"], 9);
        assert_eq!(content["context_only"][0]["id"], 7);
        let job = app.job(&work.id).expect("reviewed job");
        assert_eq!(job["status"], "failed");
        assert_eq!(job["review_task"]["status"], "completed");
        assert_eq!(job["quality"]["en"]["score"], 85.0);
        assert_eq!(job["quality"]["en"]["fluency"], 91.1);
        assert_eq!(job["quality"]["en"]["flagged_cues"], 1);
        assert_eq!(
            read_srt(&app.config, &job, "en").expect("preserved translation"),
            translated
        );
    }

    #[test]
    fn language_cancellation_keeps_other_steps_available() {
        let (_directory, app, source) = fixture();
        app.update("abcdef123456", |job| {
            job["cancelled_steps"] = json!(["translation:en"])
        })
        .expect("cancel step");
        assert_eq!(
            perform_translation(
                &app,
                "abcdef123456",
                "generation",
                &source,
                "en",
                "translator"
            )
            .expect_err("cancelled"),
            CANCELLED
        );
        app.check_cancel("abcdef123456", "")
            .expect("whole job remains active");
        let job = app.job("abcdef123456").expect("job");
        assert_eq!(job["steps"]["translation:en"]["status"], "cancelled");
        assert_eq!(job["steps"]["review:en"]["status"], "blocked");
    }

    #[test]
    fn streaming_identity_matches_legacy_sorted_json() {
        let source = canonical(
            "ja",
            &[Cue {
                start: 0.0,
                end: 1.0,
                text: "字幕".into(),
            }],
        );
        let legacy = json!({"kind":"translation","version":1,"transcript_key":source.key,"source_language":"ja","source_cues":source.cues,"target_language":"en","model":"translator","adapter":"chat-json-v1"});
        assert_eq!(
            translation_key(&source, "en", "translator", None),
            storage::digest(&legacy)
        );
        let legacy = json!({"kind":"saved-transcript","source_language":"ja","cues":source.cues});
        assert_eq!(source.key, storage::digest(&legacy));
    }

    #[test]
    fn moved_job_uses_current_snapshot_before_overwritten_shared_export() {
        let (_directory, app, source) = fixture();
        let mut job = app.job("abcdef123456").expect("fixture job");
        job["outputs"]["ja"]["path"] =
            json!("/previous-container/data/job_outputs/abcdef123456/video.ja.srt");
        job["outputs"]["ja"]["export_path"] = json!("C:\\previous-host\\output\\video.ja.srt");
        let export = export_path(&app.config, &job, "ja").expect("export path");
        fs::write(export, b"Another job replaced this shared export")
            .expect("replace shared export");
        let path = output_path(&app.config, &job, "ja").expect("portable snapshot");
        assert!(path.starts_with(app.config.data_root.join("job_outputs")));
        assert_eq!(
            read_srt(&app.config, &job, "ja").expect("original job snapshot"),
            source.cues
        );
    }

    #[test]
    fn old_python_source_cache_remains_available_without_original_media() {
        let (_directory, app, source) = fixture();
        let mut job = json!({"id":"abcdef123456","path":"missing.mkv","source_language":"ja","outputs":{},"audio_stream_index":1,"audio_offset":0.0,"whisper_model":"medium","media_fingerprint":{"path":"missing.mkv","size":1,"mtime_ns":2,"sample":"fixture"}});
        let key = transcript_key(&job, &job["media_fingerprint"], false, true);
        write_cache(
            &app.config,
            "transcripts",
            &key,
            &TranscriptCache {
                language: "ja",
                cues: &source.cues,
            },
        )
        .expect("legacy cache");
        let loaded = source_for_review(&app.config, &job).expect("legacy source without media");
        assert!(source_available_hint(&app.config, &job));
        assert_eq!(loaded.key, key);
        assert_eq!(loaded.cues, source.cues);
        job["source_language"] = json!("en");
        assert!(source_for_review(&app.config, &job).is_err());
    }

    #[test]
    fn unavailable_reviewer_keeps_completed_report_and_saved_translation() {
        let (_directory, app, source) = fixture();
        write_output(&app, "abcdef123456", "en", &source.cues).expect("translation");
        let previous =
            json!({"status":"completed","score":88.0,"fluency":91.0,"model":"previous-reviewer"});
        app.update("abcdef123456", |job| {
            job["review_task"] =
                json!({"status":"queued","languages":["en"],"model":"translategemma","force":true});
            job["quality"]["en"] = previous.clone();
        })
        .expect("review task");
        let work = Work {
            id: "abcdef123456".into(),
            kind: "review".into(),
            step: None,
            token: String::new(),
        };
        run(&app, &work).expect("review errors are contained");
        let job = app.job(&work.id).expect("job");
        assert_eq!(job["quality"]["en"], previous);
        assert_eq!(job["review_task"]["status"], "failed");
        assert_eq!(job["status"], "failed");
        assert_eq!(
            read_srt(&app.config, &job, "en").expect("translation retained"),
            source.cues
        );
    }

    #[test]
    fn cancellation_prevents_final_output_and_completed_task_publication() {
        let (_directory, app, source) = fixture();
        app.update("abcdef123456", |job| job["cancel_requested"] = json!(true))
            .expect("cancel accepted");
        assert_eq!(
            write_output(&app, "abcdef123456", "en", &source.cues)
                .expect_err("cancel blocks output"),
            CANCELLED
        );
        assert_eq!(
            finish(&app, "abcdef123456", "step", "completed", "Complete", None)
                .expect_err("cancel blocks finish"),
            CANCELLED
        );
        let job = app.job("abcdef123456").expect("job");
        assert!(job["outputs"].get("en").is_none());
        assert_ne!(job["step_task"]["status"], "completed");
    }

    #[test]
    fn cache_identity_separates_engine_and_thinking_modes() {
        let job = json!({"audio_stream_index":1,"audio_offset":0.0,"source_language":"ja","whisper_model":"medium"});
        let fingerprint = json!({"path":"video.mkv","size":7,"mtime_ns":1,"sample":"fixture"});
        assert_ne!(
            transcript_key(&job, &fingerprint, true, false),
            transcript_key(&job, &fingerprint, false, false)
        );
        let source = Source {
            key: "a".repeat(64),
            language: "ja".into(),
            cues: vec![Cue {
                start: 0.0,
                end: 1.0,
                text: "Source".into(),
            }],
        };
        let keys: std::collections::BTreeSet<_> = [None, Some(false), Some(true)]
            .into_iter()
            .map(|thinking| translation_key(&source, "en", "model", thinking))
            .collect();
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn traversal_rejected_before_creation() {
        let root = tempfile::tempdir().expect("fixture");
        assert!(contained(root.path(), &root.path().join("../outside.srt")).is_err());
        for path in [
            "../video.mkv",
            "/video.mkv",
            "C:/video.mkv",
            "folder\\video.mkv",
        ] {
            assert!(relative_media(&json!({"path":path})).is_err());
        }
        assert!(relative_media(&json!({"path":"folder/video.mkv"})).is_ok());
    }

    #[test]
    fn canonical_saved_source_normalizes_layout_and_precision() {
        let source = canonical(
            "ja",
            &[Cue {
                start: 1.0001,
                end: 2.9999,
                text: "A\n B".into(),
            }],
        );
        assert_eq!(
            source.cues,
            vec![Cue {
                start: 1.0,
                end: 3.0,
                text: "A B".into()
            }]
        );
        assert!(valid_key(&source.key));
    }
}
