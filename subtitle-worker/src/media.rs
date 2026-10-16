use crate::{App, config::Config, process, subtitle::Cue};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const MODELS: &[&str] = &[
    "tiny",
    "tiny.en",
    "tiny-q5_1",
    "tiny.en-q5_1",
    "base",
    "base.en",
    "base-q5_1",
    "base.en-q5_1",
    "small",
    "small.en",
    "small-q5_1",
    "small.en-q5_1",
    "medium",
    "medium.en",
    "medium-q5_0",
    "medium.en-q5_0",
    "large-v2",
    "large-v2-q5_0",
    "large-v3",
    "large-v3-q5_0",
    "large-v3-turbo",
    "large-v3-turbo-q5_0",
];
const MAX_MODEL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_CUES: usize = 100_000;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

fn video(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
        matches!(
            s.to_ascii_lowercase().as_str(),
            "mkv" | "mp4" | "avi" | "mov" | "m4v" | "webm" | "ts" | "m2ts" | "mpg" | "mpeg"
        )
    })
}

/// Reject every symlink component, including links whose target is inside the root.
pub fn resolve_media(
    config: &Config,
    relative: &str,
    file_required: bool,
) -> Result<PathBuf, String> {
    if relative.contains(['\\', '\0', ':'])
        || relative.starts_with('/')
        || relative.split('/').any(|part| part == "." || part == "..")
    {
        return Err("Path must stay inside the media library".into());
    }
    let root = config
        .library_root
        .canonicalize()
        .map_err(|_| "Media library is unavailable")?;
    let mut path = root.clone();
    for component in Path::new(relative).components() {
        let Component::Normal(part) = component else {
            return Err("Invalid library path".into());
        };
        path.push(part);
        let metadata = fs::symlink_metadata(&path).map_err(|_| "Media path does not exist")?;
        if metadata.file_type().is_symlink() {
            return Err("Symbolic links are not supported".into());
        }
    }
    let resolved = path
        .canonicalize()
        .map_err(|_| "Media path does not exist")?;
    if !resolved.starts_with(&root) {
        return Err("Path must stay inside the media library".into());
    }
    if file_required && (!resolved.is_file() || !video(&resolved)) {
        return Err("Select a supported video file".into());
    }
    Ok(resolved)
}

pub fn browse(config: &Config, relative: &str) -> Result<Value, String> {
    let path = resolve_media(config, relative, false)?;
    if !path.is_dir() {
        return Err("Select a folder".into());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&path).map_err(|_| "This folder cannot be read")? {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.')
            || kind.is_symlink()
            || !(kind.is_dir() || (kind.is_file() && video(&entry.path())))
        {
            continue;
        }
        if entries.len() >= 10_000 {
            return Err("Folder has too many entries; select a smaller folder".into());
        }
        let child = if relative.is_empty() {
            name.clone()
        } else {
            format!("{}/{name}", relative.trim_end_matches('/'))
        };
        entries
            .push(json!({"name":name,"path":child,"type":if kind.is_dir(){"folder"}else{"media"}}));
    }
    entries.sort_by_cached_key(|entry| {
        (
            entry["type"] != "folder",
            entry["name"].as_str().unwrap_or_default().to_lowercase(),
        )
    });
    Ok(json!({"path":relative,"entries":entries}))
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn number(value: &Value) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .filter(|n| n.is_finite())
        .unwrap_or_default()
}

fn chunk_span(position: f64, duration: f64, configured: u64) -> f64 {
    (configured.clamp(30, 900) as f64).min((duration - position).max(0.0))
}

fn scoped_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Invalid application file path".into());
    }
    let destination = std::path::absolute(root)
        .map_err(|_| "Invalid application root")?
        .join(relative);
    let mut current = PathBuf::new();
    for component in destination.components() {
        current.push(component);
        // A Windows drive/UNC prefix alone is not a filesystem path until its
        // root separator is appended (notably canonical `\\?\C:\` paths).
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Application files must not contain symbolic links".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Application file path cannot be inspected".into()),
        }
    }
    Ok(destination)
}

