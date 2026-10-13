//! Optional media-server connections. All network access is deny-by-default:
//! providers start disabled, URLs are explicitly validated, and this module
//! only emits the small set of methods and endpoints used by the app.

use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    env,
    io::{ErrorKind, Read},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use ureq::{Agent, config::Config};
use url::Url;

const PROVIDERS: [&str; 4] = ["sonarr", "radarr", "bazarr", "jellyfin"];
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const MAX_ITEMS: usize = 20_000;
const SAFE_KEY_MAX: usize = 512;

#[derive(Clone, Debug)]
struct HttpFailure {
    category: &'static str,
    status: Option<u16>,
}

impl HttpFailure {
    fn message(&self, prefix: &str) -> String {
        let detail = match self.category {
            "authentication" => self
                .status
                .map(|s| format!("API key rejected (HTTP {s})"))
                .unwrap_or_else(|| "API key rejected (HTTP 401/403)".into()),
            "refused" => "Connection refused; check the service URL and availability".into(),
            "timeout" => "Request timed out while waiting for the service".into(),
            "invalid_json" => {
                "Service returned an invalid JSON response; check its URL and proxy settings".into()
            }
            "response_limit" => "Service response exceeded the safety limit".into(),
            "http_status" => self
                .status
                .map(|s| format!("Service returned HTTP {s}"))
                .unwrap_or_else(|| "Service returned an HTTP error".into()),
            _ => "Could not reach the configured service; check its URL and network".into(),
        };
        format!("{prefix}: {detail}")
    }
}

fn http_agent() -> Agent {
    let config: Config = Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .max_redirects(0)
        .proxy(None)
        .build();
    Agent::new_with_config(config)
}

fn safe_error(error: &ureq::Error) -> HttpFailure {
    match error {
        ureq::Error::StatusCode(code) if *code == 401 || *code == 403 => HttpFailure {
            category: "authentication",
            status: Some(*code),
        },
        ureq::Error::StatusCode(code) => HttpFailure {
            category: "http_status",
            status: Some(*code),
        },
        ureq::Error::Timeout(_) => HttpFailure {
            category: "timeout",
            status: None,
        },
        ureq::Error::Io(source) if source.kind() == ErrorKind::ConnectionRefused => HttpFailure {
            category: "refused",
            status: None,
        },
        _ => HttpFailure {
            category: "unreachable",
            status: None,
        },
    }
}

fn validate_base(value: &str) -> Result<String, String> {
    let value = value.trim().trim_end_matches('/');
    let parsed = Url::parse(value).map_err(|_| "Enter a valid HTTP or HTTPS server URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || value
            .split_once("://")
            .and_then(|(_, tail)| tail.split('/').next())
            .is_some_and(|authority| authority.contains('@'))
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || value.chars().any(|c| c.is_control() || c == '\\')
    {
        return Err("Enter a valid HTTP or HTTPS server URL".into());
    }
    Ok(value.to_string())
}

fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= SAFE_KEY_MAX
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._~+/=-".contains(&b))
}

fn encode_component(input: &str) -> String {
    input
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn allowed_route(method: &str, path: &str) -> bool {
    match method {
        "GET" => {
            [
                "/api/v3/system/status",
                "/api/v3/series",
                "/api/v3/movie",
                "/api/system/status",
                "/System/Info",
                "/Library/VirtualFolders",
                "/api/system/tasks",
            ]
            .contains(&path)
                || path.starts_with("/api/series?start=")
                || path.starts_with("/api/movies?start=")
                || path.starts_with("/Items?ParentId=")
        }
        "POST" => {
            [
                "/api/v3/command",
                "/api/system/tasks",
                "/Library/Media/Updated",
            ]
            .contains(&path)
                || (path.starts_with("/Items/")
                    && path.contains("/Refresh?")
                    && !path.contains('#'))
        }
        "PATCH" => path == "/api/series" || path == "/api/movies",
        _ => false,
    }
}

fn request(
    base: &str,
    path: &str,
    method: &str,
    key: &str,
    auth: Auth,
    body: Option<&Value>,
    form: Option<&[(&str, String)]>,
) -> Result<Value, HttpFailure> {
    if !matches!(method, "GET" | "POST" | "PATCH")
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || (body.is_some() && form.is_some())
        || !allowed_route(method, path)
    {
        return Err(HttpFailure {
            category: "invalid_json",
            status: None,
        });
    }
    let base = validate_base(base).map_err(|_| HttpFailure {
        category: "unreachable",
        status: None,
    })?;
    // The service origin/path is configured; API paths are compile-time constants
    // or contain only locally generated, percent-encoded query and item IDs.
    let url = format!("{}{}", base, path);
    let mut builder = ureq::http::Request::builder()
        .method(method)
        .uri(&url)
        .header("Accept", "application/json");
    let mut content_type = None;
    if !key.is_empty() {
        match auth {
            Auth::ApiKey => builder = builder.header("X-Api-Key", key),
            Auth::Jellyfin => {
                let token = encode_component(key);
                let value = format!(
                    "MediaBrowser Client=\"Subforge\", Device=\"Subtitle converter\", DeviceId=\"subforge\", Version=\"1\", Token=\"{token}\""
                );
                builder = builder.header("Authorization", value);
            }
        }
    }
    let bytes = if let Some(value) = body {
        content_type = Some("application/json");
        value.to_string().into_bytes()
    } else if let Some(fields) = form {
        content_type = Some("application/x-www-form-urlencoded");
        let payload = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v)))
            .collect::<Vec<_>>()
            .join("&");
        payload.into_bytes()
    } else {
        Vec::new()
    };
    if let Some(content_type) = content_type {
        builder = builder.header("Content-Type", content_type);
    }
    let request = builder.body(bytes).map_err(|_| HttpFailure {
        category: "invalid_json",
        status: None,
    })?;
    let result = http_agent().run(request);
    let mut response = result.map_err(|e| safe_error(&e))?;
    if !(200..300).contains(&response.status().as_u16()) {
        return Err(HttpFailure {
            category: "http_status",
            status: Some(response.status().as_u16()),
        });
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take((MAX_RESPONSE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| HttpFailure {
            category: "unreachable",
            status: None,
        })?;
    if bytes.len() > MAX_RESPONSE {
        return Err(HttpFailure {
            category: "response_limit",
            status: None,
        });
    }
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&bytes).map_err(|_| HttpFailure {
        category: "invalid_json",
        status: None,
    })
}

#[derive(Clone, Copy)]
enum Auth {
    ApiKey,
    Jellyfin,
}

