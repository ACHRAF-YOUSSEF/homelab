use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Labels are fixed configuration names, never deployment paths.
#[derive(Clone, Copy, Debug)]
pub enum StorageRoot {
    Data,
    Output,
    Library,
    Model,
    Unspecified,
}

impl StorageRoot {
    fn label(self) -> &'static str {
        match self {
            Self::Data => "SUBTITLE_DATA_ROOT",
            Self::Output => "SUBTITLE_OUTPUT_ROOT",
            Self::Library => "SUBTITLE_LIBRARY_ROOT",
            Self::Model => "WHISPER_MODEL_ROOT",
            Self::Unspecified => "configured storage directory",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    CreateDirectory,
    ResolveDirectory,
    CreateTemporary,
    WriteTemporary,
    SyncTemporary,
    Publish,
    RemoveProbe,
}

impl Operation {
    fn label(self) -> &'static str {
        match self {
            Self::CreateDirectory => "create directory",
            Self::ResolveDirectory => "resolve directory",
            Self::CreateTemporary => "create temporary file",
            Self::WriteTemporary => "write temporary file",
            Self::SyncTemporary => "sync temporary file",
            Self::Publish => "atomically replace saved file",
            Self::RemoveProbe => "remove writable-directory check file",
        }
    }
}

/// Avoid Display for io::Error/PersistError: they may contain private paths.
pub(crate) fn io_error(root: StorageRoot, operation: Operation, error: &io::Error) -> String {
    let reason = match error.kind() {
        io::ErrorKind::PermissionDenied => {
            "permission denied; grant the runtime user directory access (container UID 1000 requires write and traversal access for data/output)"
        }
        io::ErrorKind::ReadOnlyFilesystem => {
            "read-only filesystem; data/output require writable mounts"
        }
        io::ErrorKind::StorageFull => "no space left on the filesystem",
        io::ErrorKind::QuotaExceeded => "filesystem storage quota exceeded",
        io::ErrorKind::NotFound => "directory or parent directory is missing",
        io::ErrorKind::AlreadyExists => "a conflicting file or directory already exists",
        io::ErrorKind::NotADirectory => "a path component is not a directory",
        io::ErrorKind::InvalidInput => "invalid filesystem operation or configured directory",
        _ => "filesystem operation failed",
    };
    let os_code = error
        .raw_os_error()
        .map(|code| format!("; OS error {code}"))
        .unwrap_or_default();
    format!(
        "{}: could not {}: {reason} ({:?}{os_code})",
        root.label(),
        operation.label(),
        error.kind()
    )
}

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
    write_bytes_for(path, bytes, StorageRoot::Unspecified)
}

pub fn write_bytes_for(path: &Path, bytes: &[u8], root: StorageRoot) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing destination directory")?;
    fs::create_dir_all(parent)
        .map_err(|error| io_error(root, Operation::CreateDirectory, &error))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| io_error(root, Operation::CreateTemporary, &error))?;
    temp.write_all(bytes)
        .map_err(|error| io_error(root, Operation::WriteTemporary, &error))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| io_error(root, Operation::SyncTemporary, &error))?;
    temp.persist(path)
        .map_err(|error| io_error(root, Operation::Publish, &error.error))?;
    Ok(())
}

/// Exercise the actual create/write/sync/replace/delete permissions required by
/// atomic persistence. Never probes media or preloaded model directories.
pub(crate) fn verify_writable(directory: &Path, root: StorageRoot) -> Result<(), String> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| io_error(root, Operation::CreateTemporary, &error))?;
    temporary
        .write_all(b"subtitle-studio write check\n")
        .map_err(|error| io_error(root, Operation::WriteTemporary, &error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io_error(root, Operation::SyncTemporary, &error))?;
    let destination = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| io_error(root, Operation::CreateTemporary, &error))?
        .into_temp_path();
    drop(
        temporary
            .persist(&destination)
            .map_err(|error| io_error(root, Operation::Publish, &error.error))?,
    );
    destination
        .close()
        .map_err(|error| io_error(root, Operation::RemoveProbe, &error))
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

    #[test]
    fn filesystem_diagnostics_identify_operation_and_root_without_private_details() {
        for (kind, reason) in [
            (io::ErrorKind::PermissionDenied, "UID 1000"),
            (io::ErrorKind::ReadOnlyFilesystem, "read-only filesystem"),
            (io::ErrorKind::StorageFull, "no space left"),
            (io::ErrorKind::QuotaExceeded, "quota exceeded"),
        ] {
            let private = "PRIVATE_FIXTURE_DIRECTORY/private-filename PRIVATE_FIXTURE_CONTENT";
            let error = io_error(
                StorageRoot::Data,
                Operation::CreateTemporary,
                &io::Error::new(kind, private),
            );
            assert!(error.contains("SUBTITLE_DATA_ROOT"));
            assert!(error.contains("create temporary file"));
            assert!(error.contains(reason), "{error}");
            assert!(!error.contains("PRIVATE_FIXTURE"));
            assert!(!error.contains("private-filename"));
        }
    }

    #[test]
    fn writable_probe_checks_atomic_publish_and_removes_all_probe_files() {
        let directory = tempfile::tempdir().expect("fixture directory");
        verify_writable(directory.path(), StorageRoot::Data).expect("writable data root");
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("fixture listing")
                .count(),
            0
        );
        let path = directory.path().join("state.json");
        write_bytes_for(&path, b"original", StorageRoot::Data).expect("original saved state");
        write_bytes_for(&path, b"replacement", StorageRoot::Data).expect("atomic replacement");
        assert_eq!(fs::read(path).expect("saved state"), b"replacement");
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("fixture listing")
                .count(),
            1
        );
    }

    #[test]
    fn failed_publish_preserves_existing_files_and_cleans_temporary_file() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let saved = directory.path().join("state.json");
        fs::write(&saved, b"original state").expect("saved fixture");
        let conflict = directory.path().join("existing-directory");
        fs::create_dir(&conflict).expect("conflicting destination directory");
        let error = write_bytes_for(&conflict, b"replacement", StorageRoot::Data)
            .expect_err("cannot replace directory with file");
        assert!(error.contains("SUBTITLE_DATA_ROOT"));
        assert!(error.contains("atomically replace saved file"));
        assert!(!error.contains(&directory.path().to_string_lossy().into_owned()));
        assert_eq!(
            fs::read(saved).expect("preserved saved state"),
            b"original state"
        );
        assert!(conflict.is_dir());
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("fixture listing")
                .count(),
            2
        );
    }
}
