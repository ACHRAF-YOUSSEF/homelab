//! Trusted native helpers only. Never invoke a shell or inherit credential variables.
use std::ffi::OsString;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Capture at most `limit` bytes. Cancellation and timeout kill and reap the child.
/// These fixed native tools do not launch descendant processes.
pub fn run(
    executable: &str,
    args: &[OsString],
    timeout: u64,
    limit: u64,
    cancel: impl Fn() -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    cancel()?;
    let mut command = Command::new(executable);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for name in ["PATH", "SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("OMP_NUM_THREADS", "1")
        .env("OPENBLAS_NUM_THREADS", "1");
    if limit == 0 {
        command.stdout(Stdio::null());
    } else {
        command.stdout(Stdio::piped());
    }
    let mut child = Reap(
        command
            .spawn()
            .map_err(|_| format!("Unable to start native helper: {executable}"))?,
    );
    let exceeded = Arc::new(AtomicBool::new(false));
    let reader = child.0.stdout.take().map(|stdout| {
        let exceeded = Arc::clone(&exceeded);
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(limit.saturating_add(1)).read_to_end(&mut bytes);
            if bytes.len() as u64 > limit {
                exceeded.store(true, Ordering::Release);
            }
            result.map(|_| bytes)
        })
    });
    let started = Instant::now();
    let outcome = loop {
        if let Err(error) = cancel() {
            break Err(error);
        }
        if exceeded.load(Ordering::Acquire) {
            break Err("Native helper output exceeded the configured limit".into());
        }
        if started.elapsed() >= Duration::from_secs(timeout.max(1)) {
            break Err("Native helper timed out".into());
        }
        match child.0.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(format!("Native helper failed: {executable}"))
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => break Err("Unable to wait for native helper".into()),
        }
    };
    if outcome.is_err() {
        let _ = child.0.kill();
    }
    let _ = child.0.wait();
    let bytes = match reader {
        Some(reader) => reader
            .join()
            .map_err(|_| "Native helper reader failed")?
            .map_err(|_| "Unable to read native helper output")?,
        None => Vec::new(),
    };
    outcome?;
    if bytes.len() as u64 > limit {
        return Err("Native helper output exceeded the configured limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_prevents_launch() {
        assert_eq!(
            run("nonexistent-helper", &[], 1, 64, || Err("cancelled".into())),
            Err("cancelled".into())
        );
    }
    #[test]
    fn missing_helper_is_an_error() {
        assert!(run("nonexistent-subtitle-test-helper", &[], 1, 64, || Ok(())).is_err());
    }

    fn fixture(
        name: &str,
        timeout: u64,
        limit: u64,
        cancel: impl Fn() -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        let executable = std::env::current_exe().unwrap();
        run(
            executable.to_str().unwrap(),
            &[
                "--ignored".into(),
                "--exact".into(),
                format!("process::tests::{name}").into(),
                "--nocapture".into(),
            ],
            timeout,
            limit,
            cancel,
        )
    }

    #[test]
    fn excessive_output_is_bounded() {
        let result = fixture("output_fixture", 5, 128, || Ok(()));
        assert!(result.unwrap_err().contains("output exceeded"));
    }

    #[test]
    fn timeout_reaps_long_running_child() {
        let started = Instant::now();
        assert!(
            fixture("waiting_fixture", 1, 1024, || Ok(()))
                .unwrap_err()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn cancellation_reaps_long_running_child() {
        let started = Instant::now();
        let result = fixture("waiting_fixture", 5, 1024, || {
            if started.elapsed() > Duration::from_millis(150) {
                Err("cancelled".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("cancelled".into()));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    #[ignore = "subprocess fixture only"]
    fn output_fixture() {
        print!("{}", "x".repeat(256 * 1024));
    }

    #[test]
    #[ignore = "subprocess fixture only"]
    fn waiting_fixture() {
        thread::sleep(Duration::from_secs(30));
    }
}
