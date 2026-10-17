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
    Ok(
        json!({"id":final_path.file_name().and_then(|name| name.to_str()),"files":copied,"count_bytes":total,"retained":managed.len()}),
    )
}

fn selected_backup(data: &Path, id: &str) -> Result<PathBuf, String> {
    if !id.starts_with(BACKUP_PREFIX)
        || id.len() > 96
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("Invalid backup selection".into());
    }
    let backups = data.join("backups");
    let selected = validate_delete_target(&backups, &backups.join(id))?;
    validate_backup_contents(&selected)?;
    Ok(selected)
}

fn file_sizes(path: &Path) -> Result<(u64, usize), String> {
    let mut total = 0_u64;
    let mut count = 0;
    for name in FILES {
        let file = path.join(name);
        let metadata = match fs::symlink_metadata(file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Backup file could not be inspected".into()),
        };
        if is_link_or_reparse(&metadata) || !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err("Backup file is unsafe or exceeds its size limit".into());
        }
        total = total
            .checked_add(metadata.len())
            .ok_or("Backup size limit")?;
        if total > MAX_TOTAL_BYTES {
            return Err("Backup exceeds its size limit".into());
        }
        count += 1;
    }
    Ok((total, count))
}

pub fn list(data: &Path) -> Result<Value, String> {
    let backups = data.join("backups");
    if !backups.exists() {
        return Ok(
            json!({"location":backups,"items":[],"restore_requires_restart":true,"contains_credentials":true}),
        );
    }
    if fs::symlink_metadata(&backups)
        .is_ok_and(|metadata| is_link_or_reparse(&metadata) || !metadata.is_dir())
    {
        return Err("Backup storage must be a real directory".into());
    }
    let mut items = Vec::new();
    for path in managed_backup_paths(&backups)? {
        let id = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Invalid backup name")?;
        let selected = selected_backup(data, id)?;
        let (bytes, count) = file_sizes(&selected)?;
        let created_at = fs::metadata(&selected)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs_f64());
        items.push(json!({"id":id,"created_at":created_at,"count_bytes":bytes,"file_count":count}));
    }
    items.sort_by(|left, right| right["id"].as_str().cmp(&left["id"].as_str()));
    Ok(
        json!({"location":backups,"items":items,"restore_requires_restart":true,"contains_credentials":true,"restore_mode":"overlay_saved_files","retention_count":RETAIN}),
    )
}