fn apply_progress(
    job: &mut Value,
    stage: &str,
    percent: u64,
    position: Option<f64>,
    language: Option<&str>,
) {
    if let Some(language) = language {
        job["detected_language"] = json!(language);
    }
    let target = if crate::active(&job["step_task"]["status"]) {
        &mut job["step_task"]
    } else {
        job
    };
    target["stage"] = json!(stage);
    target["progress"] = json!(percent);
    if let Some(position) = position {
        target["position"] = json!(position);
    }
}

fn progress(
    app: &App,
    id: &str,
    stage: &str,
    percent: u64,
    position: Option<f64>,
    language: Option<&str>,
) -> Result<(), String> {
    app.update(id, |job| {
        apply_progress(job, stage, percent, position, language)
    })
}

pub fn probe(config: &Config, path: &Path) -> Result<Value, String> {
    let mut arguments = args(&[
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-show_entries",
        "stream=index,codec_type,codec_name,channels,start_time:stream_tags=language,title:format=duration,start_time",
        "-of",
        "json",
    ]);
    arguments.push(path.as_os_str().into());
    let bytes = process::run("ffprobe", &arguments, 45, config.max_json_bytes, || Ok(()))?;
    let info: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid ffprobe output")?;
    let origin = number(&info["format"]["start_time"]);
    let tracks: Vec<Value> = info["streams"].as_array().into_iter().flatten().filter(|s| s["codec_type"] == "audio").map(|stream| json!({
        "index":stream["index"], "language":stream["tags"]["language"].as_str().unwrap_or("und"),
        "title":stream["tags"]["title"].as_str().unwrap_or(""), "codec":stream["codec_name"].as_str().unwrap_or(""),
        "channels":stream["channels"].as_u64().unwrap_or(0), "offset":(number(&stream["start_time"])-origin).max(0.0)
    })).collect();
    Ok(json!({"tracks":tracks,"duration":number(&info["format"]["duration"])}))
}

pub fn fingerprint(path: &Path, relative: &str) -> Result<Value, String> {
    let metadata = fs::metadata(path).map_err(|_| "Unable to inspect media file")?;
    let mut file = File::open(path).map_err(|_| "Unable to read media file")?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let count = usize::try_from(metadata.len().min(buffer.len() as u64))
        .map_err(|_| "Invalid media size")?;
    file.read_exact(buffer.get_mut(..count).ok_or("Invalid fingerprint size")?)
        .map_err(|_| "Unable to fingerprint media")?;
    hash.update(buffer.get(..count).ok_or("Invalid fingerprint read")?);
    if metadata.len() > buffer.len() as u64 {
        file.seek(SeekFrom::Start(
            metadata.len().saturating_sub(buffer.len() as u64),
        ))
        .map_err(|_| "Unable to fingerprint media")?;
        file.read_exact(&mut buffer)
            .map_err(|_| "Unable to fingerprint media")?;
        hash.update(buffer);
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|time| time.as_nanos())
        .unwrap_or(0);
    let mtime_ns = u64::try_from(modified).map_err(|_| "Media timestamp is out of range")?;
    Ok(
        json!({"path":relative,"size":metadata.len(),"mtime_ns":mtime_ns,"sample":format!("{:x}",hash.finalize())}),
    )
}

pub fn supported_model(name: &str) -> bool {
    MODELS.contains(&name)
}
fn model_path(config: &Config, name: &str) -> Result<PathBuf, String> {
    if !supported_model(name) {
        return Err("Unsupported Whisper model".into());
    }
    scoped_path(&config.model_root, Path::new(&format!("ggml-{name}.bin")))
}
fn regular_model(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| {
        m.is_file() && !m.file_type().is_symlink() && m.len() > 1024 && m.len() <= MAX_MODEL_BYTES
    })
}
pub fn models(config: &Config) -> Value {
    Value::Array(MODELS.iter().map(|name| json!({"id":name,"downloaded":model_path(config,name).is_ok_and(|p|regular_model(&p))})).collect())
}

