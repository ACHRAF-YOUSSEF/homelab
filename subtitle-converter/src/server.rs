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
const INVENTORY_LIMIT: usize = 2000;
const HISTORY_LIMIT: usize = 100;
const JSON_LIMIT: usize = 32 * 1024 * 1024;
const HTTP_WORKERS: usize = 4;

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
    Gpu(&'static str),
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

#[derive(Default)]
struct Runtime {
    roots: Vec<PathBuf>,
    languages: Vec<String>,
    running: Option<Value>,
    queue: VecDeque<Value>,
    history: VecDeque<Value>,
    logs: VecDeque<Value>,
    converted: HashSet<String>,
    inventory: Vec<Value>,
    inventory_updated: f64,
    inventory_truncated: bool,
    inventory_partial: bool,
}
pub struct App {
    runtime: Mutex<Runtime>,
    converter: Mutex<Converter>,
    metadata_usage: Arc<crate::metadata::CacheUsage>,
    resources: Mutex<crate::resources::Sampler>,
    workers: Mutex<Workers>,
    integrations: Mutex<Integrations>,
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
            && job["results"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["ok"] == false)))
    {
        return "failed";
    }
    if count(&job["summary"]["failed"]) > 0
        || job["notifications"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["ok"] == false))
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
        let tracks = row["tracks"].as_array().into_iter().flatten().take(64).map(|track| {
            let mut result = serde_json::Map::new();
            for key in ["language", "format"] {
                result.insert(key.into(), json!(track[key].as_str().unwrap_or("").chars().take(35).collect::<String>()));
            }
            for key in ["forced", "hi"] { result.insert(key.into(), json!(track[key].as_bool().unwrap_or(false))); }
            result.insert("number".into(), json!(track["number"].as_u64().unwrap_or(0)));
            Value::Object(result)
        }).collect::<Vec<_>>();
        let root_id = roots.iter().filter(|root| Path::new(path).starts_with(root)).max_by_key(|root| root.components().count()).map(|root| root_info(root, mounts)["id"].clone()).unwrap_or(Value::Null);
        let mut result = json!({"path":path,"name":name,"root_id":root_id,"type":if row["type"]=="sidecar" {"sidecar"} else {"video"},"tracks":tracks});
        if row["metadata_pending"] == true { result["metadata_pending"] = json!(true); }
        if let Some(error) = row["error"].as_str() { result["error"] = json!(error.chars().take(256).collect::<String>()); }
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
        if let Ok(saved) = read_json(&data.join("inventory.json"), JSON_LIMIT) {
            runtime.inventory = inventory_rows(&saved["items"], &mounts, &runtime.roots);
            runtime.inventory_updated = saved["updated"].as_f64().unwrap_or(0.0);
            runtime.inventory_truncated = saved["truncated"].as_bool().unwrap_or(false);
            runtime.inventory_partial = saved["partial"].as_bool().unwrap_or(false);
        }
        // Inventory is stored once, independently of rotating job history.
        for job in &mut runtime.history {
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
            .unwrap_or_else(|_| "localhost,127.0.0.1,::1,subtitle-converter".into())
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
            integrations: Mutex::new(integrations),
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
        app.log(
            "INFO",
            "Rust dashboard ready; file workers use bounded allocation",
        );
        Ok(app)
    }
    fn log(&self, level: &str, message: &str) {
        let mut runtime = lock(&self.runtime);
        runtime
            .logs
            .push_back(json!({"time":now(),"level":level,"message":message}));
        while runtime.logs.len() > 200 {
            runtime.logs.pop_front();
        }
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
        json!({"version":env!("CARGO_PKG_VERSION"),"edition":crate::edition::NAME,"running":runtime.running,"queue":runtime.queue,"history":history,"logs":runtime.logs,"converted_count":runtime.converted.len(),"inventory":{"count":runtime.inventory.len(),"updated":runtime.inventory_updated,"truncated":runtime.inventory_truncated,"partial":runtime.inventory_partial},"roots":runtime.roots.iter().map(|root|root_info(root,&self.mounts)).collect::<Vec<_>>(),"settings":settings})
    }
    fn tasks(&self) -> Value {
        let mut value = lock(&self.scheduler).public();
        let runtime = lock(&self.runtime);
        for task in value["tasks"].as_array_mut().into_iter().flatten() {
            let active = runtime
                .running
                .iter()
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
                "kind":runtime.running.as_ref().map(|job| job["kind"].clone())})
        };
        usage["worker"] = worker;
        json!({"version":env!("CARGO_PKG_VERSION"),"edition":crate::edition::NAME,"runtime":"rust","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"uptime_seconds":self.started.elapsed().as_secs(),"resource_usage":usage,"resource_policy":{"conversion_workers":limits.cpu_workers,"ocr_threads":1,"ocr_workers":limits.ocr_workers,"gpu_workers":limits.gpu_workers,"metadata_cache_max_bytes":crate::metadata::LIMIT_BYTES,"metadata_cache_max_entries":crate::metadata::MAX_ENTRIES,"discovery_interval_seconds":interval,"ocr_permission":self.ocr_permission,"automatic_ocr_new_or_changed_only":true},"scheduler":{"paused":schedule["paused"]}})
    }

    fn workers(&self) -> Value {
        let sample = lock(&self.resources).snapshot();
        lock(&self.workers).public(&sample)
    }

    fn update_workers(&self, data: &Value) -> Result<Value, String> {
        lock(&self.workers).update(data)?;
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
        json!({"items":runtime.inventory,"updated":runtime.inventory_updated,"truncated":runtime.inventory_truncated,"partial":runtime.inventory_partial})
    }
    fn widget(&self) -> Value {
        let runtime = lock(&self.runtime);
        let current=runtime.running.as_ref().map(|job|{let total=count(&job["total"]);let processed=count(&job["processed"]);json!({"kind":job["kind"],"status":job["status"],"processed":processed,"total":total,"progress_percent":if total>0 {Some((processed.saturating_mul(100)/total).min(100))}else{None}})});
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
        let runtime = lock(&self.runtime);
        json!({"roots":runtime.roots.iter().map(|root|root_info(root,&self.mounts)).collect::<Vec<_>>(),"mounts":self.mounts_value()})
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
                .chain(runtime.queue.iter())
                .any(|job| job["task_id"] == task_id)
        {
            return Err("Task is already queued or running".into());
        }
        if origin == "schedule" && (runtime.running.is_some() || !runtime.queue.is_empty()) {
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
        if kind == "scan" && data["include_ocr"] == true {
            return Err("Use selected-file extraction or the OCR task to run OCR".into());
        }
        let include_ocr = kind == "ocr"
            || (kind == "extract"
                && origin == "manual"
                && data["include_ocr"].as_bool().unwrap_or(true));
        let job = json!({"id":token(8),"task_id":task_id,"workload":if kind=="ocr" {"ocr"}else if include_ocr {"all"}else{"text"},"include_ocr":include_ocr,"kind":kind,"status":"queued","created":now(),"started":null,"finished":null,"current":null,"summary":{},"error":null,"results":[],"languages":languages,"root_ids":roots.iter().map(|root|root_info(root,&self.mounts)["id"].clone()).collect::<Vec<_>>(),"root_paths":roots,"paths":paths,"retry":data["retry"].as_bool().unwrap_or(false),"origin":origin,"provider":provider,"processed":0,"total":0});
        runtime.queue.push_back(job.clone());
        self.wake.notify_one();
        Ok(Some(job))
    }
    fn cancel(&self, id: &str) -> Result<Value, String> {
        let mut runtime = lock(&self.runtime);
        if runtime.running.as_ref().is_some_and(|job| job["id"] == id) {
            self.cancel.store(true, Ordering::Relaxed);
            return Ok(json!({"status":"cancelling"}));
        }
        if let Some(index) = runtime.queue.iter().position(|job| job["id"] == id)
            && let Some(mut job) = runtime.queue.remove(index)
        {
            job["status"] = json!("cancelled");
            job["finished"] = json!(now());
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
        if event["status"] == "converted"
            && let Some(path) = event["path"].as_str()
        {
            runtime.converted.insert(path.into());
        }
        let Some(job) = runtime.running.as_mut() else {
            return;
        };
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
        {
            job["processed"] = json!(count(&job["processed"]) + 1);
        }
        if event["status"] == "failed" {
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
        if let Some(job) = runtime.running.as_mut() {
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
        let retry = job["retry"].as_bool().unwrap_or(false);
        let automatic = job["origin"] == "schedule"
            || ["sonarr", "radarr"].contains(&job["origin"].as_str().unwrap_or(""));
        let ocr = matches!(mode, FileMode::Convert(workload) if workload != Workload::Text)
            && self.ocr_permission;
        let mut seen = HashSet::new();
        let mut pending: VecDeque<_> = paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| seen.insert(path).then_some(index))
            .collect();
        if let Some(running) = lock(&self.runtime).running.as_mut() {
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
        std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::sync_channel::<FileEvent>(16);
            let mut handles = BTreeMap::new();
            while !pending.is_empty() || !handles.is_empty() {
                if self.cancel.load(Ordering::Relaxed) || self.shutdown.load(Ordering::Relaxed) {
                    self.cancel.store(true, Ordering::Relaxed);
                    pending.clear();
                }
                loop {
                    if self.cancel.load(Ordering::Relaxed) || self.shutdown.load(Ordering::Relaxed)
                    {
                        self.cancel.store(true, Ordering::Relaxed);
                        pending.clear();
                        break;
                    }
                    if pending.is_empty() || failure.is_some() {
                        break;
                    }
                    let sample = lock(&self.resources).snapshot();
                    let admission = {
                        let mut workers = lock(&self.workers);
                        let resource = if ocr && workers.can_start_resource(&sample, "gpu") {
                            Some("gpu")
                        } else if workers
                            .can_start_resource(&sample, if ocr { "cpu_ocr" } else { "cpu" })
                        {
                            Some(if ocr { "cpu_ocr" } else { "cpu" })
                        } else {
                            None
                        };
                        if let Some(resource) = resource {
                            let next = pending.iter().position(|index| {
                                paths.get(*index).is_some_and(|path| {
                                    workers.active.iter().all(|active| {
                                        matches!(mode, FileMode::Inspect { .. })
                                            || !crate::workers::conflicts(&active.path, path)
                                    })
                                })
                            });
                            next.and_then(|next| pending.remove(next))
                                .and_then(|index| paths.get(index))
                                .and_then(|path| {
                                    let id = (0..crate::workers::HARD_TOTAL_LIMIT)
                                        .find(|id| !handles.contains_key(id))?;
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
                            self.progress(json!({"status":"failed","path":path,"error":error,"summary":summary}));
                            continue;
                        }
                    };
                    if self.cancel.load(Ordering::Relaxed) || self.shutdown.load(Ordering::Relaxed)
                    {
                        self.cancel.store(true, Ordering::Relaxed);
                        pending.clear();
                        lock(&self.workers).active.retain(|worker| worker.id != id);
                        break;
                    }
                    if resource == "gpu" {
                        worker.enable_gpu_ocr(lock(&self.workers).gpu_min_free_mib());
                    }
                    let worker_path = path.clone();
                    let worker_sender = sender.clone();
                    let worker_languages = &languages;
                    let handle = std::thread::Builder::new()
                        .name(format!("subforge-file-{id}"))
                        .stack_size(2 * 1024 * 1024)
                        .spawn_scoped(scope, move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    if let FileMode::Inspect { cached_only } = mode {
                                        let _ = worker_sender.send(FileEvent::Progress { id, event: json!({"status":"inspecting","path":worker_path}) });
                                        let description = if cached_only { Ok(worker.cached_description(&worker_path)) }
                                            else { worker.describe_with_stop(&worker_path, &self.cancel).map(Some) };
                                        let (summary, terminal) = match &description {
                                            Ok(Some(_)) => (json!({"inspected":1,"failed":0}), json!({"status":"inspected","path":worker_path})),
                                            Ok(None) => (json!({"metadata_pending":1}), json!({"status":"metadata_pending","path":worker_path})),
                                            Err(error) => (json!({"inspected":0,"failed":1}), json!({"status":"failed","path":worker_path,"error":error})),
                                        };
                                        return Ok(FileOutcome { state: None, summary, terminal: Some(terminal), gpu_health: None, description: Some(description) });
                                    }
                                    let FileMode::Convert(workload) = mode else { return Err(FileFailure::Conversion("Unsupported file worker operation".into())); };
                                    let mut terminal = None;
                                    let summary = worker.scan_workload(
                                        std::slice::from_ref(&worker_path),
                                        worker_languages,
                                        retry,
                                        workload,
                                        automatic,
                                        &self.cancel,
                                        &mut |event| {
                                            if ["converted", "cached", "failed", "pending"]
                                                .contains(&event["status"].as_str().unwrap_or(""))
                                            {
                                                terminal = Some(event);
                                            } else {
                                                let _ = worker_sender
                                                    .send(FileEvent::Progress { id, event });
                                            }
                                        },
                                    )?;
                                    let (gpu_health, gpu_failure) = worker.gpu_status();
                                    if let Some(error) = gpu_failure { return Err(FileFailure::Gpu(error)); }
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
                            failure = Some("File worker capacity is unavailable".into());
                            self.cancel.store(true, Ordering::Relaxed);
                            pending.clear();
                            break;
                        }
                    }
                }
                if pending.is_empty() && handles.is_empty() {
                    break;
                }
                match receiver.recv_timeout(Duration::from_millis(100)) {
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
                                _ => "processing",
                            };
                        }
                        if let Some(object) = event.as_object_mut() {
                            object.remove("summary");
                        }
                        self.progress(event);
                    }
                    Ok(FileEvent::Finished { id, path, outcome }) => {
                        if let Some(handle) = handles.remove(&id) {
                            let _ = handle.join();
                        }
                        lock(&self.workers).active.retain(|worker| worker.id != id);
                        match outcome {
                            Ok(outcome) => {
                                if let Some(description) = outcome.description {
                                    descriptions.insert(path.clone(), description);
                                }
                                if let Some(health) = outcome.gpu_health {
                                    lock(&self.workers).gpu.verified = Some(health);
                                }
                                if let Err(error) =
                                    lock(&self.converter).merge_file_state(&path, outcome.state)
                                {
                                    failure = Some(error);
                                    self.cancel.store(true, Ordering::Relaxed);
                                    pending.clear();
                                }
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
                                    let outputs = strings(&event["new_outputs"]);
                                    if !matches!(mode, FileMode::Inspect { cached_only: true }) {
                                        event["summary"] = summary.clone();
                                    }
                                    self.progress(event);
                                    if failure.is_none() && !outputs.is_empty() {
                                        self.notifications(integrations.notify(&outputs));
                                    }
                                }
                            }
                            Err(error) => {
                                let error = match error {
                                    FileFailure::Conversion(error) => error,
                                    FileFailure::Gpu(error) => {
                                        lock(&self.workers).gpu.failed();
                                        failure = Some(error.into());
                                        self.cancel.store(true, Ordering::Relaxed);
                                        pending.clear();
                                        error.into()
                                    }
                                };
                                summary["failed"] = json!(count(&summary["failed"]) + 1);
                                self.progress(json!({"status":"failed","path":path,"error":error,"summary":summary}));
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        failure = Some("File worker channel is unavailable".into());
                        self.cancel.store(true, Ordering::Relaxed);
                        pending.clear();
                    }
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
                "backup" => crate::maintenance::backup(&self.data, &self.cancel)?,
                _ => {
                    let unavailable = self
                        .mounts
                        .iter()
                        .filter(|path| !path.is_dir() || symlink_component(path))
                        .count();
                    json!({"configured_mounts":self.mounts.len(),"unavailable_mounts":unavailable,"state_directory_available":self.data.is_dir() && !symlink_component(&self.data),"network_checks":0})
                }
            };
            if let Some(running) = lock(&self.runtime).running.as_mut() {
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
                &self.cancel,
                &mut |event| self.progress(event),
            );
            if let Some(running) = lock(&self.runtime).running.as_mut() {
                running["results"] = json!([result]);
            }
            return Ok(());
        }
        let roots = strings(&job["root_paths"])
            .iter()
            .map(|path| validate_root(Path::new(path), &self.mounts, false))
            .collect::<Result<Vec<_>, _>>()?;
        let paths = if job["kind"] == "ocr" {
            lock(&self.converter).ocr_candidates(&roots, job["retry"].as_bool().unwrap_or(false))
        } else if strings(&job["paths"]).is_empty() {
            Converter::candidates_with_stop(&roots, &self.cancel)
        } else {
            strings(&job["paths"])
                .iter()
                .map(|path| Converter::safe_path(path, &roots))
                .collect::<Result<Vec<_>, _>>()?
        };
        if let Some(running) = lock(&self.runtime).running.as_mut() {
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
            if let Some(running) = lock(&self.runtime).running.as_mut() {
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
                if self.cancel.load(Ordering::Relaxed) {
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
                if let Some(running) = lock(&self.runtime).running.as_mut() {
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
            let cancelled = self.cancel.load(Ordering::Relaxed);
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
            }
        }
        lock(&self.converter).persist_metadata()?;
        Ok(())
    }
    fn worker(self: Arc<Self>) {
        while !self.shutdown.load(Ordering::Relaxed) {
            let due = lock(&self.scheduler).due(now());
            for id in due {
                let _ = self.enqueue_task(&id, "schedule", false);
            }
            let job = {
                let mut runtime = lock(&self.runtime);
                let paused = lock(&self.workers).allocation.paused;
                match if paused {
                    None
                } else {
                    runtime.queue.pop_front()
                } {
                    Some(mut job) => {
                        self.cancel.store(false, Ordering::Relaxed);
                        job["status"] = json!("running");
                        job["started"] = json!(now());
                        runtime.running = Some(job.clone());
                        Some(job)
                    }
                    None => {
                        let _guard = self.wake.wait_timeout(runtime, Duration::from_secs(1));
                        None
                    }
                }
            };
            let Some(job) = job else { continue };
            let task_start = job["task_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(|id| lock(&self.scheduler).start(id, now()))
                .unwrap_or(Ok(()));
            let mut integrations = lock(&self.integrations).clone();
            let result = task_start.and_then(|_| self.execute(&job, &mut integrations));
            if let Err(message) = result {
                if let Some(running) = lock(&self.runtime).running.as_mut() {
                    running["error"] = json!(message);
                }
                self.log("WARNING", "Dashboard job failed; see its safe diagnostic");
            }
            if ["scan", "extract", "ocr"].contains(&job["kind"].as_str().unwrap_or(""))
                && let Some(notification) =
                    integrations.flush_bazarr(&self.cancel, &mut |event| self.progress(event))
            {
                self.notifications(vec![notification]);
            }
            let mut runtime = lock(&self.runtime);
            if let Some(mut finished) = runtime.running.take() {
                finished["status"] = json!(if self.cancel.load(Ordering::Relaxed) {
                    "cancelled"
                } else {
                    completed_status(&finished)
                });
                finished["finished"] = json!(now());
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
            "/api/inventory" => Reply::json(200, app.inventory()),
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
    match result {
        Ok((status, value)) => Reply::json(status, value),
        Err(error) => Reply::error(400, &error),
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
        assert!(lock(&app.workers).active.is_empty());
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
        let value = json!([{"path":temporary.path().join("outside.mkv"),"tracks":[]},{"path":mount.join("inside.mkv"),"tracks":vec![json!({"language":"x".repeat(4096),"format":"x".repeat(4096) });100]}]);
        let rows = inventory_rows(
            &value,
            std::slice::from_ref(&mount),
            std::slice::from_ref(&mount),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["tracks"].as_array().unwrap().len(), 64);
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
        assert_eq!(inventory["items"][0]["tracks"], tracks);
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
}
