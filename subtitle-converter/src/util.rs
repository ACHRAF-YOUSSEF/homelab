use rand::RngCore;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn token(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut random);
    random.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn constant_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

pub fn read_json(path: &Path, limit: usize) -> Result<Value, String> {
    if fs::symlink_metadata(path)
        .map_err(|_| "State file is unavailable")?
        .file_type()
        .is_symlink()
    {
        return Err("State files must not be symlinks".into());
    }
    let file = File::open(path).map_err(|_| "State file could not be opened")?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "State file could not be read")?;
    if bytes.len() > limit {
        return Err("State file exceeds its size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "State file contains invalid JSON".into())
}

pub fn write_private(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("State storage requires a directory")?;
    fs::create_dir_all(parent).map_err(|_| "State storage is unavailable")?;
    if path.is_symlink() {
        return Err("State files must not be symlinks".into());
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "Cannot create private state file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "Cannot protect state file")?;
    }
    serde_json::to_writer(&mut temporary, value).map_err(|_| "Cannot serialize state")?;
    temporary.flush().map_err(|_| "Cannot flush state")?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| "Cannot synchronize state")?;
    temporary
        .persist(path)
        .map_err(|_| "Cannot publish state")?;
    Ok(())
}

pub fn bounded_process(
    command: &mut Command,
    timeout: Duration,
    stop: &AtomicBool,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let (success, bytes) = bounded_process_allow_failure(command, timeout, stop, max_bytes)?;
    if success {
        Ok(bytes)
    } else {
        Err("Local conversion tool failed".into())
    }
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .env_clear()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let taskkill = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("C:/Windows"))
            .join("System32/taskkill.exe");
        let _ = Command::new(taskkill)
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub fn bounded_process_allow_failure(
    command: &mut Command,
    timeout: Duration,
    stop: &AtomicBool,
    max_bytes: usize,
) -> Result<(bool, Vec<u8>), String> {
    let temporary = tempfile::tempdir().map_err(|_| "Cannot create tool scratch space")?;
    let mut stdout = tempfile::tempfile().map_err(|_| "Cannot create tool output buffer")?;
    let stderr = tempfile::tempfile().map_err(|_| "Cannot create tool error buffer")?;
    configure_tool_environment(command, temporary.path());
    command
        .stdin(Stdio::null())
        .stdout(
            stdout
                .try_clone()
                .map_err(|_| "Cannot attach tool output")?,
        )
        .stderr(
            stderr
                .try_clone()
                .map_err(|_| "Cannot attach tool errors")?,
        );
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Required conversion tool could not be started")?;
    let deadline = Instant::now() + timeout;
    let result: Result<bool, String> = loop {
        if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
            kill_tree(&mut child);
            break Err("Conversion timed out or was cancelled".into());
        }
        if stdout.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > max_bytes as u64
            || stderr.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > max_bytes as u64
        {
            kill_tree(&mut child);
            break Err("Tool output exceeded its limit".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                kill_tree(&mut child);
                break Ok(status.success());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => {
                kill_tree(&mut child);
                break Err("Conversion process failed".into());
            }
        }
    };
    let success = result?;
    stdout.rewind().map_err(|_| "Cannot read tool output")?;
    let mut bytes = Vec::new();
    stdout
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read tool output")?;
    if bytes.len() > max_bytes {
        return Err("Tool output exceeded its limit".into());
    }
    Ok((success, bytes))
}

fn configure_tool_environment(command: &mut Command, scratch: &Path) {
    command.env_clear();
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "HOME",
        "LANG",
        "LC_ALL",
        "DOTNET_BUNDLE_EXTRACT_BASE_DIR",
        "FONTCONFIG_PATH",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    // MKVToolNix serializes Unicode metadata through the process locale.
    // Debian's default C locale can silently truncate valid JSON at a
    // non-ASCII title even when the tool returns success. Use the built-in
    // UTF-8 locale on Unix regardless of the service's display language.
    #[cfg(unix)]
    command.env("LANG", "C.UTF-8").env("LC_ALL", "C.UTF-8");
    command
        .env("OMP_THREAD_LIMIT", "1")
        .env("TMPDIR", scratch)
        .env("TEMP", scratch)
        .env("TMP", scratch);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_environment_drops_custom_values_and_uses_private_scratch() {
        let mut command = Command::new("unused-test-executable");
        command.env("SUBFORGE_TEST_SECRET", "fixture-value");
        let scratch = Path::new("private-scratch");
        configure_tool_environment(&mut command, scratch);
        let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert!(!environment.contains_key(std::ffi::OsStr::new("SUBFORGE_TEST_SECRET")));
        assert_eq!(
            environment
                .get(std::ffi::OsStr::new("TMPDIR"))
                .copied()
                .flatten(),
            Some(scratch.as_os_str())
        );
        #[cfg(unix)]
        for name in ["LANG", "LC_ALL"] {
            assert_eq!(
                environment
                    .get(std::ffi::OsStr::new(name))
                    .copied()
                    .flatten(),
                Some(std::ffi::OsStr::new("C.UTF-8"))
            );
        }
    }
}