fn defaults() -> Value {
    let mut root = Map::new();
    for provider in ["sonarr", "radarr", "bazarr"] {
        let upper = provider.to_ascii_uppercase();
        let default_url = if provider == "bazarr" {
            "http://bazarr:6767"
        } else {
            ""
        };
        root.insert(provider.into(), json!({"enabled":false,"url":env::var(format!("{upper}_INTERNAL_URL")).unwrap_or_else(|_| default_url.into()),"api_key":env::var(format!("{upper}_API_KEY")).unwrap_or_default()}));
    }
    root.insert("jellyfin".into(), json!({"enabled":false,"url":env::var("JELLYFIN_INTERNAL_URL").unwrap_or_else(|_|"http://host.docker.internal:8096".into()),"api_key":env::var("JELLYFIN_API_KEY").unwrap_or_default(),"movie_libraries":[],"series_libraries":[],"refresh_movies":true,"refresh_series":true,"refresh_mode":"immediate"}));
    root.insert("path_mappings".into(), json!([]));
    Value::Object(root)
}

#[derive(Clone, Debug)]
struct NormPath {
    value: String,
    flavor: Flavor,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavor {
    Posix,
    Windows,
}

fn norm_path(raw: &str) -> Option<NormPath> {
    let value = raw.trim();
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        let drive = (bytes[0] as char).to_ascii_uppercase();
        let tail = value[3..].replace('\\', "/");
        let normalized = normalize_components(&tail, false)?;
        return Some(NormPath {
            value: if normalized.is_empty() {
                format!("{drive}:/")
            } else {
                format!("{drive}:/{normalized}")
            },
            flavor: Flavor::Windows,
        });
    }
    if !value.starts_with('/') || value.starts_with("//") {
        return None;
    }
    let normalized = normalize_components(value.trim_start_matches('/'), false)?;
    Some(NormPath {
        value: if normalized.is_empty() {
            "/".into()
        } else {
            format!("/{normalized}")
        },
        flavor: Flavor::Posix,
    })
}