fn download_url_allowed(url: &str) -> bool {
    if url.chars().any(|character| {
        character.is_whitespace() || character.is_control() || matches!(character, '\\' | '#')
    }) {
        return false;
    }
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("https")
        && uri.authority().is_some_and(|authority| {
            !authority.as_str().contains('@') && authority.port_u16().is_none()
        })
        && matches!(
            uri.host(),
            Some(
                "huggingface.co"
                    | "cdn-lfs.huggingface.co"
                    | "cdn-lfs-us-1.hf.co"
                    | "cdn-lfs-eu-1.hf.co"
                    | "cas-bridge.xethub.hf.co"
                    | "us.aws.cdn.hf.co"
                    | "eu.aws.cdn.hf.co"
            )
        )
}

/// A user-invoked download is the sole public-network operation. Redirects are
/// validated individually; response bytes stream to disk with a 4 GiB ceiling.
pub fn download_model(config: &Config, name: &str) -> Result<PathBuf, String> {
    download_model_checked(config, name, || Ok(()))
}

fn download_model_checked(
    config: &Config,
    name: &str,
    cancel: impl Fn() -> Result<(), String>,
) -> Result<PathBuf, String> {
    cancel()?;
    let destination = model_path(config, name)?;
    if regular_model(&destination) {
        return Ok(destination);
    }
    fs::create_dir_all(&config.model_root).map_err(|_| "Unable to create model directory")?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .proxy(None)
        .https_only(true)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // ureq bounds the whole body phase rather than individual reads. Keep
        // stalled automatic downloads cancellable within two minutes; larger
        // or slow model transfers can be preloaded outside the running worker.
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .timeout_global(Some(Duration::from_secs(180)))
        .build()
        .into();
    let mut url =
        format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin");
    for _ in 0..6 {
        cancel()?;
        if !download_url_allowed(&url) {
            return Err("Model download redirected to an untrusted endpoint".into());
        }
        let mut response = agent
            .get(&url)
            .call()
            .map_err(|_| "Model download request failed")?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("Invalid model download redirect")?;
            url = if location.starts_with('/') && !location.starts_with("//") {
                format!("https://huggingface.co{location}")
            } else {
                location.to_owned()
            };
            continue;
        }
        if response.status() != ureq::http::StatusCode::OK {
            return Err("Model download was rejected by the upstream server".into());
        }
        if response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|n| n > MAX_MODEL_BYTES)
        {
            return Err("Model exceeds the download limit".into());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&config.model_root)
            .map_err(|_| "Unable to create temporary model file")?;
        let mut copied = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        let mut reader = response.body_mut().as_reader();
        loop {
            cancel()?;
            let size = reader
                .read(&mut buffer)
                .map_err(|_| "Model download failed while streaming")?;
            if size == 0 {
                break;
            }
            copied = copied.saturating_add(size as u64);
            if copied > MAX_MODEL_BYTES {
                return Err("Model exceeds the download limit".into());
            }
            temporary
                .write_all(buffer.get(..size).ok_or("Invalid model download read")?)
                .map_err(|_| "Unable to write downloaded model")?;
        }
        if !(1025..=MAX_MODEL_BYTES).contains(&copied) {
            return Err("Model download was empty or exceeded the size limit".into());
        }
        temporary
            .flush()
            .map_err(|_| "Unable to flush downloaded model")?;
        cancel()?;
        temporary
            .persist(&destination)
            .map_err(|_| "Unable to save downloaded model")?;
        return Ok(destination);
    }
    Err("Too many model download redirects".into())
}