struct BoundedArchive(Vec<u8>);
impl Write for BoundedArchive {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > (MAX_TOTAL_BYTES as usize + 1024 * 1024).saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("Backup archive exceeds size limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Export only known regular files. We never unpack arbitrary archives; restore
/// operates on validated server-owned backup directories instead.
pub fn download(data: &Path, id: &str) -> Result<Vec<u8>, String> {
    let selected = selected_backup(data, id)?;
    file_sizes(&selected)?;
    let mut gzip = flate2::write::GzEncoder::new(
        BoundedArchive(Vec::with_capacity(4096)),
        flate2::Compression::fast(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut total = 0_u64;
    for name in FILES {
        let source = selected.join(name);
        if !source.exists() {
            continue;
        }
        let metadata = fs::symlink_metadata(&source).map_err(|_| "Backup file is unavailable")?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err("Unsafe backup file".into());
        }
        let size = metadata.len();
        total = total.checked_add(size).ok_or("Backup size limit")?;
        if total > MAX_TOTAL_BYTES {
            return Err("Backup size limit".into());
        }
        let mut header = [0_u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000600\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        gzip.write_all(&header)
            .map_err(|_| "Cannot encode backup archive")?;
        let mut source = File::open(source).map_err(|_| "Backup source unavailable")?;
        let mut remaining = size;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            if std::time::Instant::now() >= deadline {
                return Err("Backup download exceeded its time limit".into());
            }
            let length = buffer.len().min(remaining as usize);
            let bytes = source
                .read(&mut buffer[..length])
                .map_err(|_| "Cannot read backup archive")?;
            if bytes == 0 {
                return Err("Backup file changed while downloading".into());
            }
            gzip.write_all(&buffer[..bytes])
                .map_err(|_| "Cannot encode backup archive")?;
            remaining -= bytes as u64;
        }
        if source
            .read(&mut [0_u8; 1])
            .map_err(|_| "Cannot inspect backup archive")?
            != 0
        {
            return Err("Backup file changed while downloading".into());
        }
        let padding = (512 - size % 512) % 512;
        gzip.write_all(&[0_u8; 512][..padding as usize])
            .map_err(|_| "Cannot encode backup archive")?;
    }
    gzip.write_all(&[0_u8; 1024])
        .map_err(|_| "Cannot finish backup archive")?;
    gzip.finish()
        .map(|writer| writer.0)
        .map_err(|_| "Cannot finish backup archive".into())
}

fn copy_restore_files(
    source: &Path,
    destination: &Path,
    validate_json: bool,
) -> Result<usize, String> {
    validate_backup_contents(source)?;
    file_sizes(source)?;
    let mut count = 0;
    for name in FILES {
        let source_path = source.join(name);
        if !source_path.exists() {
            continue;
        }
        let mut bytes = Vec::new();
        File::open(&source_path)
            .map_err(|_| "Restore source unavailable")?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Restore source unreadable")?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("Restore file exceeds its limit".into());
        }
        if validate_json && name != "ui-password" {
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| "Backup contains invalid saved JSON")?;
            if (name == "jobs.json" && !value.is_array())
                || (name != "jobs.json" && !value.is_object())
            {
                return Err("Backup contains unsupported saved state".into());
            }
        }
        if name == "ui-password" {
            let password = std::str::from_utf8(&bytes)
                .map_err(|_| "Backup contains invalid dashboard credentials")?
                .trim();
            if bytes.len() > 1024
                || !(12..=256).contains(&password.len())
                || password.chars().any(char::is_control)
            {
                return Err("Backup contains invalid dashboard credentials".into());
            }
        }
        let target = destination.join(name);
        if fs::symlink_metadata(&target)
            .is_ok_and(|metadata| is_link_or_reparse(&metadata) || !metadata.is_file())
        {
            return Err("Restore target must be a regular file".into());
        }
        let mut staged = tempfile::NamedTempFile::new_in(destination)
            .map_err(|_| "Cannot stage restore file")?;
        staged
            .write_all(&bytes)
            .map_err(|_| "Cannot write restore file")?;
        staged
            .as_file()
            .sync_all()
            .map_err(|_| "Cannot synchronize restore file")?;
        staged
            .persist(&target)
            .map_err(|_| "Cannot publish restore file")?;
        count += 1;
    }
    Ok(count)
}

pub fn stage_restore(data: &Path, id: &str) -> Result<Value, String> {
    let source = selected_backup(data, id)?;
    let target = data.join("pending-restore");
    if fs::symlink_metadata(&target).is_ok() {
        return Err("A restore is already pending; restart Subforge first".into());
    }
    let staged = tempfile::tempdir_in(data).map_err(|_| "Cannot stage restore")?;
    let files = copy_restore_files(&source, staged.path(), true)?;
    if files == 0 {
        return Err("Backup contains no saved files".into());
    }
    fs::rename(staged.keep(), &target).map_err(|_| "Cannot publish pending restore")?;
    Ok(
        json!({"id":id,"staged":true,"file_count":files,"restart_required":true,"message":"Restore staged. Restart Subforge to apply the saved files. Current files absent from the backup are preserved; backed-up dashboard credentials may replace the current login."}),
    )
}

pub fn apply_pending_restore(data: &Path) -> Result<(), String> {
    let pending = data.join("pending-restore");
    if fs::symlink_metadata(&pending).is_err() {
        return Ok(());
    }
    // Keep the validated source until every replacement succeeds; interrupted
    // startup retries the same complete overlay instead of accepting partial state.
    copy_restore_files(&pending, data, true)?;
    for name in FILES {
        let path = pending.join(name);
        if path.exists() {
            fs::remove_file(path).map_err(|_| "Cannot complete pending restore")?;
        }
    }
    fs::remove_dir(&pending).map_err(|_| "Cannot finish pending restore")?;
    Ok(())
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
    fn managed_backup_download_is_a_gzip_tar_and_restore_waits_for_restart() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("preferences.json"), br#"{"languages":["fr"]}"#).unwrap();
        fs::write(data.join("media.srt"), b"unrelated media").unwrap();
        let made = backup(&data, &AtomicBool::new(false)).unwrap();
        let id = made["id"].as_str().unwrap();
        let listed = list(&data).unwrap();
        assert_eq!(listed["items"][0]["id"], id);
        assert_eq!(listed["items"][0]["file_count"], 1);
        let bytes = download(&data, id).unwrap();
        let mut archive = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut archive)
            .unwrap();
        assert_eq!(&archive[..16], b"preferences.json");
        assert!(!String::from_utf8_lossy(&archive).contains("unrelated media"));
        fs::write(data.join("preferences.json"), br#"{"languages":["en"]}"#).unwrap();
        stage_restore(&data, id).unwrap();
        assert!(
            String::from_utf8(fs::read(data.join("preferences.json")).unwrap())
                .unwrap()
                .contains("en")
        );
        apply_pending_restore(&data).unwrap();
        assert!(
            String::from_utf8(fs::read(data.join("preferences.json")).unwrap())
                .unwrap()
                .contains("fr")
        );
        assert_eq!(
            fs::read(data.join("media.srt")).unwrap(),
            b"unrelated media"
        );
        assert!(!data.join("pending-restore").exists());
    }

    #[test]
    fn restore_and_download_reject_traversal_unexpected_entries_and_invalid_state() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("state.json"), b"invalid JSON").unwrap();
        let made = backup(&data, &AtomicBool::new(false)).unwrap();
        let id = made["id"].as_str().unwrap();
        assert!(download(&data, "../outside").is_err());
        assert!(stage_restore(&data, "../outside").is_err());
        assert!(stage_restore(&data, id).is_err());
        assert!(!data.join("pending-restore").exists());
        fs::write(
            data.join("backups").join(id).join("unexpected"),
            b"not allowed",
        )
        .unwrap();
        assert!(list(&data).is_err());
        assert!(download(&data, id).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn pending_restore_rejects_symlinks_before_modifying_live_state() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        fs::create_dir(&data).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("state.json"), b"{}").unwrap();
        fs::write(data.join("state.json"), br#"{"preserved":true}"#).unwrap();
        std::os::unix::fs::symlink(&outside, data.join("pending-restore")).unwrap();
        assert!(apply_pending_restore(&data).is_err());
        assert_eq!(
            fs::read(data.join("state.json")).unwrap(),
            br#"{"preserved":true}"#
        );
    }

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
