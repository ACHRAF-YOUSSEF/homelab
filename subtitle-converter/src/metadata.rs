use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub const LIMIT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 5_000;
const MAX_ENTRY_BYTES: usize = 128 * 1024;
pub const MAX_TRACKS: usize = 256;
const MAX_LABEL: usize = 32;
const REVISION: u64 = 1;

struct Entry {
    fingerprint: Value,
    tracks: Value,
    bytes: usize,
}

/// Serialized entry accounting, not an estimate of heap or process RSS.
/// Readers never acquire the cache or converter mutex.
#[derive(Default)]
pub struct CacheUsage {
    entries: AtomicUsize,
    bytes: AtomicUsize,
}

impl CacheUsage {
    pub fn snapshot(&self) -> Value {
        json!({"entries":self.entries.load(Ordering::Relaxed),
            "accounted_bytes":self.bytes.load(Ordering::Relaxed),
            "byte_accounting":"serialized_entries", "max_entries":MAX_ENTRIES,"max_bytes":LIMIT_BYTES})
    }
}

pub struct MetadataCache {
    path: PathBuf,
    entries: BTreeMap<String, Entry>,
    bytes: usize,
    dirty: bool,
    usage: Arc<CacheUsage>,
}

impl MetadataCache {
    pub fn new(path: PathBuf) -> Self {
        let mut cache = Self {
            path,
            entries: BTreeMap::new(),
            bytes: 0,
            dirty: false,
            usage: Arc::new(CacheUsage::default()),
        };
        let Ok(saved) = crate::util::read_json(&cache.path, LIMIT_BYTES) else {
            return cache;
        };
        let Some(object) = saved.as_object() else {
            return cache;
        };
        for (key, value) in object {
            if key.chars().count() > 4096 || cache.entries.len() >= MAX_ENTRIES {
                continue;
            }
            let Some(entry) = sanitize_entry(value) else {
                continue;
            };
            let size = entry_size(key, &entry);
            if size > MAX_ENTRY_BYTES || cache.bytes.saturating_add(size) > LIMIT_BYTES {
                continue;
            }
            cache.bytes += size;
            cache.entries.insert(
                key.clone(),
                Entry {
                    bytes: size,
                    ..entry
                },
            );
        }
        cache.publish_usage();
        cache
    }

    pub fn usage(&self) -> Arc<CacheUsage> {
        Arc::clone(&self.usage)
    }

    fn publish_usage(&self) {
        self.usage
            .entries
            .store(self.entries.len(), Ordering::Relaxed);
        self.usage.bytes.store(self.bytes, Ordering::Relaxed);
    }

    pub fn get(&self, key: &str, fingerprint: &Value) -> Option<Value> {
        let entry = self.entries.get(key)?;
        (entry.fingerprint == *fingerprint).then(|| entry.tracks.clone())
    }

    pub fn insert(&mut self, key: &str, fingerprint: Value, tracks: Value) {
        if key.is_empty() || key.chars().count() > 4096 {
            return;
        }
        let candidate = json!({"revision":REVISION,"fingerprint":fingerprint,"tracks":tracks});
        let Some(mut entry) = sanitize_entry(&candidate) else {
            return;
        };
        let size = entry_size(key, &entry);
        if size > MAX_ENTRY_BYTES {
            return;
        }
        if let Some(old) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        while self.entries.len() >= MAX_ENTRIES || self.bytes.saturating_add(size) > LIMIT_BYTES {
            let Some(oldest) = self.entries.keys().next().cloned() else {
                return;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(removed.bytes);
            }
        }
        entry.bytes = size;
        self.bytes += size;
        self.entries.insert(key.to_owned(), entry);
        self.publish_usage();
        self.dirty = true;
    }

    pub fn persist(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let mut object = Map::new();
        for (key, entry) in &self.entries {
            object.insert(
                key.clone(),
                json!({"revision":REVISION,"fingerprint":entry.fingerprint,"tracks":entry.tracks}),
            );
        }
        let value = Value::Object(object);
        let bytes =
            serde_json::to_vec(&value).map_err(|_| "Metadata cache could not be serialized")?;
        if bytes.len() > LIMIT_BYTES {
            return Err("Metadata cache exceeds its size limit".into());
        }
        crate::util::write_private(&self.path, &value)?;
        self.dirty = false;
        Ok(())
    }