fn read_json(path: &Path, limit: u64) -> Result<Value, String> {
    let file = File::open(path).map_err(|_| "Unable to read Whisper output")?;
    if file
        .metadata()
        .map_err(|_| "Unable to inspect Whisper output")?
        .len()
        > limit
    {
        return Err("Whisper JSON exceeds the configured limit".into());
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| "Unable to read Whisper output")?;
    if bytes.len() as u64 > limit {
        return Err("Whisper JSON exceeds the configured limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid Whisper JSON output".into())
}

fn normalized(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some((before, after)) = remaining.split_once("[_") {
        clean.push_str(before);
        if let Some((_, rest)) = after.split_once("_]") {
            remaining = rest;
        } else {
            remaining = after;
            break;
        }
    }
    clean.push_str(remaining);
    let mut out = String::with_capacity(clean.len());
    let mut remaining = clean.as_str();
    while let Some((before, after)) = remaining.split_once("<|") {
        out.push_str(before);
        if let Some((_, rest)) = after.split_once("|>") {
            remaining = rest;
        } else {
            remaining = "";
            break;
        }
    }
    out.push_str(remaining);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_cues(data: &Value, offset: f64, max_end: f64) -> Result<Vec<Cue>, String> {
    let segments = data["transcription"]
        .as_array()
        .ok_or("Whisper output has no transcription")?;
    let mut cues = Vec::new();
    for segment in segments {
        let text = normalized(segment["text"].as_str().unwrap_or_default());
        let start = number(&segment["offsets"]["from"]) / 1000.0;
        let end = number(&segment["offsets"]["to"]) / 1000.0;
        if text.is_empty() || start < 0.0 || end <= start || start >= max_end {
            continue;
        }
        // --max-len and --split-on-word keep segments subtitle-sized upstream.
        if text.len() > 16 * 1024 {
            return Err("Whisper cue text exceeds the size limit".into());
        }
        cues.push(Cue {
            start: offset + start,
            end: offset + end.min(max_end),
            text,
        });
        if cues.len() > MAX_CUES {
            return Err("Whisper output contains too many cues".into());
        }
    }
    Ok(cues)
}

pub fn transcribe(
    app: &App,
    id: &str,
    job: &Value,
    key: &str,
    extraction_only: bool,
) -> Result<(String, Vec<Cue>), String> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid audio cache key".into());
    }
    let config = &app.config;
    let relative = job["path"].as_str().ok_or("Job has no media path")?;
    let media = resolve_media(config, relative, true)?;
    if !job["media_fingerprint"].is_null()
        && job["media_fingerprint"] != fingerprint(&media, relative)?
    {
        return Err("The media file changed after this job was queued. Submit it again.".into());
    }
    let index = job["audio_stream_index"]
        .as_u64()
        .ok_or("Job has no audio track")?;
    let audio_dir = scoped_path(&config.data_root, Path::new("cache/audio"))?;
    fs::create_dir_all(&audio_dir).map_err(|_| "Unable to create audio cache")?;
    let audio = scoped_path(&audio_dir, Path::new(&format!("{key}.wav")))?;
    app.check_cancel(id, "extraction")?;
    if !fs::symlink_metadata(&audio)
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 44)
    {
        app.step(id, "extraction", "running", 0, None)?;
        progress(app, id, "Extracting audio", 2, None, None)?;
        let temporary = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile_in(&audio_dir)
            .map_err(|_| "Unable to create audio file")?;
        let mut arguments = args(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ]);
        arguments.push(media.as_os_str().into());
        arguments.extend(args(&[
            "-map",
            &format!("0:{index}"),
            "-vn",
            "-af",
            "aresample=async=1",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-threads",
            "1",
            "-f",
            "wav",
        ]));
        arguments.push(temporary.path().as_os_str().into());
        process::run("ffmpeg", &arguments, config.process_timeout, 0, || {
            app.check_cancel(id, "extraction")
        })?;
        app.check_cancel(id, "extraction")?;
        temporary
            .persist(&audio)
            .map_err(|_| "Unable to save extracted audio")?;
    }
    app.step(id, "extraction", "completed", 100, None)?;
    if extraction_only {
        return Ok((String::new(), Vec::new()));
    }
    app.check_cancel(id, "transcription")?;
    let model_name = job["whisper_model"]
        .as_str()
        .unwrap_or(&config.whisper_model);
    let model = model_path(config, model_name)?;
    if !regular_model(&model) {
        if config.local_only {
            return Err(format!(
                "Whisper {model_name} is not downloaded. Run subtitle-studio download-model {model_name}."
            ));
        }
        progress(app, id, "Downloading Whisper model", 8, None, None)?;
        download_model_checked(config, model_name, || app.check_cancel(id, "transcription"))?;
    }
    let metadata = probe(config, &audio)?;
    let duration = number(&metadata["duration"]);
    if duration <= 0.0 || duration > 7.0 * 24.0 * 3600.0 {
        return Err("Audio duration is unavailable or exceeds seven days".into());
    }
    let requested_language = job["source_language"].as_str().unwrap_or("auto");
    if requested_language.len() > 12
        || !requested_language
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'-')
    {
        return Err("Invalid source language".into());
    }
    let mut language = requested_language.to_owned();
    let mut cues = Vec::new();
    let audio_offset = number(&job["audio_offset"]).max(0.0);
    let temporary =
        tempfile::tempdir_in(&audio_dir).map_err(|_| "Unable to create transcription workspace")?;
    let chunk_audio = temporary.path().join("chunk.wav");
    let prefix = temporary.path().join("result");
    let mut position = 0.0;
    let mut text_bytes = 0usize;
    app.step(id, "transcription", "running", 0, None)?;
    progress(app, id, "Loading Whisper model", 8, None, None)?;
    while position < duration {
        app.check_cancel(id, "transcription")?;
        let span = chunk_span(position, duration, config.audio_chunk_seconds);
        let mut extract = args(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-threads",
            "1",
            "-ss",
            &format!("{position:.3}"),
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ]);
        extract.push(audio.as_os_str().into());
        extract.extend(args(&[
            "-t",
            &format!("{span:.3}"),
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-threads",
            "1",
        ]));
        extract.push(chunk_audio.as_os_str().into());
        process::run("ffmpeg", &extract, config.process_timeout, 0, || {
            app.check_cancel(id, "transcription")
        })?;
        let mut arguments = args(&["--model"]);
        arguments.push(model.as_os_str().into());
        arguments.extend(args(&["--file"]));
        arguments.push(chunk_audio.as_os_str().into());
        arguments.extend(args(&[
            "--threads",
            &config.cpu_threads.clamp(1, 64).to_string(),
            "--processors",
            "1",
            "--output-json-full",
            "--output-file",
        ]));
        arguments.push(prefix.as_os_str().into());
        arguments.extend(args(&[
            "--language",
            &language,
            "--max-len",
            "72",
            "--split-on-word",
            "--no-prints",
        ]));
        if config.whisper_device == "cpu" {
            arguments.push("--no-gpu".into());
        }
        process::run(
            &config.whisper_cli,
            &arguments,
            config.process_timeout,
            0,
            || app.check_cancel(id, "transcription"),
        )?;
        let data = read_json(&prefix.with_extension("json"), config.max_json_bytes)?;
        if language == "auto" {
            language = data["result"]["language"]
                .as_str()
                .filter(|s| !s.is_empty() && *s != "auto")
                .ok_or(
                    "Whisper could not detect the audio language. Select it explicitly and retry.",
                )?
                .to_owned();
        }
        let chunk_cues = decode_cues(&data, position + audio_offset, span)?;
        text_bytes = chunk_cues
            .iter()
            .fold(text_bytes, |sum, cue| sum.saturating_add(cue.text.len()));
        if text_bytes > MAX_TEXT_BYTES {
            return Err("Transcription text exceeds the 16 MiB limit".into());
        }
        cues.extend(chunk_cues);
        if cues.len() > MAX_CUES {
            return Err("Transcription contains too many cues".into());
        }
        position += span;
        let percent = ((position / duration) * 100.0).min(99.0) as u64;
        app.step(id, "transcription", "running", percent, None)?;
        progress(
            app,
            id,
            "Transcribing",
            10 + percent * 52 / 100,
            Some(position + audio_offset),
            Some(&language),
        )?;
    }
    if cues.is_empty() {
        return Err("No speech was detected in the selected audio track".into());
    }
    app.check_cancel(id, "transcription")?;
    app.step(id, "transcription", "completed", 100, None)?;
    Ok((language, cues))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_names_are_fixed_and_cannot_be_paths() {
        assert!(supported_model("small-q5_1"));
        assert!(!supported_model("../small"));
        assert!(!supported_model("https://example.com/model"));
    }
    #[test]
    fn download_redirects_are_scoped() {
        assert!(download_url_allowed(
            "https://cas-bridge.xethub.hf.co/xet-bridge-us/model?key=value"
        ));
        for url in [
            "http://huggingface.co/model",
            "https://huggingface.co.evil.test/model",
            "https://user@huggingface.co/model",
            "https://127.0.0.1/model",
            "https://huggingface.co:443/model",
        ] {
            assert!(!download_url_allowed(url));
        }
    }
    #[test]
    fn whisper_offsets_are_milliseconds_and_track_offset_is_preserved() {
        let cues = decode_cues(&json!({"transcription":[{"offsets":{"from":1200,"to":2700},"text":"  Hello   world. "},{"offsets":{"from":2800,"to":2900},"text":"<|endoftext|>"}]}), 302.0, 30.0).unwrap();
        assert_eq!(cues.len(), 1);
        assert!((cues[0].start - 303.2).abs() < 0.0001);
        assert!((cues[0].end - 304.7).abs() < 0.0001);
        assert_eq!(cues[0].text, "Hello world.");
    }
    #[test]
    fn chunk_end_is_clamped_and_invalid_segments_ignored() {
        let cues = decode_cues(&json!({"transcription":[{"offsets":{"from":900,"to":5000},"text":"hello"},{"offsets":{"from":2000,"to":1000},"text":"invalid"}]}), 0.0, 1.0).unwrap();
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].end, 1.0);
    }

    #[test]
    fn long_audio_is_partitioned_without_allocating_pcm() {
        let duration = 4.0 * 3600.0 + 17.25;
        let mut position = 0.0;
        let mut count = 0;
        while position < duration {
            let span = chunk_span(position, duration, 300);
            assert!(span > 0.0 && span <= 300.0);
            position += span;
            count += 1;
        }
        assert_eq!(position, duration);
        assert_eq!(count, 49);
        assert_eq!(chunk_span(0.0, 10000.0, u64::MAX), 900.0);
    }

    #[test]
    fn library_paths_reject_traversal_and_browse_only_media() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::from_env().unwrap();
        config.library_root = directory.path().into();
        fs::write(directory.path().join("movie.mkv"), b"fixture").unwrap();
        fs::write(directory.path().join("document.txt"), b"fixture").unwrap();
        fs::create_dir(directory.path().join("folder")).unwrap();
        for relative in [
            "../outside.mkv",
            "./movie.mkv",
            "C:/movie.mkv",
            "\\movie.mkv",
            "/movie.mkv",
            "movie.mkv\0",
        ] {
            assert!(resolve_media(&config, relative, true).is_err());
        }
        assert!(resolve_media(&config, "movie.mkv", true).unwrap().is_file());
        assert!(resolve_media(&config, "document.txt", true).is_err());
        let listing = browse(&config, "").unwrap();
        assert_eq!(listing["entries"].as_array().unwrap().len(), 2);
        assert_eq!(listing["entries"][0]["name"], "folder");
    }

    #[test]
    fn sampled_fingerprint_catches_same_length_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("movie.mkv");
        fs::write(&path, [b'a'; 131072]).unwrap();
        let before = fingerprint(&path, "movie.mkv").unwrap();
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(b"b").unwrap();
        let after = fingerprint(&path, "movie.mkv").unwrap();
        assert_eq!(before["size"], after["size"]);
        assert_ne!(before["sample"], after["sample"]);
    }

    #[test]
    fn whisper_json_read_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("result.json");
        fs::write(&path, b"{\"data\":\"oversize\"}").unwrap();
        assert!(read_json(&path, 4).is_err());
        assert_eq!(read_json(&path, 64).unwrap()["data"], "oversize");
    }

    #[test]
    fn independent_transcription_progress_preserves_completed_job() {
        let mut job = json!({"status":"completed","stage":"Completed","progress":100,"position":900.0,"step_task":{"status":"running"}});
        apply_progress(&mut job, "Transcribing", 27, Some(300.0), Some("en"));
        assert_eq!(job["status"], "completed");
        assert_eq!(job["stage"], "Completed");
        assert_eq!(job["progress"], 100);
        assert_eq!(job["position"], 900.0);
        assert_eq!(job["step_task"]["stage"], "Transcribing");
        assert_eq!(job["step_task"]["progress"], 27);
        assert_eq!(job["step_task"]["position"], 300.0);
        assert_eq!(job["detected_language"], "en");
    }

    #[test]
    fn cancelled_download_does_not_create_files_or_request_network() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::from_env().unwrap();
        config.model_root = directory.path().join("models");
        assert_eq!(
            download_model_checked(&config, "small-q5_1", || Err("cancelled".into())),
            Err("cancelled".into())
        );
        assert!(!config.model_root.exists());
    }

    #[test]
    fn application_file_paths_reject_parent_components() {
        let directory = tempfile::tempdir().unwrap();
        assert!(scoped_path(directory.path(), Path::new("../outside")).is_err());
        assert!(scoped_path(directory.path(), Path::new("/outside")).is_err());
        assert!(
            scoped_path(directory.path(), Path::new("cache/audio/key.wav"))
                .unwrap()
                .starts_with(directory.path())
        );
    }

    #[test]
    #[ignore = "requires installed native ffmpeg and ffprobe"]
    fn native_ffmpeg_extracts_and_reuses_disk_audio_without_loading_a_model() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::from_env().unwrap();
        config.library_root = directory.path().join("library");
        config.output_root = directory.path().join("output");
        config.data_root = directory.path().join("data");
        config.model_root = directory.path().join("models");
        config.process_timeout = 10;
        let app = App::open(config).unwrap();
        // WAV content in an accepted media extension: FFmpeg probes the content.
        let media = app.config.library_root.join("fixture.mkv");
        let mut file = File::create(&media).unwrap();
        let length = 3u32 * 48000 * 2 * 2;
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + length).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&48000u32.to_le_bytes()).unwrap();
        file.write_all(&192000u32.to_le_bytes()).unwrap();
        file.write_all(&4u16.to_le_bytes()).unwrap();
        file.write_all(&16u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&length.to_le_bytes()).unwrap();
        let buffer = [0u8; 8192];
        let mut remaining = length as usize;
        while remaining > 0 {
            let count = remaining.min(buffer.len());
            file.write_all(buffer.get(..count).unwrap()).unwrap();
            remaining -= count;
        }
        drop(file);
        let job = app.create_job(&json!({"path":"fixture.mkv","audio_stream_index":0,"source_language":"en","transcript":true,"targets":[],"review_enabled":false})).unwrap();
        let id = job["id"].as_str().unwrap();
        let (language, cues) = transcribe(&app, id, &job, "fixture", true).unwrap();
        assert!(language.is_empty() && cues.is_empty());
        let audio = app.config.data_root.join("cache/audio/fixture.wav");
        let info = probe(&app.config, &audio).unwrap();
        assert_eq!(info["tracks"][0]["channels"], 1);
        assert!((number(&info["duration"]) - 3.0).abs() < 0.05);
        let modified = fs::metadata(&audio).unwrap().modified().unwrap();
        transcribe(&app, id, &job, "fixture", true).unwrap();
        assert_eq!(modified, fs::metadata(&audio).unwrap().modified().unwrap());
        assert_eq!(
            app.job(id).unwrap()["steps"]["extraction"]["status"],
            "completed"
        );
        assert!(
            fs::read_dir(&app.config.model_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_ancestors_cannot_escape_cache_or_model_roots() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.path().join("cache")).unwrap();
        assert!(scoped_path(directory.path(), Path::new("cache/audio/key.wav")).is_err());
        std::os::unix::fs::symlink(outside.path(), directory.path().join("models")).unwrap();
        assert!(
            scoped_path(
                &directory.path().join("models"),
                Path::new("ggml-small.bin")
            )
            .is_err()
        );
    }
}
