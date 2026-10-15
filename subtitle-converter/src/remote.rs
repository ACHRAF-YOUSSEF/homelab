//! Opt-in shared-filesystem workers. Workers can stage results, but only the
//! coordinator publishes them while their lease and source fingerprint match.
//! A worker token grants conversion access to configured shared roots; keep it
//! separate from dashboard credentials and use TLS across untrusted networks.

use crate::converter::{Converter, SubtitleSourceHint, SubtitleTarget, Workload};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tempfile::TempDir;

pub const MAX_LEASES: usize = 64;
const MAX_NODES: usize = 64;
const LEASE_TIME: Duration = Duration::from_secs(90);
const REQUEST_LIMIT: usize = 65536;
const STATE_LIMIT: usize = 40000;
const TOTAL_OUTPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OUTPUTS: usize = 64;

#[derive(Debug)]
pub struct RemoteError(&'static str);
impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for RemoteError {}

struct Node {
    name: String,
    resource: String,
    roots: Vec<String>,
    seen: Instant,
    ready: bool,
    ticket: Option<String>,
}

struct Lease {
    node: String,
    path: PathBuf,
    source: Vec<(u64, SystemTime)>,
    staging: TempDir,
    request: Value,
    result: Option<Completion>,
    expires: Instant,
    #[cfg(test)]
    validation_gate: Option<Arc<ValidationGate>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    name: String,
    resource: String,
    roots: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    node: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Heartbeat {
    node: String,
    lease: Option<String>,
    #[serde(default)]
    ready: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    node: String,
    lease: String,
    state: Option<Value>,
    summary: Value,
    terminal: Option<Value>,
    outputs: Vec<String>,
    fingerprint: Value,
}

#[cfg(test)]
#[derive(Default)]
struct ValidationGate {
    entered: AtomicBool,
    released: AtomicBool,
}

pub struct RemoteOutcome {
    pub state: Option<Value>,
    pub summary: Value,
    pub terminal: Option<Value>,
}
pub enum RemotePoll {
    Pending,
    Retry,
    Finished(RemoteOutcome),
}

pub struct Coordinator {
    token: String,
    mounts: Vec<PathBuf>,
    nodes: BTreeMap<String, Node>,
    leases: BTreeMap<String, Lease>,
}

impl Coordinator {
    pub fn new(token: String, mounts: Vec<PathBuf>) -> Result<Self, String> {
        valid_token(&token, true).map_err(|error| error.to_string())?;
        Ok(Self {
            token,
            mounts,
            nodes: BTreeMap::new(),
            leases: BTreeMap::new(),
        })
    }
    pub fn enabled(&self) -> bool {
        !self.token.is_empty()
    }
    pub fn authenticated(&self, token: &str) -> bool {
        self.enabled()
            && token.len() <= 256
            && crate::util::constant_equal(token.as_bytes(), self.token.as_bytes())
    }
    fn prune(&mut self) {
        self.nodes
            .retain(|_, node| node.seen.elapsed() < LEASE_TIME || node.ticket.is_some());
    }
    pub fn public(&self) -> Value {
        let nodes: Vec<Value> = self.nodes.iter().map(|(id, node)| json!({
            "id":id,"name":node.name,"resource":node.resource,
            "last_seen_seconds":node.seen.elapsed().as_secs(),"active":node.ticket.is_some(),
            "status":if node.seen.elapsed() >= LEASE_TIME {"offline"} else if node.ticket.is_some() {"working"} else if !node.ready {"limited"} else {"idle"}
        })).collect();
        json!({"enabled":self.enabled(),"nodes":nodes,"active":self.leases.len(),"max_nodes":MAX_NODES,"lease_seconds":LEASE_TIME.as_secs()})
    }
    /// Coordinator-owned attribution for a dispatched attempt. Capture this
    /// with the offer lock held; taking or cancelling the lease removes it.
    /// The resource is the requested slot, which may be CPU text on an
    /// OCR-capable node. Worker terminal messages cannot supply this identity.
    pub fn executor(&self, ticket: &str) -> Option<Value> {
        let lease = self.leases.get(ticket)?;
        let node = self.nodes.get(&lease.node)?;
        let resource = match lease.request.get("resource")?.as_str()? {
            "cpu" => "cpu",
            "cpu_ocr" => "cpu_ocr",
            "gpu" => "gpu",
            _ => return None,
        };
        Some(
            json!({"node_id":lease.node,"node_name":node.name,"resource":resource,"execution":"remote"}),
        )
    }
    pub fn has_capacity(&self, resource: &str) -> bool {
        self.enabled()
            && self.leases.len() < MAX_LEASES
            && self.nodes.values().any(|node| {
                node.seen.elapsed() < LEASE_TIME
                    && node.ready
                    && node.ticket.is_none()
                    && accepts(&node.resource, resource)
            })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn offer(
        &mut self,
        path: &Path,
        languages: &[String],
        retry: bool,
        automatic: bool,
        workload: Workload,
        selected: Option<&[u64]>,
        targets: Option<&[SubtitleTarget]>,
        sources: &[SubtitleSourceHint],
        force: bool,
        resource: &str,
        state: Option<Value>,
    ) -> Result<Option<String>, String> {
        self.prune();
        if !self.has_capacity(resource) {
            return Ok(None);
        }
        let path = Converter::safe_path(&path.to_string_lossy(), &self.mounts)?;
        let root = self
            .mounts
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .ok_or("Remote file is outside shared mounts")?;
        let root_name = root.to_string_lossy().into_owned();
        let Some(node_id) = self
            .nodes
            .iter()
            .find(|(_, node)| {
                node.seen.elapsed() < LEASE_TIME
                    && node.ready
                    && node.ticket.is_none()
                    && accepts(&node.resource, resource)
                    && node.roots.contains(&root_name)
            })
            .map(|(id, _)| id.clone())
        else {
            return Ok(None);
        };
        if languages.len() > 64
            || selected.is_some_and(|ids| ids.len() > 64)
            || targets.is_some_and(|targets| targets.len() > 64)
            || sources.len() > 256
            || force && selected.is_none_or(|ids| ids.is_empty())
        {
            return Err("Remote selection exceeds its limit".into());
        }
        let state = match state
            .as_ref()
            .map(|state| crate::util::bounded_json_clone(state, STATE_LIMIT))
            .transpose()
        {
            Ok(state) => state,
            // Preserve larger local state and use ordinary local dispatch.
            Err(_) => return Ok(None),
        };
        let staging = tempfile::Builder::new()
            .prefix(".subforge-remote-")
            .tempdir_in(path.parent().ok_or("Missing media parent")?)
            .map_err(|_| "Cannot create shared remote staging directory")?;
        let ticket = crate::util::token(24);
        let source = source_stamp(&path).map_err(|error| error.to_string())?;
        // A CPU text admission is an explicit per-file denial of OCR, even
        // when the remote process itself has OCR permission for other slots.
        let workload = if resource == "cpu" {
            Workload::Text
        } else {
            workload
        };
        let sources = sources
            .iter()
            .map(|hint| {
                let source = Converter::safe_path(&hint.path.to_string_lossy(), &self.mounts)?;
                if source.parent() != path.parent() {
                    return Err("Remote subtitle hint must share the media directory".to_owned());
                }
                Ok(json!({"path":relative_name(root, &source)?,"target":hint.target}))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let request = json!({"lease":ticket,"root":root_name,"path":relative_name(root, &path)?,
            "staging":relative_name(root, staging.path())?,"languages":languages,"retry":retry,"automatic":automatic,
            "workload":match workload {Workload::All=>"all",Workload::Text=>"text",Workload::Ocr=>"ocr"},
            "selected":selected,"targets":targets,"sources":sources,"force":force,"resource":resource,"state":state});
        crate::util::bounded_json_clone(&request, REQUEST_LIMIT)?;
        self.leases.insert(
            ticket.clone(),
            Lease {
                node: node_id.clone(),
                path,
                source,
                staging,
                request,
                result: None,
                expires: Instant::now() + LEASE_TIME,
                #[cfg(test)]
                validation_gate: None,
            },
        );
        if let Some(node) = self.nodes.get_mut(&node_id) {
            node.ticket = Some(ticket.clone());
        }
        Ok(Some(ticket))
    }
    pub fn cancel(&mut self, ticket: &str) {
        if let Some(lease) = self.leases.remove(ticket)
            && let Some(node) = self.nodes.get_mut(&lease.node)
        {
            node.ticket = None;
        }
    }
    pub fn take(&mut self, ticket: &str, cancelled: &AtomicBool) -> RemotePoll {
        let Some(lease) = self.leases.get(ticket) else {
            return RemotePoll::Retry;
        };
        if cancelled.load(Ordering::Relaxed) || Instant::now() >= lease.expires {
            self.cancel(ticket);
            return RemotePoll::Retry;
        }
        if lease.result.is_none() {
            return RemotePoll::Pending;
        }
        let Some(mut lease) = self.leases.remove(ticket) else {
            return RemotePoll::Retry;
        };
        if let Some(node) = self.nodes.get_mut(&lease.node) {
            node.ticket = None;
        }
        let Some(result) = lease.result.take() else {
            return RemotePoll::Retry;
        };
        match publish(&lease, result, cancelled) {
            Ok(outcome) => RemotePoll::Finished(outcome),
            Err(_) => RemotePoll::Retry,
        }
    }
    /// Called only after the HTTP layer has checked method, Host, Origin and
    /// the dedicated bearer token. JSON is bounded again for in-process users.
    pub fn handle(&mut self, route: &str, data: &Value) -> Result<Value, String> {
        if !self.enabled() {
            return Err("Remote workers are disabled".into());
        }
        crate::util::bounded_json_clone(data, REQUEST_LIMIT)?;
        self.prune();
        match route {
            "/api/remote/register" => {
                let registration: Registration = decode(data)?;
                if registration.name.is_empty()
                    || registration.name.len() > 64
                    || registration.name.chars().any(char::is_control)
                    || !["cpu", "cpu_ocr", "gpu"].contains(&registration.resource.as_str())
                    || registration.roots.is_empty()
                    || registration.roots.len() > 32
                    || registration.roots.iter().any(|root| {
                        root.len() > 4096
                            || !self
                                .mounts
                                .iter()
                                .any(|mount| mount.to_string_lossy() == root.as_str())
                    })
                {
                    return Err("Invalid remote worker registration or shared roots".into());
                }
                if self.nodes.len() >= MAX_NODES {
                    return Err("Remote node limit reached".into());
                }
                let id = crate::util::token(24);
                self.nodes.insert(
                    id.clone(),
                    Node {
                        name: registration.name,
                        resource: registration.resource,
                        roots: registration.roots,
                        seen: Instant::now(),
                        ready: false,
                        ticket: None,
                    },
                );
                Ok(json!({"node":id,"lease_seconds":LEASE_TIME.as_secs()}))
            }
            "/api/remote/heartbeat" => {
                let heartbeat: Heartbeat = decode(data)?;
                let node = self
                    .nodes
                    .get_mut(&heartbeat.node)
                    .ok_or("Remote node must register")?;
                node.seen = Instant::now();
                node.ready = heartbeat.ready;
                if let Some(ticket) = heartbeat.lease {
                    let lease = self
                        .leases
                        .get_mut(&ticket)
                        .filter(|lease| {
                            lease.node == heartbeat.node && Instant::now() < lease.expires
                        })
                        .ok_or("Remote lease is no longer active")?;
                    lease.expires = Instant::now() + LEASE_TIME;
                }
                Ok(json!({"ok":true}))
            }
            "/api/remote/lease" => {
                let identity: Identity = decode(data)?;
                let node = self
                    .nodes
                    .get_mut(&identity.node)
                    .ok_or("Remote node must register")?;
                node.seen = Instant::now();
                node.ready = true;
                Ok(node
                    .ticket
                    .as_ref()
                    .and_then(|ticket| self.leases.get(ticket))
                    .filter(|lease| Instant::now() < lease.expires && lease.result.is_none())
                    .map(|lease| lease.request.clone())
                    .unwrap_or(Value::Null))
            }
            "/api/remote/complete" => {
                let completion: Completion = decode(data)?;
                let lease = self
                    .leases
                    .get_mut(&completion.lease)
                    .filter(|lease| lease.node == completion.node && Instant::now() < lease.expires)
                    .ok_or("Remote lease is no longer active")?;
                if lease.result.is_some() {
                    return Err("Remote lease was already completed".into());
                }
                if completion.outputs.len() > MAX_OUTPUTS
                    || completion
                        .outputs
                        .iter()
                        .any(|name| !output_name(&lease.path, name))
                    || completion.state.as_ref().is_some_and(|state| {
                        crate::util::bounded_json_clone(state, STATE_LIMIT).is_err()
                    })
                {
                    return Err("Remote outcome exceeds its limits".into());
                }
                lease.result = Some(completion);
                Ok(json!({"ok":true}))
            }
            _ => Err("Unsupported remote operation".into()),
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(data: &Value) -> Result<T, String> {
    serde_json::from_value(data.clone()).map_err(|_| "Invalid remote request".into())
}
fn valid_token(token: &str, allow_empty: bool) -> Result<(), RemoteError> {
    if (allow_empty && token.is_empty())
        || ((32..=256).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_graphic()))
    {
        Ok(())
    } else {
        Err(RemoteError(
            "Worker token must contain 32–256 printable ASCII characters",
        ))
    }
}
fn accepts(node: &str, resource: &str) -> bool {
    node == resource || (node == "cpu_ocr" && resource == "cpu")
}
fn source_stamp(path: &Path) -> Result<Vec<(u64, SystemTime)>, RemoteError> {
    let mut paths = vec![path.to_path_buf()];
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str())
        && ["sub", "idx"]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    {
        let companion = path.with_extension(if extension.eq_ignore_ascii_case("sub") {
            "idx"
        } else {
            "sub"
        });
        if companion.exists() {
            paths.push(companion);
        }
    }
    paths
        .iter()
        .map(|path| {
            no_symlinks(path)?;
            let info = fs::symlink_metadata(path)
                .map_err(|_| RemoteError("Remote source is unavailable"))?;
            if !info.is_file() || info.file_type().is_symlink() {
                return Err(RemoteError("Unsafe remote source"));
            }
            Ok((
                info.len(),
                info.modified()
                    .map_err(|_| RemoteError("Remote source timestamp is unavailable"))?,
            ))
        })
        .collect()
}
fn no_symlinks(path: &Path) -> Result<(), RemoteError> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let info = fs::symlink_metadata(ancestor)
            .map_err(|_| RemoteError("Shared path is unavailable"))?;
        if info.file_type().is_symlink() {
            return Err(RemoteError("Shared paths must not contain symlinks"));
        }
    }
    Ok(())
}
fn relative_name(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "Remote path is outside shared mount")?;
    relative
        .components()
        .map(|part| match part {
            Component::Normal(name) => name
                .to_str()
                .map(str::to_owned)
                .ok_or("Remote paths require UTF-8"),
            _ => Err("Invalid remote path"),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
        .map_err(str::to_owned)
}
fn output_name(path: &Path, name: &str) -> bool {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    name.len() <= 255
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
        && name.ends_with(".srt")
        && name
            .strip_prefix(stem)
            .is_some_and(|suffix| suffix.starts_with('.'))
}
fn fingerprint_value(source: &[(u64, SystemTime)]) -> Value {
    json!(
        source
            .iter()
            .map(|(length, time)| json!([
                length,
                time.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ]))
            .collect::<Vec<_>>()
    )
}
fn valid_commit(lease: &Lease, cancelled: &AtomicBool) -> Result<(), RemoteError> {
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= lease.expires {
        return Err(RemoteError(
            "Remote lease expired or was cancelled before publication",
        ));
    }
    if source_stamp(&lease.path)? != lease.source {
        return Err(RemoteError("Remote source changed"));
    }
    Ok(())
}
fn publish(
    lease: &Lease,
    mut result: Completion,
    cancelled: &AtomicBool,
) -> Result<RemoteOutcome, RemoteError> {
    valid_commit(lease, cancelled)?;
    if result.outputs.len() > MAX_OUTPUTS {
        return Err(RemoteError("Too many remote outputs"));
    }
    no_symlinks(lease.staging.path())?;
    let expected = fingerprint_value(&lease.source);
    if result.fingerprint != expected {
        return Err(RemoteError(
            "Remote mapped source does not match coordinator source",
        ));
    }
    if let Some(state) = &result.state {
        if state
            .get("processing_fingerprint")
            .or_else(|| state.get("fingerprint"))
            != Some(&expected)
        {
            return Err(RemoteError(
                "Remote mapped source does not match coordinator source",
            ));
        }
        validate_state_outputs(&lease.path, state)?;
    }
    let parent = lease
        .path
        .parent()
        .ok_or(RemoteError("Missing remote media parent"))?;
    let mut summary = json!({});
    for key in ["converted", "cached", "pending", "failed", "planned"] {
        let count = result.summary[key]
            .as_u64()
            .filter(|count| *count <= 1)
            .ok_or(RemoteError("Invalid remote summary"))?;
        summary[key] = json!(count);
    }
    result.summary = summary;
    if let Some(terminal) = result.terminal.as_mut() {
        let status = terminal["status"]
            .as_str()
            .filter(|status| {
                ["converted", "cached", "failed", "pending", "skipped"].contains(status)
            })
            .ok_or(RemoteError("Invalid remote completion status"))?;
        let error = terminal["error"].as_str().map(|error| {
            error
                .chars()
                .filter(|character| !character.is_control())
                .take(512)
                .collect::<String>()
        });
        *terminal = json!({"status":status,"path":lease.path,"error":error});
    }
    let mut published = Vec::new();
    let mut total_bytes = 0_u64;
    let mut snapshots = Vec::with_capacity(result.outputs.len());
    // Validate all staged files before exposing any result. No submitted path
    // can escape the coordinator-created, lease-owned staging directory.
    for name in &result.outputs {
        if !output_name(&lease.path, name) {
            return Err(RemoteError("Invalid remote output name"));
        }
        let file = lease.staging.path().join(name);
        let info =
            fs::symlink_metadata(&file).map_err(|_| RemoteError("Remote output is unavailable"))?;
        if !info.is_file() || info.file_type().is_symlink() || info.len() > 64 * 1024 * 1024 {
            return Err(RemoteError("Unsafe remote output"));
        }
        // The remote process owns the staging inode. Never publish that inode:
        // it may still have it open, replace it or append during validation.
        // Snapshot through a bounded reader into a private coordinator file.
        let source =
            fs::File::open(&file).map_err(|_| RemoteError("Cannot open staged remote output"))?;
        let opened = source
            .metadata()
            .map_err(|_| RemoteError("Cannot inspect opened remote output"))?;
        no_symlinks(&file)?;
        if !opened.is_file()
            || opened.len() != info.len()
            || opened.modified().ok() != info.modified().ok()
        {
            return Err(RemoteError("Remote output changed before snapshot"));
        }
        let mut snapshot = tempfile::Builder::new()
            .prefix(".subforge-publish-")
            .tempfile_in(parent)
            .map_err(|_| RemoteError("Cannot create private coordinator publication snapshot"))?;
        let copied = std::io::copy(
            &mut source.take(64 * 1024 * 1024 + 1),
            snapshot.as_file_mut(),
        )
        .map_err(|_| RemoteError("Cannot snapshot remote output"))?;
        if copied > 64 * 1024 * 1024 {
            return Err(RemoteError("Remote output grew beyond its size limit"));
        }
        total_bytes = total_bytes
            .checked_add(copied)
            .filter(|total| *total <= TOTAL_OUTPUT_BYTES)
            .ok_or(RemoteError(
                "Remote staged outputs exceed total byte budget",
            ))?;
        crate::converter::validate_srt(snapshot.path())
            .map_err(|_| RemoteError("Invalid remote subtitle output"))?;
        snapshots.push((name, snapshot));
    }
    #[cfg(test)]
    if let Some(gate) = &lease.validation_gate {
        gate.entered.store(true, Ordering::SeqCst);
        while !gate.released.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    valid_commit(lease, cancelled)?;
    for (name, snapshot) in snapshots {
        valid_commit(lease, cancelled)?;
        match snapshot.persist_noclobber(parent.join(name)) {
            Ok(_) => published.push(parent.join(name).to_string_lossy().into_owned()),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(RemoteError(
                    "Could not publish remote output without replacement",
                ));
            }
        }
    }
    if let Some(terminal) = result.terminal.as_mut() {
        terminal["new_outputs"] = json!(published);
        terminal["outputs"] = json!(published);
    }
    Ok(RemoteOutcome {
        state: result.state,
        summary: result.summary,
        terminal: result.terminal,
    })
}
fn validate_state_outputs(path: &Path, state: &Value) -> Result<(), RemoteError> {
    if let Some(outputs) = state.get("outputs") {
        let outputs = outputs
            .as_array()
            .ok_or(RemoteError("Invalid remote state outputs"))?;
        if outputs.len() > 256
            || outputs
                .iter()
                .any(|output| !output.as_str().is_some_and(|name| output_name(path, name)))
        {
            return Err(RemoteError("Invalid remote state output name"));
        }
    }
    if let Some(selections) = state.get("track_selections") {
        let selections = selections
            .as_object()
            .ok_or(RemoteError("Invalid remote track selections"))?;
        if selections.len() > 64 {
            return Err(RemoteError("Too many remote track selections"));
        }
        for selection in selections.values() {
            if selection.get("track_selections").is_some() {
                return Err(RemoteError(
                    "Nested remote track selections are unsupported",
                ));
            }
            validate_state_outputs(path, selection)?;
        }
    }
    Ok(())
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    server: String,
    local: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Assignment {
    lease: String,
    root: String,
    path: String,
    staging: String,
    languages: Vec<String>,
    retry: bool,
    automatic: bool,
    workload: String,
    selected: Option<Vec<u64>>,
    targets: Option<Vec<SubtitleTarget>>,
    #[serde(default)]
    sources: Vec<SubtitleSourceHint>,
    #[serde(default)]
    force: bool,
    resource: String,
    state: Option<Value>,
}

#[derive(Clone)]
struct Client {
    agent: ureq::Agent,
    endpoint: String,
    token: String,
}
impl Client {
    fn post(&self, route: &str, data: &Value) -> Result<Value, RemoteError> {
        let body = crate::util::bounded_json_clone(data, REQUEST_LIMIT)
            .map_err(|_| RemoteError("Remote request exceeds limit"))?;
        let mut response = self
            .agent
            .post(format!("{}{route}", self.endpoint))
            .header("Authorization", format!("Bearer {}", self.token))
            .send_json(body)
            .map_err(|_| RemoteError("Remote coordinator request failed"))?;
        let mut bytes = Vec::with_capacity(4096);
        response
            .body_mut()
            .as_reader()
            .take(REQUEST_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| RemoteError("Cannot read remote coordinator response"))?;
        if bytes.len() > REQUEST_LIMIT {
            return Err(RemoteError("Remote response exceeds limit"));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| RemoteError("Invalid remote coordinator response"))
    }
}

fn mapped_path(mapping: &Mapping, relative: &str) -> Result<PathBuf, RemoteError> {
    if relative.is_empty()
        || relative.len() > 4096
        || relative.contains(['\\', ':'])
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(RemoteError("Unsafe shared relative path"));
    }
    let path = mapping.local.join(relative);
    Converter::safe_path(
        &path.to_string_lossy(),
        std::slice::from_ref(&mapping.local),
    )
    .map_err(|_| RemoteError("Shared media mapping is unavailable or unsafe"))
}

/// Runs an explicitly configured worker process. Its only network destination
/// comes from local configuration; server responses cannot choose endpoints,
/// executables, arbitrary commands or mappings.
pub fn run_worker() -> Result<(), String> {
    worker_main().map_err(|error| error.to_string())
}

fn worker_main() -> Result<(), RemoteError> {
    let token = std::env::var("CONVERTER_WORKER_TOKEN").unwrap_or_default();
    valid_token(&token, false)?;
    let endpoint = std::env::var("CONVERTER_COORDINATOR_URL")
        .map_err(|_| RemoteError("Set CONVERTER_COORDINATOR_URL"))?;
    let url = url::Url::parse(&endpoint).map_err(|_| RemoteError("Invalid coordinator URL"))?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(RemoteError(
            "Coordinator URL must be an HTTP(S) origin without credentials, path or query",
        ));
    }
    let maps = std::env::var("CONVERTER_WORKER_MAPPINGS")
        .map_err(|_| RemoteError("Set explicit CONVERTER_WORKER_MAPPINGS JSON"))?;
    if maps.len() > 16384 {
        return Err(RemoteError("Worker mappings exceed limit"));
    }
    let mappings: Vec<Mapping> =
        serde_json::from_str(&maps).map_err(|_| RemoteError("Invalid worker mapping JSON"))?;
    if mappings.is_empty() || mappings.len() > 32 {
        return Err(RemoteError("Choose 1–32 worker root mappings"));
    }
    for mapping in &mappings {
        if mapping.server.is_empty() || mapping.server.len() > 4096 || !mapping.local.is_absolute()
        {
            return Err(RemoteError(
                "Worker mappings require explicit absolute roots",
            ));
        }
        Converter::safe_path(
            &mapping.local.to_string_lossy(),
            std::slice::from_ref(&mapping.local),
        )
        .map_err(|_| RemoteError("Worker mapping must be an existing nonsymlink local root"))?;
    }
    let data = PathBuf::from(
        std::env::var("CONVERTER_WORKER_DATA").unwrap_or_else(|_| "/data/worker".into()),
    );
    fs::create_dir_all(&data)
        .map_err(|_| RemoteError("Cannot create private worker data directory"))?;
    no_symlinks(&data)?;
    let ocr =
        std::env::var("CONVERTER_ENABLE_OCR").is_ok_and(|value| value == "1" || value == "true");
    let mut workers = crate::workers::Workers::new(data.join("workers.conf"), ocr)
        .map_err(|_| RemoteError("Invalid worker resource configuration"))?;
    let slots = slot_count("CONVERTER_WORKER_CPU_SLOTS", 1, 32)?;
    let ocr_slots = slot_count("CONVERTER_WORKER_OCR_SLOTS", usize::from(ocr), 16)?;
    let gpu_slots = slot_count("CONVERTER_WORKER_GPU_SLOTS", 0, crate::gpu::HARD_GPU_LIMIT)?;
    if slots == 0
        || ocr_slots > slots
        || (ocr_slots > 0 && !ocr)
        || (gpu_slots > 0 && !workers.gpu.eligible)
    {
        return Err(RemoteError(
            "Requested worker slots exceed local OCR/GPU permissions",
        ));
    }
    workers.update(&json!({"cpu_workers":slots,"ocr_workers":ocr_slots,"gpu_workers":gpu_slots,"paused":false}))
        .map_err(|_|RemoteError("Cannot save worker allocation"))?;
    let workers = Arc::new(Mutex::new(workers));
    let roots = mappings
        .iter()
        .map(|mapping| mapping.local.clone())
        .collect();
    let converter = Arc::new(Mutex::new(
        Converter::new(roots, data.join("state.json"))
            .map_err(|_| RemoteError("Cannot initialize worker converter"))?,
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))
        .map_err(|_| RemoteError("Cannot install worker shutdown handler"))?;
    let client = Client {
        agent: ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(8)))
                .max_redirects(0)
                .proxy(None)
                .build(),
        ),
        endpoint: endpoint.trim_end_matches('/').to_owned(),
        token,
    };
    let name = std::env::var("CONVERTER_WORKER_NAME").unwrap_or_else(|_| "worker".into());
    if name.is_empty() || name.len() > 40 || name.chars().any(char::is_control) {
        return Err(RemoteError(
            "Worker name must contain 1–40 plain characters",
        ));
    }
    eprintln!(
        "Subforge remote worker ready: {slots} CPU slots, {ocr_slots} OCR slots, {gpu_slots} GPU slots"
    );
    std::thread::scope(|scope| {
        for id in 0..slots + gpu_slots {
            let role = if id >= slots {
                "gpu"
            } else if id < ocr_slots {
                "cpu_ocr"
            } else {
                "cpu"
            };
            let (client, mappings, workers, converter, stop) = (
                client.clone(),
                &mappings,
                workers.clone(),
                converter.clone(),
                stop.clone(),
            );
            let name = format!("{name}-{role}-{id}");
            scope.spawn(move || {
                worker_slot(id, role, name, client, mappings, workers, converter, stop)
            });
        }
    });
    Ok(())
}
fn slot_count(key: &str, default: usize, max: usize) -> Result<usize, RemoteError> {
    let value = std::env::var(key)
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| RemoteError("Invalid worker slot count"))?
        .unwrap_or(default);
    if value > max {
        Err(RemoteError("Worker slot count exceeds hard ceiling"))
    } else {
        Ok(value)
    }
}
fn guarded<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn worker_log(id: usize, phase: &'static str, event: Option<&Value>) {
    let mut entry = json!({"time":crate::util::now(),"level":if phase=="failed" {"ERROR"} else {"INFO"},"worker_id":id,"status":phase,"message":format!("Remote worker: {phase}")});
    if let Some(event) = event {
        for key in [
            "track",
            "processed",
            "total",
            "frames",
            "frames_total",
            "progress_percent",
        ] {
            if let Some(value) = event[key].as_u64() {
                entry[key] = json!(value);
            }
        }
        if let Some(backend) = event["backend"]
            .as_str()
            .filter(|backend| ["ffmpeg", "seconv", "paddle", "tesseract"].contains(backend))
        {
            entry["backend"] = json!(backend);
        }
    }
    let mut output = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut output, &entry);
    let _ = output.write_all(b"\n");
    let _ = output.flush();
}
fn pause(stop: &AtomicBool) {
    for _ in 0..10 {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
#[allow(clippy::too_many_arguments)]
fn worker_slot(
    id: usize,
    role: &'static str,
    name: String,
    client: Client,
    mappings: &[Mapping],
    workers: Arc<Mutex<crate::workers::Workers>>,
    converter: Arc<Mutex<Converter>>,
    stop: Arc<AtomicBool>,
) {
    let mut node = None;
    let mut sampler = crate::resources::Sampler::default();
    while !stop.load(Ordering::Relaxed) {
        if node.is_none() {
            node=client.post("/api/remote/register",&json!({"name":name,"resource":role,"roots":mappings.iter().map(|mapping|&mapping.server).collect::<Vec<_>>()}))
                .ok().and_then(|result|result["node"].as_str().map(str::to_owned));
            if node.is_none() {
                pause(&stop);
                continue;
            }
        }
        let Some(identity) = node.as_ref() else {
            continue;
        };
        // Registration and every assignment are bounded by the same local
        // kernel-memory and deployment reservations as dashboard file workers.
        if !guarded(&workers).can_start_resource(&sampler.snapshot(), role) {
            let _ = client.post(
                "/api/remote/heartbeat",
                &json!({"node":identity,"lease":null}),
            );
            pause(&stop);
            continue;
        }
        let assignment = match client.post("/api/remote/lease", &json!({"node":identity})) {
            Ok(Value::Null) => {
                pause(&stop);
                continue;
            }
            Ok(value) => serde_json::from_value::<Assignment>(value).ok(),
            Err(_) => {
                node = None;
                pause(&stop);
                continue;
            }
        };
        let Some(assignment) = assignment else {
            node = None;
            pause(&stop);
            continue;
        };
        if let Err(error) = execute_assignment(
            id,
            role,
            &client,
            identity,
            assignment,
            mappings,
            &workers,
            &converter,
            &stop,
            &mut sampler,
        ) {
            // Error strings contain no URLs, tokens, media names or response bodies.
            eprintln!("Subforge remote slot {id}: {error}");
            pause(&stop);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_assignment(
    id: usize,
    role: &'static str,
    client: &Client,
    node: &str,
    assignment: Assignment,
    mappings: &[Mapping],
    workers: &Mutex<crate::workers::Workers>,
    converter: &Mutex<Converter>,
    shutdown: &AtomicBool,
    sampler: &mut crate::resources::Sampler,
) -> Result<(), RemoteError> {
    if !accepts(role, &assignment.resource) || assignment.lease.len() != 48 {
        return Err(RemoteError("Unsupported remote assignment"));
    }
    let mapping = mappings
        .iter()
        .find(|mapping| mapping.server == assignment.root)
        .ok_or(RemoteError("Assignment has no explicit local mapping"))?;
    let path = mapped_path(mapping, &assignment.path)?;
    let staging = mapped_path(mapping, &assignment.staging)?;
    if staging.parent() != path.parent()
        || !staging.is_dir()
        || !staging
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".subforge-remote-"))
    {
        return Err(RemoteError("Unsafe remote staging directory"));
    }
    let languages = Converter::normalize_languages(&json!(assignment.languages))
        .map_err(|_| RemoteError("Invalid remote languages"))?;
    let workload = match assignment.workload.as_str() {
        "all" => Workload::All,
        "text" => Workload::Text,
        "ocr" => Workload::Ocr,
        _ => return Err(RemoteError("Unsupported conversion workload")),
    };
    if assignment.resource == "cpu" && workload != Workload::Text {
        return Err(RemoteError("CPU text assignments cannot enable OCR"));
    }
    let mut worker = guarded(converter)
        .file_worker(&path)
        .map_err(|_| RemoteError("Cannot create isolated remote converter"))?;
    worker.state = json!({});
    if let Some(state) = assignment.state {
        worker.state[path.to_string_lossy().as_ref()] = state;
    }
    if let Some(selected) = assignment.selected {
        worker
            .select_tracks(selected)
            .map_err(|_| RemoteError("Invalid remote subtitle selection"))?;
    }
    if let Some(targets) = assignment.targets {
        worker
            .set_targets(targets)
            .map_err(|_| RemoteError("Invalid remote subtitle targets"))?;
    }
    if assignment.sources.len() > 256 {
        return Err(RemoteError("Remote subtitle hints exceed their limit"));
    }
    let sources = assignment
        .sources
        .into_iter()
        .map(|hint| {
            let relative = hint
                .path
                .to_str()
                .ok_or(RemoteError("Invalid remote subtitle hint path"))?;
            let source = mapped_path(mapping, relative)?;
            if source.parent() != path.parent() {
                return Err(RemoteError(
                    "Remote subtitle hint must share the media directory",
                ));
            }
            Ok(SubtitleSourceHint {
                path: source,
                target: hint.target,
            })
        })
        .collect::<Result<Vec<_>, RemoteError>>()?;
    worker
        .set_source_hints(sources)
        .map_err(|_| RemoteError("Invalid remote subtitle hints"))?;
    worker
        .force_covered_tracks(assignment.force)
        .map_err(|_| RemoteError("Remote coverage override requires explicit track selection"))?;
    worker
        .enable_remote_staging(staging)
        .map_err(|_| RemoteError("Cannot enable remote staging"))?;
    {
        let mut local = guarded(workers);
        if !local.can_start_resource(&sampler.snapshot(), role) {
            return Err(RemoteError(
                "Local resources changed before assignment admission",
            ));
        }
        if role == "gpu" {
            worker.enable_gpu_ocr(local.gpu_min_free_mib());
        }
        local.active.push(crate::workers::ActiveWorker {
            id,
            resource: role,
            path: path.clone(),
            started: crate::util::now(),
            status: "processing",
        });
    }
    let cancelled = AtomicBool::new(false);
    let done = AtomicBool::new(false);
    worker_log(id, "dispatched", None);
    let result = std::thread::scope(|scope| {
        let lease = &assignment.lease;
        let cancel_ref = &cancelled;
        let done_ref = &done;
        scope.spawn(move || {
            while !done_ref.load(Ordering::Relaxed) {
                for _ in 0..20 {
                    if done_ref.load(Ordering::Relaxed) {
                        return;
                    }
                    if shutdown.load(Ordering::Relaxed) {
                        cancel_ref.store(true, Ordering::Relaxed);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                if client
                    .post("/api/remote/heartbeat", &json!({"node":node,"lease":lease}))
                    .is_err()
                {
                    cancel_ref.store(true, Ordering::Relaxed);
                    return;
                }
            }
        });
        let mut terminal = None;
        let mut previous_phase = None;
        let mut last_progress = Instant::now();
        let result = worker.scan_workload(
            std::slice::from_ref(&path),
            &languages,
            assignment.retry,
            workload,
            assignment.automatic,
            &cancelled,
            &mut |event| {
                let phase = match event["status"].as_str().unwrap_or("") {
                    "extracting" => Some("extracting"),
                    "ocr" => Some("ocr"),
                    "ocr_progress" => Some("ocr_progress"),
                    "validating" => Some("validating"),
                    "track_complete" => Some("track_complete"),
                    "converted" => Some("staged"),
                    "cached" => Some("cached"),
                    "failed" => Some("failed"),
                    "pending" => Some("pending"),
                    "skipped" => Some("skipped"),
                    _ => None,
                };
                if let Some(phase) = phase
                    && (phase != "ocr_progress"
                        || previous_phase != Some(phase)
                        || last_progress.elapsed() >= Duration::from_secs(5))
                {
                    worker_log(id, phase, Some(&event));
                    previous_phase = Some(phase);
                    last_progress = Instant::now();
                }
                if ["converted", "cached", "failed", "pending", "skipped"]
                    .contains(&event["status"].as_str().unwrap_or(""))
                {
                    terminal = Some(event);
                }
            },
        );
        done.store(true, Ordering::Relaxed);
        result.map(|summary| (summary, terminal))
    });
    guarded(workers).active.retain(|active| active.id != id);
    if worker.gpu_status().1.is_some() {
        guarded(workers).gpu.failed();
    }
    if cancelled.load(Ordering::Relaxed) || shutdown.load(Ordering::Relaxed) {
        worker_log(id, "cancelled", None);
        return Err(RemoteError(
            "Remote lease cancelled or coordinator unavailable",
        ));
    }
    let (summary, terminal) = result.map_err(|_| RemoteError("Remote conversion failed safely"))?;
    client.post("/api/remote/complete",&json!({"node":node,"lease":assignment.lease,"state":worker.take_file_state(&path),
        "summary":summary,"terminal":terminal,"outputs":worker.staged_outputs(),"fingerprint":fingerprint_value(&source_stamp(&path)?)}))?;
    worker_log(id, "reported", None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "1\n00:00:00,000 --> 00:00:01,000\nShared worker fixture\n";

    struct Fixture {
        _temporary: TempDir,
        root: PathBuf,
        path: PathBuf,
        coordinator: Coordinator,
        node: String,
    }
    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let root = crate::converter::normalize_canonical_local_path(
                temporary.path().canonicalize().unwrap(),
            );
            let path = root.join("movie.ass");
            fs::write(&path, b"disposable media fixture").unwrap();
            let mut coordinator = Coordinator::new("t".repeat(32), vec![root.clone()]).unwrap();
            let result = coordinator
                .handle(
                    "/api/remote/register",
                    &json!({"name":"test-cpu","resource":"cpu","roots":[root]}),
                )
                .unwrap();
            let node = result["node"].as_str().unwrap().to_owned();
            coordinator
                .handle("/api/remote/lease", &json!({"node":node}))
                .unwrap();
            Self {
                _temporary: temporary,
                root,
                path,
                coordinator,
                node,
            }
        }
        fn offer(&mut self) -> String {
            self.coordinator
                .offer(
                    &self.path,
                    &["en".into()],
                    false,
                    false,
                    Workload::Text,
                    None,
                    None,
                    &[],
                    false,
                    "cpu",
                    None,
                )
                .unwrap()
                .unwrap()
        }
        fn result(&self, ticket: &str) -> Value {
            let lease = &self.coordinator.leases[ticket];
            fs::write(lease.staging.path().join("movie.en.srt"), SRT).unwrap();
            let fingerprint = json!(
                lease
                    .source
                    .iter()
                    .map(|(length, time)| json!([
                        length,
                        time.duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                    ]))
                    .collect::<Vec<_>>()
            );
            json!({"node":self.node,"lease":ticket,"fingerprint":fingerprint,"state":{"fingerprint":fingerprint,"processing_fingerprint":fingerprint,"ok":true,"outputs":["movie.en.srt"]},
                "summary":{"converted":1,"cached":0,"pending":0,"failed":0,"planned":0},"terminal":{"status":"converted","path":"untrusted-client-path"},"outputs":["movie.en.srt"]})
        }
    }

    #[test]
    fn leases_preserve_exact_targets_and_require_explicit_tracks_for_force() {
        let mut fixture = Fixture::new();
        let targets = vec![SubtitleTarget {
            language: "fr".into(),
            forced: true,
            hi: false,
        }];
        assert!(
            fixture
                .coordinator
                .offer(
                    &fixture.path,
                    &["*".into()],
                    false,
                    false,
                    Workload::Text,
                    None,
                    Some(&targets),
                    &[],
                    true,
                    "cpu",
                    None,
                )
                .is_err()
        );
        let ticket = fixture
            .coordinator
            .offer(
                &fixture.path,
                &["*".into()],
                false,
                false,
                Workload::Text,
                Some(&[3]),
                Some(&targets),
                &[SubtitleSourceHint {
                    path: fixture.path.clone(),
                    target: targets[0].clone(),
                }],
                true,
                "cpu",
                None,
            )
            .unwrap()
            .unwrap();
        let value = fixture
            .coordinator
            .handle("/api/remote/lease", &json!({"node":fixture.node}))
            .unwrap();
        let assignment: Assignment = serde_json::from_value(value).unwrap();
        assert_eq!(assignment.lease, ticket);
        assert_eq!(assignment.sources.len(), 1);
        assert_eq!(assignment.sources[0].path, Path::new("movie.ass"));
        assert_eq!(assignment.sources[0].target, targets[0]);
        assert_eq!(assignment.targets, Some(targets));
        assert_eq!(assignment.selected, Some(vec![3]));
        assert!(assignment.force);
    }

    #[test]
    fn an_empty_target_list_is_not_an_unrestricted_remote_assignment() {
        let mut fixture = Fixture::new();
        fixture
            .coordinator
            .offer(
                &fixture.path,
                &["*".into()],
                false,
                false,
                Workload::Text,
                None,
                Some(&[]),
                &[],
                false,
                "cpu",
                None,
            )
            .unwrap()
            .unwrap();
        let value = fixture
            .coordinator
            .handle("/api/remote/lease", &json!({"node":fixture.node}))
            .unwrap();
        let assignment: Assignment = serde_json::from_value(value).unwrap();
        assert_eq!(assignment.targets, Some(vec![]));
        assert!(!assignment.force);
    }

    #[test]
    fn dedicated_token_is_opt_in_bounded_and_not_exposed() {
        assert!(Coordinator::new("short".into(), vec![]).is_err());
        assert!(Coordinator::new("t\n".repeat(32), vec![]).is_err());
        let disabled = Coordinator::new(String::new(), vec![]).unwrap();
        assert!(!disabled.authenticated(""));
        let fixture = Fixture::new();
        assert!(fixture.coordinator.authenticated(&"t".repeat(32)));
        assert!(!fixture.coordinator.authenticated(&"x".repeat(32)));
        assert!(
            !fixture
                .coordinator
                .public()
                .to_string()
                .contains(&"t".repeat(32))
        );
    }

    #[test]
    fn executor_uses_registered_identity_and_requested_slot_not_worker_terminal_fields() {
        let mut fixture = Fixture::new();
        assert!(fixture.coordinator.executor("missing-ticket").is_none());
        fixture.coordinator.nodes.clear();
        let registration = fixture
            .coordinator
            .handle(
                "/api/remote/register",
                &json!({"name":"Remote OCR node","resource":"cpu_ocr","roots":[fixture.root]}),
            )
            .unwrap();
        fixture.node = registration["node"].as_str().unwrap().to_owned();
        fixture
            .coordinator
            .handle("/api/remote/lease", &json!({"node":fixture.node}))
            .unwrap();
        let ticket = fixture.offer();
        let expected = json!({"node_id":fixture.node,"node_name":"Remote OCR node","resource":"cpu","execution":"remote"});
        assert_eq!(
            fixture.coordinator.executor(&ticket),
            Some(expected.clone())
        );
        let mut completion = fixture.result(&ticket);
        completion["terminal"]["node_id"] = json!("forged-id");
        completion["terminal"]["node_name"] = json!("Forged node");
        completion["terminal"]["resource"] = json!("gpu");
        completion["terminal"]["execution"] = json!("local");
        fixture
            .coordinator
            .handle("/api/remote/complete", &completion)
            .unwrap();
        let executor = fixture.coordinator.executor(&ticket).unwrap();
        assert_eq!(executor, expected);
        assert_eq!(
            executor.as_object().unwrap().len(),
            4,
            "attribution must expose only its four operational fields"
        );
        fixture.coordinator.cancel(&ticket);
        assert!(fixture.coordinator.executor(&ticket).is_none());
    }

    #[test]
    fn registration_is_strict_and_cannot_request_arbitrary_roots_or_commands() {
        let mut fixture = Fixture::new();
        for value in [
            json!({"name":"node","resource":"cpu","roots":["/not-authorized"]}),
            json!({"name":"node","resource":"shell","roots":[fixture.root]}),
            json!({"name":"node","resource":"cpu","roots":[fixture.root],"command":"anything"}),
            json!({"name":"node","resource":"cpu","roots":[fixture.root],"token":"anything"}),
        ] {
            assert!(
                fixture
                    .coordinator
                    .handle("/api/remote/register", &value)
                    .is_err()
            );
        }
        assert!(
            fixture
                .coordinator
                .handle("/api/remote/unknown", &json!({}))
                .is_err()
        );
        assert!(
            fixture
                .coordinator
                .handle(
                    "/api/remote/lease",
                    &json!({"node":fixture.node,"padding":"x".repeat(REQUEST_LIMIT)})
                )
                .is_err()
        );
    }

    #[test]
    fn nodes_and_outstanding_leases_have_hard_ceilings() {
        let mut fixture = Fixture::new();
        for _ in 1..MAX_NODES {
            fixture
                .coordinator
                .handle(
                    "/api/remote/register",
                    &json!({"name":"bounded","resource":"cpu","roots":[fixture.root]}),
                )
                .unwrap();
        }
        assert!(
            fixture
                .coordinator
                .handle(
                    "/api/remote/register",
                    &json!({"name":"too-many","resource":"cpu","roots":[fixture.root]})
                )
                .is_err()
        );
        let ticket = fixture.offer();
        assert!(!fixture.coordinator.has_capacity("cpu"));
        fixture.coordinator.cancel(&ticket);
        assert!(fixture.coordinator.has_capacity("cpu"));
    }

    #[test]
    fn completion_is_staged_until_coordinator_publishes_and_merges() {
        let mut fixture = Fixture::new();
        let ticket = fixture.offer();
        let assignment = fixture
            .coordinator
            .handle("/api/remote/lease", &json!({"node":fixture.node}))
            .unwrap();
        assert_eq!(assignment["path"], "movie.ass");
        assert!(
            assignment["staging"]
                .as_str()
                .unwrap()
                .starts_with(".subforge-remote-")
        );
        let result = fixture.result(&ticket);
        fixture
            .coordinator
            .handle("/api/remote/complete", &result)
            .unwrap();
        assert!(!fixture.root.join("movie.en.srt").exists());
        // Polling again before the coordinator consumes the result must never
        // execute the same file a second time.
        assert!(
            fixture
                .coordinator
                .handle("/api/remote/lease", &json!({"node":fixture.node}))
                .unwrap()
                .is_null()
        );
        let RemotePoll::Finished(outcome) =
            fixture.coordinator.take(&ticket, &AtomicBool::new(false))
        else {
            panic!("completed lease must finish");
        };
        assert_eq!(
            fs::read_to_string(fixture.root.join("movie.en.srt")).unwrap(),
            SRT
        );
        assert_eq!(outcome.summary["converted"], 1);
        let terminal = outcome.terminal.unwrap();
        assert_eq!(terminal["path"], json!(fixture.path));
        assert_eq!(
            terminal["new_outputs"],
            json!([fixture.root.join("movie.en.srt")])
        );
        assert_eq!(outcome.state.unwrap()["ok"], true);
        assert!(matches!(
            fixture.coordinator.take(&ticket, &AtomicBool::new(false)),
            RemotePoll::Retry
        ));
        assert!(
            fixture
                .coordinator
                .handle("/api/remote/complete", &result)
                .is_err()
        );
    }

    #[test]
    fn existing_outputs_are_preserved_and_not_notified_again() {
        let mut fixture = Fixture::new();
        fs::write(fixture.root.join("movie.en.srt"), "existing user subtitle").unwrap();
        let ticket = fixture.offer();
        let result = fixture.result(&ticket);
        fixture
            .coordinator
            .handle("/api/remote/complete", &result)
            .unwrap();
        let RemotePoll::Finished(outcome) =
            fixture.coordinator.take(&ticket, &AtomicBool::new(false))
        else {
            panic!("lease must finish");
        };
        assert_eq!(
            fs::read_to_string(fixture.root.join("movie.en.srt")).unwrap(),
            "existing user subtitle"
        );
        assert_eq!(outcome.terminal.unwrap()["new_outputs"], json!([]));
    }

    #[test]
    fn cancellation_expiry_and_changed_sources_never_publish_late_results() {
        for mode in ["cancelled", "expired", "changed"] {
            let mut fixture = Fixture::new();
            let ticket = fixture.offer();
            let result = fixture.result(&ticket);
            fixture
                .coordinator
                .handle("/api/remote/complete", &result)
                .unwrap();
            match mode {
                "expired" => {
                    fixture.coordinator.leases.get_mut(&ticket).unwrap().expires =
                        Instant::now() - Duration::from_secs(1)
                }
                "changed" => {
                    fs::write(&fixture.path, b"different changed disposable source").unwrap()
                }
                _ => {}
            }
            assert!(matches!(
                fixture
                    .coordinator
                    .take(&ticket, &AtomicBool::new(mode == "cancelled")),
                RemotePoll::Retry
            ));
            assert!(!fixture.root.join("movie.en.srt").exists());
            assert!(
                fixture
                    .coordinator
                    .handle("/api/remote/complete", &result)
                    .is_err()
            );
            assert!(
                fixture
                    .coordinator
                    .handle(
                        "/api/remote/heartbeat",
                        &json!({"node":fixture.node,"lease":ticket})
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn heartbeat_extends_current_lease_and_resource_pressure_stops_dispatch() {
        let mut fixture = Fixture::new();
        let ticket = fixture.offer();
        let old = Instant::now() + Duration::from_secs(1);
        fixture.coordinator.leases.get_mut(&ticket).unwrap().expires = old;
        fixture
            .coordinator
            .handle(
                "/api/remote/heartbeat",
                &json!({"node":fixture.node,"lease":ticket}),
            )
            .unwrap();
        assert!(fixture.coordinator.leases[&ticket].expires > old);
        fixture.coordinator.cancel(&ticket);
        assert!(!fixture.coordinator.has_capacity("cpu"));
        fixture
            .coordinator
            .handle("/api/remote/lease", &json!({"node":fixture.node}))
            .unwrap();
        assert!(fixture.coordinator.has_capacity("cpu"));
    }

    #[test]
    fn malformed_results_and_foreign_paths_are_rejected_before_publication() {
        for mode in [
            "srt",
            "output",
            "state",
            "fingerprint",
            "summary",
            "terminal",
        ] {
            let mut fixture = Fixture::new();
            let ticket = fixture.offer();
            let mut result = fixture.result(&ticket);
            match mode {
                "srt" => fs::write(
                    fixture.coordinator.leases[&ticket]
                        .staging
                        .path()
                        .join("movie.en.srt"),
                    "not subtitle cues",
                )
                .unwrap(),
                "output" => result["outputs"] = json!(["../outside.srt"]),
                "state" => result["state"]["outputs"] = json!(["another.srt"]),
                "fingerprint" => result["state"]["processing_fingerprint"] = json!([]),
                "summary" => result["summary"]["converted"] = json!(u64::MAX),
                "terminal" => result["terminal"]["status"] = json!("anything"),
                _ => unreachable!(),
            }
            let complete = fixture.coordinator.handle("/api/remote/complete", &result);
            if complete.is_ok() {
                assert!(matches!(
                    fixture.coordinator.take(&ticket, &AtomicBool::new(false)),
                    RemotePoll::Retry
                ));
            }
            assert!(
                !fixture.root.join("movie.en.srt").exists(),
                "invalid {mode} must not publish"
            );
        }
    }

    #[test]
    fn mapped_paths_reject_traversal_absolute_paths_and_unmapped_sources() {
        let fixture = Fixture::new();
        let mapping = Mapping {
            server: "/media".into(),
            local: fixture.root.clone(),
        };
        assert_eq!(mapped_path(&mapping, "movie.ass").unwrap(), fixture.path);
        for path in [
            "../movie.ass",
            "/movie.ass",
            "a//b",
            "a/./b",
            "a/../b",
            "C:/movie.ass",
            "\\\\server\\media",
            "movie.ass/",
        ] {
            assert!(
                mapped_path(&mapping, path).is_err(),
                "{path} must be rejected"
            );
        }
    }

    #[test]
    fn vobsub_fingerprint_includes_both_companion_files() {
        let fixture = Fixture::new();
        let idx = fixture.root.join("paired.idx");
        fs::write(&idx, b"index").unwrap();
        let before = source_stamp(&idx).unwrap();
        fs::write(idx.with_extension("sub"), b"new companion").unwrap();
        assert_ne!(before, source_stamp(&idx).unwrap());
    }

    #[test]
    fn oversized_state_keeps_file_eligible_for_local_dispatch() {
        let mut fixture = Fixture::new();
        let state = json!({"legacy_cache":"x".repeat(STATE_LIMIT+1)});
        let offered = fixture
            .coordinator
            .offer(
                &fixture.path,
                &["en".into()],
                false,
                false,
                Workload::Text,
                None,
                None,
                &[],
                false,
                "cpu",
                Some(state),
            )
            .unwrap();
        assert!(offered.is_none());
        assert!(fixture.coordinator.leases.is_empty());
        assert!(fixture.coordinator.has_capacity("cpu"));
        assert_eq!(
            fs::read_dir(&fixture.root).unwrap().count(),
            1,
            "fallback must not leave a staging directory"
        );
    }

    #[test]
    fn cpu_admission_cannot_inherit_remote_ocr_permission() {
        let mut fixture = Fixture::new();
        let ticket = fixture
            .coordinator
            .offer(
                &fixture.path,
                &["en".into()],
                false,
                false,
                Workload::All,
                None,
                None,
                &[],
                false,
                "cpu",
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            fixture.coordinator.leases[&ticket].request["workload"],
            "text"
        );
    }

    #[test]
    fn cancellation_source_change_and_expiry_during_validation_block_publication() {
        for mode in ["cancelled", "source", "expired"] {
            let mut fixture = Fixture::new();
            let ticket = fixture.offer();
            let result = fixture.result(&ticket);
            fixture
                .coordinator
                .handle("/api/remote/complete", &result)
                .unwrap();
            let gate = Arc::new(ValidationGate::default());
            let lease = fixture.coordinator.leases.get_mut(&ticket).unwrap();
            lease.validation_gate = Some(gate.clone());
            if mode == "expired" {
                lease.expires = Instant::now() + Duration::from_millis(100);
            }
            let cancelled = AtomicBool::new(false);
            let path = fixture.path.clone();
            let outcome = std::thread::scope(|scope| {
                let coordinator = &mut fixture.coordinator;
                let flag = &cancelled;
                let ticket_ref = &ticket;
                let worker = scope.spawn(move || coordinator.take(ticket_ref, flag));
                let deadline = Instant::now() + Duration::from_secs(3);
                while !gate.entered.load(Ordering::SeqCst) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let entered = gate.entered.load(Ordering::SeqCst);
                if entered {
                    match mode {
                        "cancelled" => cancelled.store(true, Ordering::SeqCst),
                        "source" => {
                            fs::write(&path, b"source changed during remote validation").unwrap()
                        }
                        "expired" => std::thread::sleep(Duration::from_millis(120)),
                        _ => {}
                    }
                }
                gate.released.store(true, Ordering::SeqCst);
                let result = worker.join().unwrap();
                assert!(
                    entered,
                    "publication must reach deterministic validation gate"
                );
                result
            });
            assert!(matches!(outcome, RemotePoll::Retry));
            assert!(
                !fixture.root.join("movie.en.srt").exists(),
                "{mode} during validation must block commit"
            );
        }
    }

    #[test]
    fn publication_uses_private_snapshot_even_if_worker_changes_staged_inode() {
        let mut fixture = Fixture::new();
        let ticket = fixture.offer();
        let result = fixture.result(&ticket);
        fixture
            .coordinator
            .handle("/api/remote/complete", &result)
            .unwrap();
        let gate = Arc::new(ValidationGate::default());
        let lease = fixture.coordinator.leases.get_mut(&ticket).unwrap();
        lease.validation_gate = Some(gate.clone());
        let staged = lease.staging.path().join("movie.en.srt");
        let cancelled = AtomicBool::new(false);
        let outcome = std::thread::scope(|scope| {
            let coordinator = &mut fixture.coordinator;
            let worker = scope.spawn(|| coordinator.take(&ticket, &cancelled));
            let deadline = Instant::now() + Duration::from_secs(3);
            while !gate.entered.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            let entered = gate.entered.load(Ordering::SeqCst);
            if entered {
                fs::write(
                    &staged,
                    b"worker modified original inode after coordinator validation",
                )
                .unwrap();
            }
            gate.released.store(true, Ordering::SeqCst);
            let result = worker.join().unwrap();
            assert!(entered, "publication must reach validation gate");
            result
        });
        assert!(matches!(outcome, RemotePoll::Finished(_)));
        assert_eq!(
            fs::read_to_string(fixture.root.join("movie.en.srt")).unwrap(),
            SRT
        );
    }

    #[test]
    fn independent_worker_fingerprint_is_required_even_without_state() {
        let mut fixture = Fixture::new();
        let ticket = fixture.offer();
        let mut result = fixture.result(&ticket);
        result["state"] = Value::Null;
        result["fingerprint"] = json!([]);
        fixture
            .coordinator
            .handle("/api/remote/complete", &result)
            .unwrap();
        assert!(matches!(
            fixture.coordinator.take(&ticket, &AtomicBool::new(false)),
            RemotePoll::Retry
        ));
        assert!(!fixture.root.join("movie.en.srt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_mappings_sources_and_staged_outputs_are_denied() {
        use std::os::unix::fs::symlink;
        let mut fixture = Fixture::new();
        let link = fixture.root.join("link.ass");
        symlink(&fixture.path, &link).unwrap();
        let mapping = Mapping {
            server: "/media".into(),
            local: fixture.root.clone(),
        };
        assert!(mapped_path(&mapping, "link.ass").is_err());
        let ticket = fixture.offer();
        let result = fixture.result(&ticket);
        let output = fixture.coordinator.leases[&ticket]
            .staging
            .path()
            .join("movie.en.srt");
        fs::remove_file(&output).unwrap();
        symlink(&fixture.path, &output).unwrap();
        fixture
            .coordinator
            .handle("/api/remote/complete", &result)
            .unwrap();
        assert!(matches!(
            fixture.coordinator.take(&ticket, &AtomicBool::new(false)),
            RemotePoll::Retry
        ));
        assert!(!fixture.root.join("movie.en.srt").exists());
    }
}