    pub fn compact(&mut self, mut keep: impl FnMut(&str) -> bool) -> usize {
        let remove: Vec<_> = self
            .entries
            .keys()
            .filter(|key| !keep(key))
            .cloned()
            .collect();
        for key in &remove {
            if let Some(entry) = self.entries.remove(key) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
        if !remove.is_empty() {
            self.dirty = true;
            self.publish_usage();
        }
        remove.len()
    }
}

fn entry_size(key: &str, entry: &Entry) -> usize {
    let value = json!({"revision":REVISION,"fingerprint":entry.fingerprint,"tracks":entry.tracks});
    serde_json::to_vec(&value).map_or(usize::MAX, |bytes| {
        bytes.len().saturating_add(key.len()).saturating_add(4)
    })
}

fn sanitize_entry(value: &Value) -> Option<Entry> {
    let object = value.as_object()?;
    if object.len() != 3 || object.get("revision")?.as_u64()? != REVISION {
        return None;
    }
    let fingerprint = object.get("fingerprint")?.clone();
    let tracks = object.get("tracks")?.as_array()?;
    if tracks.len() > MAX_TRACKS {
        return None;
    }
    let sanitized: Vec<Value> = tracks.iter().map(sanitize_track).collect::<Option<_>>()?;
    Some(Entry {
        fingerprint,
        tracks: Value::Array(sanitized),
        bytes: 0,
    })
}

fn sanitize_track(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    const KEYS: [&str; 7] = [
        "number",
        "native_number",
        "stream_index",
        "language",
        "format",
        "forced",
        "hi",
    ];
    if object.len() != KEYS.len() || object.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return None;
    }
    let number = object.get("number")?.as_u64()?;
    let native_number = object.get("native_number")?;
    if !native_number.is_null() {
        let value = native_number.as_str()?;
        if value.is_empty()
            || value.len() > 20
            || !value.bytes().all(|b| b.is_ascii_digit())
            || value.parse::<u64>().is_err()
        {
            return None;
        }
    }
    let stream_index = object.get("stream_index")?;
    if !stream_index.is_null() && stream_index.as_u64().is_none() {
        return None;
    }
    let language = object.get("language")?.as_str()?;
    let format = object.get("format")?.as_str()?;
    if language.is_empty()
        || language.len() > MAX_LABEL
        || !language
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || format.is_empty()
        || format.len() > MAX_LABEL
        || !format.is_ascii()
    {
        return None;
    }
    if !format
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    let forced = object.get("forced")?.as_bool()?;
    let hi = object.get("hi")?.as_bool()?;
    Some(
        json!({"number":number,"native_number":native_number,"stream_index":stream_index,"language":language,"format":format,"forced":forced,"hi":hi}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn tracks() -> Value {
        json!([{"number":2,"native_number":"7","stream_index":4,"language":"eng","format":"STEXTUTF8","forced":false,"hi":true}])
    }

    #[test]
    fn persists_and_matches_fingerprint() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        let fingerprint = json!({"size":42,"mtime":10});
        let mut cache = MetadataCache::new(path.clone());
        cache.insert("/safe/movie.mkv", fingerprint.clone(), tracks());
        cache.persist().unwrap();
        let cache = MetadataCache::new(path);
        assert_eq!(cache.get("/safe/movie.mkv", &fingerprint), Some(tracks()));
        assert!(cache.get("/safe/movie.mkv", &json!({"size":43})).is_none());
    }

    #[test]
    fn more_than_sixty_four_tracks_survive_cache_persistence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        let many = json!(
            (0..100)
                .map(|number| {
                    let mut track = tracks()[0].clone();
                    track["number"] = json!(number);
                    track
                })
                .collect::<Vec<_>>()
        );
        let mut cache = MetadataCache::new(path.clone());
        cache.insert("fixture.mkv", json!(1), many.clone());
        cache.persist().unwrap();
        assert_eq!(
            MetadataCache::new(path).get("fixture.mkv", &json!(1)),
            Some(many)
        );
    }

    #[test]
    fn rejects_bad_revision_and_oversized_or_invalid_tracks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        let bad_track = json!([{"number":2,"native_number":null,"stream_index":null,"language":"eng","format":"bad format","forced":false,"hi":false}]);
        std::fs::write(&path, json!({"bad":{"revision":99,"fingerprint":{},"tracks":tracks()},"invalid":{"revision":1,"fingerprint":{},"tracks":bad_track}}).to_string()).unwrap();
        let cache = MetadataCache::new(path);
        assert!(cache.get("bad", &json!({})).is_none());
        assert!(cache.get("invalid", &json!({})).is_none());
        let mut cache = MetadataCache::new(dir.path().join("other.json"));
        cache.insert(
            "oversize",
            json!({}),
            json!(vec![tracks()[0].clone(); MAX_TRACKS + 1]),
        );
        assert!(cache.get("oversize", &json!({})).is_none());
    }

    #[test]
    fn compact_uses_authoritative_keep_set() {
        let dir = tempdir().unwrap();
        let mut cache = MetadataCache::new(dir.path().join("metadata.json"));
        cache.insert("/root/a.mkv", json!(1), tracks());
        cache.insert("/other/b.mkv", json!(1), tracks());
        assert_eq!(cache.compact(|key| key.starts_with("/root/")), 1);
        cache.persist().unwrap();
        assert!(cache.get("/root/a.mkv", &json!(1)).is_some());
        assert!(cache.get("/other/b.mkv", &json!(1)).is_none());
    }

    #[test]
    fn shared_usage_tracks_load_replace_eviction_and_compaction() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        let mut cache = MetadataCache::new(path.clone());
        let usage = cache.usage();
        assert_eq!(usage.snapshot()["entries"], 0);
        for index in 0..MAX_ENTRIES + 1 {
            cache.insert(&format!("/root/{index:05}.mkv"), json!(index), tracks());
        }
        assert_eq!(usage.snapshot()["entries"], MAX_ENTRIES);
        assert!(usage.snapshot()["accounted_bytes"].as_u64().unwrap() <= LIMIT_BYTES as u64);
        assert!(cache.get("/root/00000.mkv", &json!(0)).is_none());
        let previous_bytes = usage.snapshot()["accounted_bytes"].clone();
        cache.insert("/root/00001.mkv", json!(1), json!([]));
        assert!(usage.snapshot()["accounted_bytes"].as_u64() < previous_bytes.as_u64());
        cache.persist().unwrap();
        let loaded = MetadataCache::new(path);
        assert_eq!(loaded.usage().snapshot(), usage.snapshot());
        cache.compact(|_| false);
        assert_eq!(usage.snapshot()["entries"], 0);
        assert_eq!(usage.snapshot()["accounted_bytes"], 0);
    }
}