fn normalize_components(value: &str, allow_parent: bool) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in value.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." if allow_parent => {
                parts.pop();
            }
            ".." => return None,
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn path_eq(a: &NormPath, b: &NormPath) -> bool {
    a.flavor == b.flavor
        && if a.flavor == Flavor::Windows {
            a.value.eq_ignore_ascii_case(&b.value)
        } else {
            a.value == b.value
        }
}
fn path_case_eq(a: &str, b: &str, flavor: Flavor) -> bool {
    if flavor == Flavor::Windows {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}
fn under(path: &NormPath, root: &NormPath) -> bool {
    if path.flavor != root.flavor {
        return false;
    }
    if path_eq(path, root) {
        return true;
    }
    let prefix = if root.value.ends_with('/') {
        root.value.clone()
    } else {
        format!("{}/", root.value)
    };
    if path.flavor == Flavor::Windows {
        path.value
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    } else {
        path.value.starts_with(&prefix)
    }
}
fn join_map(path: &NormPath, source: &NormPath, target_raw: &str) -> Option<String> {
    if !under(path, source) {
        return None;
    }
    let target = norm_path(target_raw)?;
    let suffix = path.value[source.value.len()..].trim_start_matches('/');
    let target_sep = if target.flavor == Flavor::Windows {
        '\\'
    } else {
        '/'
    };
    let base = if target.flavor == Flavor::Windows {
        target.value.replace('/', "\\")
    } else {
        target.value.clone()
    };
    Some(if suffix.is_empty() {
        base
    } else {
        format!(
            "{}{}{}",
            base.trim_end_matches(target_sep),
            target_sep,
            suffix.replace('/', &target_sep.to_string())
        )
    })
}
fn get_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn enabled(conf: &Value) -> bool {
    conf.get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Integration settings are persisted without Jellyfin endpoint or token.
#[derive(Clone)]
pub struct Integrations {
    settings_path: PathBuf,
    settings: Value,
    bazarr_pending_paths: BTreeSet<String>,
}

impl Integrations {
    pub fn new(settings_path: PathBuf) -> Result<Self, String> {
        let mut settings = defaults();
        if let Ok(saved) = crate::util::read_json(&settings_path, 64 * 1024)
            && let (Some(target), Some(source)) = (settings.as_object_mut(), saved.as_object())
        {
            for provider in PROVIDERS {
                if provider == "jellyfin" {
                    continue;
                }
                if let (Some(dst), Some(src)) = (
                    target.get_mut(provider).and_then(Value::as_object_mut),
                    source.get(provider).and_then(Value::as_object),
                ) {
                    for (key, value) in src {
                        if ["enabled", "url", "api_key"].contains(&key.as_str()) {
                            dst.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            if let Some(paths) = source.get("path_mappings").and_then(Value::as_array)
                && let Some(slot) = target.get_mut("path_mappings")
            {
                *slot = Value::Array(paths.iter().take(100).cloned().collect());
            }
            if let (Some(dst), Some(src)) = (
                target.get_mut("jellyfin").and_then(Value::as_object_mut),
                source.get("jellyfin").and_then(Value::as_object),
            ) {
                for key in [
                    "enabled",
                    "movie_libraries",
                    "series_libraries",
                    "refresh_movies",
                    "refresh_series",
                    "refresh_mode",
                ] {
                    if let Some(v) = src.get(key) {
                        dst.insert(key.into(), v.clone());
                    }
                }
            }
        }
        Ok(Self {
            settings_path,
            settings,
            bazarr_pending_paths: BTreeSet::new(),
        })
    }

    fn conf(&self, provider: &str) -> &Value {
        &self.settings[provider]
    }
    fn persist(&self, settings: &Value) -> Result<(), String> {
        let mut saved = settings.clone();
        if let Some(jf) = saved.get_mut("jellyfin").and_then(Value::as_object_mut) {
            jf.remove("url");
            jf.remove("api_key");
        }
        crate::util::write_private(&self.settings_path, &saved)
    }
    pub fn public_settings(&self) -> Value {
        let mut result = self.settings.clone();
        for provider in PROVIDERS {
            let conf = &mut result[provider];
            let has = !get_str(conf, "api_key").is_empty();
            if let Some(obj) = conf.as_object_mut() {
                obj.remove("api_key");
                obj.insert("api_key".into(), json!(""));
                obj.insert("api_key_set".into(), json!(has));
            }
        }
        result["jellyfin"]["env_managed"] = json!(true);
        result
    }
    pub fn update(&mut self, data: &Value) -> Result<Value, String> {
        let input = data.as_object().ok_or("Settings must be an object")?;
        let mut candidate = self.settings.clone();
        for provider in PROVIDERS {
            let Some(incoming) = input.get(provider) else {
                continue;
            };
            let inc = incoming
                .as_object()
                .ok_or("Integration settings must be objects")?;
            if provider == "jellyfin" {
                if let Some(url) = inc.get("url").and_then(Value::as_str) {
                    let current = env::var("JELLYFIN_INTERNAL_URL")
                        .unwrap_or_else(|_| "http://host.docker.internal:8096".into());
                    if url.trim_end_matches('/').to_lowercase()
                        != current.trim_end_matches('/').to_lowercase()
                    {
                        return Err("Jellyfin URL is managed by environment variables".into());
                    }
                }
                if inc
                    .get("api_key")
                    .is_some_and(|v| !v.as_str().unwrap_or("").is_empty())
                    || inc.get("clear_api_key").and_then(Value::as_bool) == Some(true)
                {
                    return Err("Jellyfin API key is managed by environment variables".into());
                }
            }
            if let Some(v) = inc.get("enabled") {
                candidate[provider]["enabled"] = json!(v.as_bool().unwrap_or(false));
            }
            if provider != "jellyfin" {
                if let Some(v) = inc.get("url") {
                    let url = if v.is_null() {
                        ""
                    } else {
                        v.as_str()
                            .ok_or("Enter a valid HTTP or HTTPS server URL")?
                            .trim()
                    };
                    let next = if url.is_empty() {
                        String::new()
                    } else {
                        validate_base(url)?
                    };
                    if get_str(&candidate[provider], "url").trim_end_matches('/')
                        != next.trim_end_matches('/')
                    {
                        candidate[provider]["api_key"] = json!("");
                    }
                    candidate[provider]["url"] = json!(next);
                }
                if inc.get("clear_api_key").and_then(Value::as_bool) == Some(true) {
                    candidate[provider]["api_key"] = json!("");
                } else if let Some(key) = inc
                    .get("api_key")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    if !valid_key(key) {
                        return Err("API key contains unsupported characters".into());
                    }
                    candidate[provider]["api_key"] = json!(key);
                }
            } else {
                for field in ["movie_libraries", "series_libraries"] {
                    if let Some(list) = inc.get(field) {
                        let vals = list.as_array().ok_or("Library selections must be a list")?;
                        if vals.len() > 100 {
                            return Err("Library selections must be a list".into());
                        }
                        candidate[provider][field] = Value::Array(
                            vals.iter()
                                .map(|v| {
                                    json!(
                                        v.as_str()
                                            .unwrap_or("")
                                            .chars()
                                            .take(128)
                                            .collect::<String>()
                                    )
                                })
                                .collect(),
                        );
                    }
                }
                for field in ["refresh_movies", "refresh_series"] {
                    if let Some(v) = inc.get(field) {
                        candidate[provider][field] = json!(v.as_bool().unwrap_or(false));
                    }
                }
                if let Some(v) = inc.get("refresh_mode") {
                    let mode = v.as_str().unwrap_or("");
                    if !["immediate", "async"].contains(&mode) {
                        return Err("Unsupported Jellyfin refresh mode".into());
                    }
                    candidate[provider]["refresh_mode"] = json!(mode);
                }
            }
        }
        if let Some(mappings) = input.get("path_mappings") {
            let list = mappings.as_array().ok_or("Path mappings must be a list")?;
            if list.len() > 100 {
                return Err("Path mappings must be a list".into());
            }
            let mut safe = Vec::new();
            for entry in list {
                let m = entry.as_object().ok_or("Invalid path mapping")?;
                let remote = get_str(entry, "remote");
                let local = get_str(entry, "local");
                if norm_path(remote).is_none() || norm_path(local).is_none() {
                    return Err("Path mappings must use absolute paths".into());
                }
                let prov = m.get("provider").and_then(Value::as_str);
                if prov.is_some_and(|p| !PROVIDERS.contains(&p)) {
                    return Err(
                        "Path mapping provider must be Sonarr, Radarr, Bazarr, or Jellyfin".into(),
                    );
                }
                let mut row = json!({"remote":remote.trim_end_matches(['/', '\\']),"local":local.trim_end_matches(['/', '\\'])});
                if let Some(p) = prov {
                    row["provider"] = json!(p);
                }
                safe.push(row);
            }
            candidate["path_mappings"] = Value::Array(safe);
        }
        self.persist(&candidate)?;
        self.settings = candidate;
        Ok(self.public_settings())
    }
    pub fn map_path(&self, provider: &str, path: &str) -> Option<String> {
        self.map(path, provider, true)
    }
    pub fn remote_path(&self, provider: &str, path: &str) -> Option<String> {
        self.map(path, provider, false)
            .or_else(|| norm_path(path).map(|_| path.into()))
    }
    fn map(&self, path: &str, provider: &str, remote_to_local: bool) -> Option<String> {
        if !PROVIDERS.contains(&provider) {
            return None;
        }
        let p = norm_path(path)?;
        let mappings = self.settings["path_mappings"].as_array()?;
        let mut selected: Option<(usize, String, String)> = None;
        for item in mappings {
            if item
                .get("provider")
                .and_then(Value::as_str)
                .is_some_and(|v| v != provider)
            {
                continue;
            }
            let (source, target) = if remote_to_local {
                (get_str(item, "remote"), get_str(item, "local"))
            } else {
                (get_str(item, "local"), get_str(item, "remote"))
            };
            let Some(root) = norm_path(source) else {
                continue;
            };
            if under(&p, &root)
                && selected
                    .as_ref()
                    .is_none_or(|(length, _, _)| root.value.len() > *length)
            {
                selected = Some((root.value.len(), source.into(), target.into()));
            }
        }
        let (_, source, target) = selected?;
        join_map(&p, &norm_path(&source)?, &target)
    }
    pub fn webhook_paths(provider: &str, payload: &Value) -> Vec<String> {
        if !["sonarr", "radarr"].contains(&provider) {
            return vec![];
        }
        let event = get_str(payload, "eventType").to_ascii_lowercase();
        if !["download", "upgrade", "rename"].contains(&event.as_str()) {
            return vec![];
        }
        let list_key = if provider == "sonarr" {
            "renamedEpisodeFiles"
        } else {
            "renamedMovieFiles"
        };
        if event == "rename"
            && let Some(list) = payload.get(list_key).and_then(Value::as_array)
        {
            return list
                .iter()
                .filter_map(|v| v.get("path").and_then(Value::as_str).map(str::to_owned))
                .collect();
        }
        if provider == "sonarr"
            && let Some(list) = payload.get("episodeFiles").and_then(Value::as_array)
        {
            return list
                .iter()
                .filter_map(|v| v.get("path").and_then(Value::as_str).map(str::to_owned))
                .collect();
        }
        let key = if provider == "sonarr" {
            "episodeFile"
        } else {
            "movieFile"
        };
        payload
            .get(key)
            .and_then(|v| v.get("path"))
            .and_then(Value::as_str)
            .map(|s| vec![s.into()])
            .unwrap_or_default()
    }
    fn api(
        &self,
        provider: &str,
        path: &str,
        method: &str,
        body: Option<&Value>,
        form: Option<&[(&str, String)]>,
    ) -> Result<Value, HttpFailure> {
        let conf = self.conf(provider);
        let auth = if provider == "jellyfin" {
            Auth::Jellyfin
        } else {
            Auth::ApiKey
        };
        request(
            get_str(conf, "url"),
            path,
            method,
            get_str(conf, "api_key"),
            auth,
            body,
            form,
        )
    }
    pub fn test(&self, provider: &str) -> Value {
        if !PROVIDERS.contains(&provider) {
            return json!({"connected":false,"message":"Unknown integration"});
        }
        let c = self.conf(provider);
        if get_str(c, "url").is_empty() || get_str(c, "api_key").is_empty() {
            return json!({"connected":false,"message":"Configure the server URL and API key"});
        }
        let path = match provider {
            "sonarr" | "radarr" => "/api/v3/system/status",
            "bazarr" => "/api/system/status",
            _ => "/System/Info",
        };
        match self.api(provider, path, "GET", None, None) {
            Ok(data) => {
                let data = if provider == "bazarr" {
                    &data["data"]
                } else {
                    &data
                };
                if !data.is_object() {
                    return json!({"connected":false,"message":"Connection failed; check the URL, API key, and service logs"});
                }
                let version = data
                    .get("version")
                    .or_else(|| data.get("Version"))
                    .or_else(|| data.get("bazarr_version"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                json!({"connected":true,"version":version.chars().take(64).collect::<String>(),"message":"Connected"})
            }
            Err(e) => json!({"connected":false,"message":e.message("Connection failed")}),
        }
    }
    pub fn libraries(&self) -> Result<Value, String> {
        let c = self.conf("jellyfin");
        if get_str(c, "url").is_empty() || get_str(c, "api_key").is_empty() {
            return Err("Configure Jellyfin before loading libraries".into());
        }
        let data = self
            .api("jellyfin", "/Library/VirtualFolders", "GET", None, None)
            .map_err(|e| e.message("Connection failed"))?;
        let Some(folders) = data.as_array() else {
            return Ok(json!([]));
        };
        Ok(Value::Array(folders.iter().filter_map(|f|{let f=f.as_object()?; let coll=f.get("CollectionType").and_then(Value::as_str).unwrap_or("").to_lowercase(); let kind=match coll.as_str(){"movies"=>"movies","tvshows"|"tv"=>"series",_=>"other"}; Some(json!({"id":f.get("ItemId").and_then(Value::as_str).unwrap_or(""),"name":f.get("Name").and_then(Value::as_str).unwrap_or("").chars().take(128).collect::<String>(),"type":kind,"locations":f.get("Locations").and_then(Value::as_array).cloned().unwrap_or_default()}))}).collect()))
    }
    pub fn notify(&mut self, paths: &[String]) -> Vec<Value> {
        for path in paths.iter().take(MAX_ITEMS) {
            if self.bazarr_pending_paths.len() < MAX_ITEMS
                && path.len() <= 4096
                && norm_path(path).is_some()
            {
                self.bazarr_pending_paths.insert(path.clone());
            }
        }
        let mut results = Vec::new();
        for provider in ["sonarr", "radarr", "jellyfin"] {
            let c = self.conf(provider);
            if !enabled(c) || get_str(c, "url").is_empty() || get_str(c, "api_key").is_empty() {
                continue;
            }
            let (ok, count, message) = if provider == "jellyfin" {
                let n = self.jellyfin_refresh(paths, false, &AtomicBool::new(false), &mut |_| {});
                match n {
                    Ok(n) => (
                        true,
                        n,
                        if n > 0 {
                            "Scan requested"
                        } else {
                            "No matching media item"
                        }
                        .to_owned(),
                    ),
                    Err(m) => (false, 0, m),
                }
            } else {
                match self.arr_refresh(provider, paths) {
                    Ok(n) => (
                        true,
                        n,
                        if n > 0 {
                            "Scan requested"
                        } else {
                            "No matching media item"
                        }
                        .to_owned(),
                    ),
                    Err(m) => (false, 0, m),
                }
            };
            results.push(json!({"provider":provider,"ok":ok,"triggered":count,"message":message}));
        }
        results
    }
    fn arr_refresh(&self, provider: &str, paths: &[String]) -> Result<usize, String> {
        let endpoint = if provider == "sonarr" {
            "/api/v3/series"
        } else {
            "/api/v3/movie"
        };
        let data = self
            .api(provider, endpoint, "GET", None, None)
            .map_err(|e| e.message("Scan request failed"))?;
        let mut ids = BTreeSet::new();
        if let Some(entities) = data.as_array() {
            for entity in entities {
                let Some(id) = entity.get("id").and_then(Value::as_i64) else {
                    continue;
                };
                let Some(root) = entity
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(norm_path)
                else {
                    continue;
                };
                if paths
                    .iter()
                    .filter_map(|p| self.remote_path(provider, p))
                    .filter_map(|p| norm_path(&p))
                    .any(|p| under(&p, &root))
                {
                    ids.insert(id);
                }
            }
        }
        let cmd = if provider == "sonarr" {
            "RescanSeries"
        } else {
            "RescanMovie"
        };
        for id in &ids {
            let mut body = json!({"name":cmd});
            body[if provider == "sonarr" {
                "seriesId"
            } else {
                "movieId"
            }] = json!(id);
            self.api(provider, "/api/v3/command", "POST", Some(&body), None)
                .map_err(|e| e.message("Scan request failed"))?;
        }
        Ok(ids.len())
    }
    fn jellyfin_refresh(
        &self,
        paths: &[String],
        full: bool,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(Value),
    ) -> Result<usize, String> {
        let conf = self.conf("jellyfin");
        let libs = self.libraries()?;
        let selected: BTreeSet<String> = libs
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| {
                let kind = get_str(l, "type");
                let id = get_str(l, "id");
                let field = if kind == "movies" {
                    "movie_libraries"
                } else {
                    "series_libraries"
                };
                ["movies", "series"].contains(&kind)
                    && conf[field]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id)))
                    && conf[if kind == "movies" {
                        "refresh_movies"
                    } else {
                        "refresh_series"
                    }]
                    .as_bool()
                    .unwrap_or(true)
            })
            .map(|l| get_str(l, "id").to_owned())
            .collect();
        if selected.is_empty() {
            return Err("Select at least one Jellyfin library to scan".into());
        }
        let mut count = 0;
        for library in selected {
            let Some(folder) = libs
                .as_array()
                .into_iter()
                .flatten()
                .find(|l| get_str(l, "id") == library)
            else {
                continue;
            };
            let mut start = 0;
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Ok(count);
                }
                let (query, scoped) = if full {
                    (
                        format!(
                            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Movie%2CEpisode&StartIndex={start}&Limit=200&EnableTotalRecordCount=false",
                            encode_component(&library)
                        ),
                        Vec::new(),
                    )
                } else {
                    let selected_paths: Vec<String> = paths
                        .iter()
                        .filter_map(|p| {
                            let normalized = norm_path(p)?;
                            let in_library = folder
                                .get("locations")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .any(|loc| {
                                    let mapped = self
                                        .map_path("jellyfin", loc)
                                        .unwrap_or_else(|| loc.into());
                                    norm_path(&mapped).is_some_and(|root| under(&normalized, &root))
                                });
                            in_library.then_some(p.clone())
                        })
                        .collect();
                    (String::new(), selected_paths)
                };
                if !full {
                    if get_str(conf, "refresh_mode") == "async" && !scoped.is_empty() {
                        if stop.load(Ordering::Relaxed) {
                            return Ok(count);
                        }
                        let updates: Vec<Value> = scoped
                            .iter()
                            .map(|sidecar| {
                                let path = self
                                    .remote_path("jellyfin", sidecar)
                                    .unwrap_or_else(|| sidecar.clone());
                                json!({"Path":path,"UpdateType":"Created"})
                            })
                            .collect();
                        let payload = json!({"Updates":updates});
                        self.api(
                            "jellyfin",
                            "/Library/Media/Updated",
                            "POST",
                            Some(&payload),
                            None,
                        )
                        .map_err(|e| e.message("Scan request failed"))?;
                        count += scoped.len();
                        break;
                    }
                    for sidecar in scoped {
                        if stop.load(Ordering::Relaxed) {
                            return Ok(count);
                        }
                        let remote = self
                            .remote_path("jellyfin", &sidecar)
                            .unwrap_or(sidecar.clone());
                        let Some(normalized) = norm_path(&remote) else {
                            continue;
                        };
                        let stem = path_stem(&normalized.value);
                        if stem.is_empty() {
                            continue;
                        }
                        let query = format!(
                            "/Items?ParentId={}&Recursive=true&Fields=Path&SearchTerm={}&Limit=100&EnableTotalRecordCount=false",
                            encode_component(&library),
                            encode_component(&stem)
                        );
                        let response = self
                            .api("jellyfin", &query, "GET", None, None)
                            .map_err(|e| e.message("Scan request failed"))?;
                        let items = response
                            .get("Items")
                            .or_else(|| response.get("items"))
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let target_parent = parent(&normalized);
                        let mut matched = false;
                        for item in items {
                            let Some(id) = item
                                .get("Id")
                                .or_else(|| item.get("id"))
                                .and_then(Value::as_str)
                            else {
                                continue;
                            };
                            let Some(item_path) = item
                                .get("Path")
                                .or_else(|| item.get("path"))
                                .and_then(Value::as_str)
                                .and_then(norm_path)
                            else {
                                continue;
                            };
                            if !target_parent
                                .as_ref()
                                .zip(parent(&item_path).as_ref())
                                .is_some_and(|(a, b)| path_eq(a, b))
                                || !path_case_eq(
                                    &path_stem(&item_path.value),
                                    &stem,
                                    item_path.flavor,
                                )
                            {
                                continue;
                            }
                            self.jellyfin_refresh_item(id)?;
                            count += 1;
                            matched = true;
                        }
                        if !matched {
                            let payload =
                                json!({"Updates":[{"Path":remote,"UpdateType":"Created"}]});
                            self.api(
                                "jellyfin",
                                "/Library/Media/Updated",
                                "POST",
                                Some(&payload),
                                None,
                            )
                            .map_err(|e| e.message("Scan request failed"))?;
                            count += 1;
                        }
                    }
                    break;
                }
                let page = self
                    .api("jellyfin", &query, "GET", None, None)
                    .map_err(|e| e.message("Scan request failed"))?;
                let items = page
                    .get("Items")
                    .or_else(|| page.get("items"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for item in &items {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(count);
                    }
                    let Some(id) = item
                        .get("Id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    self.jellyfin_refresh_item(id)?;
                    count += 1;
                    progress(
                        json!({"status":"refreshing","path":"jellyfin","output":item.get("Name").and_then(Value::as_str).unwrap_or(id)}),
                    );
                }
                if items.len() < 200 {
                    break;
                }
                start += items.len();
                if start >= MAX_ITEMS {
                    return Err("Library refresh reached its 20000-item limit; narrow the selected libraries".into());
                }
            }
        }
        Ok(count)
    }
    fn jellyfin_refresh_item(&self, id: &str) -> Result<(), String> {
        let path = format!(
            "/Items/{}/Refresh?MetadataRefreshMode=ValidationOnly&ImageRefreshMode=ValidationOnly&ReplaceAllMetadata=false&ReplaceAllImages=false",
            encode_component(id)
        );
        self.api("jellyfin", &path, "POST", None, None)
            .map(|_| ())
            .map_err(|e| e.message("Scan request failed"))
    }
    pub fn refresh(
        &self,
        provider: &str,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(Value),
    ) -> Value {
        if !PROVIDERS.contains(&provider) {
            return json!({"provider":provider,"ok":false,"message":"Unknown integration"});
        }
        let c = self.conf(provider);
        if get_str(c, "url").is_empty() || get_str(c, "api_key").is_empty() {
            return json!({"provider":provider,"ok":false,"message":"Configure the server URL and API key"});
        }
        if provider == "jellyfin" {
            return match self.jellyfin_refresh(&[], true, stop, progress) {
                Ok(_) if stop.load(Ordering::Relaxed) => {
                    json!({"provider":provider,"ok":true,"message":"Refresh cancelled"})
                }
                Ok(n) => {
                    json!({"provider":provider,"ok":true,"message":format!("Scan requested for {n} media items")})
                }
                Err(m) => json!({"provider":provider,"ok":false,"message":m}),
            };
        }
        if provider == "bazarr" {
            return self.bazarr_refresh(stop, progress);
        }
        let command = if provider == "sonarr" {
            "RescanSeries"
        } else {
            "RescanMovie"
        };
        let body = json!({"name":command});
        match self.api(provider, "/api/v3/command", "POST", Some(&body), None) {
            Ok(_) => json!({"provider":provider,"ok":true,"message":"Scan requested"}),
            Err(e) => {
                json!({"provider":provider,"ok":false,"message":e.message("Scan request failed")})
            }
        }
    }
    fn bazarr_refresh(&self, stop: &AtomicBool, progress: &mut dyn FnMut(Value)) -> Value {
        let response = match self.api("bazarr", "/api/system/tasks", "GET", None, None) {
            Ok(v) => v,
            Err(e) => {
                return json!({"provider":"bazarr","ok":false,"message":e.message("Scan request failed")});
            }
        };
        let tasks = response
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let wanted = [
            "update_series",
            "update_movies",
            "series_full_scan_subtitles",
            "movies_full_scan_subtitles",
        ];
        let mut queued = 0;
        for id in wanted {
            if stop.load(Ordering::Relaxed) {
                return json!({"provider":"bazarr","ok":true,"message":"Bazarr refresh cancelled before all tasks were queued"});
            }
            let Some(task) = tasks.iter().find(|v| get_str(v, "job_id") == id) else {
                continue;
            };
            if task.get("job_running").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            progress(
                json!({"status":"refreshing","path":format!("Bazarr · {}",get_str(task,"name"))}),
            );
            let form = [("taskid", id.to_owned())];
            if let Err(e) = self.api("bazarr", "/api/system/tasks", "POST", None, Some(&form)) {
                return json!({"provider":"bazarr","ok":false,"message":e.message("Bazarr scan failed")});
            }
            queued += 1;
        }
        if queued == 0 {
            json!({"provider":"bazarr","ok":true,"message":"Bazarr sync and subtitle-index tasks are already running"})
        } else {
            json!({"provider":"bazarr","ok":true,"message":format!("Queued {queued} Bazarr sync and subtitle-index task(s)")})
        }
    }
    pub fn flush_bazarr(
        &mut self,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(Value),
    ) -> Option<Value> {
        let paths: Vec<String> = std::mem::take(&mut self.bazarr_pending_paths)
            .into_iter()
            .collect();
        if paths.is_empty() {
            return None;
        }
        let conf = self.conf("bazarr");
        if !enabled(conf) || get_str(conf, "url").is_empty() || get_str(conf, "api_key").is_empty()
        {
            return Some(
                json!({"provider":"bazarr","ok":true,"triggered":0,"message":"Bazarr integration is disabled"}),
            );
        }
        if stop.load(Ordering::Relaxed) {
            return Some(
                json!({"provider":"bazarr","ok":true,"triggered":0,"message":"Bazarr scan cancelled"}),
            );
        }
        let result = (|| -> Result<usize, String> {
            let mut series_ids = BTreeSet::new();
            let mut movie_ids = BTreeSet::new();
            for (kind, endpoint) in [("series", "/api/series"), ("movie", "/api/movies")] {
                let mut start = 0;
                let mut records = Vec::new();
                while start < MAX_ITEMS {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(0);
                    }
                    let path = format!("{endpoint}?start={start}&length=200");
                    let page = self
                        .api("bazarr", &path, "GET", None, None)
                        .map_err(|e| e.message("Bazarr scan failed"))?;
                    let data = page
                        .get("data")
                        .and_then(Value::as_array)
                        .ok_or("Bazarr returned invalid media data")?;
                    records.extend(data.iter().take(MAX_ITEMS - records.len()).cloned());
                    let total = page.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
                    if records.len() >= total || data.len() < 200 {
                        break;
                    }
                    start += data.len();
                }
                if records.len() >= MAX_ITEMS {
                    return Err("Bazarr media list exceeds the configured scan limit".into());
                }
                for path in &paths {
                    let remote = self
                        .remote_path("bazarr", path)
                        .unwrap_or_else(|| path.clone());
                    let Some(norm) = norm_path(&remote) else {
                        continue;
                    };
                    if kind == "movie" {
                        for movie in &records {
                            let Some(id) = movie.get("radarrId").and_then(Value::as_i64) else {
                                continue;
                            };
                            let Some(mp) = movie
                                .get("path")
                                .and_then(Value::as_str)
                                .and_then(norm_path)
                            else {
                                continue;
                            };
                            if path_case_eq(
                                &path_stem(&norm.value),
                                &path_stem(&mp.value),
                                mp.flavor,
                            ) && parent(&norm)
                                .zip(parent(&mp))
                                .is_some_and(|(a, b)| path_eq(&a, &b))
                            {
                                movie_ids.insert(id);
                            }
                        }
                    } else {
                        let mut best: Option<(usize, i64)> = None;
                        for s in &records {
                            let Some(id) = s.get("sonarrSeriesId").and_then(Value::as_i64) else {
                                continue;
                            };
                            let Some(root) =
                                s.get("path").and_then(Value::as_str).and_then(norm_path)
                            else {
                                continue;
                            };
                            if under(&norm, &root)
                                && best.as_ref().is_none_or(|(n, _)| root.value.len() > *n)
                            {
                                best = Some((root.value.len(), id));
                            }
                        }
                        if let Some((_, id)) = best {
                            series_ids.insert(id);
                        }
                    }
                }
            }
            let mut count = 0;
            for (kind, id) in series_ids
                .iter()
                .map(|id| ("series", id))
                .chain(movie_ids.iter().map(|id| ("movie", id)))
            {
                if stop.load(Ordering::Relaxed) {
                    return Ok(count);
                }
                progress(json!({"status":"refreshing","path":format!("Bazarr · {kind} {id}")}));
                let (endpoint, field) = if kind == "series" {
                    ("/api/series", "seriesid")
                } else {
                    ("/api/movies", "radarrid")
                };
                for action in ["sync", "scan-disk"] {
                    let fields = [(field, id.to_string()), ("action", action.to_owned())];
                    self.api("bazarr", endpoint, "PATCH", None, Some(&fields))
                        .map_err(|e| e.message("Bazarr scan failed"))?;
                }
                count += 1;
            }
            Ok(count)
        })();
        Some(match result {
            Ok(count) => {
                json!({"provider":"bazarr","ok":true,"triggered":count,"message":if count>0{format!("Scanned {count} Bazarr item(s)")}else{"No matching Bazarr media item".into()}})
            }
            Err(message) => json!({"provider":"bazarr","ok":false,"triggered":0,"message":message}),
        })
    }
}

fn parent(path: &NormPath) -> Option<NormPath> {
    let idx = path.value.rfind('/')?;
    let value = if idx == 0 {
        "/".into()
    } else {
        path.value[..idx].into()
    };
    Some(NormPath {
        value,
        flavor: path.flavor,
    })
}
fn path_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut s = name
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(name)
        .to_owned();
    loop {
        let lower = s.to_ascii_lowercase();
        if let Some(suffix) = [".forced", ".hi", ".sdh", ".cc"]
            .iter()
            .find(|v| lower.ends_with(**v))
        {
            s.truncate(s.len() - suffix.len());
            continue;
        }
        let Some((base, tag)) = s.rsplit_once('.') else {
            break;
        };
        let lower_tag = tag.to_ascii_lowercase();
        let is_track = lower_tag
            .strip_prefix("track")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let lang_parts: Vec<&str> = tag.split('-').collect();
        let is_language = (2..=3).contains(&lang_parts[0].len())
            && lang_parts[0].bytes().all(|b| b.is_ascii_alphabetic())
            && lang_parts.iter().skip(1).all(|p| {
                (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric())
            });
        if is_track || is_language {
            s = base.to_owned();
            continue;
        }
        break;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn fixture(status: &str, body: &str, expected_auth: Option<&str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_owned();
        let status = status.to_owned();
        let auth = expected_auth.map(str::to_owned);
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = vec![0u8; 8192];
                let n = stream.read(&mut request).unwrap_or(0);
                let text = String::from_utf8_lossy(&request[..n]);
                if let Some(expected) = auth {
                    assert!(
                        text.lines().any(|line| {
                            line.split_once(':').is_some_and(|(name, value)| {
                                name.eq_ignore_ascii_case("authorization")
                                    && value.trim() == expected
                            })
                        }),
                        "authorization header missing"
                    );
                }
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn fixture_sequence(
        count: usize,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = seen.clone();
        let handle = thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 2048];
                let mut expected_len = None;
                loop {
                    let Ok(n) = stream.read(&mut chunk) else {
                        break;
                    };
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if expected_len.is_none()
                        && let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&bytes[..pos]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        expected_len = Some(pos + 4 + length);
                    }
                    if expected_len.is_some_and(|needed| bytes.len() >= needed) {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes).to_string();
                output.lock().unwrap().push(request.clone());
                let body = if request.starts_with("GET /api/v3/series ") {
                    r#"[{"id":12,"path":"/tv/Show"}]"#
                } else if request.starts_with("GET /Library/VirtualFolders ") {
                    r#"[{"ItemId":"movies","Name":"Movies","CollectionType":"movies","Locations":["/movies"]}]"#
                } else if request.starts_with("GET /api/series?") {
                    r#"{"data":[{"sonarrSeriesId":12,"path":"/tv/Show"}],"total":1}"#
                } else if request.starts_with("GET /api/movies?") {
                    r#"{"data":[{"radarrId":34,"path":"/movies/Film.mkv"},{"radarrId":35,"path":"/movies/film.mkv"}],"total":2}"#
                } else {
                    "{}"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{addr}"), seen, handle)
    }
    fn temp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        (dir, path)
    }

    #[test]
    fn settings_mask_keys_and_keep_jellyfin_environment_only() {
        let (_d, p) = temp();
        let mut i = Integrations::new(p.clone()).unwrap();
        i.update(&json!({"sonarr":{"url":"http://sonarr:8989","api_key":"private"},"jellyfin":{"enabled":true}})).unwrap();
        let public = i.public_settings();
        let public_json = serde_json::to_string(&public).unwrap();
        assert!(!public_json.contains("private"));
        assert_eq!(get_str(&public["sonarr"], "api_key"), "");
        assert!(public["jellyfin"]["env_managed"].as_bool().unwrap());
        let persisted = std::fs::read_to_string(p).unwrap();
        let saved: Value = serde_json::from_str(&persisted).unwrap();
        assert_eq!(get_str(&saved["sonarr"], "api_key"), "private");
        assert!(saved["jellyfin"].get("url").is_none());
        assert!(saved["jellyfin"].get("api_key").is_none());
    }
    #[test]
    fn oversized_or_symlinked_settings_fail_closed_to_disabled_integrations() {
        let (_d, p) = temp();
        let padding = "x".repeat(65 * 1024);
        let oversized = format!(
            r#"{{"sonarr":{{"enabled":true,"url":"http://sonarr:8989","api_key":"secret"}},"padding":"{padding}"}}"#
        );
        std::fs::write(&p, oversized).unwrap();
        let loaded = Integrations::new(p.clone()).unwrap();
        assert!(!enabled(loaded.conf("sonarr")));

        #[cfg(unix)]
        {
            let target = p.with_extension("target.json");
            std::fs::write(
                &target,
                r#"{"sonarr":{"enabled":true,"url":"http://sonarr:8989","api_key":"secret"}}"#,
            )
            .unwrap();
            std::fs::remove_file(&p).unwrap();
            std::os::unix::fs::symlink(target, &p).unwrap();
            let loaded = Integrations::new(p).unwrap();
            assert!(!enabled(loaded.conf("sonarr")));
        }
    }
    #[test]
    fn updates_are_transactional_and_urls_reject_credentials() {
        let (_d, p) = temp();
        let mut i = Integrations::new(p).unwrap();
        let before = i.public_settings();
        assert!(i.update(&json!({"sonarr":{"enabled":true},"path_mappings":[{"remote":"relative","local":"/mnt"}]})).is_err());
        assert_eq!(before, i.public_settings());
        assert!(
            i.update(&json!({"radarr":{"url":"http://user:pass@example.invalid"}}))
                .is_err()
        );
    }
    #[test]
    fn path_mapping_has_case_aware_boundary_and_longest_prefix() {
        let (_d, p) = temp();
        let mut i = Integrations::new(p).unwrap();
        i.update(&json!({"path_mappings":[{"remote":"/data","local":"/mnt"},{"remote":"/data/tv","local":"/shows"},{"remote":"Z:\\Media","local":"D:\\Library"}]})).unwrap();
        assert_eq!(
            i.map_path("sonarr", "/data/tv/Show/e.mkv").as_deref(),
            Some("/shows/Show/e.mkv")
        );
        assert!(i.map_path("sonarr", "/data2/e.mkv").is_none());
        assert_eq!(
            i.map_path("radarr", "z:\\media\\Movie.mkv").as_deref(),
            Some("D:\\Library\\Movie.mkv")
        );
    }
    #[test]
    fn arr_notification_can_read_scoped_entities_and_post_rescan_command() {
        let (_d, p) = temp();
        let mut i = Integrations::new(p).unwrap();
        let (url, calls, server) = fixture_sequence(2);
        i.update(&json!({"sonarr":{"enabled":true,"url":url,"api_key":"sonarr-secret"}}))
            .unwrap();
        let result = i.notify(&["/tv/Show/S01/episode.en.srt".into()]);
        assert!(result[0]["ok"].as_bool().unwrap());
        assert_eq!(result[0]["triggered"], 1);
        server.join().unwrap();
        let calls = calls.lock().unwrap();
        assert!(calls[0].starts_with("GET /api/v3/series HTTP/1.1"));
        assert!(calls[1].starts_with("POST /api/v3/command HTTP/1.1"));
        assert!(calls[1].ends_with(r#"{"name":"RescanSeries","seriesId":12}"#));
    }
    #[test]
    fn webhook_parsing_filters_events_and_bad_entries() {
        assert_eq!(
            Integrations::webhook_paths(
                "sonarr",
                &json!({"eventType":"Download","episodeFiles":[{"path":"/tv/a"},null,{"path":7}]})
            ),
            vec!["/tv/a"]
        );
        assert!(Integrations::webhook_paths("radarr", &json!({"eventType":"Grab"})).is_empty());
    }
    #[test]
    fn modern_jellyfin_auth_header_and_response_are_supported() {
        let key = "a secret+/token";
        let encoded = encode_component(key);
        let header = format!(
            "MediaBrowser Client=\"Subforge\", Device=\"Subtitle converter\", DeviceId=\"subforge\", Version=\"1\", Token=\"{encoded}\""
        );
        let url = fixture(
            "200 OK",
            r#"{"ServerName":"Jellyfin","Version":"12.0"}"#,
            Some(&header),
        );
        let got = request(&url, "/System/Info", "GET", key, Auth::Jellyfin, None, None).unwrap();
        assert_eq!(got["Version"], "12.0");
    }
    #[test]
    fn authentication_failures_are_sanitized() {
        let url = fixture("401 Unauthorized", r#"{"error":"secret payload"}"#, None);
        let err = request(
            &url,
            "/System/Info",
            "GET",
            "secret",
            Auth::Jellyfin,
            None,
            None,
        )
        .unwrap_err();
        let message = err.message("Connection failed");
        assert!(message.contains("HTTP 401"));
        assert!(!message.contains("secret payload"));
        assert!(!message.contains(&url));
    }
    #[test]
    fn bazarr_batch_uses_authenticated_scoped_form_patches_and_clears_batch() {
        let (_d, p) = temp();
        let mut i = Integrations::new(p).unwrap();
        let (url, requests, server) = fixture_sequence(4);
        i.update(&json!({"bazarr":{"enabled":true,"url":url,"api_key":"secret"}}))
            .unwrap();
        i.notify(&["/tv/Show/S01/ep.en.srt".into()]);
        let (stop, mut progress) = (AtomicBool::new(false), |_: Value| {});
        let result = i.flush_bazarr(&stop, &mut progress).unwrap();
        assert_eq!(result["triggered"], 1);
        assert!(i.bazarr_pending_paths.is_empty());
        server.join().unwrap();
        let calls = requests.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert!(calls[0].starts_with("GET /api/series?start=0&length=200 HTTP/1.1"));
        assert!(calls[1].starts_with("GET /api/movies?start=0&length=200 HTTP/1.1"));
        let patches: Vec<&String> = calls
            .iter()
            .filter(|call| call.starts_with("PATCH "))
            .collect();
        assert_eq!(patches.len(), 2);
        assert!(patches[0].starts_with("PATCH /api/series HTTP/1.1"));
        assert!(
            patches[0]
                .to_ascii_lowercase()
                .contains("x-api-key: secret")
        );
        assert!(patches[0].ends_with("seriesid=12&action=sync"));
        assert!(patches[1].ends_with("seriesid=12&action=scan-disk"));
    }
    #[test]
    fn bazarr_cancel_consumes_queued_batch_before_any_network_request() {
        let (_d, p) = temp();
        let mut i = Integrations::new(p).unwrap();
        i.update(&json!({"bazarr":{"enabled":true,"url":"http://127.0.0.1:1","api_key":"secret"}}))
            .unwrap();
        i.notify(&["/tv/Show/ep.en.srt".into()]);
        let stop = AtomicBool::new(true);
        let mut progress = |_| {};
        let result = i.flush_bazarr(&stop, &mut progress).unwrap();
        assert_eq!(result["message"], "Bazarr scan cancelled");
        assert!(i.bazarr_pending_paths.is_empty());
        assert!(i.flush_bazarr(&stop, &mut progress).is_none());
    }
    #[test]
    fn bazarr_movie_matching_preserves_posix_case() {
        let (_d, p) = temp();
        let mut i = Integrations::new(p).unwrap();
        let (url, requests, server) = fixture_sequence(4);
        i.update(&json!({"bazarr":{"enabled":true,"url":url,"api_key":"secret"}}))
            .unwrap();
        i.notify(&["/movies/Film.en.srt".into()]);
        let (stop, mut progress) = (AtomicBool::new(false), |_: Value| {});
        let result = i.flush_bazarr(&stop, &mut progress).unwrap();
        assert_eq!(result["triggered"], 1);
        server.join().unwrap();
        let calls = requests.lock().unwrap();
        let patches: Vec<&String> = calls
            .iter()
            .filter(|call| call.starts_with("PATCH "))
            .collect();
        assert_eq!(patches.len(), 2);
        assert!(patches[0].ends_with("radarrid=34&action=sync"));
        assert!(patches[1].ends_with("radarrid=34&action=scan-disk"));
    }
    #[test]
    fn jellyfin_async_sidecar_refresh_posts_once_per_library_and_stops() {
        let (_d, p) = temp();
        let mut i = Integrations::new(p).unwrap();
        let (url, calls, server) = fixture_sequence(2);
        i.settings["jellyfin"]["enabled"] = json!(true);
        i.settings["jellyfin"]["url"] = json!(url);
        i.settings["jellyfin"]["api_key"] = json!("fixture-token");
        i.settings["jellyfin"]["movie_libraries"] = json!(["movies"]);
        i.settings["jellyfin"]["refresh_mode"] = json!("async");
        let stop = AtomicBool::new(false);
        let mut progress = |_: Value| {};
        let result = i
            .jellyfin_refresh(&["/movies/Film.en.srt".into()], false, &stop, &mut progress)
            .unwrap();
        assert_eq!(result, 1);
        server.join().unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].starts_with("POST /Library/Media/Updated HTTP/1.1"));
        assert!(
            calls[1]
                .contains(r#"{"Updates":[{"Path":"/movies/Film.en.srt","UpdateType":"Created"}]}"#)
        );

        let (url, calls, server) = fixture_sequence(1);
        i.settings["jellyfin"]["url"] = json!(url);
        let stop = AtomicBool::new(true);
        let cancelled = i
            .jellyfin_refresh(&["/movies/Film.en.srt".into()], false, &stop, &mut progress)
            .unwrap();
        assert_eq!(cancelled, 0);
        server.join().unwrap();
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}
