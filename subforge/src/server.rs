//! A bounded HTTP/1.1 dashboard. Fixed worker counts and bounded headers/body
//! prevent request traffic from allocating a thread or unbounded input buffer.

use crate::converter::{Converter, Workload};
use crate::integrations::Integrations;
use crate::tasks::Scheduler;
use crate::util::{constant_equal, now, read_json, token, write_private};
use crate::workers::{ActiveWorker, Workers};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc, Condvar, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

const HEADER_LIMIT: usize = 16 * 1024;
const BODY_LIMIT: usize = 64 * 1024;
const INVENTORY_LIMIT: usize = 20_000;
const PROBE_LIMIT: usize = 2000;
const HISTORY_LIMIT: usize = 100;
const JSON_LIMIT: usize = 32 * 1024 * 1024;
const HTTP_WORKERS: usize = 4;
const LOG_LIMIT: usize = 200;
const EXECUTOR_LIMIT: usize = 64;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
// Job coordinators wait for the same global file admission gate. This ceiling
// bounds coordinator stacks independently of requested slot counts.
const ACTIVE_JOB_LIMIT: usize = 32;
const PUBLICATION_JOB_LIMIT: usize = HISTORY_LIMIT + ACTIVE_JOB_LIMIT;
// Fingerprints bound confirmation state to about 1.3 MiB, even for a large job.
const INDEX_CONFIRM_LIMIT: usize = 20_000;

fn srt_index_key(value: &str) -> Option<String> {
    let path = Path::new(value);
    if value.len() > 4096
        || !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || !path.extension()?.to_str()?.eq_ignore_ascii_case("srt")
    {
        return None;
    }
    let normalized = path
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    let normalized = normalized.to_lowercase();
    Some(format!("{:x}", Sha256::digest(normalized.as_bytes())))
}

fn index_keys(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(INDEX_CONFIRM_LIMIT)
        .filter_map(Value::as_str)
        .filter(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_owned)
        .collect()
}

fn unconfirmed_index_keys(rows: &[Value], expected: &[String]) -> Vec<String> {
    let mut pending: HashSet<String> = expected.iter().cloned().collect();
    for row in rows {
        for subtitle in row["subtitles"].as_array().into_iter().flatten().take(256) {
            if subtitle["source"] == "sidecar"
                && let Some(key) = subtitle["path"].as_str().and_then(srt_index_key)
            {
                pending.remove(&key);
            }
        }
        if pending.is_empty() {
            break;
        }
    }
    let mut pending: Vec<_> = pending.into_iter().collect();
    pending.sort();
    pending
}

fn remember_published_srts(job: &mut Value, outputs: &[String]) {
    if !job["published_srt_keys"].is_array() {
        job["published_srt_keys"] = json!([]);
    }
    for output in outputs {
        let Some(key) = srt_index_key(output) else {
            job["verification_limited"] = json!(true);
            continue;
        };
        let entries = job["published_srt_keys"]
            .as_array_mut()
            .expect("initialized array");
        if entries.len() >= INDEX_CONFIRM_LIMIT {
            job["verification_limited"] = json!(true);
            break;
        }
        entries.push(json!(key));
    }
}

fn log_text(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect()
}

fn executor_details(value: &Value) -> Option<Value> {
    let resource = match value["resource"].as_str()? {
        "cpu" => "cpu",
        "cpu_ocr" => "cpu_ocr",
        "gpu" => "gpu",
        _ => return None,
    };
    let execution = match value["execution"].as_str()? {
        "local" => "local",
        "remote" => "remote",
        _ => return None,
    };
    let node_id = log_text(value["node_id"].as_str()?, 64);
    let node_name = log_text(value["node_name"].as_str()?, 64);
    if node_id.is_empty() || node_name.is_empty() {
        return None;
    }
    Some(json!({"node_id":node_id,"node_name":node_name,"resource":resource,"execution":execution}))
}

fn worker_event(mut event: Value, worker_id: usize, executor: &Value) -> Value {
    if let Some(fields) = event.as_object_mut() {
        // Execution identity comes only from coordinator admission, including
        // on failure and after a remote lease has been removed.
        for key in ["node_id", "node_name", "resource", "execution"] {
            fields.remove(key);
        }
        if let Some(executor) =
            executor_details(executor).and_then(|value| value.as_object().cloned())
        {
            fields.extend(executor);
        }
        fields.insert("worker_id".into(), json!(worker_id));
    }
    event
}

fn remember_executor(job: &mut Value, event: &Value) {
    let Some(executor) = executor_details(event) else {
        return;
    };
    if !job["executors"].is_array() {
        job["executors"] = json!([]);
    }
    let entries = job["executors"].as_array_mut().expect("initialized array");
    if entries.iter().any(|entry| {
        entry["node_id"] == executor["node_id"]
            && entry["resource"] == executor["resource"]
            && entry["execution"] == executor["execution"]
    }) {
        return;
    }
    if entries.len() < EXECUTOR_LIMIT {
        entries.push(executor);
    } else {
        job["executors_truncated"] = json!(true);
    }
}

/// Both sinks receive the same bounded JSON record. Only explicit operational
/// fields enter logs; requests, headers, endpoints and subprocess output do not.
fn progress_log(event: &Value) -> Value {
    let status = log_text(event["status"].as_str().unwrap_or("progress"), 48);
    let name = event["path"]
        .as_str()
        .and_then(|path| Path::new(path).file_name())
        .map(|name| log_text(&name.to_string_lossy(), 240));
    let error = event["error"].as_str().map(|error| log_text(error, 240));
    let mut message = format!("File {status}");
    if let Some(name) = &name {
        message.push_str(&format!(": {name}"));
    }
    let executor = executor_details(event);
    if let Some(executor) = &executor {
        let processor = match executor["resource"].as_str() {
            Some("gpu") => "GPU OCR",
            Some("cpu_ocr") => "CPU OCR",
            _ => "CPU",
        };
        message.push_str(&format!(
            "; node {} ({processor}, {})",
            executor["node_name"].as_str().unwrap_or(""),
            executor["execution"].as_str().unwrap_or("")
        ));
    }
    if let Some(worker) = event["worker_id"].as_u64() {
        message.push_str(&format!("; worker #{worker}"));
    }
    if let Some(track) = event["track"].as_u64() {
        message.push_str(&format!("; track {track}"));
    }
    if let Some(backend) = event["backend"].as_str() {
        message.push_str(&format!("; backend {}", log_text(backend, 32)));
    }
    if let Some(error) = &error {
        message.push_str(&format!("; {error}"));
    }
    if let Some(reason) = event["reason"].as_str() {
        message.push_str(&format!("; {}", log_text(reason, 240)));
    }
    let level = match status.as_str() {
        "failed" => "ERROR",
        "remote_retry" => "WARNING",
        _ => "INFO",
    };
    let mut entry = json!({"time":now(),"level":level,"message":message,"status":status});
    if let Some(name) = name {
        entry["file"] = json!(name);
    }
    if let Some(error) = error {
        entry["error"] = json!(error);
    }
    for key in [
        "worker_id",
        "track",
        "processed",
        "total",
        "progress_percent",
        "frames",
        "frames_total",
    ] {
        if let Some(value) = event[key].as_u64() {
            entry[key] = json!(value);
        }
    }
    for key in ["backend", "resource", "language", "coverage_status"] {
        if let Some(value) = event[key].as_str() {
            entry[key] = json!(log_text(value, 48));
        }
    }
    if let Some(executor) = executor.and_then(|value| value.as_object().cloned()) {
        entry.as_object_mut().expect("log object").extend(executor);
    }
    entry
}

fn write_log(output: &mut impl Write, entry: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *output, entry)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn record_log(runtime: &mut Runtime, entry: Value) {
    // A failed/disconnected collector must not turn a completed file into a failure.
    let _ = write_log(&mut std::io::stdout().lock(), &entry);
    runtime.logs.push_back(entry);
    while runtime.logs.len() > LOG_LIMIT {
        runtime.logs.pop_front();
    }
}

struct FileOutcome {
    state: Option<Value>,
    summary: Value,
    terminal: Option<Value>,
    gpu_health: Option<crate::gpu::Health>,
    description: Option<Result<Option<Value>, String>>,
}

#[derive(Clone, Copy)]
enum FileMode {
    Convert(Workload),
    Inspect { cached_only: bool },
}

type Descriptions = BTreeMap<PathBuf, Result<Option<Value>, String>>;

enum FileFailure {
    Conversion(String),
    Published {
        message: String,
        gpu: bool,
        state: Option<Value>,
        outputs: Vec<String>,
    },
}

impl From<String> for FileFailure {
    fn from(message: String) -> Self {
        Self::Conversion(message)
    }
}

enum FileEvent {
    Progress {
        id: usize,
        event: Value,
    },
    Finished {
        id: usize,
        path: PathBuf,
        outcome: Result<FileOutcome, FileFailure>,
    },
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
fn providers() -> [&'static str; 4] {
    ["sonarr", "radarr", "jellyfin", "bazarr"]
}
fn count(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}

fn available_sidecar(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            [
                "ass", "ssa", "vtt", "ttml", "dfxp", "smi", "sami", "sbv", "sub", "idx", "sup",
            ]
            .iter()
            .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

fn coverage_counts(rows: &[Value]) -> Value {
    let total = |status: &str| {
        rows.iter()
            .filter(|row| row["coverage"]["status"] == status)
            .count()
    };
    json!({"covered":total("covered"),"needs_conversion":total("needs_conversion"),"no_convertible_source":total("no_convertible_source"),"unknown":total("unknown")})
}

fn bazarr_source_hints(
    row: &Value,
    mounts: &[PathBuf],
) -> Vec<crate::converter::SubtitleSourceHint> {
    row["subtitles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|source| {
            if source["source"] != "sidecar" {
                return None;
            }
            let language = source["language"]
                .as_str()
                .filter(|language| !["und", "unknown", "zxx"].contains(language))?;
            let path = Converter::safe_path(source["path"].as_str()?, mounts).ok()?;
            Some(crate::converter::SubtitleSourceHint {
                path,
                target: crate::converter::SubtitleTarget {
                    language: language.into(),
                    forced: source["forced"].as_bool()?,
                    hi: source["hi"].as_bool()?,
                },
            })
        })
        .take(256)
        .collect()
}

#[derive(Default)]
struct Runtime {
    roots: Vec<PathBuf>,
    languages: Vec<String>,
    running: Option<Value>,
    active_jobs: BTreeMap<String, ActiveJob>,
    queue: VecDeque<Value>,
    history: VecDeque<Value>,
    logs: VecDeque<Value>,
    converted: HashSet<String>,
    inventory: Vec<Value>,
    inventory_updated: f64,
    inventory_truncated: bool,
    inventory_partial: bool,
    inventory_source: String,
    inventory_authority: Value,
    cached_roots: Vec<Value>,
    pipeline_counts: Value,
    state_revision: u64,
}
struct ActiveJob {
    owner: Option<std::thread::ThreadId>,
    cancel: Arc<AtomicBool>,
    job: Value,
}

impl Runtime {
    fn current_job(&self) -> Option<&Value> {
        let owner = std::thread::current().id();
        self.active_jobs
            .values()
            .find(|active| active.owner == Some(owner))
            .map(|active| &active.job)
            .or(self.running.as_ref())
    }

    fn current_job_mut(&mut self) -> Option<&mut Value> {
        let owner = std::thread::current().id();
        self.active_jobs
            .values_mut()
            .find(|active| active.owner == Some(owner))
            .map(|active| &mut active.job)
            .or(self.running.as_mut())
    }

    fn primary_job(&self) -> Option<&Value> {
        self.running.as_ref().or_else(|| {
            self.active_jobs
                .values()
                .min_by(|a, b| {
                    a.job["started"]
                        .as_f64()
                        .unwrap_or(0.0)
                        .total_cmp(&b.job["started"].as_f64().unwrap_or(0.0))
                })
                .map(|active| &active.job)
        })
    }
}

#[derive(Default)]
struct Publications {
    paths: std::collections::BTreeSet<String>,
    jobs: std::collections::BTreeSet<String>,
    limited: bool,
}
pub struct App {
    runtime: Mutex<Runtime>,
    converter: Mutex<Converter>,
    metadata_usage: Arc<crate::metadata::CacheUsage>,
    resources: Mutex<crate::resources::Sampler>,
    workers: Mutex<Workers>,
    remote: Mutex<crate::remote::Coordinator>,
    integrations: Mutex<Integrations>,
    publications: Mutex<Publications>,
    dispatch: Mutex<()>,
    inventory_sync: Mutex<()>,
    inventory_persist: Mutex<()>,
    public_integrations: Mutex<Value>,
    mounts: Vec<PathBuf>,
    data: PathBuf,
    password: String,
    csrf: String,
    webhook: String,
    widget: String,
    hosts: HashSet<String>,
    scheduler: Mutex<Scheduler>,
    started: Instant,
    ocr_permission: bool,
    shutdown: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    wake: Condvar,
}

fn symlink_component(path: &Path) -> bool {
    let mut part = PathBuf::new();
    for component in path.components() {
        part.push(component);
        if fs::symlink_metadata(&part).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return true;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if fs::symlink_metadata(&part)
                .is_ok_and(|metadata| metadata.file_attributes() & 0x400 != 0)
            {
                return true;
            }
        }
    }
    false
}
fn validate_root(path: &Path, mounts: &[PathBuf], exists: bool) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir))
        || symlink_component(path)
    {
        return Err(
            "Choose a directory inside a configured media mount; symlinks and traversal are denied"
                .into(),
        );
    }
    if !mounts.iter().any(|mount| path.starts_with(mount)) {
        return Err("Directory is outside configured media mounts".into());
    }
    if exists && !path.is_dir() {
        return Err("Directory is unavailable or unreadable".into());
    }
    Ok(path.to_path_buf())
}
fn root_info(root: &Path, mounts: &[PathBuf]) -> Value {
    let path = root.to_string_lossy().into_owned();
    let id = mounts
        .iter()
        .position(|mount| mount == root)
        .map(|index| index.to_string())
        .unwrap_or_else(|| {
            let digest = format!("{:x}", Sha256::digest(path.as_bytes()));
            format!("r{}", &digest[..12])
        });
    let available = root.is_dir() && !symlink_component(root);
    json!({"id":id,"path":path,"label":root.file_name().map(|name|name.to_string_lossy().into_owned()).unwrap_or_else(||path.clone()),
           "status":if available {"ready"} else {"unavailable"},"free_bytes":if available {fs2::available_space(root).ok()} else {None}})
}
fn completed_status(job: &Value) -> &'static str {
    if job["error"]
        .as_str()
        .is_some_and(|message| !message.is_empty())
        || (job["kind"] == "refresh"
            && job["results"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["ok"] == false && item["pending"] != true)
            }))
    {
        return "failed";
    }
    if count(&job["summary"]["failed"]) > 0
        || job["notifications"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["ok"] == false && item["pending"] != true)
        })
    {
        "completed_with_errors"
    } else {
        "completed"
    }
}

fn inventory_rows(value: &Value, mounts: &[PathBuf], roots: &[PathBuf]) -> Vec<Value> {
    value.as_array().into_iter().flatten().take(INVENTORY_LIMIT).filter_map(|row| {
        let path = row["path"].as_str()?;
        if path.len() > 4096 || validate_root(Path::new(path), mounts, false).is_err() { return None; }
        let name = Path::new(path).file_name()?.to_string_lossy().chars().take(256).collect::<String>();
        let tracks = row["tracks"].as_array().into_iter().flatten().take(crate::metadata::MAX_TRACKS).map(|track| {
            let mut result = serde_json::Map::new();
            for key in ["language", "format"] {
                result.insert(key.into(), json!(track[key].as_str().unwrap_or("").chars().take(35).collect::<String>()));
            }
            for key in ["forced", "hi"] { result.insert(key.into(), json!(track[key].as_bool().unwrap_or(false))); }
            result.insert("number".into(), json!(track["number"].as_u64()));
            result.insert("id".into(), json!(track["number"].as_u64()));
            Value::Object(result)
        }).collect::<Vec<_>>();
        let root_id = roots.iter().filter(|root| Path::new(path).starts_with(root)).max_by_key(|root| root.components().count()).map(|root| root_info(root, mounts)["id"].clone()).unwrap_or(Value::Null);
        let mut result = json!({"path":path,"name":name,"root_id":root_id,"type":if row["type"]=="sidecar" {"sidecar"} else {"video"},"tracks":tracks});
        if row["metadata_pending"] == true { result["metadata_pending"] = json!(true); }
        if let Some(error) = row["error"].as_str() { result["error"] = json!(error.chars().take(256).collect::<String>()); }
        for key in ["wanted_targets", "missing_targets", "bazarr_missing_targets", "subtitles", "source_hints", "coverage", "kind", "id", "series_id", "profile_id", "missing_srt", "has_convertible_source", "source"] {
            if let Some(value) = row.get(key).and_then(|value| crate::util::bounded_json_clone(value, 64 * 1024).ok()) { result[key] = value; }
        }
        Some(result)
    }).collect()
}

