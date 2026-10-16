use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const BACKUP_PREFIX: &str = "subforge-backup-";
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 32;
const RETAIN: usize = 3;
const FILES: [&str; 11] = [
    "state.json",
    "metadata.json",
    "tasks.json",
    "settings.json",
    "preferences.json",
    "workers.json",
    "workers.conf",
    "inventory.json",
    "jobs.json",
    "publication-queue.json",
    "ui-password",
];

pub fn backup(data: &Path, stop: &AtomicBool) -> Result<Value, String> {
    if stop.load(Ordering::Relaxed) {
        return Err("Backup cancelled".into());
    }
    let backups = data.join("backups");
    if fs::symlink_metadata(&backups).is_ok_and(|metadata| is_link_or_reparse(&metadata)) {
        return Err("Backup directory must not be a symlink".into());
    }
    fs::create_dir_all(&backups).map_err(|_| "Backup storage is unavailable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&backups, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect backup storage")?;
    }
    let entries = fs::read_dir(&backups).map_err(|_| "Backup storage is unavailable")?;
    let existing: Vec<_> = entries
        .take(MAX_ENTRIES + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Backup storage could not be read")?;
    if existing.len() > MAX_ENTRIES {
        return Err("Backup storage has too many entries".into());
    }
    if existing.len() >= MAX_ENTRIES {
        return Err("Backup storage has too many entries".into());
    }
    validate_managed_backups(&existing)?;

    let temp =
        tempfile::tempdir_in(&backups).map_err(|_| "Cannot create private backup staging")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect backup staging")?;
    }
    let mut copied = Vec::new();
    let mut total = 0_u64;
    for name in FILES {
        if stop.load(Ordering::Relaxed) {
            return Err("Backup cancelled".into());
        }
        let source = data.join(name);
        let metadata = match fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Backup source could not be inspected".into()),
        };
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err("Backup source must be a regular file".into());
        }
        if metadata.len() > MAX_FILE_BYTES || total.saturating_add(metadata.len()) > MAX_TOTAL_BYTES
        {
            return Err("Backup data exceeds its size limit".into());
        }
        let mut input = File::open(&source).map_err(|_| "Backup source could not be opened")?;
        let destination = temp.path().join(name);
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|_| "Cannot create private backup file")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| "Cannot protect backup file")?;
        }
        let mut buffer = [0_u8; 64 * 1024];
        let mut file_bytes = 0_u64;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err("Backup cancelled".into());
            }
            let count = input
                .read(&mut buffer)
                .map_err(|_| "Backup source could not be read")?;
            if count == 0 {
                break;
            }
            file_bytes = file_bytes.saturating_add(count as u64);
            total = total.saturating_add(count as u64);
            if file_bytes > MAX_FILE_BYTES || total > MAX_TOTAL_BYTES {
                return Err("Backup data exceeds its size limit".into());
            }
            output
                .write_all(&buffer[..count])
                .map_err(|_| "Backup file could not be written")?;
        }
        output
            .sync_all()
            .map_err(|_| "Backup file could not be synchronized")?;
        copied.push(name);
    }
    if stop.load(Ordering::Relaxed) {
        return Err("Backup cancelled".into());
    }
    let final_path = unique_backup_path(&backups)?;
    let staged = temp.keep();
    fs::rename(staged, &final_path).map_err(|_| "Backup could not be published")?;

    let mut managed = managed_backup_paths(&backups)?;
    managed.sort();
    while managed.len() > RETAIN {
        if stop.load(Ordering::Relaxed) {
            return Err("Backup cancelled".into());
        }
        let remove = managed.remove(0);
        let checked = validate_delete_target(&backups, &remove)?;
        validate_backup_contents(&checked)?;
        // Revalidate directly before recursive deletion in case the directory
        // was replaced between inspection and the removal decision.
        let remove = validate_delete_target(&backups, &remove)?;
        fs::remove_dir_all(remove).map_err(|_| "Expired backup could not be removed")?;
    }
    Ok(json!({"files":copied,"count_bytes":total,"retained":managed.len()}))
}

