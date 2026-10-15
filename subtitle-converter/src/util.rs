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

pub fn bounded_json_clone(value: &Value, limit: usize) -> Result<Value, String> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("JSON entry size limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::with_capacity(limit.min(4096)),
        limit,
    };
    serde_json::to_writer(&mut buffer, value)
        .map_err(|_| "State entry exceeds its worker size limit")?;
    serde_json::from_slice(&buffer.bytes).map_err(|_| "State entry could not be prepared".into())
}

pub fn write_private(path: &Path, value: &Value) -> Result<(), String> {
    write_private_limited(path, value, u64::MAX)
}

pub fn write_private_limited(path: &Path, value: &Value, limit: u64) -> Result<(), String> {
    write_json_private(path, value, limit, false)
}

pub fn write_private_pretty(path: &Path, value: &Value) -> Result<(), String> {
    write_json_private(path, value, 4096, true)
}

fn write_json_private(path: &Path, value: &Value, limit: u64, pretty: bool) -> Result<(), String> {
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
    struct LimitedWriter<'a> {
        output: &'a mut tempfile::NamedTempFile,
        remaining: u64,
    }
    impl Write for LimitedWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let length = u64::try_from(bytes.len())
                .map_err(|_| std::io::Error::other("State size limit"))?;
            if length > self.remaining {
                return Err(std::io::Error::other("State size limit"));
            }
            let written = self.output.write(bytes)?;
            self.remaining = self.remaining.saturating_sub(written as u64);
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.output.flush()
        }
    }
    let mut output = LimitedWriter {
        output: &mut temporary,
        remaining: limit,
    };
    if pretty {
        serde_json::to_writer_pretty(&mut output, value)
    } else {
        serde_json::to_writer(&mut output, value)
    }
    .map_err(|_| "State could not be serialized within its size limit")?;
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
            .env_clear()
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
    run_process(command, timeout, stop, max_bytes, None)
}

pub fn bounded_gpu_process(
    command: &mut Command,
    timeout: Duration,
    stop: &AtomicBool,
    max_bytes: usize,
    minimum_free_mib: u32,
) -> Result<(bool, Vec<u8>), String> {
    run_process(
        command,
        timeout,
        stop,
        max_bytes,
        Some(minimum_free_mib.min(65536)),
    )
}

fn run_process(
    command: &mut Command,
    timeout: Duration,
    stop: &AtomicBool,
    max_bytes: usize,
    gpu: Option<u32>,
) -> Result<(bool, Vec<u8>), String> {
    if stop.load(Ordering::Relaxed) {
        return Err("Conversion was cancelled before tool dispatch".into());
    }
    let temporary = tempfile::tempdir().map_err(|_| "Cannot create tool scratch space")?;
    let mut stdout = tempfile::tempfile().map_err(|_| "Cannot create tool output buffer")?;
    let stderr = tempfile::tempfile().map_err(|_| "Cannot create tool error buffer")?;
    configure_tool_environment(command, temporary.path());
    if let Some(minimum_free_mib) = gpu {
        configure_gpu_environment(command, minimum_free_mib);
    }
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
    if stop.load(Ordering::Relaxed) {
        return Err("Conversion was cancelled before tool dispatch".into());
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

fn configure_gpu_environment(command: &mut Command, minimum_free_mib: u32) {
    // These fixed values describe only the explicitly granted first device.
    // No credentials or arbitrary inherited library paths enter GPU children.
    command
        .env("CUDA_VISIBLE_DEVICES", "0")
        .env("NVIDIA_VISIBLE_DEVICES", "0")
        .env("NVIDIA_DRIVER_CAPABILITIES", "compute,utility")
        .env(
            "LD_LIBRARY_PATH",
            "/usr/lib/wsl/lib:/usr/local/nvidia/lib64:/usr/local/nvidia/lib:/usr/local/lib",
        )
        .env("SUBFORGE_OCR_BACKEND", "cuda")
        .env("GPU_MIN_FREE_MIB", minimum_free_mib.min(65536).to_string());
}

fn configure_tool_environment(command: &mut Command, scratch: &Path) {
    command.env_clear();
    command.env("SUBFORGE_OCR_BACKEND", "cpu");
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
        .env("OMP_NUM_THREADS", "1")
        .env("DOTNET_PROCESSOR_COUNT", "1")
        .env("TMPDIR", scratch)
        .env("TEMP", scratch)
        .env("TMP", scratch);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_json_and_persistence_do_not_replace_state_on_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let original = serde_json::json!({"safe":true});
        write_private(&path, &original).unwrap();
        let oversized = serde_json::json!({"oversized":"x".repeat(1024)});
        assert!(bounded_json_clone(&oversized, 64).is_err());
        assert!(write_private_limited(&path, &oversized, 64).is_err());
        assert_eq!(read_json(&path, 4096).unwrap(), original);
        assert_eq!(bounded_json_clone(&original, 64).unwrap(), original);
    }

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

    #[test]
    fn gpu_environment_is_explicit_and_does_not_enter_cpu_children() {
        let scratch = Path::new("private-scratch");
        let mut command = Command::new("unused-test-executable");
        command
            .env("SUBFORGE_TEST_SECRET", "fixture-value")
            .env("LD_LIBRARY_PATH", "untrusted-path")
            .env("CUDA_VISIBLE_DEVICES", "all");
        configure_tool_environment(&mut command, scratch);
        assert!(
            !command
                .get_envs()
                .any(|(name, _)| name == "CUDA_VISIBLE_DEVICES" || name == "LD_LIBRARY_PATH")
        );
        configure_gpu_environment(&mut command, 2048);
        let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert!(!environment.contains_key(std::ffi::OsStr::new("SUBFORGE_TEST_SECRET")));
        assert_eq!(
            environment
                .get(std::ffi::OsStr::new("CUDA_VISIBLE_DEVICES"))
                .copied()
                .flatten(),
            Some(std::ffi::OsStr::new("0"))
        );
        assert_eq!(
            environment
                .get(std::ffi::OsStr::new("LD_LIBRARY_PATH"))
                .copied()
                .flatten(),
            Some(std::ffi::OsStr::new(
                "/usr/lib/wsl/lib:/usr/local/nvidia/lib64:/usr/local/nvidia/lib:/usr/local/lib"
            ))
        );
    }
}