impl App {
    pub fn new(
        mounts: Vec<PathBuf>,
        data: PathBuf,
        interval: Duration,
        auto_scan: bool,
    ) -> Result<Arc<Self>, String> {
        if !data.is_absolute() || symlink_component(&data) {
            return Err("State storage must be an absolute directory without symlinks".into());
        }
        fs::create_dir_all(&data)
            .map_err(|_| "State storage must be writable by the process user")?;
        for mount in &mounts {
            if !mount.is_absolute()
                || mount.parent().is_none()
                || symlink_component(mount)
                || [
                    "/etc", "/proc", "/sys", "/dev", "/usr", "/var", "/root", "/bin", "/sbin",
                ]
                .iter()
                .any(|system| mount == Path::new(system))
            {
                return Err(
                    "Media mounts must be explicit non-system directories without symlinks".into(),
                );
            }
        }
        let converter = Converter::new(mounts.clone(), data.join("state.json"))?;
        let ocr_permission = converter.ocr_allowed();
        let metadata_usage = converter.metadata_usage();
        let workers = Workers::new(data.join("workers.conf"), ocr_permission)?;
        let remote = crate::remote::Coordinator::new(
            std::env::var("CONVERTER_WORKER_TOKEN").unwrap_or_default(),
            mounts.clone(),
        )?;
        let schedule_path = data.join("tasks.json");
        let new_schedule = !schedule_path.exists();
        let mut scheduler = Scheduler::new(schedule_path, now(), auto_scan)?;
        if new_schedule && interval.as_secs() >= 21600 {
            scheduler.update(&json!({"tasks":[{"id":"discovery","enabled":true,"interval_seconds":interval.as_secs().min(2592000)}]}), now())?;
        }
        let integrations = Integrations::new(data.join("settings.json"))?;
        let mut runtime = Runtime {
            roots: mounts.clone(),
            languages: vec!["*".into()],
            inventory_source: if integrations.bazarr_enabled() {
                "bazarr"
            } else {
                "filesystem"
            }
            .into(),
            inventory_authority: json!({"status":"not_synced","error":null}),
            ..Runtime::default()
        };
        if data.join("preferences.json").exists() {
            match read_json(&data.join("preferences.json"), 64 * 1024) {
                Ok(saved) => {
                    runtime.languages = Converter::normalize_languages(&saved["languages"])
                        .unwrap_or_else(|_| vec!["*".into()]);
                    if let Some(roots) = saved.get("root_paths") {
                        runtime.roots = if roots.as_array().is_some_and(|items| {
                            items.len() <= 32 && items.iter().all(Value::is_string)
                        }) {
                            strings(roots)
                                .iter()
                                .map(|value| validate_root(Path::new(value), &mounts, false))
                                .collect::<Result<Vec<_>, _>>()
                                .unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                    }
                }
                Err(_) => runtime.roots.clear(),
            }
        }
        if let Ok(saved) = read_json(&data.join("jobs.json"), JSON_LIMIT)
            && let Some(history) = saved.as_array()
        {
            runtime.history.extend(
                history
                    .iter()
                    .rev()
                    .take(HISTORY_LIMIT)
                    .rev()
                    .filter(|entry| entry.is_object())
                    .cloned(),
            );
        }
        if let Ok(saved) = read_json(&data.join("inventory.json"), JSON_LIMIT)
            && saved["source"].as_str().unwrap_or("filesystem") == runtime.inventory_source
        {
            runtime.inventory = inventory_rows(&saved["items"], &mounts, &runtime.roots);
            runtime.inventory_updated = saved["updated"].as_f64().unwrap_or(0.0);
            runtime.inventory_truncated = saved["truncated"].as_bool().unwrap_or(false);
            runtime.inventory_partial = saved["partial"].as_bool().unwrap_or(false);
            runtime.inventory_authority = json!({"status":if runtime.inventory_source == "bazarr" {"stale"} else {"ready"},"error":saved["authority"]["error"],"previous_status":saved["authority"]["status"],"pending_srt_keys":index_keys(&saved["authority"]["pending_srt_keys"]),"verification_limited":saved["authority"]["verification_limited"]==true});
        }
        // Inventory is stored once, independently of rotating job history.
        runtime.cached_roots = runtime
            .roots
            .iter()
            .map(|root| root_info(root, &mounts))
            .collect();
        runtime.pipeline_counts = converter.pipeline_counts();
        for job in &mut runtime.history {
            if let Some(fields) = job.as_object_mut() {
                fields.remove("published_paths");
                fields.remove("published_srt_keys");
            }
            if job["kind"] == "inspect" {
                job["results"] = json!([]);
            }
        }
        if let Some(state) = converter.state.as_object() {
            runtime.converted.extend(
                state
                    .iter()
                    .filter(|(_, item)| {
                        item["ok"] == true
                            || item["text_complete"] == true
                            || item["outputs"].as_array().is_some_and(|outputs| {
                                outputs.iter().any(|output| {
                                    output.as_str().is_some_and(|name| {
                                        Path::new(name).file_name().and_then(|value| value.to_str())
                                            == Some(name)
                                    })
                                })
                            })
                    })
                    .map(|(path, _)| path.clone()),
            );
        }
        let password = match std::env::var("CONVERTER_UI_PASSWORD")
            .ok()
            .filter(|password| !password.is_empty())
        {
            Some(password) => password,
            None => {
                let path = data.join("ui-password");
                if path.exists() {
                    if path.is_symlink() {
                        return Err("Saved dashboard password must not be a symlink".into());
                    }
                    let mut bytes = String::new();
                    fs::File::open(&path)
                        .map_err(|_| "Saved password is unavailable")?
                        .take(1025)
                        .read_to_string(&mut bytes)
                        .map_err(|_| "Saved password could not be read")?;
                    bytes.trim().to_owned()
                } else {
                    let password = token(32);
                    let mut options = OpenOptions::new();
                    options.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let mut file = options
                        .open(&path)
                        .map_err(|_| "Cannot create private dashboard password")?;
                    writeln!(file, "{password}").map_err(|_| "Cannot save dashboard password")?;
                    password
                }
            }
        };
        if !(12..=256).contains(&password.len()) || password.chars().any(char::is_control) {
            return Err("CONVERTER_UI_PASSWORD must have 12–256 printable characters or be blank for a generated password".into());
        }
        let widget = std::env::var("CONVERTER_WIDGET_TOKEN").unwrap_or_default();
        if !widget.is_empty()
            && (!(32..=256).contains(&widget.len())
                || !widget
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._~+/-=".contains(&b)))
        {
            return Err(
                "CONVERTER_WIDGET_TOKEN must be a 32–256 character bearer token or blank".into(),
            );
        }
        let hosts = std::env::var("CONVERTER_ALLOWED_HOSTS")
            .unwrap_or_else(|_| "localhost,127.0.0.1,::1,subforge,subtitle-converter".into())
            .split(',')
            .map(|host| host.trim().to_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        let app = Arc::new(Self {
            public_integrations: Mutex::new(integrations.public_settings()),
            runtime: Mutex::new(runtime),
            converter: Mutex::new(converter),
            metadata_usage,
            resources: Mutex::new(crate::resources::Sampler::default()),
            workers: Mutex::new(workers),
            remote: Mutex::new(remote),
            integrations: Mutex::new(integrations),
            publications: Mutex::new(Publications::default()),
            dispatch: Mutex::new(()),
            inventory_sync: Mutex::new(()),
            inventory_persist: Mutex::new(()),
            mounts,
            data,
            password,
            csrf: token(32),
            webhook: std::env::var("CONVERTER_WEBHOOK_TOKEN").unwrap_or_default(),
            widget,
            hosts,
            scheduler: Mutex::new(scheduler),
            started: Instant::now(),
            ocr_permission,
            shutdown: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            wake: Condvar::new(),
        });
        app.restore_publications();
        app.log(
            "INFO",
            "Rust dashboard ready; file workers use bounded allocation",
        );
        Ok(app)
    }
    fn log(&self, level: &str, message: &str) {
        let mut runtime = lock(&self.runtime);
        record_log(
            &mut runtime,
            json!({"time":now(),"level":log_text(level,16),"message":log_text(message,1024)}),
        );
    }
    fn cancellation(&self) -> Arc<AtomicBool> {
        let owner = std::thread::current().id();
        lock(&self.runtime)
            .active_jobs
            .values()
            .find(|active| active.owner == Some(owner))
            .map(|active| Arc::clone(&active.cancel))
            .unwrap_or_else(|| Arc::clone(&self.cancel))
    }
    fn settings(&self) -> Value {
        let schedule = lock(&self.scheduler).public();
        let scan_interval = schedule["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|task| task["id"] == "discovery")
            .map(|task| task["interval_seconds"].clone())
            .unwrap_or(json!(21600));
        let public = lock(&self.public_integrations).clone();
        let runtime = lock(&self.runtime);
        let mut connections = json!({});
        for name in providers() {
            connections[name] = public[name].clone();
        }
        for name in ["sonarr", "radarr"] {
            connections[name]["webhook_enabled"] = json!(!self.webhook.is_empty());
        }
        json!({"languages":runtime.languages,"integrations":connections,"path_mappings":public["path_mappings"],"scan_interval":scan_interval,"permissions":{"ocr":std::env::var("CONVERTER_ENABLE_OCR").is_ok_and(|value|value=="true" || value=="1")},"runtime":"rust","version":env!("CARGO_PKG_VERSION"),"edition":crate::edition::NAME})
    }
    fn status(&self) -> Value {
        let settings = self.settings();
        let runtime = lock(&self.runtime);
        let mut history: Vec<Value> = runtime.history.iter().rev().cloned().collect();
        for job in &mut history {
            if job["status"] == "completed" {
                job["status"] = json!(completed_status(job));
            }
        }
        let mut pipeline = runtime.pipeline_counts.clone();
        pipeline["files_inspected"] = json!(runtime.inventory.len());
        pipeline["root_count"] = json!(runtime.roots.len());
        pipeline["unreachable_roots"] = json!(
            runtime
                .cached_roots
                .iter()
                .filter(|root| root["status"] == "unavailable")
                .count()
        );
        pipeline["scope"] = json!({"outputs":"tracked_published_outputs","files":"saved_inventory","roots":"cached_root_metadata"});
        pipeline["available"] = json!({"generated_srt_count":pipeline["generated_srt_count_available"],"pending_ocr_files":true,"failed_files":true,"files_inspected":runtime.inventory_updated>0.0,"unreachable_roots":true});
        if let Some(object) = pipeline.as_object_mut() {
            object.remove("generated_srt_count_available");
        }
        pipeline["inventory_updated"] = json!(runtime.inventory_updated);
        pipeline["inventory_truncated"] = json!(runtime.inventory_truncated);
        pipeline["inventory_partial"] = json!(runtime.inventory_partial);
        json!({"version":env!("CARGO_PKG_VERSION"),"edition":crate::edition::NAME,"running":runtime.primary_job(),"active_jobs":runtime.active_jobs.values().map(|active| &active.job).collect::<Vec<_>>(),"queue":runtime.queue,"history":history,"logs":runtime.logs,"converted_count":runtime.converted.len(),"pipeline":pipeline,"inventory":{"count":runtime.inventory.len(),"updated":runtime.inventory_updated,"truncated":runtime.inventory_truncated,"partial":runtime.inventory_partial,"state_revision":runtime.state_revision,"source":runtime.inventory_source,"authority":runtime.inventory_authority},"roots":runtime.cached_roots,"settings":settings})
    }
    fn tasks(&self) -> Value {
        let mut value = lock(&self.scheduler).public();
        let runtime = lock(&self.runtime);
        for task in value["tasks"].as_array_mut().into_iter().flatten() {
            let active = runtime
                .running
                .iter()
                .chain(runtime.active_jobs.values().map(|active| &active.job))
                .chain(runtime.queue.iter())
                .find(|job| job["task_id"] == task["id"]);
            task["job_id"] = active.map(|job| job["id"].clone()).unwrap_or(Value::Null);
            if let Some(job) = active {
                task["status"] = job["status"].clone();
            }
        }
        value
    }
    fn system(&self) -> Value {
        let schedule = lock(&self.scheduler).public();
        let interval = schedule["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|task| task["id"] == "discovery")
            .map(|task| task["interval_seconds"].clone())
            .unwrap_or(json!(21600));
        let sample = lock(&self.resources).snapshot();
        let limits = lock(&self.workers).limits(&sample);
        let active = lock(&self.workers).active.len();
        let mut usage = json!(sample);
        usage["metadata_cache"] = self.metadata_usage.snapshot();
        let worker = {
            let runtime = lock(&self.runtime);
            json!({"active":active,
                "kind":runtime.primary_job().map(|job| job["kind"].clone())})
        };
        usage["worker"] = worker;
        json!({"version":env!("CARGO_PKG_VERSION"),"edition":crate::edition::NAME,"runtime":"rust","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"uptime_seconds":self.started.elapsed().as_secs(),"resource_usage":usage,"resource_policy":{"conversion_workers":limits.cpu_workers,"ocr_threads":1,"ocr_workers":limits.ocr_workers,"gpu_workers":limits.gpu_workers,"metadata_cache_max_bytes":crate::metadata::LIMIT_BYTES,"metadata_cache_max_entries":crate::metadata::MAX_ENTRIES,"discovery_interval_seconds":interval,"ocr_permission":self.ocr_permission,"automatic_ocr_new_or_changed_only":true},"scheduler":{"paused":schedule["paused"]}})
    }

    fn workers(&self) -> Value {
        let sample = lock(&self.resources).snapshot();
        let mut value = lock(&self.workers).public(&sample);
        value["remote"] = lock(&self.remote).public();
        value
    }

    fn update_workers(&self, data: &Value) -> Result<Value, String> {
        lock(&self.workers).update(data)?;
        let allocation = lock(&self.workers).allocation;
        self.log(
            "INFO",
            &format!(
                "Worker targets updated: CPU {}, CPU OCR {}, GPU {}; paused {}",
                allocation.cpu_workers,
                allocation.ocr_workers,
                allocation.gpu_workers,
                allocation.paused
            ),
        );
        self.wake.notify_all();
        Ok(self.workers())
    }
    fn enqueue_task(&self, id: &str, origin: &str, retry: bool) -> Result<Option<Value>, String> {
        let kind = match id {
            "discovery" => "scan",
            "ocr" => "ocr",
            "inventory" => "inspect",
            "health" => "health",
            "cleanup" => "cleanup",
            "backup" => "backup",
            _ => return Err("Unknown task".into()),
        };
        if id == "ocr" && !self.ocr_permission {
            return Err("OCR is disabled by deployment permissions".into());
        }
        self.enqueue(&json!({"kind":kind,"task_id":id,"retry":retry}), origin)
    }
    fn inventory(&self) -> Value {
        let runtime = lock(&self.runtime);
        let converter = lock(&self.converter);
        let mut rows = runtime.inventory.clone();
        for row in &mut rows {
            row["processing"] = converter.file_processing(row["path"].as_str().unwrap_or(""));
        }
        let coverage = coverage_counts(&rows);
        json!({"items":rows,"updated":runtime.inventory_updated,"truncated":runtime.inventory_truncated,"partial":runtime.inventory_partial,"state_revision":runtime.state_revision,"source":runtime.inventory_source,"authority":runtime.inventory_authority,"coverage_counts":coverage,"covered_count":coverage["covered"],"needs_conversion_count":coverage["needs_conversion"]})
    }

    fn persist_inventory(&self) -> Result<(), String> {
        let _writer = lock(&self.inventory_persist);
        let runtime = lock(&self.runtime);
        let snapshot = json!({"items":runtime.inventory,"updated":runtime.inventory_updated,"truncated":runtime.inventory_truncated,"partial":runtime.inventory_partial,"source":runtime.inventory_source,"authority":runtime.inventory_authority});
        drop(runtime);
        crate::util::write_private_limited(
            &self.data.join("inventory.json"),
            &snapshot,
            JSON_LIMIT as u64,
        )
    }

    fn bazarr_failure(&self, message: &str) {
        let mut runtime = lock(&self.runtime);
        if runtime.inventory_source != "bazarr" {
            runtime.inventory.clear();
        }
        runtime.inventory_source = "bazarr".into();
        runtime.inventory_partial = true;
        runtime.inventory_authority = json!({"status":"unavailable","error":log_text(message,512),"pending_srt_keys":index_keys(&runtime.inventory_authority["pending_srt_keys"]),"verification_limited":runtime.inventory_authority["verification_limited"]==true});
        runtime.state_revision = runtime.state_revision.saturating_add(1);
        drop(runtime);
        let _ = self.persist_inventory();
        self.log("ERROR", &format!("Bazarr inventory unavailable: {}; conversion remains restricted to explicit requests", log_text(message,512)));
    }

    /// Bazarr selects the library and target variants. Local probing only
    /// answers whether those missing variants have a conversion source.
    fn sync_bazarr_inventory(
        &self,
        integrations: &mut Integrations,
        job: &Value,
        probe: bool,
    ) -> Result<Vec<Value>, String> {
        let inventory = match integrations.bazarr_inventory(&self.cancellation()) {
            Ok(inventory) => inventory,
            Err(error) => {
                self.bazarr_failure(&error);
                return Err(error);
            }
        };
        let roots = lock(&self.runtime).roots.clone();
        let mut rows = inventory_rows(&inventory["items"], &self.mounts, &roots);
        // The profile is authoritative about requirements; a stale index does
        // not make an absent, empty or malformed local SRT valid coverage.
        for row in &mut rows {
            row["bazarr_missing_targets"] = row["missing_targets"].clone();
            let hints = bazarr_source_hints(row, &self.mounts);
            row["source_hints"] = json!(hints);
            let mut coverage_worker = lock(&self.converter).inspection_worker();
            if let Err(error) = coverage_worker.set_source_hints(hints) {
                row["source_hints_error"] = json!(true);
                row["error"] = json!(log_text(&error, 256));
                continue;
            }
            let wanted: Vec<crate::converter::SubtitleTarget> =
                serde_json::from_value(row["wanted_targets"].clone())
                    .map_err(|_| "Bazarr returned invalid wanted subtitle targets")?;
            if !wanted.is_empty()
                && let Some(path) = row["path"].as_str()
            {
                match coverage_worker.conversion_coverage(Path::new(path), &json!([]), &wanted) {
                    Ok(coverage) => {
                        row["missing_targets"] = coverage["missing_targets"].clone();
                        row["coverage"] = coverage;
                    }
                    Err(error) => {
                        row["source_hints_error"] = json!(true);
                        row["error"] = json!(log_text(&error, 256));
                    }
                }
            }
        }
        let mut paths = Vec::new();
        let explicit_inspection = if job["kind"] == "inspect" {
            strings(&job["paths"])
        } else {
            Vec::new()
        };
        let requested_paths = strings(&job["paths"]);
        let scope_roots = strings(&job["root_paths"]);
        for row in &rows {
            if row["source_hints_error"] == true {
                continue;
            }
            let row_path = row["path"].as_str().unwrap_or("");
            if !scope_roots.is_empty()
                && !scope_roots
                    .iter()
                    .any(|root| Path::new(row_path).starts_with(root))
            {
                continue;
            }
            if !requested_paths.is_empty()
                && !requested_paths.iter().any(|requested| {
                    requested == row_path
                        || row["subtitles"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|subtitle| subtitle["path"] == *requested)
                })
            {
                continue;
            }
            let known_sources_cover_targets =
                row["missing_targets"].as_array().is_some_and(|targets| {
                    targets.iter().all(|target| {
                        row["coverage"]["available_targets"]
                            .as_array()
                            .is_some_and(|available| available.contains(target))
                    })
                });
            if ((row["missing_targets"]
                .as_array()
                .is_some_and(|targets| !targets.is_empty())
                && !known_sources_cover_targets)
                || row["path"].as_str().is_some_and(|path| {
                    explicit_inspection
                        .iter()
                        .any(|requested| requested == path)
                }))
                && let Some(path) = row["path"].as_str()
                && let Ok(path) = Converter::safe_path(path, &self.mounts)
                && paths.len() < PROBE_LIMIT
            {
                paths.push(path);
            }
        }
        let (mut descriptions, probe_error) = if probe && !paths.is_empty() {
            match self.run_files(
                &paths,
                job,
                FileMode::Inspect { cached_only: false },
                integrations,
            ) {
                Ok((_, descriptions)) => (descriptions, None),
                Err(error) => {
                    self.log(
                        "WARNING",
                        &format!(
                            "Bazarr candidates could not be probed: {}",
                            log_text(&error, 256)
                        ),
                    );
                    (BTreeMap::new(), Some(error))
                }
            }
        } else {
            (BTreeMap::new(), None)
        };
        let mut metadata_pending = 0_usize;
        for row in &mut rows {
            let targets: Vec<crate::converter::SubtitleTarget> =
                serde_json::from_value(row["missing_targets"].clone())
                    .map_err(|_| "Bazarr returned invalid missing subtitle targets")?;
            row["source"] = json!("bazarr");
            if row["source_hints_error"] == true {
                row["coverage"] =
                    json!({"status":"unknown","reason":row["error"],"missing_targets":targets});
                metadata_pending += 1;
                continue;
            }
            let path = PathBuf::from(row["path"].as_str().unwrap_or(""));
            let described = descriptions
                .remove(&path)
                .unwrap_or_else(|| Ok(lock(&self.converter).cached_description(&path)));
            if targets.is_empty() {
                if let Ok(Some(description)) = &described {
                    row["tracks"] = description.clone();
                }
                if row["wanted_targets"]
                    .as_array()
                    .is_some_and(|targets| !targets.is_empty())
                {
                    row["missing_srt"] = json!(false);
                    row["coverage"] = json!({"status":"covered","reason":"Valid external SRT already covers every configured Bazarr language and variant","existing_targets":row["wanted_targets"],"missing_targets":[]});
                } else {
                    row["missing_srt"] = Value::Null;
                    row["coverage"] = json!({"status":"unknown","reason":"Bazarr has no resolved language profile targets for this media; automatic conversion is disabled","existing_targets":[],"missing_targets":[]});
                }
                continue;
            }
            match described {
                Ok(Some(description)) => {
                    row["tracks"] = description.clone();
                    let wanted: Vec<crate::converter::SubtitleTarget> =
                        serde_json::from_value(row["wanted_targets"].clone())
                            .map_err(|_| "Bazarr returned invalid wanted subtitle targets")?;
                    let mut coverage_worker = lock(&self.converter).inspection_worker();
                    let hints: Vec<crate::converter::SubtitleSourceHint> =
                        serde_json::from_value(row["source_hints"].clone())
                            .map_err(|_| "Invalid Bazarr source hints")?;
                    coverage_worker.set_source_hints(hints)?;
                    let coverage =
                        coverage_worker.conversion_coverage(&path, &description, &wanted);
                    match coverage {
                        Ok(mut coverage) => {
                            coverage["status"] =
                                json!(match coverage["status"].as_str().unwrap_or("") {
                                    "already_covered" => "covered",
                                    "needed" => "needs_conversion",
                                    "no_available_conversion" => "no_convertible_source",
                                    _ => "unknown",
                                });
                            row["missing_srt"] = json!(coverage["status"] != "covered");
                            row["missing_targets"] = coverage["missing_targets"].clone();
                            row["coverage"] = coverage;
                        }
                        Err(error) => {
                            row["coverage"] = json!({"status":"unknown","reason":log_text(&error,256),"missing_targets":targets});
                            row["error"] = json!(log_text(&error, 256));
                            metadata_pending += 1;
                        }
                    }
                }
                Ok(None) if !strings(&row["coverage"]["available_paths"]).is_empty() => {
                    row["coverage"]["status"] = json!("needs_conversion");
                    row["missing_srt"] = json!(true);
                }
                result => {
                    metadata_pending += 1;
                    row["metadata_pending"] = json!(true);
                    let reason = match result { Err(error) => log_text(&error,256), _=>probe_error.as_ref().map(|error|log_text(error,256)).unwrap_or_else(||"Track metadata has not been inspected; only missing Bazarr targets will be probed".into()) };
                    row["coverage"] =
                        json!({"status":"unknown","reason":reason,"missing_targets":targets});
                }
            }
        }
        let partial = inventory["partial"].as_bool().unwrap_or(false)
            || metadata_pending > 0
            || self.cancellation().load(Ordering::Relaxed);
        let truncated = inventory["truncated"].as_bool().unwrap_or(false)
            || inventory["items"]
                .as_array()
                .is_some_and(|items| items.len() > INVENTORY_LIMIT);
        let mut runtime = lock(&self.runtime);
        runtime.inventory = rows.clone();
        runtime.inventory_source = "bazarr".into();
        runtime.inventory_updated = now();
        runtime.inventory_partial = partial;
        runtime.inventory_truncated = truncated;
        let previous_keys = index_keys(&runtime.inventory_authority["pending_srt_keys"]);
        let limited = runtime.inventory_authority["verification_limited"] == true;
        let unconfirmed = unconfirmed_index_keys(&rows, &previous_keys);
        runtime.inventory_authority = json!({"status":if !unconfirmed.is_empty() || limited {"refresh_pending"} else {"ready"},"error":null,"metadata_pending":metadata_pending,"probe_limit":PROBE_LIMIT,"unmapped_or_unavailable":inventory["items"].as_array().map_or(0,Vec::len).saturating_sub(rows.len()),"pending_srt_keys":unconfirmed,"verification_limited":limited});
        if !previous_keys.is_empty() && runtime.inventory_authority["status"] == "ready" {
            for history in &mut runtime.history {
                if let Some(notices) = history["notifications"].as_array_mut() {
                    for notice in notices.iter_mut().filter(|notice| {
                        notice["provider"] == "bazarr" && notice["pending"] == true
                    }) {
                        notice["ok"] = json!(true);
                        notice["pending"] = json!(false);
                        notice["uncertain"] = json!(false);
                        notice["verified"] = json!(true);
                        notice["message"] =
                            json!("Published SRT files confirmed by a later Bazarr inventory sync");
                    }
                }
            }
            self.persist_history(&runtime);
        }
        runtime.state_revision = runtime.state_revision.saturating_add(1);
        drop(runtime);
        self.persist_inventory()?;
        self.log(
            "INFO",
            &format!(
                "Bazarr inventory synchronized: {} media rows; {metadata_pending} pending metadata",
                rows.len()
            ),
        );
        Ok(rows)
    }

    fn execute_bazarr(&self, job: &Value, integrations: &mut Integrations) -> Result<(), String> {
        let requested = strings(&job["paths"]);
        let selected = |row: &Value| {
            requested.is_empty()
                || requested.iter().any(|path| row["path"] == *path)
                || row["coverage"]["available_paths"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .any(|path| requested.iter().any(|selected| selected == path))
        };
        let rows = {
            // Inventory refresh/inspection is a single writer. Conversion
            // coordinators may then consume their selected snapshot in parallel.
            let _sync = lock(&self.inventory_sync);
            let existing = {
                let runtime = lock(&self.runtime);
                (job["kind"] != "inspect"
                    && runtime.inventory_source == "bazarr"
                    && runtime.inventory_updated >= job["created"].as_f64().unwrap_or(f64::MAX)
                    && ["ready", "refresh_pending"]
                        .iter()
                        .any(|status| runtime.inventory_authority["status"] == *status))
                .then(|| {
                    runtime
                        .inventory
                        .iter()
                        .filter(|row| selected(row))
                        .cloned()
                        .collect::<Vec<_>>()
                })
            };
            match existing {
                Some(rows) => rows,
                None => self
                    .sync_bazarr_inventory(integrations, job, true)?
                    .into_iter()
                    .filter(selected)
                    .collect(),
            }
        };
        if job["kind"] == "inspect" {
            if let Some(running) = lock(&self.runtime).current_job_mut() {
                running["total"] = json!(rows.len());
                running["processed"] = json!(rows.len());
                running["summary"] = json!({"inspected":rows.len(),"source":"bazarr","covered":rows.iter().filter(|row|row["coverage"]["status"]=="covered").count(),"needed":rows.iter().filter(|row|row["coverage"]["status"]=="needs_conversion").count()});
            }
            return Ok(());
        }
        let roots = strings(&job["root_paths"])
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let languages = strings(&job["languages"]);
        let mut targets_by_path: BTreeMap<PathBuf, Vec<crate::converter::SubtitleTarget>> =
            BTreeMap::new();
        let mut sources_by_path: BTreeMap<PathBuf, Vec<crate::converter::SubtitleSourceHint>> =
            BTreeMap::new();
        let mut skipped = 0_u64;
        if let Some(running) = lock(&self.runtime).current_job_mut() {
            running["processed"] = json!(0);
            running["total"] = json!(rows.len());
        }
        for row in &rows {
            let path = row["path"].as_str().unwrap_or("");
            if !roots.iter().any(|root| Path::new(path).starts_with(root)) {
                continue;
            }
            let available_paths = strings(&row["coverage"]["available_paths"]);
            if !requested.is_empty()
                && !requested
                    .iter()
                    .any(|requested| requested == path || available_paths.contains(requested))
            {
                continue;
            }
            let targets: Vec<crate::converter::SubtitleTarget> =
                serde_json::from_value(row["missing_targets"].clone())
                    .map_err(|_| "Bazarr returned invalid subtitle targets")?;
            let targets = targets
                .into_iter()
                .filter(|target| {
                    languages.iter().any(|language| {
                        language == "*"
                            || language == &target.language
                            || target
                                .language
                                .strip_prefix(language)
                                .is_some_and(|suffix| suffix.starts_with('-'))
                    })
                })
                .collect::<Vec<_>>();
            if row["coverage"]["status"] != "needs_conversion" || targets.is_empty() {
                skipped += 1;
                self.progress(json!({"status":"skipped","file_terminal":true,"path":path,"reason":if targets.is_empty() && row["coverage"]["status"]=="needs_conversion" {"No missing targets match the requested languages"} else {row["coverage"]["reason"].as_str().unwrap_or("No available conversion is required")},"coverage_status":row["coverage"]["status"]}));
                continue;
            }
            let mut candidates = available_paths;
            candidates.sort_by_key(|candidate| candidate == path);
            if job.get("track_ids").is_some() {
                candidates.retain(|candidate| candidate == path);
            }
            for candidate in candidates {
                if !requested.is_empty()
                    && requested
                        .iter()
                        .any(|requested| available_sidecar(requested))
                    && !requested.contains(&candidate)
                {
                    continue;
                }
                let candidate = Converter::safe_path(&candidate, &roots)?;
                if targets_by_path.len() >= INVENTORY_LIMIT
                    && !targets_by_path.contains_key(&candidate)
                {
                    return Err("Bazarr conversion candidates exceed the bounded job limit; select a smaller scope".into());
                }
                let allocated = targets_by_path.entry(candidate.clone()).or_default();
                for target in &targets {
                    if !allocated.contains(target) {
                        allocated.push(target.clone());
                    }
                }
                let sources: Vec<crate::converter::SubtitleSourceHint> =
                    serde_json::from_value(row["source_hints"].clone())
                        .map_err(|_| "Invalid Bazarr source hints")?;
                let allocated = sources_by_path.entry(candidate.clone()).or_default();
                for source in sources
                    .into_iter()
                    .filter(|source| crate::workers::conflicts(&candidate, &source.path))
                {
                    if !allocated.iter().any(|existing| {
                        existing.path == source.path && existing.target == source.target
                    }) {
                        allocated.push(source);
                    }
                }
            }
        }
        let mut execution = job.clone();
        execution["file_targets"] = json!(
            targets_by_path
                .iter()
                .map(|(path, targets)| (path.to_string_lossy().into_owned(), json!(targets)))
                .collect::<BTreeMap<_, _>>()
        );
        execution["file_languages"] = json!(
            targets_by_path
                .iter()
                .map(|(path, targets)| (
                    path.to_string_lossy().into_owned(),
                    json!(
                        targets
                            .iter()
                            .map(|target| target.language.clone())
                            .collect::<HashSet<_>>()
                    )
                ))
                .collect::<BTreeMap<_, _>>()
        );
        execution["file_sources"] = json!(
            sources_by_path
                .into_iter()
                .map(|(path, sources)| (path.to_string_lossy().into_owned(), sources))
                .collect::<BTreeMap<_, _>>()
        );
        let mut paths = targets_by_path.into_keys().collect::<Vec<_>>();
        paths.sort_by_key(|path| !available_sidecar(&path.to_string_lossy()));
        let workload = match job["workload"].as_str().unwrap_or("text") {
            "ocr" => Workload::Ocr,
            "all" => Workload::All,
            _ => Workload::Text,
        };
        let mut summary = self.convert_files(&paths, &execution, workload, integrations)?;
        summary["skipped"] = json!(skipped);
        summary["source"] = json!("bazarr");
        if let Some(running) = lock(&self.runtime).current_job_mut() {
            running["summary"] = summary;
            running["total"] = json!(paths.len() as u64 + skipped);
        }
        Ok(())
    }

    fn prepare_bazarr_verification(&self) -> Result<(), String> {
        let mut runtime = lock(&self.runtime);
        let Some(job) = runtime
            .current_job()
            .filter(|job| count(&job["published_outputs"]) > 0)
        else {
            return Ok(());
        };
        let job_keys = index_keys(&job["published_srt_keys"]);
        let mut limited = job_keys.is_empty()
            || job["verification_limited"] == true
            || runtime.inventory_authority["verification_limited"] == true;
        let mut keys: HashSet<_> = index_keys(&runtime.inventory_authority["pending_srt_keys"])
            .into_iter()
            .collect();
        for key in job_keys {
            if keys.len() >= INDEX_CONFIRM_LIMIT && !keys.contains(&key) {
                limited = true;
                break;
            }
            keys.insert(key);
        }
        let mut keys: Vec<_> = keys.into_iter().collect();
        keys.sort();
        runtime.inventory_authority["pending_srt_keys"] = json!(keys);
        runtime.inventory_authority["verification_limited"] = json!(limited);
        drop(runtime);
        self.persist_inventory()
    }

    fn refresh_bazarr_after_publication(
        &self,
        integrations: &mut Integrations,
        job: &Value,
        mut reindex: Option<&mut Value>,
    ) -> Result<(), String> {
        let (published, keys, limited) = lock(&self.runtime)
            .current_job()
            .map(|job| {
                (
                    count(&job["published_outputs"]),
                    index_keys(&job["published_srt_keys"]),
                    job["verification_limited"] == true,
                )
            })
            .unwrap_or_default();
        if !integrations.bazarr_enabled() || published == 0 {
            return Ok(());
        }
        self.prepare_bazarr_verification()?;
        if self.cancellation().load(Ordering::Relaxed) {
            let mut runtime = lock(&self.runtime);
            runtime.inventory_authority["status"] = json!("refresh_pending");
            runtime.inventory_authority["reason"] = json!(
                "SRT files were published before cancellation; sync Bazarr inventory to verify indexing"
            );
            drop(runtime);
            return self.persist_inventory();
        }
        let refreshed = self.sync_bazarr_inventory(integrations, job, false);
        let uncertain = reindex
            .as_ref()
            .is_some_and(|result| result["pending"] == true || result["uncertain"] == true);
        let verified = !keys.is_empty()
            && !limited
            && refreshed
                .as_ref()
                .is_ok_and(|rows| unconfirmed_index_keys(rows, &keys).is_empty());
        if uncertain && verified {
            if let Some(result) = reindex.as_mut() {
                result["ok"] = json!(true);
                result["pending"] = json!(false);
                result["uncertain"] = json!(false);
                result["verified"] = json!(true);
                result["verified_outputs"] = json!(keys.len());
                result["message"] = json!(format!(
                    "{} published SRT file(s) confirmed in Bazarr inventory after an unconfirmed scan response",
                    keys.len()
                ));
            }
            self.log(
                "INFO",
                &format!(
                    "Fresh Bazarr inventory contains all {} published SRT files",
                    keys.len()
                ),
            );
        } else if uncertain {
            let mut runtime = lock(&self.runtime);
            if refreshed.is_ok() {
                runtime.inventory_authority["status"] = json!("refresh_pending");
            }
            runtime.inventory_authority["reason"] = json!(
                "Bazarr scan completion is not yet confirmed. Sync Bazarr inventory to check again; the rescan is not automatically repeated."
            );
            if keys.is_empty() || limited {
                runtime.inventory_authority["verification_limited"] = json!(true);
            }
            drop(runtime);
            self.persist_inventory()?;
            self.log("WARNING", "Bazarr scan response is unconfirmed; published SRT files remain safe and no automatic rescan retry was sent");
            // A failed verification read cannot turn an uncertain scan into a
            // definite scan failure. Its diagnostic is retained in authority.
            return Ok(());
        } else if reindex
            .as_ref()
            .is_none_or(|result| result["ok"] != true || count(&result["triggered"]) == 0)
        {
            let error = reindex
                .as_ref()
                .and_then(|result| result["message"].as_str())
                .unwrap_or("Scoped Bazarr reindex did not complete");
            let mut runtime = lock(&self.runtime);
            runtime.inventory_authority["status"] = json!("reindex_failed");
            runtime.inventory_authority["error"] = json!(log_text(error, 512));
            runtime.inventory_authority["published_outputs"] = json!(published);
            drop(runtime);
            self.persist_inventory()?;
            self.log(
                "WARNING",
                &format!(
                    "SRT publication succeeded but Bazarr reindex failed: {}",
                    log_text(error, 256)
                ),
            );
        }
        refreshed.map(|_| ())
    }

    fn refresh_pipeline(&self) {
        let counts = lock(&self.converter).pipeline_counts();
        let mut runtime = lock(&self.runtime);
        runtime.pipeline_counts = counts;
        runtime.state_revision = runtime.state_revision.saturating_add(1);
    }
    fn widget(&self) -> Value {
        let runtime = lock(&self.runtime);
        let current=runtime.primary_job().map(|job|{let total=count(&job["total"]);let processed=count(&job["processed"]);json!({"kind":job["kind"],"status":job["status"],"processed":processed,"total":total,"progress_percent":if total>0 {Some((processed.saturating_mul(100)/total).min(100))}else{None}})});
        let scan = runtime
            .running
            .iter()
            .chain(runtime.history.iter().rev())
            .find(|job| ["scan", "extract", "ocr"].contains(&job["kind"].as_str().unwrap_or("")));
        let failures = scan.map_or(0, |job| count(&job["summary"]["failed"]));
        json!({"converted_count":runtime.converted.len(),"current_run":current,"queued_count":runtime.queue.len(),"configured_root_count":runtime.roots.len(),"selected_languages":runtime.languages,"recent_failures_count":failures})
    }
    fn mounts_value(&self) -> Value {
        json!(
            self.mounts
                .iter()
                .map(|root| {
                    let info = root_info(root, &self.mounts);
                    json!({"path":info["path"],"label":info["label"],"status":info["status"]})
                })
                .collect::<Vec<_>>()
        )
    }
    fn root_folders(&self) -> Value {
        let mut runtime = lock(&self.runtime);
        runtime.cached_roots = runtime
            .roots
            .iter()
            .map(|root| root_info(root, &self.mounts))
            .collect();
        json!({"roots":runtime.cached_roots,"mounts":self.mounts_value()})
    }
    fn save_preferences(&self, languages: &[String], roots: &[PathBuf]) -> Result<(), String> {
        write_private(
            &self.data.join("preferences.json"),
            &json!({"languages":languages,"root_paths":roots}),
        )
    }
    fn update_settings(&self, data: &Value) -> Result<Value, String> {
        let languages = Converter::normalize_languages(
            data.get("languages")
                .unwrap_or(&json!(lock(&self.runtime).languages)),
        )?;
        let mut update = data
            .get("integrations")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !update.is_object() {
            return Err("Invalid integration settings".into());
        }
        if let Some(mappings) = data.get("path_mappings") {
            update["path_mappings"] = mappings.clone();
        }
        let mut integrations = lock(&self.integrations);
        integrations.update(&update)?;
        *lock(&self.public_integrations) = integrations.public_settings();
        drop(integrations);
        let mut runtime = lock(&self.runtime);
        self.save_preferences(&languages, &runtime.roots)?;
        runtime.languages = languages;
        drop(runtime);
        Ok(self.settings())
    }
    fn update_roots(&self, data: &Value) -> Result<Value, String> {
        let values = data["paths"]
            .as_array()
            .ok_or("Choose a list of scan folders")?;
        if values.len() > 32 || !values.iter().all(Value::is_string) {
            return Err("Select at most 32 scan folders".into());
        }
        let mut roots = Vec::new();
        for value in strings(&data["paths"]) {
            let root = validate_root(Path::new(&value), &self.mounts, true)?;
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        let mut runtime = lock(&self.runtime);
        self.save_preferences(&runtime.languages, &roots)?;
        runtime.roots = roots;
        drop(runtime);
        Ok(self.root_folders())
    }
    fn folders(&self, path: Option<&str>) -> Result<Value, String> {
        let root = match path.filter(|path| !path.is_empty()) {
            Some(value) => validate_root(Path::new(value), &self.mounts, true)?,
            None => match self.mounts.first() {
                Some(root) => root.clone(),
                None => return Ok(json!({"path":"","parent":null,"folders":[]})),
            },
        };
        let parent = root
            .parent()
            .filter(|parent| self.mounts.iter().any(|mount| parent.starts_with(mount)))
            .map(|path| path.to_string_lossy().into_owned());
        let mut folders = Vec::new();
        let mut truncated = false;
        for entry in fs::read_dir(&root).map_err(|_| "Directory is unavailable or unreadable")? {
            let Ok(entry) = entry else { continue };
            if !entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            {
                continue;
            }
            if folders.len() >= 200 {
                truncated = true;
                break;
            }
            if let Ok(path) = validate_root(&entry.path(), &self.mounts, true) {
                folders.push(json!({"name":entry.file_name().to_string_lossy(),"path":path}));
            }
        }
        folders.sort_by_key(|value| value["name"].as_str().unwrap_or("").to_lowercase());
        Ok(json!({"path":root,"parent":parent,"folders":folders,"truncated":truncated}))
    }
    fn enqueue(&self, data: &Value, origin: &str) -> Result<Option<Value>, String> {
        let kind = data["kind"].as_str().unwrap_or("scan");
        if kind == "ocr" && !self.ocr_permission {
            return Err("OCR is disabled by deployment permissions".into());
        }
        if ![
            "scan", "inspect", "extract", "refresh", "ocr", "health", "cleanup", "backup",
        ]
        .contains(&kind)
        {
            return Err("Unknown job type".into());
        }
        let provider = data["provider"].as_str().unwrap_or("");
        if kind == "refresh" && !providers().contains(&provider) {
            return Err("Unknown integration".into());
        }
        let mut runtime = lock(&self.runtime);
        let task_id = if ["schedule", "task_manual"].contains(&origin) {
            data["task_id"].as_str().unwrap_or("")
        } else {
            ""
        };
        if !task_id.is_empty()
            && runtime
                .running
                .iter()
                .chain(runtime.active_jobs.values().map(|active| &active.job))
                .chain(runtime.queue.iter())
                .any(|job| job["task_id"] == task_id)
        {
            return Err("Task is already queued or running".into());
        }
        if origin == "schedule" && (runtime.primary_job().is_some() || !runtime.queue.is_empty()) {
            return Ok(None);
        }
        if runtime.queue.len() >= 32 {
            return Err("Job queue is full".into());
        }
        let requested = data.get("root_ids").cloned().unwrap_or_else(|| json!([]));
        if !requested
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
        {
            return Err("Invalid library selection".into());
        }
        let ids = strings(&requested);
        let mut roots = Vec::new();
        if ids.is_empty() {
            roots = runtime.roots.clone();
        } else {
            for id in ids {
                let root = runtime
                    .roots
                    .iter()
                    .find(|root| root_info(root, &self.mounts)["id"] == id)
                    .ok_or("Invalid library selection")?;
                roots.push(root.clone());
            }
        }
        let requested = data.get("paths").cloned().unwrap_or_else(|| json!([]));
        if !requested
            .as_array()
            .is_some_and(|paths| paths.len() <= 256 && paths.iter().all(Value::is_string))
        {
            return Err("Select at most 256 media files".into());
        }
        let paths = strings(&requested)
            .iter()
            .map(|value| {
                Converter::safe_path(value, &roots).map(|path| path.to_string_lossy().into_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if kind == "extract" && paths.is_empty() {
            return Err("Choose a file to extract".into());
        }
        let track_ids = if let Some(selection) = data.get("track_ids") {
            if kind != "extract" || origin != "manual" || paths.len() != 1 {
                return Err(
                    "Track selection requires manual extraction of exactly one file".into(),
                );
            }
            let values = selection
                .as_array()
                .filter(|values| !values.is_empty() && values.len() <= 64)
                .ok_or("Choose 1–64 unique subtitle track IDs")?;
            let ids = values
                .iter()
                .map(|id| {
                    id.as_u64()
                        .ok_or("Subtitle track IDs must be nonnegative integers")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let source = paths.first().ok_or("Choose one file to extract")?;
            let description = runtime
                .inventory
                .iter()
                .find(|row| row["path"] == *source)
                .map(|row| row["tracks"].clone())
                .or_else(|| lock(&self.converter).cached_description(Path::new(source)))
                .ok_or("Inspect this file before selecting subtitle tracks")?;
            crate::converter::validate_track_ids(&description, &ids)?;
            Some(ids)
        } else {
            None
        };
        let languages = Converter::normalize_languages(
            data.get("languages").unwrap_or(&json!(runtime.languages)),
        )?;
        if data.get("retry").is_some_and(|retry| !retry.is_boolean()) {
            return Err("Invalid retry option".into());
        }
        if data
            .get("include_ocr")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err("Invalid OCR option".into());
        }
        if data.get("force").is_some_and(|force| !force.is_boolean()) {
            return Err("Invalid force option".into());
        }
        let force = data["force"].as_bool().unwrap_or(false);
        if force && (kind != "extract" || origin != "manual" || track_ids.is_none()) {
            return Err("Force requires manual extraction with explicit subtitle track IDs".into());
        }
        if kind == "scan" && data["include_ocr"] == true && origin != "manual" {
            return Err("Scheduled discovery does not grant OCR; use an explicit manual conversion or the OCR task".into());
        }
        if kind == "scan" && data["include_ocr"] == true && !self.ocr_permission {
            return Err("OCR is disabled by deployment permissions".into());
        }
        let include_ocr = kind == "ocr"
            || (kind == "scan" && origin == "manual" && data["include_ocr"] == true)
            || (kind == "extract"
                && origin == "manual"
                && data["include_ocr"].as_bool().unwrap_or(true));
        let job = json!({"id":token(8),"task_id":task_id,"workload":if kind=="ocr" {"ocr"}else if include_ocr {"all"}else{"text"},"include_ocr":include_ocr,"kind":kind,"status":"queued","created":now(),"started":null,"finished":null,"current":null,"summary":{},"error":null,"results":[],"languages":languages,"root_ids":roots.iter().map(|root|root_info(root,&self.mounts)["id"].clone()).collect::<Vec<_>>(),"root_paths":roots,"paths":paths,"retry":data["retry"].as_bool().unwrap_or(false),"origin":origin,"provider":provider,"processed":0,"total":0});
        let mut job = job;
        job["force"] = json!(force);
        if let Some(track_ids) = track_ids {
            job["track_ids"] = json!(track_ids);
        }
        runtime.queue.push_back(job.clone());
        record_log(
            &mut runtime,
            json!({"time":now(),"level":"INFO","message":format!("Job queued: {kind}"),"job_id":job["id"],"kind":kind,"status":"queued"}),
        );
        self.wake.notify_one();
        Ok(Some(job))
    }
    fn cancel(&self, id: &str) -> Result<Value, String> {
        let mut runtime = lock(&self.runtime);
        let cancellation = runtime
            .active_jobs
            .get(id)
            .map(|active| Arc::clone(&active.cancel))
            .or_else(|| {
                runtime
                    .running
                    .as_ref()
                    .filter(|job| job["id"] == id)
                    .map(|_| Arc::clone(&self.cancel))
            });
        if let Some(cancellation) = cancellation {
            cancellation.store(true, Ordering::Relaxed);
            record_log(
                &mut runtime,
                json!({"time":now(),"level":"INFO","message":"Job cancellation requested; waiting for active tools to stop","job_id":id,"status":"cancelling"}),
            );
            return Ok(json!({"status":"cancelling"}));
        }
        if let Some(index) = runtime.queue.iter().position(|job| job["id"] == id)
            && let Some(mut job) = runtime.queue.remove(index)
        {
            job["status"] = json!("cancelled");
            job["finished"] = json!(now());
            record_log(
                &mut runtime,
                json!({"time":now(),"level":"INFO","message":"Queued job cancelled","job_id":id,"status":"cancelled"}),
            );
            runtime.history.push_back(job);
            while runtime.history.len() > HISTORY_LIMIT {
                runtime.history.pop_front();
            }
            self.persist_history(&runtime);
            return Ok(json!({"status":"cancelled"}));
        }
        Err("Job is no longer queued or running".into())
    }
    fn persist_history(&self, runtime: &Runtime) {
        let history = runtime
            .history
            .iter()
            .map(|job| {
                let mut job = job.clone();
                if let Some(fields) = job.as_object_mut() {
                    fields.remove("published_paths");
                    fields.remove("published_srt_keys");
                }
                if job["kind"] == "inspect" {
                    job["results"] = json!([]);
                }
                job
            })
            .collect::<Vec<_>>();
        if write_private(&self.data.join("jobs.json"), &json!(history)).is_err() {
            eprintln!("Dashboard history could not be saved");
        }
    }
    fn progress(&self, event: Value) {
        let mut runtime = lock(&self.runtime);
        let mut entry = progress_log(&event);
        if let Some(job) = runtime.current_job() {
            entry["job_id"] = job["id"].clone();
            entry["kind"] = job["kind"].clone();
        }
        record_log(&mut runtime, entry);
        if event["status"] == "converted"
            && let Some(path) = event["path"].as_str()
        {
            runtime.converted.insert(path.into());
        }
        let Some(job) = runtime.current_job_mut() else {
            return;
        };
        remember_executor(job, &event);
        job["current"] = event.clone();
        if let Some(summary) = event.get("summary") {
            job["summary"] = summary.clone();
        }
        if [
            "converted",
            "failed",
            "pending",
            "cached",
            "inspected",
            "metadata_pending",
        ]
        .contains(&event["status"].as_str().unwrap_or(""))
            || (event["status"] == "skipped" && event["file_terminal"] == true)
        {
            job["processed"] = json!(count(&job["processed"]) + 1);
        }
        if event["status"] == "failed" {
            if let Some(path) = event["path"].as_str().filter(|path| path.len() <= 4096) {
                let source = Path::new(path);
                let authorized = source.is_absolute()
                    && !source
                        .components()
                        .any(|part| matches!(part, Component::ParentDir))
                    && job["root_paths"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .any(|root| source.starts_with(Path::new(root)));
                if authorized {
                    if !job["failed_paths"].is_array() {
                        job["failed_paths"] = json!([]);
                    }
                    if let Some(paths) = job["failed_paths"].as_array_mut()
                        && !paths.iter().any(|item| item == path)
                    {
                        if paths.len() < 64 {
                            paths.push(json!(path));
                        } else {
                            job["failed_paths_truncated"] = json!(true);
                        }
                    }
                }
            }
            let message = event["error"]
                .as_str()
                .unwrap_or("Conversion failed")
                .chars()
                .take(240)
                .collect::<String>();
            if !job["file_errors"].is_array() {
                job["file_errors"] = json!([]);
            }
            if let Some(groups) = job["file_errors"].as_array_mut() {
                let existing = groups.iter().position(|group| group["message"] == message);
                if existing.is_none() && groups.len() >= 24 {
                    job["other_file_errors"] = json!(count(&job["other_file_errors"]) + 1);
                    return;
                }
                let index = existing.unwrap_or_else(|| {
                    groups.push(json!({"message":message,"count":0,"examples":[]}));
                    groups.len() - 1
                });
                if let Some(group) = groups.get_mut(index) {
                    group["count"] = json!(count(&group["count"]) + 1);
                    let name = event["path"]
                        .as_str()
                        .and_then(|path| Path::new(path).file_name())
                        .map(|name| name.to_string_lossy().chars().take(240).collect::<String>());
                    if let (Some(name), Some(examples)) = (name, group["examples"].as_array_mut())
                        && examples.len() < 3
                        && !examples.iter().any(|item| item == &name)
                    {
                        examples.push(json!(name));
                    }
                }
            }
        }
    }
    fn notifications(&self, notifications: Vec<Value>) {
        let mut runtime = lock(&self.runtime);
        if let Some(job) = runtime.current_job_mut() {
            if !job["notifications"].is_array() {
                job["notifications"] = json!([]);
            }
            if let Some(items) = job["notifications"].as_array_mut() {
                items.extend(notifications);
                if items.len() > 200 {
                    items.drain(..items.len() - 200);
                }
            }
        }
    }

    /// A single job/cancellation scope owns the bounded per-file threads.
    /// Workers never persist shared state or call network integrations.
    fn convert_files(
        &self,
        paths: &[PathBuf],
        job: &Value,
        workload: Workload,
        integrations: &mut Integrations,
    ) -> Result<Value, String> {
        self.run_files(paths, job, FileMode::Convert(workload), integrations)
            .map(|(summary, _)| summary)
    }

    fn run_files(
        &self,
        paths: &[PathBuf],
        job: &Value,
        mode: FileMode,
        integrations: &mut Integrations,
    ) -> Result<(Value, Descriptions), String> {
        if paths.len() > 20_000 {
            return Err("File worker input exceeds its path limit".into());
        }
        let languages = strings(&job["languages"]);
        let file_targets: BTreeMap<String, Vec<crate::converter::SubtitleTarget>> = job
            .get("file_targets")
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|_| "Invalid per-file subtitle targets")
            })
            .transpose()?
            .unwrap_or_default();
        let file_languages: BTreeMap<String, Vec<String>> = job
            .get("file_languages")
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|_| "Invalid per-file subtitle languages")
            })
            .transpose()?
            .unwrap_or_default();
        let file_sources: BTreeMap<String, Vec<crate::converter::SubtitleSourceHint>> = job
            .get("file_sources")
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|_| "Invalid per-file subtitle source metadata")
            })
            .transpose()?
            .unwrap_or_default();
        let force = job["force"].as_bool().unwrap_or(false);
        let track_ids = job
            .get("track_ids")
            .map(|ids| {
                ids.as_array()
                    .ok_or("Invalid track selection")?
                    .iter()
                    .map(|id| id.as_u64().ok_or("Invalid track selection"))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let retry = job["retry"].as_bool().unwrap_or(false);
        let automatic = job["origin"] == "schedule"
            || ["sonarr", "radarr"].contains(&job["origin"].as_str().unwrap_or(""));
        let ocr = matches!(mode, FileMode::Convert(workload) if workload != Workload::Text)
            && self.ocr_permission;
        // Cached track metadata lets text-only selections use CPU text slots
        // even when extraction's default mode also permits OCR. Unknown video
        // metadata keeps the conservative OCR reservation.
        let ocr_paths = if ocr {
            let converter = lock(&self.converter);
            paths
                .iter()
                .enumerate()
                .filter_map(|(index, path)| {
                    let needs_ocr = if matches!(mode, FileMode::Convert(Workload::Ocr)) {
                        true
                    } else if path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            ["srt", "ass", "ssa", "vtt", "ttml"]
                                .iter()
                                .any(|text| extension.eq_ignore_ascii_case(text))
                        })
                    {
                        false
                    } else {
                        converter.cached_description(path).is_none_or(|tracks| {
                            tracks.as_array().is_none_or(|tracks| {
                                tracks.iter().any(|track| {
                                    track_ids.as_ref().is_none_or(|ids| {
                                        track["number"].as_u64().is_some_and(|id| ids.contains(&id))
                                    }) && crate::converter::is_image_format(
                                        track["format"].as_str().unwrap_or(""),
                                    )
                                })
                            })
                        })
                    };
                    needs_ocr.then_some(index)
                })
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let mut seen = HashSet::new();
        let mut pending: VecDeque<_> = paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| seen.insert(path).then_some(index))
            .collect();
        if let Some(running) = lock(&self.runtime).current_job_mut() {
            running["total"] = json!(pending.len());
            if matches!(mode, FileMode::Inspect { .. }) {
                running["processed"] = json!(0);
            }
        }
        let mut summary = if matches!(mode, FileMode::Inspect { .. }) {
            json!({"inspected":0,"failed":0,"metadata_pending":0})
        } else {
            json!({"converted":0,"cached":0,"pending":0,"failed":0,"planned":0})
        };
        let mut descriptions = BTreeMap::new();
        let mut failure = None;
        let mut last_heartbeat = Instant::now();
        let mut last_wait_reason = None;
        std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::sync_channel::<FileEvent>(16);
            let mut handles = BTreeMap::new();
            let mut executors = BTreeMap::new();
            let mut remote_jobs: BTreeMap<usize, (usize, PathBuf, String)> = BTreeMap::new();
            let mut remote_retries: BTreeMap<usize, u8> = BTreeMap::new();
            while !pending.is_empty() || !handles.is_empty() || !remote_jobs.is_empty() {
                if self.cancellation().load(Ordering::Relaxed)
                    || self.shutdown.load(Ordering::Relaxed)
                {
                    self.cancellation().store(true, Ordering::Relaxed);
                    pending.clear();
                    let mut remote = lock(&self.remote);
                    for (_, _, ticket) in remote_jobs.values() {
                        remote.cancel(ticket);
                    }
                    for id in remote_jobs.keys() {
                        executors.remove(id);
                    }
                    remote_jobs.clear();
                }
                loop {
                    let _dispatch = lock(&self.dispatch);
                    if self.cancellation().load(Ordering::Relaxed)
                        || self.shutdown.load(Ordering::Relaxed)
                    {
                        self.cancellation().store(true, Ordering::Relaxed);
                        pending.clear();
                        break;
                    }
                    if pending.is_empty() || failure.is_some() {
                        break;
                    }
                    if let FileMode::Convert(workload) = mode {
                        let candidate = {
                            let remote = lock(&self.remote);
                            let workers = lock(&self.workers);
                            pending.iter().enumerate().find_map(|(position, index)| {
                                if workers.allocation.paused {
                                    return None;
                                }
                                if remote_retries
                                    .get(index)
                                    .is_some_and(|attempts| *attempts >= 2)
                                {
                                    return None;
                                }
                                let path = paths.get(*index)?;
                                if workers
                                    .active
                                    .iter()
                                    .any(|active| crate::workers::conflicts(&active.path, path))
                                    || remote.path_active(path)
                                    || remote_jobs.values().any(|(_, active, _)| {
                                        crate::workers::conflicts(active, path)
                                    })
                                {
                                    return None;
                                }
                                let needs_ocr = ocr_paths.contains(index);
                                let resource = if needs_ocr && remote.has_capacity("gpu") {
                                    "gpu"
                                } else if remote.has_capacity(if needs_ocr {
                                    "cpu_ocr"
                                } else {
                                    "cpu"
                                }) {
                                    if needs_ocr { "cpu_ocr" } else { "cpu" }
                                } else {
                                    return None;
                                };
                                let id = (crate::workers::HARD_TOTAL_LIMIT
                                    ..crate::workers::HARD_TOTAL_LIMIT + crate::remote::MAX_LEASES)
                                    .find(|id| !remote_jobs.contains_key(id))?;
                                Some((position, *index, path.clone(), resource, id))
                            })
                        };
                        if let Some((position, index, path, resource, id)) = candidate {
                            let state_result = {
                                let converter = lock(&self.converter);
                                converter
                                    .state
                                    .get(path.to_string_lossy().as_ref())
                                    .map(|state| crate::util::bounded_json_clone(state, 48 * 1024))
                                    .transpose()
                            };
                            let state = match state_result {
                                Ok(state) => state,
                                Err(error) => {
                                    failure = Some(error);
                                    pending.clear();
                                    break;
                                }
                            };
                            let mut remote = lock(&self.remote);
                            let offered = match remote.offer(
                                &path,
                                file_languages
                                    .get(path.to_string_lossy().as_ref())
                                    .unwrap_or(&languages),
                                retry,
                                automatic,
                                // A CPU text reservation never grants OCR,
                                // even if that worker enables OCR locally.
                                if resource == "cpu" || !self.ocr_permission {
                                    Workload::Text
                                } else {
                                    workload
                                },
                                track_ids.as_deref(),
                                file_targets
                                    .get(path.to_string_lossy().as_ref())
                                    .map(Vec::as_slice),
                                file_sources
                                    .get(path.to_string_lossy().as_ref())
                                    .map(Vec::as_slice)
                                    .unwrap_or(&[]),
                                force,
                                resource,
                                state,
                            ) {
                                Ok(offered) => offered,
                                Err(error) => {
                                    failure = Some(error);
                                    pending.clear();
                                    break;
                                }
                            };
                            if let Some(ticket) = offered {
                                let Some(executor) = remote.executor(&ticket) else {
                                    remote.cancel(&ticket);
                                    failure =
                                        Some("Remote worker attribution is unavailable".into());
                                    pending.clear();
                                    break;
                                };
                                drop(remote);
                                pending.remove(position);
                                remote_jobs.insert(id, (index, path.clone(), ticket));
                                self.progress(worker_event(
                                    json!({"status":"remote_queued","path":path}),
                                    id,
                                    &executor,
                                ));
                                executors.insert(id, executor);
                                continue;
                            }
                        }
                    }
                    let sample = lock(&self.resources).snapshot();
                    let admission = {
                        let mut workers = lock(&self.workers);
                        let next = pending.iter().enumerate().find_map(|(position, index)| {
                            let needs_ocr = ocr_paths.contains(index);
                            let resource =
                                if needs_ocr && workers.can_start_resource(&sample, "gpu") {
                                    "gpu"
                                } else if workers.can_start_resource(
                                    &sample,
                                    if needs_ocr { "cpu_ocr" } else { "cpu" },
                                ) {
                                    if needs_ocr { "cpu_ocr" } else { "cpu" }
                                } else {
                                    return None;
                                };
                            paths
                                .get(*index)
                                .filter(|path| {
                                    workers.active.iter().all(|active| {
                                        matches!(mode, FileMode::Inspect { .. })
                                            || !crate::workers::conflicts(&active.path, path)
                                    }) && !lock(&self.remote).path_active(path)
                                        && remote_jobs.values().all(|(_, active, _)| {
                                            !crate::workers::conflicts(active, path)
                                        })
                                })
                                .map(|_| (position, resource))
                        });
                        if let Some((next, resource)) = next {
                            pending
                                .remove(next)
                                .and_then(|index| paths.get(index))
                                .and_then(|path| {
                                    let id = (0..crate::workers::HARD_TOTAL_LIMIT).find(|id| {
                                        workers.active.iter().all(|active| active.id != *id)
                                    })?;
                                    workers.active.push(ActiveWorker {
                                        id,
                                        resource,
                                        path: path.clone(),
                                        started: now(),
                                        status: if resource == "gpu" {
                                            "checking_gpu"
                                        } else {
                                            "processing"
                                        },
                                    });
                                    Some((id, path.clone(), resource))
                                })
                        } else {
                            None
                        }
                    };
                    let Some((id, path, resource)) = admission else {
                        break;
                    };
                    let executor = json!({"node_id":"local","node_name":"Local","resource":resource,"execution":"local"});
                    let worker = {
                        let converter = lock(&self.converter);
                        if matches!(mode, FileMode::Inspect { .. }) {
                            Ok(converter.inspection_worker())
                        } else {
                            converter.file_worker(&path)
                        }
                    };
                    let mut worker = match worker {
                        Ok(worker) => worker,
                        Err(error) => {
                            lock(&self.workers).active.retain(|worker| worker.id != id);
                            summary["failed"] = json!(count(&summary["failed"]) + 1);
                            self.progress(worker_event(json!({"status":"failed","path":path,"error":error,"summary":summary}), id, &executor));
                            continue;
                        }
                    };
                    if let Some(ids) = &track_ids
                        && let Err(error) = worker.select_tracks(ids.clone())
                    {
                        lock(&self.workers).active.retain(|worker| worker.id != id);
                        failure = Some(error);
                        self.cancellation().store(true, Ordering::Relaxed);
                        pending.clear();
                        break;
                    }
                    if let Some(targets) = file_targets.get(path.to_string_lossy().as_ref())
                        && let Err(error) = worker.set_targets(targets.clone())
                    {
                        lock(&self.workers).active.retain(|worker| worker.id != id);
                        failure = Some(error);
                        pending.clear();
                        self.cancellation().store(true, Ordering::Relaxed);
                        break;
                    }
                    if let Err(error) = worker.set_source_hints(
                        file_sources
                            .get(path.to_string_lossy().as_ref())
                            .cloned()
                            .unwrap_or_default(),
                    ) {
                        lock(&self.workers).active.retain(|worker| worker.id != id);
                        failure = Some(error);
                        pending.clear();
                        self.cancellation().store(true, Ordering::Relaxed);
                        break;
                    }
                    if let Err(error) = worker.force_covered_tracks(force) {
                        lock(&self.workers).active.retain(|worker| worker.id != id);
                        failure = Some(error);
                        pending.clear();
                        self.cancellation().store(true, Ordering::Relaxed);
                        break;
                    }
                    if self.cancellation().load(Ordering::Relaxed)
                        || self.shutdown.load(Ordering::Relaxed)
                    {
                        self.cancellation().store(true, Ordering::Relaxed);
                        pending.clear();
                        lock(&self.workers).active.retain(|worker| worker.id != id);
                        break;
                    }
                    if resource == "gpu" {
                        worker.enable_gpu_ocr(lock(&self.workers).gpu_min_free_mib());
                    }
                    self.progress(worker_event(
                        json!({"status":"dispatched","path":path}),
                        id,
                        &executor,
                    ));
                    executors.insert(id, executor);
                    let worker_path = path.clone();
                    let worker_sender = sender.clone();
                    let worker_languages = file_languages
                        .get(path.to_string_lossy().as_ref())
                        .unwrap_or(&languages);
                    let cancellation = self.cancellation();
                    let handle = std::thread::Builder::new()
                        .name(format!("subforge-file-{id}"))
                        .stack_size(2 * 1024 * 1024)
                        .spawn_scoped(scope, move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    if let FileMode::Inspect { cached_only } = mode {
                                        let _ = worker_sender.send(FileEvent::Progress { id, event: json!({"status":"inspecting","path":worker_path}) });
                                        let description = if cached_only { Ok(worker.cached_description(&worker_path)) }
                                            else { worker.describe_with_stop(&worker_path, &cancellation).map(Some) };
                                        let (summary, terminal) = match &description {
                                            Ok(Some(_)) => (json!({"inspected":1,"failed":0}), json!({"status":"inspected","path":worker_path})),
                                            Ok(None) => (json!({"metadata_pending":1}), json!({"status":"metadata_pending","path":worker_path})),
                                            Err(error) => (json!({"inspected":0,"failed":1}), json!({"status":"failed","path":worker_path,"error":error})),
                                        };
                                        return Ok(FileOutcome { state: None, summary, terminal: Some(terminal), gpu_health: None, description: Some(description) });
                                    }
                                    let FileMode::Convert(workload) = mode else { return Err(FileFailure::Conversion("Unsupported file worker operation".into())); };
                                    let mut terminal = None;
                                    let result = worker.scan_workload(
                                        std::slice::from_ref(&worker_path),
                                        worker_languages,
                                        retry,
                                        workload,
                                        automatic,
                                        &cancellation,
                                        &mut |event| {
                                            if ["converted", "cached", "failed", "pending"]
                                                .contains(&event["status"].as_str().unwrap_or(""))
                                                || (event["status"]=="skipped" && event.get("summary").is_some())
                                            {
                                                terminal = Some(event);
                                            } else {
                                                let _ = worker_sender
                                                    .send(FileEvent::Progress { id, event });
                                            }
                                        },
                                    );
                                    let outputs = worker.take_published_outputs();
                                    let (gpu_health, gpu_failure) = worker.gpu_status();
                                    let summary = match (result, gpu_failure) {
                                        (Err(message), _) => return Err(FileFailure::Published { message, gpu:gpu_failure.is_some(), state:worker.take_file_state(&worker_path), outputs }),
                                        (_, Some(message)) => return Err(FileFailure::Published { message:message.into(), gpu:true, state:worker.take_file_state(&worker_path), outputs }),
                                        (Ok(summary), None) => summary,
                                    };
                                    if !outputs.is_empty() {
                                        let event = terminal.get_or_insert_with(|| json!({"status":if cancellation.load(Ordering::Relaxed) {"cancelled"} else {"failed"},"path":worker_path,"error":"SRT published before final conversion state could be completed"}));
                                        event["new_outputs"] = json!(outputs);
                                    }
                                    Ok(FileOutcome {
                                        state: worker.take_file_state(&worker_path),
                                        summary,
                                        terminal,
                                        gpu_health,
                                        description: None,
                                    })
                                }))
                                .unwrap_or_else(|_| {
                                    Err(FileFailure::Conversion("File worker failed safely".into()))
                                });
                            let _ = worker_sender.send(FileEvent::Finished {
                                id,
                                path: worker_path,
                                outcome: result,
                            });
                        });
                    match handle {
                        Ok(handle) => {
                            handles.insert(id, handle);
                        }
                        Err(_) => {
                            lock(&self.workers).active.retain(|worker| worker.id != id);
                            executors.remove(&id);
                            failure = Some("File worker capacity is unavailable".into());
                            self.cancellation().store(true, Ordering::Relaxed);
                            pending.clear();
                            break;
                        }
                    }
                }
                if pending.is_empty() && handles.is_empty() && remote_jobs.is_empty() {
                    break;
                }
                // A completed remote lease stops reserving its output family
                // in `take`; keep admission closed until its state is merged.
                let mut remote_commit_guard = Some(lock(&self.dispatch));
                let remote_event = {
                    let mut completed = None;
                    let mut remote = lock(&self.remote);
                    let ready =
                        remote_jobs.iter().find_map(|(id, (_, path, ticket))| {
                            match remote.take(ticket, &self.cancellation()) {
                                crate::remote::RemotePoll::Pending => None,
                                crate::remote::RemotePoll::Retry => Some((*id, path.clone(), None)),
                                crate::remote::RemotePoll::Finished(outcome) => {
                                    Some((*id, path.clone(), Some(outcome)))
                                }
                            }
                        });
                    if let Some((id, path, outcome)) = ready {
                        let entry = remote_jobs.remove(&id);
                        if let Some(outcome) = outcome {
                            completed = Some(FileEvent::Finished {
                                id,
                                path,
                                outcome: Ok(FileOutcome {
                                    state: outcome.state,
                                    summary: outcome.summary,
                                    terminal: outcome.terminal,
                                    gpu_health: None,
                                    description: None,
                                }),
                            });
                        } else if !self.cancellation().load(Ordering::Relaxed)
                            && let Some((index, _, _)) = entry
                        {
                            let attempts = remote_retries.entry(index).or_default();
                            *attempts = attempts.saturating_add(1);
                            pending.push_back(index);
                            if let Some(executor) = executors.remove(&id) {
                                self.progress(worker_event(json!({"status":"remote_retry","path":path,"reason":format!("Remote lease expired or result was rejected; file requeued (attempt {attempts}, local fallback after two attempts)")}), id, &executor));
                            }
                        }
                    }
                    completed
                };
                if remote_event.is_none() {
                    drop(remote_commit_guard.take());
                }
                match remote_event
                    .map(Ok)
                    .unwrap_or_else(|| receiver.recv_timeout(Duration::from_millis(100)))
                {
                    Ok(FileEvent::Progress { id, mut event }) => {
                        if let Some(worker) = lock(&self.workers)
                            .active
                            .iter_mut()
                            .find(|worker| worker.id == id)
                        {
                            worker.status = match event["status"].as_str() {
                                Some("extracting") => "extracting",
                                Some("inspecting") => "inspecting",
                                Some("checking_gpu") => "checking_gpu",
                                Some("ocr") => "ocr",
                                Some("validating") => "validating",
                                Some("track_complete") => "track_complete",
                                _ => "processing",
                            };
                        }
                        if let Some(object) = event.as_object_mut() {
                            object.remove("summary");
                            object.insert("worker_id".into(), json!(id));
                        }
                        if let Some(executor) = executors.get(&id) {
                            event = worker_event(event, id, executor);
                        }
                        self.progress(event);
                    }
                    Ok(FileEvent::Finished { id, path, outcome }) => {
                        let executor = executors.remove(&id);
                        if let Some(handle) = handles.remove(&id) {
                            let _ = handle.join();
                        }
                        match outcome {
                            Ok(outcome) => {
                                if let Some(description) = outcome.description {
                                    descriptions.insert(path.clone(), description);
                                }
                                if let Some(health) = outcome.gpu_health {
                                    lock(&self.workers).gpu.verified = Some(health);
                                }
                                let changed = outcome.state.is_some();
                                let merged =
                                    lock(&self.converter).merge_file_state(&path, outcome.state);
                                if let Err(error) = merged {
                                    failure = Some(error);
                                    self.cancellation().store(true, Ordering::Relaxed);
                                    pending.clear();
                                } else if changed {
                                    self.refresh_pipeline();
                                }
                                lock(&self.workers).active.retain(|worker| worker.id != id);
                                drop(remote_commit_guard.take());
                                for &key in if matches!(mode, FileMode::Inspect { .. }) {
                                    &["inspected", "failed", "metadata_pending"][..]
                                } else {
                                    &["converted", "cached", "pending", "failed", "planned"][..]
                                } {
                                    summary[key] = json!(
                                        count(&summary[key])
                                            .saturating_add(count(&outcome.summary[key]))
                                    );
                                }
                                if let Some(mut event) = outcome.terminal {
                                    if let Some(executor) = &executor {
                                        event = worker_event(event, id, executor);
                                    }
                                    event["file_terminal"] = json!(true);
                                    let outputs = strings(&event["new_outputs"]);
                                    if !matches!(mode, FileMode::Inspect { cached_only: true }) {
                                        event["summary"] = summary.clone();
                                    }
                                    self.progress(event);
                                    if !outputs.is_empty() {
                                        self.published_outputs(&outputs, integrations);
                                    }
                                }
                            }
                            Err(error) => {
                                let (error, outputs) = match error {
                                    FileFailure::Conversion(error) => (error, Vec::new()),
                                    FileFailure::Published {
                                        message,
                                        gpu,
                                        state,
                                        outputs,
                                    } => {
                                        if let Err(error) =
                                            lock(&self.converter).merge_file_state(&path, state)
                                        {
                                            failure = Some(error);
                                            self.cancellation().store(true, Ordering::Relaxed);
                                            pending.clear();
                                        }
                                        if gpu {
                                            lock(&self.workers).gpu.failed();
                                            failure = Some(message.clone());
                                            self.cancellation().store(true, Ordering::Relaxed);
                                            pending.clear();
                                        }
                                        (message, outputs)
                                    }
                                };
                                lock(&self.workers).active.retain(|worker| worker.id != id);
                                drop(remote_commit_guard.take());
                                if !outputs.is_empty() {
                                    self.published_outputs(&outputs, integrations);
                                }
                                summary["failed"] = json!(count(&summary["failed"]) + 1);
                                let mut event = json!({"status":"failed","path":path,"error":error,"summary":summary});
                                if let Some(executor) = &executor {
                                    event = worker_event(event, id, executor);
                                }
                                self.progress(event);
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        failure = Some("File worker channel is unavailable".into());
                        self.cancellation().store(true, Ordering::Relaxed);
                        pending.clear();
                    }
                }
                drop(remote_commit_guard);
                if !pending.is_empty()
                    && handles.is_empty()
                    && remote_jobs.is_empty()
                    && failure.is_none()
                {
                    let sample = lock(&self.resources).snapshot();
                    let workers = lock(&self.workers);
                    let ocr = pending
                        .front()
                        .is_some_and(|index| ocr_paths.contains(index));
                    let resource = if ocr { "cpu_ocr" } else { "cpu" };
                    let reason = workers.admission_reason(&sample, resource);
                    let gpu_reason = ocr
                        .then(|| workers.admission_reason(&sample, "gpu"))
                        .flatten();
                    let wait_reason = format!(
                        "Dispatch waiting: {}; pending files {}{}",
                        reason.unwrap_or("output family is active"),
                        pending.len(),
                        gpu_reason
                            .map(|reason| format!("; GPU: {reason}"))
                            .unwrap_or_default()
                    );
                    let limits = workers.limits(&sample);
                    let impossible = !workers.allocation.paused
                        && if ocr {
                            limits.ocr_workers == 0 && limits.gpu_workers == 0
                        } else {
                            limits.cpu_workers == 0
                        };
                    drop(workers);
                    if impossible {
                        failure = Some(wait_reason.clone());
                        pending.clear();
                        self.log("ERROR", &wait_reason);
                    } else if last_wait_reason.as_ref() != Some(&wait_reason) {
                        self.log("INFO", &wait_reason);
                        last_wait_reason = Some(wait_reason);
                    }
                } else {
                    last_wait_reason = None;
                }
                if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                    let workers = lock(&self.workers);
                    let active = workers.active.len() + remote_jobs.len();
                    let details = workers
                        .active
                        .iter()
                        .map(|worker| {
                            format!(
                                "worker {} {} {} ({}s)",
                                worker.id,
                                worker.resource,
                                worker.status,
                                (now() - worker.started).max(0.0) as u64
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    drop(workers);
                    self.log(
                        "INFO",
                        &format!(
                            "Job progress: {active} active files, {} pending; {details}",
                            pending.len()
                        ),
                    );
                    last_heartbeat = Instant::now();
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        lock(&self.converter).persist_metadata()?;
        Ok((summary, descriptions))
    }

    fn execute(&self, job: &Value, integrations: &mut Integrations) -> Result<(), String> {
        if ["health", "cleanup", "backup"].contains(&job["kind"].as_str().unwrap_or("")) {
            let result = match job["kind"].as_str().unwrap_or("") {
                "cleanup" => lock(&self.converter).compact_cache()?,
                "backup" => crate::maintenance::backup(&self.data, &self.cancellation())?,
                _ => {
                    let unavailable = self
                        .mounts
                        .iter()
                        .filter(|path| !path.is_dir() || symlink_component(path))
                        .count();
                    json!({"configured_mounts":self.mounts.len(),"unavailable_mounts":unavailable,"state_directory_available":self.data.is_dir() && !symlink_component(&self.data),"network_checks":0})
                }
            };
            if job["kind"] == "cleanup" {
                self.refresh_pipeline();
            }
            if job["kind"] == "health" {
                self.root_folders();
            }
            if let Some(running) = lock(&self.runtime).current_job_mut() {
                running["results"] = json!([result]);
                running["summary"] = json!({"processed":1});
                running["processed"] = json!(1);
                running["total"] = json!(1);
            }
            return Ok(());
        }
        if job["kind"] == "refresh" {
            let result = integrations.refresh(
                job["provider"].as_str().unwrap_or(""),
                &self.cancellation(),
                &mut |event| self.progress(event),
            );
            if let Some(running) = lock(&self.runtime).current_job_mut() {
                running["results"] = json!([result]);
            }
            return Ok(());
        }
        if integrations.bazarr_enabled() && job["force"] != true {
            return self.execute_bazarr(job, integrations);
        }
        let roots = strings(&job["root_paths"])
            .iter()
            .map(|path| validate_root(Path::new(path), &self.mounts, false))
            .collect::<Result<Vec<_>, _>>()?;
        let paths = if job["kind"] == "ocr" {
            lock(&self.converter).ocr_candidates(&roots, job["retry"].as_bool().unwrap_or(false))
        } else if strings(&job["paths"]).is_empty() {
            Converter::candidates_with_stop(&roots, &self.cancellation())
        } else {
            strings(&job["paths"])
                .iter()
                .map(|path| Converter::safe_path(path, &roots))
                .collect::<Result<Vec<_>, _>>()?
        };
        if let Some(running) = lock(&self.runtime).current_job_mut() {
            running["total"] = json!(paths.len());
        }
        if job["kind"] != "inspect" {
            let paths = paths
                .iter()
                .map(|path| Converter::safe_path(&path.to_string_lossy(), &roots))
                .collect::<Result<Vec<_>, _>>()?;
            let workload = match job["workload"].as_str().unwrap_or("text") {
                "ocr" => Workload::Ocr,
                "all" => Workload::All,
                _ => Workload::Text,
            };
            let summary = self.convert_files(&paths, job, workload, integrations)?;
            if let Some(running) = lock(&self.runtime).current_job_mut() {
                running["summary"] = summary;
            }
        }
        if job["kind"] == "inspect" || job["task_id"] == "discovery" {
            let (_, mut descriptions) = self.run_files(
                &paths[..paths.len().min(INVENTORY_LIMIT)],
                job,
                FileMode::Inspect {
                    cached_only: job["task_id"] == "discovery",
                },
                integrations,
            )?;
            let previous_rows = lock(&self.runtime)
                .inventory
                .iter()
                .filter_map(|row| {
                    row["path"]
                        .as_str()
                        .map(|path| (path.to_owned(), row.clone()))
                })
                .collect::<BTreeMap<_, _>>();
            let mut results = Vec::new();
            let mut inspected = 0_u64;
            let mut failed = 0_u64;
            let mut metadata_pending = 0_u64;
            for path in paths.iter().take(INVENTORY_LIMIT) {
                if self.cancellation().load(Ordering::Relaxed) {
                    break;
                }
                let Some(result) = descriptions.remove(path) else {
                    continue;
                };
                let cache_miss = matches!(result, Ok(None));
                let root_id = roots
                    .iter()
                    .find(|root| path.starts_with(root))
                    .map(|root| root_info(root, &self.mounts)["id"].clone())
                    .unwrap_or(Value::Null);
                let result = if cache_miss {
                    metadata_pending += 1;
                    let mut row = previous_rows.get(&*path.to_string_lossy()).cloned().unwrap_or_else(||json!({"path":path,"name":path.file_name().map(|name|name.to_string_lossy().into_owned()),"type":"video","tracks":[]}));
                    row["root_id"] = root_id;
                    row["metadata_pending"] = json!(true);
                    if let Some(object) = row.as_object_mut() {
                        object.remove("error");
                    }
                    row
                } else {
                    match result {
                        Ok(Some(tracks)) => {
                            inspected += 1;
                            json!({"path":path,"name":path.file_name().map(|name|name.to_string_lossy().into_owned()),"root_id":root_id,"type":if ["mkv","mks","mp4","m4v","webm","mov","avi","ts","m2ts"].contains(&path.extension().and_then(|value|value.to_str()).unwrap_or("")){"video"}else{"sidecar"},"tracks":tracks})
                        }
                        Err(error) => {
                            failed += 1;
                            json!({"path":path,"name":path.file_name().map(|name|name.to_string_lossy().into_owned()),"root_id":root_id,"type":"video","tracks":[],"error":error})
                        }
                        Ok(None) => continue,
                    }
                };
                results.push(result);
                if let Some(running) = lock(&self.runtime).current_job_mut() {
                    running["processed"] = json!(inspected + failed + metadata_pending);
                    if job["kind"] == "inspect" {
                        running["summary"] = json!({"inspected":inspected,"failed":failed});
                    } else {
                        running["summary"]["inspected"] = json!(inspected);
                        running["summary"]["inspection_failed"] = json!(failed);
                        running["summary"]["metadata_pending"] = json!(metadata_pending);
                    }
                    running["truncated"] = json!(paths.len() > INVENTORY_LIMIT);
                }
            }
            let cancelled = self.cancellation().load(Ordering::Relaxed);
            let partial = cancelled || metadata_pending > 0;
            let keep_previous = cancelled && !lock(&self.runtime).inventory.is_empty();
            if !keep_previous {
                let updated = now();
                let truncated = paths.len() > INVENTORY_LIMIT;
                write_private(
                    &self.data.join("inventory.json"),
                    &json!({"items":results,"updated":updated,"truncated":truncated,"partial":partial}),
                )?;
                let mut runtime = lock(&self.runtime);
                runtime.inventory = results;
                runtime.inventory_updated = updated;
                runtime.inventory_truncated = truncated;
                runtime.inventory_partial = partial;
                runtime.inventory_source = "filesystem".into();
                runtime.inventory_authority = json!({"status":"ready","error":null});
                drop(runtime);
                self.persist_inventory()?;
            }
        }
        lock(&self.converter).persist_metadata()?;
        Ok(())
    }
    fn finish_job(&self, id: &str) {
        let mut runtime = lock(&self.runtime);
        if let Some(active) = runtime.active_jobs.remove(id) {
            let mut finished = active.job;
            finished["status"] = json!(if active.cancel.load(Ordering::Relaxed) {
                "cancelled"
            } else {
                completed_status(&finished)
            });
            finished["finished"] = json!(now());
            if let Some(fields) = finished.as_object_mut() {
                fields.remove("published_srt_keys");
                fields.remove("published_paths");
            }

            record_log(
                &mut runtime,
                json!({"time":now(),"level":if finished["status"]=="failed" {"ERROR"} else {"INFO"},"message":format!("Job {}: {}; processed {}, failed {}", finished["status"].as_str().unwrap_or("finished"),finished["kind"].as_str().unwrap_or("job"), count(&finished["processed"]),count(&finished["summary"]["failed"])),"job_id":finished["id"],"status":finished["status"]}),
            );
            if let Some(id) = finished["task_id"].as_str().filter(|id| !id.is_empty()) {
                let status = finished["status"].as_str().unwrap_or("failed");
                let message = finished["error"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        format!(
                            "{}; processed {}, failed {}",
                            status,
                            count(&finished["processed"]),
                            count(&finished["summary"]["failed"])
                        )
                    });
                if lock(&self.scheduler)
                    .finish(id, now(), status, &message)
                    .is_err()
                {
                    eprintln!("Task result could not be saved");
                }
            }
            if finished["kind"] == "inspect" {
                finished["results"] = json!([]);
                for prior in &mut runtime.history {
                    if prior["kind"] == "inspect" {
                        prior["results"] = json!([]);
                    }
                }
            }
            runtime.history.push_back(finished);
            while runtime.history.len() > HISTORY_LIMIT {
                runtime.history.pop_front();
            }
            self.persist_history(&runtime);
        }
        self.wake.notify_all();
    }

    fn execute_job(&self, job: &Value) {
        let task_start = job["task_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(|id| lock(&self.scheduler).start(id, now()))
            .unwrap_or(Ok(()));
        let mut integrations = lock(&self.integrations).clone();
        let result = task_start.and_then(|_| self.execute(job, &mut integrations));
        if let Err(message) = result {
            if let Some(running) = lock(&self.runtime).current_job_mut() {
                running["error"] = json!(message);
            }
            self.log("ERROR", &format!("Job failed: {}", log_text(&message, 512)));
        }
        if integrations.bazarr_enabled()
            && let Err(error) = self.prepare_bazarr_verification()
        {
            self.log(
                "ERROR",
                &format!(
                    "Pending Bazarr verification state could not be saved: {}",
                    log_text(&error, 256)
                ),
            );
        }
    }

    fn persist_publications(&self, pending: &Publications, attempted: bool) -> Result<(), String> {
        crate::util::write_private_limited(
            &self.data.join("publication-queue.json"),
            &json!({
                "paths":pending.paths,"jobs":pending.jobs,"limited":pending.limited,"attempted":attempted
            }),
            JSON_LIMIT as u64,
        )
    }

    fn restore_publications(&self) {
        let Ok(saved) = read_json(&self.data.join("publication-queue.json"), JSON_LIMIT) else {
            return;
        };
        if saved["attempted"] == true {
            self.log("WARNING", "A previous Bazarr scan may have been sent before shutdown; pending indexing is verified by inventory sync, without automatically repeating the scan");
            return;
        }
        let mut pending = lock(&self.publications);
        let mut bytes = 0_usize;
        for path in saved["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .take(INDEX_CONFIRM_LIMIT)
            .filter_map(Value::as_str)
        {
            if srt_index_key(path).is_some()
                && validate_root(Path::new(path), &self.mounts, false).is_ok()
                && bytes.saturating_add(path.len()) <= 8 * 1024 * 1024
            {
                bytes += path.len();
                pending.paths.insert(path.to_owned());
            } else {
                pending.limited = true;
            }
        }
        pending.jobs.extend(
            saved["jobs"]
                .as_array()
                .into_iter()
                .flatten()
                .take(PUBLICATION_JOB_LIMIT)
                .filter_map(Value::as_str)
                .filter(|id| id.len() <= 64)
                .map(str::to_owned),
        );
        pending.limited |= saved["limited"] == true;
    }

    fn stage_publications(&self, outputs: &[String]) -> Result<(), String> {
        let (id, visible_jobs) = {
            let runtime = lock(&self.runtime);
            let id = runtime
                .current_job()
                .and_then(|job| job["id"].as_str())
                .map(str::to_owned);
            let visible = runtime
                .history
                .iter()
                .chain(runtime.running.iter())
                .chain(runtime.active_jobs.values().map(|active| &active.job))
                .filter_map(|job| job["id"].as_str().map(str::to_owned))
                .collect::<HashSet<_>>();
            (id, visible)
        };
        let mut pending = lock(&self.publications);
        let mut bytes: usize = pending.paths.iter().map(String::len).sum();
        for path in outputs {
            if pending.paths.contains(path) {
                continue;
            }
            if pending.paths.len() >= INDEX_CONFIRM_LIMIT
                || bytes.saturating_add(path.len()) > 8 * 1024 * 1024
                || srt_index_key(path).is_none()
                || validate_root(Path::new(path), &self.mounts, false).is_err()
            {
                pending.limited = true;
                continue;
            }
            bytes += path.len();
            pending.paths.insert(path.clone());
        }
        pending.jobs.retain(|id| visible_jobs.contains(id));
        if let Some(id) = id
            && pending.jobs.len() < PUBLICATION_JOB_LIMIT
        {
            pending.jobs.insert(id);
        }
        self.persist_publications(&pending, false)?;
        if pending.limited {
            Err("Deferred Bazarr publication tracking reached its safety limit; sync inventory after the queue drains to verify all outputs".into())
        } else {
            Ok(())
        }
    }

    fn published_outputs(&self, outputs: &[String], integrations: &mut Integrations) {
        let mut runtime = lock(&self.runtime);
        if let Some(job) = runtime.current_job_mut() {
            remember_published_srts(job, outputs);
            job["published_outputs"] =
                json!(count(&job["published_outputs"]).saturating_add(outputs.len() as u64));
        }
        if integrations.bazarr_enabled() {
            runtime.inventory_authority["status"] = json!("refresh_pending");
            runtime.inventory_authority["error"] = Value::Null;
            runtime.inventory_authority["reason"] = json!(
                "SRT files published; Bazarr indexing starts after the conversion queue drains"
            );
        }
        drop(runtime);
        if integrations.bazarr_enabled()
            && let Err(error) = self.stage_publications(outputs)
        {
            self.notifications(vec![
                json!({"provider":"bazarr","ok":false,"message":error}),
            ]);
        }
        self.notifications(integrations.notify_other_providers(outputs));
    }

    /// The dispatcher calls this only after every active coordinator joined
    /// and the queue was observed empty. New submissions wait behind this
    /// drain boundary; a cancelled file never cancels other jobs' receipts.
    fn flush_publications(&self) {
        let mut pending = lock(&self.publications);
        if pending.paths.is_empty() || self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let mut integrations = lock(&self.integrations).clone();
        if !integrations.bazarr_enabled() {
            return;
        }
        let paths: Vec<_> = pending.paths.iter().cloned().collect();
        let job = json!({"id":token(8),"kind":"refresh","provider":"bazarr","origin":"queue_drain",
            "status":"running","started":now(),"root_paths":self.mounts,"languages":["*"],
            "published_outputs":paths.len(),"published_srt_keys":paths.iter().filter_map(|path|srt_index_key(path)).collect::<Vec<_>>(),
            "verification_limited":pending.limited});
        {
            let mut runtime = lock(&self.runtime);
            if !runtime.queue.is_empty() || !runtime.active_jobs.is_empty() {
                return;
            }
            runtime.running = Some(job.clone());
        }
        self.cancel.store(false, Ordering::Relaxed);
        // Durably mark the uncertain network boundary before sending a PATCH.
        // On restart it is never blindly repeated.
        if let Err(error) = self
            .prepare_bazarr_verification()
            .and_then(|()| self.persist_publications(&pending, true))
        {
            lock(&self.runtime).running = None;
            self.log(
                "ERROR",
                &format!("Bazarr queue drain deferred: {}", log_text(&error, 256)),
            );
            return;
        }
        self.log(
            "INFO",
            &format!(
                "Conversion queue drained; updating Bazarr for {} published SRT files",
                paths.len()
            ),
        );
        integrations.stage_bazarr(&paths);
        let mut receipt =
            integrations.flush_bazarr(&self.cancel, &mut |event| self.progress(event));
        let verification =
            self.refresh_bazarr_after_publication(&mut integrations, &job, receipt.as_mut());
        let notice = match (verification, receipt) {
            (Err(error), _) => {
                json!({"provider":"bazarr","ok":false,"message":format!("SRT published; Bazarr inventory refresh failed: {}",log_text(&error,256))})
            }
            (Ok(()), Some(receipt)) => receipt,
            _ => {
                json!({"provider":"bazarr","ok":false,"message":"Bazarr queue-drain scan returned no receipt"})
            }
        };
        let mut runtime = lock(&self.runtime);
        runtime.running = None;
        for finished in &mut runtime.history {
            if finished["id"]
                .as_str()
                .is_some_and(|id| pending.jobs.contains(id))
            {
                if !finished["notifications"].is_array() {
                    finished["notifications"] = json!([]);
                }
                if let Some(items) = finished["notifications"].as_array_mut() {
                    items.push(notice.clone());
                }
                if ["completed", "completed_with_errors"]
                    .iter()
                    .any(|status| finished["status"] == *status)
                {
                    finished["status"] = json!(completed_status(finished));
                }
            }
        }
        self.persist_history(&runtime);
        drop(runtime);
        *pending = Publications::default();
        if self.persist_publications(&pending, false).is_err() {
            self.log("ERROR", "Bazarr drain receipt could not be saved; pending indexing remains verifiable without repeating the scan");
        }
    }

    fn parallel_job(job: &Value) -> bool {
        ["extract", "ocr", "scan"].contains(&job["kind"].as_str().unwrap_or(""))
            && job["task_id"] != "discovery"
            && job["paths"]
                .as_array()
                .is_some_and(|paths| !paths.is_empty())
    }

    fn worker(self: Arc<Self>) {
        let mut handles: BTreeMap<String, std::thread::JoinHandle<()>> = BTreeMap::new();
        while !self.shutdown.load(Ordering::Relaxed) {
            let finished: Vec<_> = handles
                .iter()
                .filter(|(_, handle)| handle.is_finished())
                .map(|(id, _)| id.clone())
                .collect();
            for id in finished {
                if let Some(handle) = handles.remove(&id) {
                    let _ = handle.join();
                }
            }
            let due = lock(&self.scheduler).due(now());
            for id in due {
                let _ = self.enqueue_task(&id, "schedule", false);
            }
            let job = {
                let mut runtime = lock(&self.runtime);
                let paused = lock(&self.workers).allocation.paused;
                let available = !paused
                    && runtime.active_jobs.len() < ACTIVE_JOB_LIMIT
                    && runtime.queue.front().is_some_and(|next| {
                        runtime.active_jobs.is_empty()
                            || Self::parallel_job(next)
                                && runtime
                                    .active_jobs
                                    .values()
                                    .all(|active| Self::parallel_job(&active.job))
                    });
                if available {
                    let mut job = runtime.queue.pop_front().expect("checked queue");
                    job["status"] = json!("running");
                    job["started"] = json!(now());
                    runtime.active_jobs.insert(
                        job["id"].as_str().unwrap_or("").to_owned(),
                        ActiveJob {
                            owner: None,
                            cancel: Arc::new(AtomicBool::new(false)),
                            job: job.clone(),
                        },
                    );
                    Some(job)
                } else {
                    None
                }
            };
            if let Some(job) = job {
                let id = job["id"].as_str().unwrap_or("").to_owned();
                let runner = Arc::clone(&self);
                let thread_id = id.clone();
                match std::thread::Builder::new()
                    .name("subforge-job".into())
                    .stack_size(2 * 1024 * 1024)
                    .spawn(move || {
                        if let Some(active) = lock(&runner.runtime).active_jobs.get_mut(&thread_id)
                        {
                            active.owner = Some(std::thread::current().id());
                        }
                        runner.log(
                            "INFO",
                            &format!(
                                "Job started: {} ({thread_id})",
                                job["kind"].as_str().unwrap_or("job")
                            ),
                        );
                        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            runner.execute_job(&job)
                        }))
                        .is_err()
                            && let Some(active) =
                                lock(&runner.runtime).active_jobs.get_mut(&thread_id)
                        {
                            active.job["error"] = json!("Job coordinator failed safely");
                        }
                        runner.finish_job(&thread_id);
                    }) {
                    Ok(handle) => {
                        handles.insert(id, handle);
                    }
                    Err(_) => {
                        if let Some(active) = lock(&self.runtime).active_jobs.get_mut(&id) {
                            active.job["error"] = json!("Job coordinator capacity is unavailable");
                        }
                        self.finish_job(&id);
                    }
                }
                continue;
            }
            let drained = {
                let runtime = lock(&self.runtime);
                runtime.queue.is_empty() && runtime.active_jobs.is_empty() && handles.is_empty()
            };
            if drained {
                self.flush_publications();
            }
            let runtime = lock(&self.runtime);
            let _guard = self.wake.wait_timeout(runtime, Duration::from_millis(100));
        }
        for active in lock(&self.runtime).active_jobs.values() {
            active.cancel.store(true, Ordering::Relaxed);
        }
        for (_, handle) in handles {
            let _ = handle.join();
        }
    }
}

struct Request {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
struct Reply {
    status: u16,
    body: Vec<u8>,
    content: &'static str,
    extra: Vec<(&'static str, String)>,
}
impl Reply {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            body: serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec()),
            content: "application/json; charset=utf-8",
            extra: Vec::new(),
        }
    }
    fn error(status: u16, message: &str) -> Self {
        Self::json(status, json!({"error":message}))
    }
    fn asset(content: &'static str, body: &'static [u8]) -> Self {
        Self {
            status: 200,
            body: body.to_vec(),
            content,
            extra: Vec::new(),
        }
    }
}
fn read_request(socket: &mut TcpStream) -> Result<Request, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut bytes = Vec::with_capacity(4096);
    let mut scratch = [0_u8; 4096];
    let header_end = loop {
        if Instant::now() >= deadline {
            return Err("Request headers timed out".into());
        }
        socket
            .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .map_err(|_| "Cannot bound request reads")?;
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() >= HEADER_LIMIT {
            return Err("Request headers exceed their size limit".into());
        }
        let amount = (HEADER_LIMIT - bytes.len()).min(scratch.len());
        let read = socket
            .read(&mut scratch[..amount])
            .map_err(|_| "Request headers timed out")?;
        if read == 0 {
            return Err("Incomplete HTTP request".into());
        }
        bytes.extend_from_slice(&scratch[..read]);
    };
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    if !parsed
        .parse(&bytes[..header_end])
        .map_err(|_| "Invalid HTTP headers")?
        .is_complete()
    {
        return Err("Incomplete HTTP headers".into());
    }
    let method = parsed.method.ok_or("Missing HTTP method")?.to_owned();
    let target = parsed.path.ok_or("Missing request path")?.to_owned();
    if target.len() > 8192 || !target.starts_with('/') || target.starts_with("//") {
        return Err("Invalid request target".into());
    }
    let mut request_headers = BTreeMap::new();
    for header in parsed.headers.iter() {
        let name = header.name.to_ascii_lowercase();
        if request_headers.contains_key(&name) {
            return Err("Duplicate HTTP headers are denied".into());
        }
        let value = std::str::from_utf8(header.value)
            .map_err(|_| "Invalid HTTP header encoding")?
            .trim();
        if value.chars().any(char::is_control) {
            return Err("Invalid HTTP header value".into());
        }
        request_headers.insert(name, value.to_owned());
    }
    if request_headers.contains_key("transfer-encoding") {
        return Err("Transfer encoding is not supported".into());
    }
    let length = request_headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| "Invalid request body length")?
        .unwrap_or(0);
    if length > BODY_LIMIT {
        return Err("Request body exceeds 65536 bytes".into());
    }
    let mut body = bytes.get(header_end..).unwrap_or_default().to_vec();
    if body.len() > length {
        return Err("Pipelined requests are not supported".into());
    }
    body.resize(length, 0);
    let already = bytes.len() - header_end;
    let mut offset = already;
    while offset < length {
        if Instant::now() >= deadline {
            return Err("Request body timed out".into());
        }
        socket
            .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .map_err(|_| "Cannot bound request reads")?;
        let read = socket
            .read(&mut body[offset..])
            .map_err(|_| "Incomplete request body")?;
        if read == 0 {
            return Err("Incomplete request body".into());
        }
        offset += read;
    }
    Ok(Request {
        method,
        target,
        headers: request_headers,
        body,
    })
}
fn guard(app: &App, request: &Request) -> Option<Reply> {
    let host = request
        .headers
        .get("host")
        .map(String::as_str)
        .unwrap_or("");
    let parsed = url::Url::parse(&format!("http://{host}"));
    let valid = parsed.as_ref().is_ok_and(|url| {
        url.host_str()
            .is_some_and(|host| app.hosts.contains(host.trim_matches(['[', ']'])))
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
    });
    if !valid {
        return Some(Reply::error(
            403,
            "Host is not allowed; configure CONVERTER_ALLOWED_HOSTS",
        ));
    }
    if request.headers.get("origin").is_some_and(|origin| {
        origin != &format!("http://{host}") && origin != &format!("https://{host}")
    }) {
        return Some(Reply::error(403, "Cross-origin requests are not allowed"));
    }
    if request.target.starts_with("/api/remote/") {
        if ![
            "/api/remote/register",
            "/api/remote/heartbeat",
            "/api/remote/lease",
            "/api/remote/complete",
        ]
        .contains(&request.target.as_str())
        {
            return Some(Reply::error(404, "Not found"));
        }
        if request.method != "POST" {
            return Some(Reply::error(405, "Method is not allowed"));
        }
        let supplied = request
            .headers
            .get("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or("");
        let remote = lock(&app.remote);
        if !remote.enabled() {
            return Some(Reply::error(404, "Remote workers are disabled"));
        }
        if !remote.authenticated(supplied) {
            return Some(Reply::error(401, "Worker token is missing or invalid"));
        }
        return None;
    }
    if request.target.starts_with("/hooks/") {
        if !["/hooks/sonarr", "/hooks/radarr"].contains(&request.target.as_str()) {
            return Some(Reply::error(404, "Not found"));
        }
        if app.webhook.is_empty()
            || !constant_equal(
                request
                    .headers
                    .get("x-webhook-token")
                    .map(String::as_bytes)
                    .unwrap_or_default(),
                app.webhook.as_bytes(),
            )
        {
            return Some(Reply::error(401, "Webhook token is missing or invalid"));
        }
        return None;
    }
    if request.target == "/api/widget" {
        if app.widget.is_empty() {
            return Some(Reply::error(404, "Not found"));
        }
        let supplied = request
            .headers
            .get("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or("");
        if request.method != "GET" || !constant_equal(supplied.as_bytes(), app.widget.as_bytes()) {
            let mut reply = Reply::error(401, "Widget token is missing or invalid");
            reply.extra.push((
                "WWW-Authenticate",
                "Bearer realm=\"Subforge widget\"".into(),
            ));
            return Some(reply);
        }
        return None;
    }
    if request.method == "GET" && request.target == "/health" {
        return None;
    }
    let authenticated = request
        .headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
        .is_some_and(|bytes| constant_equal(&bytes, format!("admin:{}", app.password).as_bytes()));
    if !authenticated {
        let mut reply = Reply::error(401, "Dashboard login required");
        reply.extra.push((
            "WWW-Authenticate",
            "Basic realm=\"Subforge\", charset=\"UTF-8\"".into(),
        ));
        return Some(reply);
    }
    if request.method != "GET"
        && !constant_equal(
            request
                .headers
                .get("x-csrf-token")
                .map(String::as_bytes)
                .unwrap_or_default(),
            app.csrf.as_bytes(),
        )
    {
        return Some(Reply::error(
            403,
            "Reload the dashboard before making changes",
        ));
    }
    None
}
fn handle(app: &App, request: &Request) -> Reply {
    if let Some(reply) = guard(app, request) {
        return reply;
    }
    if request.method == "GET" {
        return match request.target.as_str() {
            "/" => Reply::asset(
                "text/html; charset=utf-8",
                include_bytes!("../web/index.html"),
            ),
            "/static/app.js" | "/app.js" => Reply::asset(
                "text/javascript; charset=utf-8",
                include_bytes!("../web/app.js"),
            ),
            "/static/style.css" | "/style.css" => Reply::asset(
                "text/css; charset=utf-8",
                include_bytes!("../web/style.css"),
            ),
            "/static/icon.svg" => Reply::asset("image/svg+xml", include_bytes!("../web/icon.svg")),
            "/favicon.ico" => Reply {
                status: 204,
                body: Vec::new(),
                content: "image/x-icon",
                extra: Vec::new(),
            },
            "/health" => Reply::json(200, json!({"ok":true})),
            "/api/session" => Reply::json(200, json!({"csrf_token":app.csrf})),
            "/api/status" => Reply::json(200, app.status()),
            "/api/tasks" => Reply::json(200, app.tasks()),
            "/api/system" => Reply::json(200, app.system()),
            "/api/workers" => Reply::json(200, app.workers()),
            "/api/nodes" => Reply::json(200, lock(&app.remote).public()),
            "/api/inventory" => Reply::json(200, app.inventory()),
            "/api/bazarr/inventory" => Reply::json(200, app.inventory()),
            "/api/widget" => Reply::json(200, app.widget()),
            "/api/settings" => Reply::json(200, app.settings()),
            "/api/root-folders" => Reply::json(200, app.root_folders()),
            "/api/integrations/jellyfin/libraries" => match lock(&app.integrations).libraries() {
                Ok(libraries) => Reply::json(200, json!({"libraries":libraries})),
                Err(error) => Reply::error(400, &error),
            },
            target if target == "/api/folders" || target.starts_with("/api/folders?") => {
                let query = target.split_once('?').map(|(_, query)| query).unwrap_or("");
                let paths = url::form_urlencoded::parse(query.as_bytes())
                    .filter(|(key, _)| key == "path")
                    .map(|(_, value)| value.into_owned())
                    .collect::<Vec<_>>();
                if paths.len() > 1 {
                    return Reply::error(400, "Choose one folder path");
                }
                match app.folders(paths.first().map(String::as_str)) {
                    Ok(value) => Reply::json(200, value),
                    Err(error) => Reply::error(400, &error),
                }
            }
            _ => Reply::error(404, "Not found"),
        };
    }
    if !["POST", "PUT"].contains(&request.method.as_str()) {
        return Reply::error(405, "Method is not allowed");
    }
    if request
        .headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        != Some("application/json")
        || request.body.is_empty()
    {
        return Reply::error(400, "Send a nonempty application/json body");
    }
    let data: Value = match serde_json::from_slice(&request.body) {
        Ok(data) if Value::is_object(&data) => data,
        _ => return Reply::error(400, "Expected a JSON object"),
    };
    let result: Result<(u16, Value), String> = if request.method == "PUT" {
        match request.target.as_str() {
            "/api/settings" => app.update_settings(&data).map(|value| (200, value)),
            "/api/workers" => app.update_workers(&data).map(|value| (200, value)),
            "/api/root-folders" => app.update_roots(&data).map(|value| (200, value)),
            "/api/tasks" => lock(&app.scheduler)
                .update(&data, now())
                .map(|value| (200, value)),
            _ => return Reply::error(404, "Not found"),
        }
    } else if request.target.starts_with("/api/remote/") {
        lock(&app.remote)
            .handle(&request.target, &data)
            .map(|value| (200, value))
    } else if request.target.starts_with("/api/tasks/") && request.target.ends_with("/run") {
        let parts = request.target.split('/').collect::<Vec<_>>();
        if parts.len() != 5 {
            return Reply::error(404, "Not found");
        }
        if data
            .as_object()
            .is_none_or(|body| body.keys().any(|key| key != "retry"))
            || data.get("retry").is_some_and(|retry| !retry.is_boolean())
        {
            return Reply::error(400, "Invalid task run options");
        }
        app.enqueue_task(
            parts[3],
            "task_manual",
            data["retry"].as_bool().unwrap_or(false),
        )
        .map(|job| (202, json!({"job":job})))
    } else if request.target == "/api/jobs" {
        app.enqueue(&data, "manual")
            .map(|job| (202, json!({"job":job})))
    } else if request.target.starts_with("/api/jobs/") && request.target.ends_with("/cancel") {
        let parts = request.target.split('/').collect::<Vec<_>>();
        if parts.len() != 5 {
            return Reply::error(404, "Not found");
        }
        app.cancel(parts[3]).map(|value| (200, value))
    } else if ["/hooks/sonarr", "/hooks/radarr"].contains(&request.target.as_str()) {
        if data["eventType"]
            .as_str()
            .is_some_and(|event| event.eq_ignore_ascii_case("test"))
        {
            return Reply::json(200, json!({"ok":true}));
        }
        let provider = request.target.rsplit('/').next().unwrap_or("");
        let paths = Integrations::webhook_paths(provider, &data);
        let integrations = lock(&app.integrations);
        let mapped = paths
            .iter()
            .map(|path| {
                integrations
                    .map_path(provider, path)
                    .unwrap_or_else(|| path.clone())
            })
            .collect::<Vec<_>>();
        drop(integrations);
        if mapped.is_empty() {
            return Reply::error(400, "Webhook has no mapped media files");
        }
        app.enqueue(&json!({"kind":"extract","paths":mapped}), provider)
            .map(|job| (202, json!({"job":job})))
    } else if request.target.starts_with("/api/integrations/") {
        let parts = request.target.split('/').collect::<Vec<_>>();
        if parts.len() != 5 || !providers().contains(&parts[3]) {
            return Reply::error(404, "Not found");
        }
        match parts[4] {
            "test" => Ok((200, lock(&app.integrations).test(parts[3]))),
            "refresh" => app
                .enqueue(&json!({"kind":"refresh","provider":parts[3]}), "manual")
                .map(|job| (202, json!({"job":job}))),
            _ => return Reply::error(404, "Not found"),
        }
    } else {
        return Reply::error(404, "Not found");
    };
    let operation = match request.target.as_str() {
        "/api/settings" => Some("Settings update"),
        "/api/root-folders" => Some("Media folder update"),
        "/api/tasks" => Some("Schedule update"),
        "/api/remote/register" => Some("Remote worker registration"),
        "/api/remote/complete" => Some("Remote completion report"),
        "/api/integrations/sonarr/test"
        | "/api/integrations/radarr/test"
        | "/api/integrations/jellyfin/test"
        | "/api/integrations/bazarr/test" => Some("Integration connection test"),
        _ => None,
    };
    match result {
        Ok((status, value)) => {
            if let Some(operation) = operation {
                app.log("INFO", &format!("{operation} processed"));
            }
            Reply::json(status, value)
        }
        Err(error) => {
            if let Some(operation) = operation {
                app.log("WARNING", &format!("{operation} rejected"));
            }
            Reply::error(400, &error)
        }
    }
}
fn send(socket: &mut TcpStream, reply: Reply) -> std::io::Result<()> {
    let reason = match reply.status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(
        socket,
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'\r\n",
        reply.status,
        reason,
        reply.body.len(),
        reply.content
    )?;
    for (name, value) in reply.extra {
        write!(socket, "{name}: {value}\r\n")?;
    }
    socket.write_all(b"\r\n")?;
    socket.write_all(&reply.body)?;
    socket.flush()
}
pub fn run(arguments: Vec<String>) -> Result<(), String> {
    let mut host = "127.0.0.1".to_string();
    let mut port = 8100_u16;
    let mut data =
        PathBuf::from(std::env::var("CONVERTER_DATA").unwrap_or_else(|_| "/data".into()));
    let mut roots = Vec::new();
    let mut auto_scan = true;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--no-auto-scan" => auto_scan = false,
            "--host" | "--port" | "--data" | "--root" => {
                index += 1;
                let value = arguments.get(index).ok_or("CLI option needs a value")?;
                match argument.as_str() {
                    "--host" => host = value.clone(),
                    "--port" => port = value.parse().map_err(|_| "Port must be a valid integer")?,
                    "--data" => data = PathBuf::from(value),
                    "--root" => roots.push(PathBuf::from(value)),
                    _ => {}
                }
            }
            "--help" | "-h" => {
                println!("Subforge --host HOST --port PORT --data DIR --root DIR [--no-auto-scan]");
                return Ok(());
            }
            _ => return Err("Unknown command-line option".into()),
        }
        index += 1;
    }
    if roots.is_empty() {
        roots = ["/movies", "/movies-C", "/tv", "/tv-C"]
            .iter()
            .map(PathBuf::from)
            .collect();
    }
    let interval = std::env::var("CONVERTER_SCAN_INTERVAL")
        .unwrap_or_else(|_| "21600".into())
        .parse::<u64>()
        .map_err(|_| "CONVERTER_SCAN_INTERVAL must be an integer of at least 1 second")?;
    if interval == 0 {
        return Err("CONVERTER_SCAN_INTERVAL must be at least 1 second".into());
    }
    let app = App::new(
        roots,
        data,
        Duration::from_secs(interval.max(21600)),
        auto_scan,
    )?;
    let listener = TcpListener::bind((host.as_str(), port))
        .map_err(|_| "Listener address or port is unavailable")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "Cannot configure listener")?;
    let shutdown = app.shutdown.clone();
    let cancel = app.cancel.clone();
    ctrlc::set_handler(move || {
        shutdown.store(true, Ordering::Relaxed);
        cancel.store(true, Ordering::Relaxed);
    })
    .map_err(|_| "Cannot install shutdown handler")?;
    let worker_app = app.clone();
    let conversion = std::thread::spawn(move || worker_app.worker());
    let (sender, receiver) = mpsc::sync_channel::<TcpStream>(16);
    let receiver = Arc::new(Mutex::new(receiver));
    let mut handlers = Vec::new();
    for _ in 0..HTTP_WORKERS {
        let receiver = receiver.clone();
        let app = app.clone();
        handlers.push(std::thread::spawn(move || {
            loop {
                if app.shutdown.load(Ordering::Relaxed) {
                    break;
                }
                let socket = lock(&receiver).recv();
                let Ok(mut socket) = socket else { break };
                let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
                let _ = socket.set_write_timeout(Some(Duration::from_secs(10)));
                let reply = match read_request(&mut socket) {
                    Ok(request) => handle(&app, &request),
                    Err(error) => Reply::error(400, &error),
                };
                let _ = send(&mut socket, reply);
            }
        }));
    }
    println!("Subforge Rust dashboard listening on port {port}; username admin");
    while !app.shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut socket, _)) => {
                if let Err(mpsc::TrySendError::Full(socket_value)) = sender.try_send(socket) {
                    socket = socket_value;
                    let _ = socket.set_write_timeout(Some(Duration::from_secs(1)));
                    let _ = send(&mut socket, Reply::error(503, "Request capacity reached"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(_) => break,
        }
    }
    app.cancel.store(true, Ordering::Relaxed);
    app.shutdown.store(true, Ordering::Relaxed);
    app.wake.notify_all();
    drop(sender);
    let _ = conversion.join();
    for handler in handlers {
        let _ = handler.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BazarrFixture {
        url: String,
        movies: Arc<Mutex<Vec<Value>>>,
        fail: Arc<AtomicBool>,
        scanned: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl BazarrFixture {
        fn new(movies: Vec<Value>, languages: &[&str]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let movies = Arc::new(Mutex::new(movies));
            let fail = Arc::new(AtomicBool::new(false));
            let scanned = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let profile = json!([{"profileId":1,"items":languages.iter().map(|language|json!({"language":language,"forced":"False","hi":"False"})).collect::<Vec<_>>()}]);
            let (server_movies, server_fail, server_scanned, server_requests, server_stop) = (
                Arc::clone(&movies),
                Arc::clone(&fail),
                Arc::clone(&scanned),
                Arc::clone(&requests),
                Arc::clone(&stop),
            );
            let thread = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(15);
                while !server_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    let (mut socket, _) = match listener.accept() {
                        Ok(socket) => socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let _ = socket.set_nonblocking(false);
                    let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));
                    let Ok(request) = read_request(&mut socket) else {
                        continue;
                    };
                    lock(&server_requests).push(format!("{} {}", request.method, request.target));
                    let response = if server_fail.load(Ordering::Relaxed) {
                        Reply::json(503, json!({"error":"Fixture service unavailable"}))
                    } else if request.method == "PATCH" {
                        server_scanned.store(true, Ordering::Relaxed);
                        Reply::json(200, json!({}))
                    } else if request.target == "/api/system/languages/profiles" {
                        Reply::json(200, profile.clone())
                    } else if request.target.starts_with("/api/movies?") {
                        let query = request.target.split_once('?').unwrap().1;
                        let start = url::form_urlencoded::parse(query.as_bytes())
                            .find(|(key, _)| key == "start")
                            .and_then(|(_, value)| value.parse::<usize>().ok())
                            .unwrap_or(0);
                        let movies = lock(&server_movies);
                        Reply::json(
                            200,
                            json!({"data":movies.iter().skip(start).take(200).cloned().collect::<Vec<_>>(),"total":movies.len()}),
                        )
                    } else {
                        Reply::json(200, json!({"data":[],"total":0}))
                    };
                    let _ = send(&mut socket, response);
                }
            });
            Self {
                url,
                movies,
                fail,
                scanned,
                requests,
                stop,
                thread: Some(thread),
            }
        }
        fn integration(&self, app: &App) -> Integrations {
            let mut integrations =
                Integrations::new(app.data.join("fixture-integrations.json")).unwrap();
            integrations.update(&json!({"bazarr":{"enabled":true,"url":self.url,"api_key":"disposable-fixture"}})).unwrap();
            integrations
        }
    }
    impl Drop for BazarrFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    fn target_srt(path: &Path) {
        fs::write(
            path,
            b"1\n00:00:01,000 --> 00:00:02,000\nDisposable fixture\n",
        )
        .unwrap();
    }

    #[test]
    fn bazarr_language_metadata_identifies_an_untagged_external_source_without_probing() {
        let (_temporary, app, _paths, gate) = gated_workers_fixture();
        let movie = app.mounts[0].join("film.mkv");
        let source = app.mounts[0].join("film.ass");
        fs::write(&movie, b"Disposable media").unwrap();
        fs::write(&source, b"Disposable subtitle").unwrap();
        let fixture = BazarrFixture::new(
            vec![
                json!({"radarrId":1,"profileId":1,"path":movie,"subtitles":[{"code2":"fr","path":source,"forced":false,"hi":false}]}),
            ],
            &["fr"],
        );
        let mut integrations = fixture.integration(&app);
        let rows = app
            .sync_bazarr_inventory(
                &mut integrations,
                &json!({"kind":"inspect","languages":["*"]}),
                true,
            )
            .unwrap();
        assert_eq!(rows[0]["coverage"]["status"], "needs_conversion");
        assert_eq!(rows[0]["coverage"]["available_paths"], json!([source]));
        assert_eq!(rows[0]["source_hints"][0]["target"]["language"], "fr");
        assert_eq!(gate.entered.load(Ordering::Relaxed), 0);
        assert!(lock(&app.workers).active.is_empty());
    }

    #[test]
    fn bazarr_covered_profile_skips_probe_and_ocr_and_survives_restart() {
        let (_temporary, app, _paths, gate) = gated_workers_fixture();
        let movie = app.mounts[0].join("film.mkv");
        fs::write(&movie, b"disposable media").unwrap();
        let subtitles = ["en", "fr", "ar"]
            .iter()
            .map(|language| {
                let path = app.mounts[0].join(format!("film.{language}.srt"));
                target_srt(&path);
                json!({"code2":language,"path":path,"file_size":50,"forced":"False","hi":"False"})
            })
            .collect::<Vec<_>>();
        let fixture = BazarrFixture::new(
            vec![json!({"radarrId":1,"profileId":1,"path":movie,"subtitles":subtitles})],
            &["en", "fr", "ar"],
        );
        let mut integrations = fixture.integration(&app);
        let job = json!({"kind":"scan","workload":"all","languages":["*"],"root_paths":app.mounts,"paths":[]});
        lock(&app.runtime).running = Some(job.clone());
        app.execute(&job, &mut integrations).unwrap();
        let inventory = app.inventory();
        assert_eq!(inventory["source"], "bazarr");
        assert_eq!(inventory["covered_count"], 1);
        assert_eq!(inventory["items"][0]["coverage"]["status"], "covered");
        assert_eq!(
            inventory["items"][0]["coverage"]["existing_targets"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            gate.entered.load(Ordering::Relaxed),
            0,
            "covered files must never enter conversion workers"
        );
        assert!(
            !lock(&app.runtime)
                .logs
                .iter()
                .any(|entry| entry["status"] == "inspecting")
        );
        let saved = read_json(&app.data.join("inventory.json"), JSON_LIMIT).unwrap();
        assert_eq!(saved["source"], "bazarr");
        write_private(
            &app.data.join("settings.json"),
            &read_json(&app.data.join("fixture-integrations.json"), 64 * 1024).unwrap(),
        )
        .unwrap();
        let restarted = App::new(
            app.mounts.clone(),
            app.data.clone(),
            Duration::from_secs(21600),
            false,
        )
        .unwrap();
        assert_eq!(restarted.inventory()["items"].as_array().unwrap().len(), 1);
        assert_eq!(restarted.inventory()["authority"]["status"], "stale");
    }

    #[test]
    fn enabled_bazarr_failure_never_falls_back_to_filesystem_conversion() {
        let (_temporary, app, _paths, gate) = gated_workers_fixture();
        let fixture = BazarrFixture::new(Vec::new(), &["en"]);
        fixture.fail.store(true, Ordering::Relaxed);
        let mut integrations = fixture.integration(&app);
        let job = json!({"kind":"scan","workload":"all","languages":["*"],"root_paths":app.mounts,"paths":[]});
        lock(&app.runtime).running = Some(job.clone());
        assert!(app.execute(&job, &mut integrations).is_err());
        assert_eq!(gate.entered.load(Ordering::Relaxed), 0);
        assert_eq!(app.inventory()["source"], "bazarr");
        assert_eq!(app.inventory()["authority"]["status"], "unavailable");
        assert_eq!(lock(&fixture.requests).len(), 1);
    }

    #[test]
    fn selected_bazarr_with_missing_credentials_never_admits_filesystem_workers() {
        let (_temporary, app, _paths, gate) = gated_workers_fixture();
        let fixture = BazarrFixture::new(Vec::new(), &["en"]);
        let job = json!({"kind":"scan","workload":"all","languages":["*"],"root_paths":app.mounts,"paths":[]});
        for update in [
            json!({"bazarr":{"enabled":true,"url":"","clear_api_key":true}}),
            json!({"bazarr":{"clear_api_key":true}}),
            json!({"bazarr":{"url":format!("{}/changed",fixture.url)}}),
        ] {
            let mut integrations = fixture.integration(&app);
            integrations.update(&update).unwrap();
            assert!(
                integrations.bazarr_enabled(),
                "the selected authority remains Bazarr when its connection is incomplete"
            );
            lock(&app.runtime).running = Some(job.clone());
            assert!(app.execute(&job, &mut integrations).is_err());
            assert_eq!(gate.entered.load(Ordering::Relaxed), 0);
            assert!(lock(&app.workers).active.is_empty());
            assert_eq!(app.workers()["remote"]["active"], 0);
            assert_eq!(app.inventory()["source"], "bazarr");
            assert_eq!(app.inventory()["authority"]["status"], "unavailable");
        }
        assert!(
            lock(&fixture.requests).is_empty(),
            "missing connection fields fail before network dispatch"
        );
    }

    #[test]
    fn bazarr_inventory_retains_more_than_two_thousand_authoritative_rows() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let rows=(1..=2253).map(|id|json!({"radarrId":id,"profileId":null,"path":app.mounts[0].join(format!("film-{id}.mkv")),"subtitles":[]})).collect();
        let fixture = BazarrFixture::new(rows, &["en"]);
        let mut integrations = fixture.integration(&app);
        app.sync_bazarr_inventory(
            &mut integrations,
            &json!({"kind":"inspect","languages":["*"]}),
            false,
        )
        .unwrap();
        let inventory = app.inventory();
        assert_eq!(inventory["items"].as_array().unwrap().len(), 2253);
        assert_eq!(inventory["truncated"], false);
        assert_eq!(inventory["source"], "bazarr");
    }

    #[test]
    fn bazarr_publication_reindexes_then_refetches_and_persists_coverage() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let movie = app.mounts[0].join("film.mkv");
        fs::write(&movie, b"disposable media").unwrap();
        let fixture = BazarrFixture::new(
            vec![json!({"radarrId":1,"profileId":1,"path":movie,"subtitles":[]})],
            &["en"],
        );
        let mut integrations = fixture.integration(&app);
        let job = json!({"kind":"extract","root_paths":app.mounts,"languages":["en"]});
        let output = app.mounts[0].join("film.en.srt");
        target_srt(&output);
        published_job(&app, std::slice::from_ref(&output));
        integrations.notify(&[output.to_string_lossy().into_owned()]);
        let mut reindex = integrations.flush_bazarr(&app.cancel, &mut |_| {}).unwrap();
        assert_eq!(reindex["triggered"], 1);
        assert!(fixture.scanned.load(Ordering::Relaxed));
        lock(&fixture.movies)[0]["subtitles"] =
            json!([{"code2":"en","path":output,"file_size":50,"forced":"False","hi":"False"}]);
        app.refresh_bazarr_after_publication(&mut integrations, &job, Some(&mut reindex))
            .unwrap();
        assert_eq!(app.inventory()["items"][0]["coverage"]["status"], "covered");
        assert_eq!(app.inventory()["authority"]["status"], "ready");
        let calls = lock(&fixture.requests);
        let patch = calls
            .iter()
            .position(|request| request == "PATCH /api/movies")
            .unwrap();
        assert!(
            calls
                .iter()
                .skip(patch + 1)
                .any(|request| request == "GET /api/system/languages/profiles")
        );
        assert!(!calls.iter().any(|request| request.contains("system/tasks")));
        drop(calls);
        assert_eq!(
            read_json(&app.data.join("inventory.json"), JSON_LIMIT).unwrap()["items"][0]["coverage"]
                ["status"],
            "covered"
        );
        app.refresh_bazarr_after_publication(
            &mut integrations,
            &job,
            Some(&mut json!({"ok":false,"triggered":0,"message":"Fixture scoped reindex failed"})),
        )
        .unwrap();
        assert_eq!(app.inventory()["authority"]["status"], "reindex_failed");
        assert_eq!(
            app.inventory()["items"][0]["coverage"]["status"],
            "covered",
            "valid published output stays visible after notification failure"
        );
    }

    fn published_job(app: &App, outputs: &[PathBuf]) -> Value {
        fs::write(app.mounts[0].join("film.mkv"), b"disposable media").unwrap();
        let mut job = json!({"kind":"extract","published_outputs":outputs.len(),"root_paths":app.mounts,"languages":["*"]});
        remember_published_srts(
            &mut job,
            &outputs
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        );
        let mut runtime = lock(&app.runtime);
        runtime.inventory_source = "bazarr".into();
        runtime.running = Some(job.clone());
        job
    }

    fn pending_scan() -> Value {
        json!({"provider":"bazarr","ok":false,"pending":true,"uncertain":true,"attempted":1,"acknowledged":0,"triggered":0,"message":"Bazarr response timed out; scan completion not yet confirmed"})
    }

    #[test]
    fn bazarr_confirmation_requires_exact_external_srt_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("film.en.srt");
        let expected = vec![srt_index_key(output.to_str().unwrap()).unwrap()];
        for row in [
            json!({"subtitles":[{"source":"sidecar","path":temporary.path().join("other.en.srt") }]}),
            json!({"subtitles":[{"source":"embedded","path":output}]}),
            json!({"subtitles":[{"source":"sidecar","path":temporary.path().join("film.en.forced.srt") }]}),
            json!({"coverage":{"status":"covered"},"subtitles":[]}),
        ] {
            assert_eq!(unconfirmed_index_keys(&[row], &expected), expected);
        }
        assert!(
            unconfirmed_index_keys(
                &[json!({"subtitles":[{"source":"sidecar","path":output}]})],
                &expected
            )
            .is_empty()
        );
        assert!(srt_index_key("relative.en.srt").is_none());
        assert!(srt_index_key(temporary.path().join("../film.en.srt").to_str().unwrap()).is_none());
        assert!(srt_index_key(temporary.path().join("film.en.ass").to_str().unwrap()).is_none());
        #[cfg(not(windows))]
        assert_ne!(
            srt_index_key(output.to_str().unwrap()),
            srt_index_key(temporary.path().join("Film.en.srt").to_str().unwrap())
        );
    }

    #[test]
    fn bazarr_uncertain_scan_is_successful_when_every_output_is_in_fresh_inventory() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let outputs: Vec<_> = ["en", "fr", "ar"]
            .iter()
            .map(|language| app.mounts[0].join(format!("film.{language}.srt")))
            .collect();
        for output in &outputs {
            target_srt(output);
        }
        let subtitles: Vec<_> = ["en", "fr", "ar"].iter().zip(&outputs).map(|(language, output)| json!({"code2":language,"path":output,"forced":false,"hi":false})).collect();
        let fixture = BazarrFixture::new(
            vec![
                json!({"radarrId":1,"profileId":1,"path":app.mounts[0].join("film.mkv"),"subtitles":subtitles}),
            ],
            &["en", "fr", "ar"],
        );
        let mut integrations = fixture.integration(&app);
        let job = published_job(&app, &outputs);
        let mut receipt = pending_scan();
        app.refresh_bazarr_after_publication(&mut integrations, &job, Some(&mut receipt))
            .unwrap();
        assert_eq!(receipt["ok"], true);
        assert_eq!(receipt["pending"], false);
        assert_eq!(receipt["verified_outputs"], 3);
        assert_eq!(
            receipt["acknowledged"], 0,
            "inventory confirmation does not invent a PATCH acknowledgment"
        );
        assert_eq!(app.inventory()["authority"]["status"], "ready");
        assert!(index_keys(&app.inventory()["authority"]["pending_srt_keys"]).is_empty());
        assert!(
            lock(&fixture.requests)
                .iter()
                .all(|request| request.starts_with("GET "))
        );
    }

    #[test]
    fn bazarr_partial_index_stays_pending_even_when_local_files_cover_languages() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let outputs = vec![
            app.mounts[0].join("film.en.srt"),
            app.mounts[0].join("film.fr.srt"),
        ];
        for output in &outputs {
            target_srt(output);
        }
        let fixture = BazarrFixture::new(
            vec![
                json!({"radarrId":1,"profileId":1,"path":app.mounts[0].join("film.mkv"),"subtitles":[{"code2":"en","path":outputs[0],"forced":false,"hi":false}]}),
            ],
            &["en", "fr"],
        );
        let mut integrations = fixture.integration(&app);
        let job = published_job(&app, &outputs);
        let mut receipt = pending_scan();
        app.refresh_bazarr_after_publication(&mut integrations, &job, Some(&mut receipt))
            .unwrap();
        assert_eq!(receipt["pending"], true);
        assert_eq!(
            app.inventory()["items"][0]["coverage"]["status"],
            "covered",
            "local coverage is distinct from Bazarr indexing"
        );
        assert_eq!(app.inventory()["authority"]["status"], "refresh_pending");
        assert_eq!(
            index_keys(&app.inventory()["authority"]["pending_srt_keys"]),
            vec![srt_index_key(outputs[1].to_str().unwrap()).unwrap()]
        );
        assert_eq!(
            completed_status(&json!({"kind":"extract","notifications":[receipt]})),
            "completed"
        );
    }

    #[test]
    fn bazarr_failed_verification_read_keeps_scan_uncertain_and_keys_persisted() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let fixture = BazarrFixture::new(Vec::new(), &["en"]);
        fixture.fail.store(true, Ordering::Relaxed);
        let mut integrations = fixture.integration(&app);
        let job = published_job(&app, &[app.mounts[0].join("film.en.srt")]);
        let mut receipt = pending_scan();
        app.refresh_bazarr_after_publication(&mut integrations, &job, Some(&mut receipt))
            .unwrap();
        assert_eq!(receipt["pending"], true);
        assert_eq!(receipt["ok"], false);
        assert_eq!(
            completed_status(&json!({"kind":"extract","notifications":[receipt]})),
            "completed"
        );
        let authority = &app.inventory()["authority"];
        assert_eq!(authority["status"], "unavailable");
        assert!(authority["error"].is_string());
        assert_eq!(index_keys(&authority["pending_srt_keys"]).len(), 1);
        assert_eq!(
            read_json(&app.data.join("inventory.json"), JSON_LIMIT).unwrap()["authority"]["pending_srt_keys"],
            authority["pending_srt_keys"]
        );
        assert_eq!(lock(&fixture.requests).len(), 1);
    }

    #[test]
    fn bazarr_untrackable_outputs_cannot_be_vacuously_confirmed() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let fixture = BazarrFixture::new(Vec::new(), &["en"]);
        let mut integrations = fixture.integration(&app);
        for limited in [false, true] {
            lock(&app.runtime).running = Some(
                json!({"kind":"extract","published_outputs":1,"verification_limited":limited}),
            );
            let mut receipt = pending_scan();
            app.refresh_bazarr_after_publication(
                &mut integrations,
                &json!({"kind":"extract"}),
                Some(&mut receipt),
            )
            .unwrap();
            assert_eq!(receipt["pending"], true);
            assert_eq!(app.inventory()["authority"]["verification_limited"], true);
            assert_eq!(app.inventory()["authority"]["status"], "refresh_pending");
        }
        let output = app.mounts[0].join("film.en.srt");
        target_srt(&output);
        lock(&fixture.movies).push(json!({"radarrId":1,"profileId":1,"path":app.mounts[0].join("film.mkv"),"subtitles":[{"code2":"en","path":output,"forced":false,"hi":false}]}));
        let job = published_job(&app, std::slice::from_ref(&output));
        lock(&app.runtime).running.as_mut().unwrap()["verification_limited"] = json!(true);
        let mut receipt = pending_scan();
        app.refresh_bazarr_after_publication(&mut integrations, &job, Some(&mut receipt))
            .unwrap();
        assert_eq!(
            receipt["pending"], true,
            "visible known paths cannot confirm outputs omitted by the tracking limit"
        );
        assert_eq!(app.inventory()["authority"]["verification_limited"], true);
    }

    #[test]
    fn bazarr_pending_verification_survives_cancel_restart_and_later_read_only_sync() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let output = app.mounts[0].join("film.en.srt");
        target_srt(&output);
        let fixture = BazarrFixture::new(
            vec![
                json!({"radarrId":1,"profileId":1,"path":app.mounts[0].join("film.mkv"),"subtitles":[]}),
            ],
            &["en"],
        );
        let mut integrations = fixture.integration(&app);
        let job = published_job(&app, std::slice::from_ref(&output));
        app.cancel.store(true, Ordering::Relaxed);
        app.refresh_bazarr_after_publication(&mut integrations, &job, Some(&mut pending_scan()))
            .unwrap();
        assert!(
            lock(&fixture.requests).is_empty(),
            "cancellation preserves state without dispatching more requests"
        );
        let mut history = json!({"kind":"extract","status":"completed","notifications":[pending_scan()],"published_paths":[output],"published_srt_keys":["old"]});
        history["published_outputs"] = json!(1);
        {
            let mut runtime = lock(&app.runtime);
            runtime.history.push_back(history);
            app.persist_history(&runtime);
        }
        let saved = read_json(&app.data.join("jobs.json"), JSON_LIMIT).unwrap();
        assert!(saved[0].get("published_paths").is_none());
        assert!(saved[0].get("published_srt_keys").is_none());
        write_private(
            &app.data.join("settings.json"),
            &read_json(&app.data.join("fixture-integrations.json"), 64 * 1024).unwrap(),
        )
        .unwrap();
        let restarted = App::new(
            app.mounts.clone(),
            app.data.clone(),
            Duration::from_secs(21600),
            false,
        )
        .unwrap();
        assert_eq!(
            index_keys(&restarted.inventory()["authority"]["pending_srt_keys"]).len(),
            1
        );
        lock(&fixture.movies)[0]["subtitles"] =
            json!([{"code2":"en","path":output,"forced":false,"hi":false}]);
        restarted
            .sync_bazarr_inventory(
                &mut integrations,
                &json!({"kind":"inspect","languages":["*"]}),
                false,
            )
            .unwrap();
        assert_eq!(restarted.inventory()["authority"]["status"], "ready");
        let notice = &restarted.status()["history"][0]["notifications"][0];
        assert_eq!(notice["pending"], false);
        assert_eq!(notice["verified"], true);
        assert_eq!(notice["ok"], true);
        assert!(
            lock(&fixture.requests)
                .iter()
                .all(|request| request.starts_with("GET "))
        );
    }

    #[test]
    fn bazarr_confirmation_tracking_is_bounded_and_overflow_remains_unconfirmed() {
        let temporary = tempfile::tempdir().unwrap();
        let outputs: Vec<_> = (0..INDEX_CONFIRM_LIMIT + 1)
            .map(|index| {
                temporary
                    .path()
                    .join(format!("{index}.en.srt"))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let mut job = json!({});
        remember_published_srts(&mut job, &outputs);
        assert_eq!(
            index_keys(&job["published_srt_keys"]).len(),
            INDEX_CONFIRM_LIMIT
        );
        assert_eq!(job["verification_limited"], true);
        assert!(serde_json::to_vec(&job).unwrap().len() < 2 * 1024 * 1024);
    }

    #[test]
    fn operational_logs_are_bounded_single_line_records_with_explicit_fields() {
        let event = json!({"status":"extracting", "path":"/media/private-root/movie\n.ass", "track":7,"backend":"ffmpeg","password":"omit-this-field","stdout":"omit-subtitle-text"});
        let entry = progress_log(&event);
        assert_eq!(entry["file"], "movie.ass");
        assert_eq!(entry["track"], 7);
        assert!(entry["message"].as_str().unwrap().contains("track 7"));
        let mut output = Vec::new();
        write_log(&mut output, &entry).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.lines().count(), 1);
        assert!(!output.contains("private-root"));
        assert!(!output.contains("omit-this-field"));
        assert!(!output.contains("omit-subtitle-text"));
        let failure = progress_log(&json!({"status":"failed","error":"x".repeat(10000)}));
        assert_eq!(failure["level"], "ERROR");
        assert_eq!(failure["error"].as_str().unwrap().len(), 240);
    }

    #[test]
    fn progress_populates_dashboard_logs_during_a_running_job() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        app.progress(
            json!({"status":"ocr","path":"/media/movie.sup","track":2,"backend":"paddle"}),
        );
        let status = app.status();
        let latest = status["logs"].as_array().unwrap().last().unwrap();
        assert_eq!(latest["status"], "ocr");
        assert_eq!(latest["backend"], "paddle");
        assert_eq!(status["running"]["current"]["track"], 2);
        assert!(status["logs"].as_array().unwrap().len() <= LOG_LIMIT);
    }

    #[test]
    fn assigned_executor_overwrites_event_identity_and_logs_bounded_fields() {
        let executor = json!({"node_id":"assigned-node","node_name":"Studio\nGPU","resource":"gpu","execution":"remote","lease":"do-not-log","path":"/private/do-not-log"});
        for status in ["extracting", "ocr", "converted", "failed", "remote_retry"] {
            let event = worker_event(
                json!({"status":status,"node_id":"forged","node_name":"forged","resource":"cpu","execution":"local","path":"/media/film.mkv"}),
                42,
                &executor,
            );
            let entry = progress_log(&event);
            assert_eq!(entry["node_id"], "assigned-node");
            assert_eq!(entry["node_name"], "StudioGPU");
            assert_eq!(entry["resource"], "gpu");
            assert_eq!(entry["execution"], "remote");
            assert_eq!(entry["worker_id"], 42);
            assert!(
                entry["message"]
                    .as_str()
                    .unwrap()
                    .contains("StudioGPU (GPU OCR, remote)")
            );
            assert!(!entry.to_string().contains("forged"));
            assert!(!entry.to_string().contains("do-not-log"));
            if status == "remote_retry" {
                assert_eq!(entry["level"], "WARNING");
            }
        }
        let legacy = progress_log(&json!({"status":"converted","path":"/media/old.mkv"}));
        assert!(
            legacy.get("node_name").is_none(),
            "do not infer a processor for older events"
        );
    }

    #[test]
    fn completed_job_preserves_multiple_executors_after_refresh_and_restart() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let cpu =
            json!({"node_id":"local","node_name":"Local","resource":"cpu","execution":"local"});
        let gpu = json!({"node_id":"remote-gpu","node_name":"Studio GPU","resource":"gpu","execution":"remote"});
        app.progress(worker_event(
            json!({"status":"dispatched","path":"/media/first.mkv"}),
            0,
            &cpu,
        ));
        app.progress(worker_event(
            json!({"status":"converted","path":"/media/first.mkv"}),
            0,
            &cpu,
        ));
        app.progress(worker_event(
            json!({"status":"converted","path":"/media/second.mkv"}),
            128,
            &gpu,
        ));
        app.progress(json!({"status":"refreshing","path":"Bazarr inventory"}));
        {
            let mut runtime = lock(&app.runtime);
            let mut job = runtime.running.take().unwrap();
            assert_eq!(job["executors"], json!([cpu, gpu]));
            job["status"] = json!("completed");
            runtime.history.push_back(job);
            app.persist_history(&runtime);
        }
        let restarted = App::new(
            app.mounts.clone(),
            app.data.clone(),
            Duration::from_secs(21600),
            false,
        )
        .unwrap();
        assert_eq!(
            restarted.status()["history"][0]["executors"][1]["node_name"],
            "Studio GPU"
        );
        assert_eq!(
            restarted.status()["history"][0]["executors"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn executor_history_deduplicates_and_caps_distinct_node_resource_pairs() {
        let mut job = json!({});
        for index in 0..EXECUTOR_LIMIT + 5 {
            let event = json!({"node_id":format!("node-{index}"),"node_name":"x".repeat(1000),"resource":"cpu","execution":"remote"});
            remember_executor(&mut job, &event);
            remember_executor(&mut job, &event);
        }
        assert_eq!(job["executors"].as_array().unwrap().len(), EXECUTOR_LIMIT);
        assert_eq!(job["executors_truncated"], true);
        assert_eq!(job["executors"][0]["node_name"].as_str().unwrap().len(), 64);
        assert!(serde_json::to_vec(&job).unwrap().len() < 16 * 1024);
        let mut mixed = json!({});
        for resource in ["cpu", "cpu_ocr", "gpu"] {
            remember_executor(
                &mut mixed,
                &json!({"node_id":"same-node","node_name":"Same node","resource":resource,"execution":"remote"}),
            );
        }
        assert_eq!(mixed["executors"].as_array().unwrap().len(), 3);
    }

    fn gated_workers_fixture() -> (
        tempfile::TempDir,
        Arc<App>,
        Vec<PathBuf>,
        Arc<crate::converter::FileGate>,
    ) {
        let temporary = tempfile::tempdir().expect("fixture");
        let mount = temporary.path().join("media");
        fs::create_dir(&mount).expect("fixture");
        let app = App::new(
            vec![mount.clone()],
            temporary.path().join("data"),
            Duration::from_secs(21600),
            false,
        )
        .expect("app");
        app.update_workers(
            &json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":false}),
        )
        .expect("allocation");
        lock(&app.resources).set_fixture(crate::resources::Snapshot::fixture(
            4.0,
            Some(1536 * 1024 * 1024),
        ));
        let gate = Arc::new(crate::converter::FileGate::default());
        lock(&app.converter)
            .set_file_gate(Arc::clone(&gate), temporary.path().join("no-tool-can-run"));
        let paths = (1..=3)
            .map(|episode| {
                let path = mount.join(format!("show.S01E{episode:02}.ass"));
                fs::write(&path, b"disposable fixture").expect("fixture");
                fs::write(path.with_extension("srt"), b"existing output must survive")
                    .expect("fixture");
                Converter::safe_path(&path.to_string_lossy(), std::slice::from_ref(&mount))
                    .expect("path")
            })
            .collect();
        lock(&app.runtime).running = Some(json!({"kind":"extract","processed":0,"summary":{}}));
        (temporary, app, paths, gate)
    }

    fn wait_for_fixture(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(condition(), "bounded worker fixture must make progress");
    }

    struct FixtureDispatcher {
        app: Arc<App>,
        handle: Option<std::thread::JoinHandle<()>>,
    }
    impl FixtureDispatcher {
        fn start(app: &Arc<App>) -> Self {
            let runner = Arc::clone(app);
            Self {
                app: Arc::clone(app),
                handle: Some(std::thread::spawn(move || runner.worker())),
            }
        }
    }
    impl Drop for FixtureDispatcher {
        fn drop(&mut self) {
            self.app.shutdown.store(true, Ordering::Relaxed);
            self.app.wake.notify_all();
            if let Some(handle) = self.handle.take() {
                handle.join().unwrap();
            }
        }
    }

    #[test]
    fn separately_queued_jobs_resize_live_and_downscale_without_stopping_files() {
        let (_temporary, app, paths, gate) = gated_workers_fixture();
        lock(&app.runtime).running = None;
        app.update_workers(
            &json!({"cpu_workers":1,"ocr_workers":0,"gpu_workers":0,"paused":false}),
        )
        .unwrap();
        let mut ids = Vec::new();
        for number in 0..15 {
            let path = if let Some(path) = paths.get(number) {
                path.clone()
            } else {
                let path = app.mounts[0].join(format!("extra-{number}.ass"));
                fs::write(&path, b"Disposable subtitle fixture").unwrap();
                path
            };
            let job = app.enqueue(&json!({"kind":"extract","paths":[path],"include_ocr":false,"languages":["*"],"retry":true}), "manual").unwrap().unwrap();
            ids.push(job["id"].as_str().unwrap().to_owned());
        }
        let _dispatcher = FixtureDispatcher::start(&app);
        wait_for_fixture(|| gate.entered.load(Ordering::Relaxed) == 1);
        app.update_workers(
            &json!({"cpu_workers":4,"ocr_workers":0,"gpu_workers":0,"paused":false}),
        )
        .unwrap();
        wait_for_fixture(|| gate.entered.load(Ordering::Relaxed) == 4);
        assert_eq!(app.workers()["active"]["cpu_workers"], 4);
        let active_ids = lock(&app.workers)
            .active
            .iter()
            .map(|worker| worker.id)
            .collect::<HashSet<_>>();
        assert_eq!(
            active_ids.len(),
            4,
            "IDs are global across jobs, not per coordinator"
        );
        app.update_workers(
            &json!({"cpu_workers":3,"ocr_workers":0,"gpu_workers":0,"paused":false}),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            app.workers()["active"]["cpu_workers"],
            4,
            "downscale drains already running files"
        );
        assert!(
            lock(&app.runtime)
                .active_jobs
                .values()
                .all(|active| !active.cancel.load(Ordering::Relaxed))
        );
        gate.released_count.store(1, Ordering::Relaxed);
        wait_for_fixture(|| app.workers()["active"]["cpu_workers"] == 3);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            gate.entered.load(Ordering::Relaxed),
            4,
            "no admission while active equals reduced target"
        );
        gate.released_count.store(2, Ordering::Relaxed);
        wait_for_fixture(|| gate.entered.load(Ordering::Relaxed) == 5);
        assert_eq!(
            app.workers()["active"]["cpu_workers"],
            3,
            "one slot refills once active falls below target"
        );
        gate.released.store(true, Ordering::Relaxed);
        wait_for_fixture(|| lock(&app.runtime).history.len() == 15);
        let status = app.status();
        for id in ids {
            let job = status["history"]
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["id"] == id)
                .unwrap();
            assert_eq!(job["status"], "completed");
            assert_eq!(
                job["processed"], 1,
                "completion belongs to exactly its own job"
            );
            assert_eq!(job["executors"][0]["resource"], "cpu");
        }
        assert_eq!(gate.entered.load(Ordering::Relaxed), 15);
        assert!(lock(&app.workers).active.is_empty());
    }

    #[test]
    fn concurrent_jobs_do_not_overlap_subtitle_output_families() {
        let (_temporary, app, paths, gate) = gated_workers_fixture();
        lock(&app.runtime).running = None;
        let sibling = paths[0].with_extension("en.ass");
        fs::write(&sibling, b"Disposable subtitle fixture").unwrap();
        for path in [&paths[0], &sibling] {
            app.enqueue(
                &json!({"kind":"extract","paths":[path],"include_ocr":false}),
                "manual",
            )
            .unwrap();
        }
        let _dispatcher = FixtureDispatcher::start(&app);
        wait_for_fixture(|| gate.entered.load(Ordering::Relaxed) == 1);
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(gate.entered.load(Ordering::Relaxed), 1);
        assert_eq!(
            app.workers()["active"]["cpu_workers"],
            1,
            "a free CPU slot cannot bypass an output-family reservation"
        );
        gate.released.store(true, Ordering::Relaxed);
        wait_for_fixture(|| lock(&app.runtime).history.len() == 2);
        assert_eq!(gate.entered.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn cancelling_one_concurrent_job_does_not_cancel_another() {
        let (_temporary, app, paths, gate) = gated_workers_fixture();
        lock(&app.runtime).running = None;
        let first = app
            .enqueue(
                &json!({"kind":"extract","paths":[paths[0]],"include_ocr":false}),
                "manual",
            )
            .unwrap()
            .unwrap();
        let second = app
            .enqueue(
                &json!({"kind":"extract","paths":[paths[1]],"include_ocr":false}),
                "manual",
            )
            .unwrap()
            .unwrap();
        let _dispatcher = FixtureDispatcher::start(&app);
        wait_for_fixture(|| gate.entered.load(Ordering::Relaxed) == 2);
        app.cancel(first["id"].as_str().unwrap()).unwrap();
        wait_for_fixture(|| {
            lock(&app.runtime)
                .history
                .iter()
                .any(|job| job["id"] == first["id"])
        });
        {
            let runtime = lock(&app.runtime);
            assert_eq!(runtime.history.back().unwrap()["status"], "cancelled");
            assert!(
                !runtime.active_jobs[second["id"].as_str().unwrap()]
                    .cancel
                    .load(Ordering::Relaxed)
            );
        }
        assert_eq!(app.workers()["active"]["cpu_workers"], 1);
        gate.released.store(true, Ordering::Relaxed);
        wait_for_fixture(|| lock(&app.runtime).history.len() == 2);
        let status = app.status();
        assert_eq!(
            status["history"]
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["id"] == second["id"])
                .unwrap()["status"],
            "completed"
        );
    }

    #[test]
    fn bazarr_notifications_wait_for_queue_and_all_active_jobs_then_deduplicate() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let movie = app.mounts[0].join("film.mkv");
        fs::write(&movie, b"Disposable media").unwrap();
        let outputs = [
            app.mounts[0].join("film.en.srt"),
            app.mounts[0].join("film.fr.srt"),
        ];
        for output in &outputs {
            target_srt(output);
        }
        let fixture = BazarrFixture::new(
            vec![json!({"radarrId":1,"profileId":1,"path":movie,"subtitles":[]})],
            &["en", "fr"],
        );
        *lock(&app.integrations) = fixture.integration(&app);
        {
            let mut runtime = lock(&app.runtime);
            runtime.running = Some(json!({"id":"finished-with-output"}));
            runtime.inventory_source = "bazarr".into();
            runtime.history.push_back(
                json!({"id":"finished-with-output","kind":"extract","status":"cancelled"}),
            );
            runtime.history.push_back(json!({"id":"failed-with-output","kind":"extract","status":"failed","error":"Fixture partial failure"}));
        }
        app.stage_publications(&[outputs[0].to_string_lossy().into_owned()])
            .unwrap();
        lock(&app.runtime).running = Some(json!({"id":"failed-with-output"}));
        app.stage_publications(
            &outputs
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        {
            let mut runtime = lock(&app.runtime);
            runtime.running = None;
            runtime
                .queue
                .push_back(json!({"id":"queued","status":"queued"}));
        }
        app.flush_publications();
        assert!(
            lock(&fixture.requests).is_empty(),
            "queued work prevents all drain requests"
        );
        {
            let mut runtime = lock(&app.runtime);
            runtime.queue.clear();
            runtime.active_jobs.insert(
                "working".into(),
                ActiveJob {
                    owner: None,
                    cancel: Arc::new(AtomicBool::new(false)),
                    job: json!({"id":"working"}),
                },
            );
        }
        app.flush_publications();
        assert!(
            lock(&fixture.requests).is_empty(),
            "an empty queue is insufficient while a file job is active"
        );
        lock(&app.runtime).active_jobs.clear();
        app.cancel.store(true, Ordering::Relaxed);
        app.flush_publications();
        assert_eq!(
            lock(&fixture.requests)
                .iter()
                .filter(|request| request.starts_with("PATCH "))
                .count(),
            1,
            "same media is scanned once for outputs from multiple jobs"
        );
        let status = app.status();
        for job in status["history"].as_array().unwrap() {
            assert_eq!(job["notifications"].as_array().unwrap().len(), 1);
            assert_eq!(job["notifications"][0]["provider"], "bazarr");
        }
        assert_eq!(status["history"][0]["status"], "failed");
        assert_eq!(status["history"][1]["status"], "cancelled");
        app.flush_publications();
        assert_eq!(
            lock(&fixture.requests)
                .iter()
                .filter(|request| request.starts_with("PATCH "))
                .count(),
            1,
            "idle polls never repeat an attempted scan"
        );
        assert!(lock(&app.publications).paths.is_empty());
        assert!(
            read_json(&app.data.join("publication-queue.json"), JSON_LIMIT).unwrap()["paths"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn publication_queue_survives_restart_but_attempted_scans_are_not_replayed() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let output = app.mounts[0].join("film.en.srt");
        target_srt(&output);
        app.stage_publications(&[output.to_string_lossy().into_owned()])
            .unwrap();
        let restored = App::new(
            app.mounts.clone(),
            app.data.clone(),
            Duration::from_secs(21600),
            false,
        )
        .unwrap();
        assert_eq!(lock(&restored.publications).paths.len(), 1);
        restored
            .persist_publications(&lock(&restored.publications), true)
            .unwrap();
        let attempted = App::new(
            app.mounts.clone(),
            app.data.clone(),
            Duration::from_secs(21600),
            false,
        )
        .unwrap();
        assert!(
            lock(&attempted.publications).paths.is_empty(),
            "uncertain PATCH is verified by a read-only sync instead of automatically replayed"
        );
    }

    #[test]
    fn parallel_files_deduplicate_and_pause_only_new_dispatch() {
        let (_temporary, app, mut paths, gate) = gated_workers_fixture();
        paths.push(paths[0].clone());
        let runner = Arc::clone(&app);
        let thread = std::thread::spawn(move || {
            let mut integrations = Integrations::new(runner.data.join("settings.json")).unwrap();
            runner.convert_files(
                &paths,
                &json!({"languages":["*"],"retry":true}),
                Workload::Text,
                &mut integrations,
            )
        });
        wait_for_fixture(|| gate.entered.load(Ordering::Relaxed) == 2);
        assert_eq!(app.workers()["active"]["cpu_workers"], 2);
        assert_eq!(app.status()["running"]["total"], 3);
        app.update_workers(&json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":true}))
            .unwrap();
        gate.released.store(true, Ordering::Relaxed);
        wait_for_fixture(|| lock(&app.workers).active.is_empty());
        assert_eq!(gate.entered.load(Ordering::Relaxed), 2);
        assert!(
            !thread.is_finished(),
            "paused dispatch leaves the third file pending"
        );
        app.update_workers(
            &json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":false}),
        )
        .unwrap();
        let summary = thread.join().unwrap().unwrap();
        assert_eq!(gate.entered.load(Ordering::Relaxed), 3);
        assert_eq!(summary["converted"], 3);
        assert_eq!(
            read_json(&app.data.join("state.json"), JSON_LIMIT)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(app.status()["running"]["processed"], 3);
        assert_eq!(
            app.status()["running"]["executors"],
            json!([{"node_id":"local","node_name":"Local","resource":"cpu","execution":"local"}])
        );
        let status = app.status();
        for entry in status["logs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["status"] == "converted")
        {
            assert_eq!(entry["node_name"], "Local");
            assert_eq!(entry["resource"], "cpu");
            assert!(
                entry["worker_id"].is_number(),
                "finished files retain their worker after slot removal"
            );
        }
        assert!(lock(&app.workers).active.is_empty());
    }

    #[test]
    fn all_mode_text_sidecars_use_text_slots_when_cpu_ocr_is_zero() {
        let (_temporary, mut app, paths, gate) = gated_workers_fixture();
        Arc::get_mut(&mut app).unwrap().ocr_permission = true;
        app.update_workers(
            &json!({"cpu_workers":2,"ocr_workers":0,"gpu_workers":0,"paused":false}),
        )
        .unwrap();
        let runner = Arc::clone(&app);
        let thread = std::thread::spawn(move || {
            let mut integrations = Integrations::new(runner.data.join("settings.json")).unwrap();
            runner.convert_files(
                &paths,
                &json!({"languages":["*"]}),
                Workload::All,
                &mut integrations,
            )
        });
        wait_for_fixture(|| gate.entered.load(Ordering::Relaxed) == 2);
        assert_eq!(app.workers()["active"]["ocr_workers"], 0);
        gate.released.store(true, Ordering::Relaxed);
        assert_eq!(thread.join().unwrap().unwrap()["converted"], 3);
    }

    #[test]
    fn remote_routes_require_dedicated_bearer_and_preserve_host_and_origin_guards() {
        let (_temporary, app, _paths, _gate) = gated_workers_fixture();
        let token = "r".repeat(48);
        *lock(&app.remote) =
            crate::remote::Coordinator::new(token.clone(), app.mounts.clone()).unwrap();
        let mut request = Request {
            method: "POST".into(),
            target: "/api/remote/register".into(),
            headers: BTreeMap::from([("host".into(), "localhost".into())]),
            body: b"{}".to_vec(),
        };
        assert_eq!(guard(&app, &request).unwrap().status, 401);
        request
            .headers
            .insert("authorization".into(), format!("Bearer {token}"));
        assert!(
            guard(&app, &request).is_none(),
            "worker token authorizes its fixed routes without dashboard CSRF"
        );
        request
            .headers
            .insert("origin".into(), "https://unexpected.invalid".into());
        assert_eq!(guard(&app, &request).unwrap().status, 403);
        request.headers.remove("origin");
        request
            .headers
            .insert("host".into(), "unexpected.invalid".into());
        assert_eq!(guard(&app, &request).unwrap().status, 403);
        request.headers.insert("host".into(), "localhost".into());
        request.target = "/api/settings".into();
        assert_eq!(
            guard(&app, &request).unwrap().status,
            401,
            "worker token cannot access dashboard settings"
        );
        request.target = "/api/remote/arbitrary".into();
        assert_eq!(guard(&app, &request).unwrap().status, 404);
        request.target = "/api/remote/register".into();
        request.method = "GET".into();
        assert_eq!(guard(&app, &request).unwrap().status, 405);
        *lock(&app.remote) =
            crate::remote::Coordinator::new(String::new(), app.mounts.clone()).unwrap();
        request.method = "POST".into();
        assert_eq!(guard(&app, &request).unwrap().status, 404);
    }

    #[test]
    fn remote_all_mode_respects_coordinator_ocr_permission_and_merges_outputs() {
        let (_temporary, mut app, paths, gate) = gated_workers_fixture();
        Arc::get_mut(&mut app).unwrap().ocr_permission = false;
        *lock(&app.remote) =
            crate::remote::Coordinator::new("r".repeat(48), app.mounts.clone()).unwrap();
        let node = lock(&app.remote)
            .handle(
                "/api/remote/register",
                &json!({"name":"fixture-node","resource":"cpu","roots":app.mounts}),
            )
            .unwrap()["node"]
            .clone();
        lock(&app.remote)
            .handle("/api/remote/lease", &json!({"node":node}))
            .unwrap();
        let path = paths[0].clone();
        let runner = Arc::clone(&app);
        let thread = std::thread::spawn(move || {
            let mut integrations = Integrations::new(runner.data.join("settings.json")).unwrap();
            runner.convert_files(
                &[path],
                &json!({"languages":["*"]}),
                Workload::All,
                &mut integrations,
            )
        });
        wait_for_fixture(|| lock(&app.remote).public()["active"] == 1);
        let assignment = lock(&app.remote)
            .handle("/api/remote/lease", &json!({"node":node}))
            .unwrap();
        assert_eq!(assignment["resource"], "cpu");
        assert_eq!(
            assignment["workload"], "text",
            "remote workers may not enable OCR for a text-only coordinator reservation"
        );
        let staging = app.mounts[0].join(assignment["staging"].as_str().unwrap());
        let output = "show.S01E01.en.srt";
        fs::write(
            staging.join(output),
            b"1\n00:00:01,000 --> 00:00:02,000\nFixture\n",
        )
        .unwrap();
        assert!(
            !paths[0].parent().unwrap().join(output).exists(),
            "remote staging is unpublished"
        );
        let metadata = fs::metadata(&paths[0]).unwrap();
        let modified = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        lock(&app.remote).handle("/api/remote/complete",&json!({"node":node,"lease":assignment["lease"],"fingerprint":[[metadata.len(),modified]],"state":{"ok":true,"outputs":[output],"fingerprint":[[metadata.len(),modified]]},"summary":{"converted":1,"cached":0,"pending":0,"failed":0,"planned":0},"terminal":{"status":"converted"},"outputs":[output]})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let finished = thread.is_finished();
        if !finished {
            app.cancel.store(true, Ordering::Relaxed);
            gate.released.store(true, Ordering::Relaxed);
        }
        let result = thread.join().unwrap();
        assert!(
            finished,
            "valid remote completion must reach the coordinator promptly"
        );
        assert_eq!(result.unwrap()["converted"], 1);
        assert!(paths[0].parent().unwrap().join(output).is_file());
        assert_eq!(gate.entered.load(Ordering::Relaxed), 0);
        assert_eq!(app.workers()["remote"]["active"], 0);
        assert_eq!(
            app.status()["running"]["executors"],
            json!([{"node_id":node,"node_name":"fixture-node","resource":"cpu","execution":"remote"}])
        );
        let status = app.status();
        let terminal = status["logs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["status"] == "converted")
            .unwrap();
        assert_eq!(terminal["node_name"], "fixture-node");
        assert_eq!(terminal["resource"], "cpu");
        assert_eq!(terminal["execution"], "remote");
        assert!(terminal["worker_id"].as_u64().unwrap() >= crate::workers::HARD_TOTAL_LIMIT as u64);
        assert!(lock(&app.workers).active.is_empty());
        assert_eq!(
            read_json(&app.data.join("state.json"), JSON_LIMIT).unwrap()
                [paths[0].to_string_lossy().as_ref()]["ok"],
            true
        );
    }

    #[test]
    fn cancellation_joins_all_active_files_and_preserves_existing_outputs() {
        let (_temporary, app, paths, gate) = gated_workers_fixture();
        let output = paths[0].with_extension("srt");
        let runner = Arc::clone(&app);
        let thread = std::thread::spawn(move || {
            let mut integrations = Integrations::new(runner.data.join("settings.json")).unwrap();
            runner.convert_files(
                &paths,
                &json!({"languages":["*"]}),
                Workload::Text,
                &mut integrations,
            )
        });
        wait_for_fixture(|| gate.entered.load(Ordering::Relaxed) == 2);
        app.cancel.store(true, Ordering::Relaxed);
        assert!(thread.join().unwrap().is_ok());
        assert_eq!(gate.entered.load(Ordering::Relaxed), 2);
        assert!(lock(&app.workers).active.is_empty());
        assert_eq!(fs::read(output).unwrap(), b"existing output must survive");
    }

    #[test]
    fn system_snapshot_stays_responsive_with_converter_locked() {
        let temporary = tempfile::tempdir().expect("fixture");
        let mount = temporary.path().join("media");
        fs::create_dir(&mount).expect("fixture");
        let app = App::new(
            vec![mount],
            temporary.path().join("data"),
            Duration::from_secs(21600),
            false,
        )
        .expect("app");
        lock(&app.runtime).running = Some(json!({"kind":"ocr","status":"running"}));
        let converter_guard = lock(&app.converter);
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader_app = Arc::clone(&app);
        let reader = std::thread::spawn(move || {
            let status = reader_app.status();
            assert!(status["pipeline"].is_object());
            sender.send(reader_app.system()).expect("reader");
        });
        let response = receiver.recv_timeout(Duration::from_secs(2));
        drop(converter_guard);
        reader.join().expect("reader");
        let system = response.expect("system must not wait for long conversion or OCR");
        assert_eq!(
            system["resource_policy"]["metadata_cache_max_entries"],
            crate::metadata::MAX_ENTRIES
        );
        assert_eq!(
            system["resource_policy"]["metadata_cache_max_bytes"],
            crate::metadata::LIMIT_BYTES
        );
        assert_eq!(system["resource_policy"]["conversion_workers"], 1);
        assert_eq!(system["resource_policy"]["ocr_threads"], 1);
        assert_eq!(system["resource_usage"]["worker"]["active"], 0);
        assert_eq!(system["resource_usage"]["worker"]["kind"], "ocr");
        assert_eq!(system["resource_usage"]["metadata_cache"]["entries"], 0);
        assert_eq!(
            system["resource_usage"]["metadata_cache"]["byte_accounting"],
            "serialized_entries"
        );
    }

    #[test]
    fn statuses_distinguish_partial_and_refresh_failure() {
        assert_eq!(
            completed_status(&json!({"kind":"scan","summary":{"failed":3}})),
            "completed_with_errors"
        );
        assert_eq!(
            completed_status(&json!({"kind":"refresh","results":[{"ok":false}]})),
            "failed"
        );
    }
    #[test]
    fn directory_permissions_are_closed_outside_mounts() {
        let temporary = tempfile::tempdir().expect("fixture");
        let mount = temporary.path().join("media");
        fs::create_dir(&mount).expect("fixture");
        assert!(validate_root(&mount, std::slice::from_ref(&mount), true).is_ok());
        assert!(validate_root(temporary.path(), &[mount], true).is_err());
    }
    #[test]
    fn constant_time_comparison_handles_length_and_contents() {
        assert!(constant_equal(b"private", b"private"));
        assert!(!constant_equal(b"private", b"privat"));
        assert!(!constant_equal(b"private", b"prIvate"));
    }
    #[test]
    fn inventory_survives_restart_separately_from_job_history() {
        let temporary = tempfile::tempdir().expect("fixture");
        let mount = temporary.path().join("media");
        let data = temporary.path().join("data");
        fs::create_dir(&mount).expect("fixture");
        fs::create_dir(&data).expect("fixture");
        let row = json!({"path":mount.join("example.mkv"),"type":"video","tracks":[{"number":3,"language":"en","format":"SubRip"}]});
        write_private(
            &data.join("inventory.json"),
            &json!({"items":[row.clone()],"updated":10.0,"truncated":true}),
        )
        .expect("fixture");
        write_private(
            &data.join("jobs.json"),
            &json!([{"kind":"inspect","results":[row],"status":"completed"}]),
        )
        .expect("fixture");
        let app = App::new(vec![mount], data, Duration::from_secs(60), false).expect("app");
        assert_eq!(app.inventory()["items"].as_array().unwrap().len(), 1);
        assert_eq!(app.inventory()["updated"], 10.0);
        assert_eq!(app.status()["history"][0]["results"], json!([]));
        assert!(app.status()["inventory"].get("items").is_none());
    }
    #[test]
    fn stored_inventory_is_bounded_and_stays_inside_mounts() {
        let temporary = tempfile::tempdir().expect("fixture");
        let mount = temporary.path().join("media");
        fs::create_dir(&mount).expect("fixture");
        let value = json!([{"path":temporary.path().join("outside.mkv"),"tracks":[]},{"path":mount.join("inside.mkv"),"tracks":vec![json!({"language":"x".repeat(4096),"format":"x".repeat(4096) });crate::metadata::MAX_TRACKS + 20]}]);
        let rows = inventory_rows(
            &value,
            std::slice::from_ref(&mount),
            std::slice::from_ref(&mount),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["tracks"].as_array().unwrap().len(),
            crate::metadata::MAX_TRACKS
        );
        assert!(rows[0]["tracks"][0]["language"].as_str().unwrap().len() <= 35);
    }

    #[test]
    fn discovery_retains_legacy_inventory_when_track_metadata_is_not_cached() {
        let temporary = tempfile::tempdir().expect("fixture");
        let mount = temporary.path().join("media");
        let data = temporary.path().join("data");
        fs::create_dir(&mount).expect("fixture");
        fs::create_dir(&data).expect("fixture");
        let source = mount.join("legacy.mkv");
        fs::write(&source, b"fresh fixture is deferred by minimum age").expect("fixture");
        let tracks =
            json!([{"number":3,"language":"en","format":"SubRip","forced":false,"hi":false}]);
        write_private(&data.join("inventory.json"), &json!({"items":[{"path":source,"name":"legacy.mkv","type":"video","tracks":tracks}],"updated":10.0})).expect("fixture");
        let app =
            App::new(vec![mount], data.clone(), Duration::from_secs(21600), false).expect("app");
        let job = app
            .enqueue_task("discovery", "task_manual", false)
            .expect("queue")
            .expect("job");
        lock(&app.runtime).running = Some(job.clone());
        let mut integrations = Integrations::new(data.join("settings.json")).expect("fixture");
        app.execute(&job, &mut integrations).expect("discovery");
        let inventory = app.inventory();
        assert_eq!(inventory["items"].as_array().unwrap().len(), 1);
        let mut expected_tracks = tracks;
        expected_tracks[0]["id"] = json!(3);
        assert_eq!(inventory["items"][0]["tracks"], expected_tracks);
        assert_eq!(inventory["items"][0]["metadata_pending"], true);
        assert!(inventory["items"][0].get("error").is_none());
        assert_eq!(app.status()["running"]["summary"]["inspection_failed"], 0);
    }

    #[test]
    fn conversion_count_survives_later_ocr_failure_with_preserved_text_outputs() {
        let temporary = tempfile::tempdir().expect("fixture");
        let mount = temporary.path().join("media");
        let data = temporary.path().join("data");
        fs::create_dir(&mount).expect("fixture");
        fs::create_dir(&data).expect("fixture");
        let source = mount.join("processed.mkv").to_string_lossy().into_owned();
        write_private(
            &data.join("state.json"),
            &json!({source.clone():{"ok":true,"outputs":["processed.en.srt"]}}),
        )
        .expect("fixture");
        let app = App::new(
            vec![mount.clone()],
            data.clone(),
            Duration::from_secs(21600),
            false,
        )
        .expect("app");
        assert_eq!(app.status()["converted_count"], 1);
        drop(app);
        write_private(&data.join("state.json"), &json!({source:{"ok":false,"ocr_pending":true,"outputs":["processed.en.srt"]},"completed-text-pass":{"ok":false,"text_complete":true,"ocr_pending":true,"outputs":[]},"failed-without-output":{"ok":false,"outputs":[]},"invalid-output":{"ok":false,"outputs":["../outside.srt"]}})).expect("fixture");
        let restarted =
            App::new(vec![mount], data, Duration::from_secs(21600), false).expect("app");
        assert_eq!(restarted.status()["converted_count"], 2);
    }

    #[test]
    fn pipeline_counts_unique_published_outputs_from_state_and_uses_cached_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let mount = temporary.path().join("media");
        let data = temporary.path().join("data");
        fs::create_dir(&mount).unwrap();
        fs::create_dir(&data).unwrap();
        let first = mount.join("first.mkv");
        let second = mount.join("second.mkv");
        write_private(&data.join("state.json"), &json!({
            first.to_string_lossy():{"ok":true,"outputs":["shared.en.srt","first.fr.srt","../escape.srt","cover.jpg"],"ocr_pending":true},
            second.to_string_lossy():{"ok":false,"error":"Fixture conversion failure","outputs":["shared.en.srt"]},
            mount.join("no-output.mkv").to_string_lossy():{"ok":true,"outputs":[]},
            "untrusted-outside-path":{"ok":true,"outputs":["outside.srt"]}
        })).unwrap();
        write_private(&data.join("inventory.json"), &json!({"updated":10.0,"items":[{"path":first,"type":"video","tracks":[]},{"path":second,"type":"video","tracks":[]}]})).unwrap();
        let app = App::new(vec![mount.clone()], data, Duration::from_secs(21600), false).unwrap();
        let status = app.status();
        assert_eq!(status["pipeline"]["generated_srt_count"], 2);
        assert_eq!(status["pipeline"]["pending_ocr_files"], 1);
        assert_eq!(status["pipeline"]["failed_files"], 1);
        assert_eq!(status["pipeline"]["files_inspected"], 2);
        assert_eq!(
            status["pipeline"]["scope"]["outputs"],
            "tracked_published_outputs"
        );
        assert!(status["pipeline"].get("processed_count").is_none());
        assert_eq!(
            app.inventory()["items"][0]["processing"]["pending_ocr"],
            true
        );
        assert_eq!(app.inventory()["items"][1]["processing"]["failed"], true);
        fs::remove_dir(&mount).unwrap();
        assert_eq!(app.status()["pipeline"]["unreachable_roots"], 0);
        app.root_folders();
        assert_eq!(app.status()["pipeline"]["unreachable_roots"], 1);
        app.refresh_pipeline();
        assert_eq!(app.status()["inventory"]["state_revision"], 1);
        assert_eq!(app.status()["inventory"]["updated"], 10.0);
        assert_eq!(app.inventory()["state_revision"], 1);
    }

    #[test]
    fn failed_paths_are_authorized_unique_and_bounded_for_retry() {
        let temporary = tempfile::tempdir().unwrap();
        let mount = temporary.path().join("media");
        fs::create_dir(&mount).unwrap();
        let app = App::new(
            vec![mount.clone()],
            temporary.path().join("data"),
            Duration::from_secs(21600),
            false,
        )
        .unwrap();
        lock(&app.runtime).running = Some(json!({"root_paths":[mount]}));
        for index in 0..65 {
            let path = mount.join(format!("{index}.mkv"));
            app.progress(json!({"status":"failed","path":path,"error":"Disposable fixture error"}));
            app.progress(json!({"status":"failed","path":path,"error":"Disposable fixture error"}));
        }
        app.progress(json!({"status":"failed","path":temporary.path().join("outside.mkv"),"error":"Fixture"}));
        let running = &app.status()["running"];
        assert_eq!(running["failed_paths"].as_array().unwrap().len(), 64);
        assert_eq!(running["failed_paths_truncated"], true);
        assert!(
            running["failed_paths"]
                .as_array()
                .unwrap()
                .iter()
                .all(|path| Path::new(path.as_str().unwrap()).starts_with(&mount))
        );
        assert_eq!(
            running["file_errors"][0]["examples"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }
}
