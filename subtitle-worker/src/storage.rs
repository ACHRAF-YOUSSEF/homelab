use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn read_json(path: &Path, max_bytes: u64) -> Result<Value, String> {
    let file = File::open(path).map_err(|_| "Saved file is unavailable")?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read saved file")?;
    if bytes.len() as u64 > max_bytes {
        return Err("Saved JSON exceeds configured size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Saved JSON is invalid".into())
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing destination directory")?;
    fs::create_dir_all(parent).map_err(|_| "Could not create destination directory")?;
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "Could not create temporary file")?;
    temp.write_all(bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|_| "Could not save file")?;
    temp.persist(path)
        .map_err(|_| "Could not publish saved file")?;
    Ok(())
}

pub fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "Could not encode saved state")?;
    write_bytes(path, &bytes)
}

pub fn digest(value: &Value) -> String {
    Sha256::digest(serde_json::to_vec(value).expect("JSON values serialize"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn token() -> String {
    format!(
        "{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    )
}

pub fn now() -> String {
    // UTC civil date conversion; avoids a timezone database or date runtime.
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_atomic_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_json(&path, &serde_json::json!({"a":1})).unwrap();
        write_json(&path, &serde_json::json!({"a":2})).unwrap();
        assert_eq!(read_json(&path, 100).unwrap()["a"], 2);
        assert!(read_json(&path, 2).is_err());
    }
}