fn unique_backup_path(backups: &Path) -> Result<PathBuf, String> {
    for _ in 0..8 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "System clock is unavailable")?
            .as_nanos();
        let name = format!("{BACKUP_PREFIX}{nanos:020}-{}", crate::util::token(8));
        let path = backups.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("Cannot allocate a unique backup name".into())
}

fn validate_managed_backups(entries: &[fs::DirEntry]) -> Result<(), String> {
    for entry in entries {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(BACKUP_PREFIX) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| "Managed backup could not be inspected")?;
        if is_link_or_reparse(&metadata) || !metadata.is_dir() {
            return Err("Managed backup must be a real directory".into());
        }
        validate_backup_contents(&entry.path())?;
    }
    Ok(())
}

fn managed_backup_paths(backups: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(backups).map_err(|_| "Backup storage could not be read")?;
    let mut paths = Vec::new();
    let mut entry_count = 0;
    for entry in entries.take(MAX_ENTRIES + 1) {
        let entry = entry.map_err(|_| "Backup storage could not be read")?;
        entry_count += 1;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(BACKUP_PREFIX)
        {
            paths.push(entry.path());
        }
    }
    if entry_count > MAX_ENTRIES {
        return Err("Backup storage has too many entries".into());
    }
    Ok(paths)
}

fn validate_delete_target(backups: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.parent() != Some(backups)
        || !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(BACKUP_PREFIX))
    {
        return Err("Expired backup is outside managed storage".into());
    }
    let root_metadata =
        fs::symlink_metadata(backups).map_err(|_| "Backup storage could not be inspected")?;
    let target_metadata =
        fs::symlink_metadata(path).map_err(|_| "Managed backup could not be inspected")?;
    if is_link_or_reparse(&root_metadata)
        || !root_metadata.is_dir()
        || is_link_or_reparse(&target_metadata)
        || !target_metadata.is_dir()
    {
        return Err("Managed backup must be a real directory".into());
    }
    let canonical_root =
        fs::canonicalize(backups).map_err(|_| "Backup storage could not be resolved")?;
    let canonical_target =
        fs::canonicalize(path).map_err(|_| "Managed backup could not be resolved")?;
    if canonical_target.parent() != Some(canonical_root.as_path()) {
        return Err("Expired backup is outside managed storage".into());
    }
    Ok(canonical_target)
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    false
}

fn validate_backup_contents(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Managed backup could not be inspected")?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err("Managed backup must be a real directory".into());
    }
    let entries = fs::read_dir(path).map_err(|_| "Managed backup could not be read")?;
    let entries: Vec<_> = entries
        .take(FILES.len() + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Managed backup could not be read")?;
    if entries.len() > FILES.len() {
        return Err("Managed backup contains too many entries".into());
    }
    for entry in entries {
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| "Managed backup entry could not be inspected")?;
        if is_link_or_reparse(&metadata)
            || !metadata.is_file()
            || !FILES.contains(&entry.file_name().to_string_lossy().as_ref())
        {
            return Err("Managed backup contains an unexpected entry".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn copies_only_allowlisted_files_and_retains_three() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("state.json"), b"state").unwrap();
        fs::write(data.join("media.srt"), b"private media").unwrap();
        let stop = AtomicBool::new(false);
        for _ in 0..4 {
            backup(&data, &stop).unwrap();
        }
        let dirs: Vec<_> = fs::read_dir(data.join("backups"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(dirs.len(), RETAIN);
        for dir in dirs {
            assert!(dir.path().join("state.json").exists());
            assert!(!dir.path().join("media.srt").exists());
        }
    }

    #[test]
    fn rejects_symlink_source_and_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        fs::create_dir(&data).unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, data.join("state.json")).unwrap();
        let stop = AtomicBool::new(false);
        #[cfg(unix)]
        assert!(backup(&data, &stop).is_err());
        stop.store(true, Ordering::Relaxed);
        assert!(backup(&data, &stop).is_err());
    }
}
