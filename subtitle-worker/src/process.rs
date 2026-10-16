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

fn native_command(executable: &str, args: &[OsString]) -> Command {
    let mut command = Command::new(executable);
    command.args(args).env_clear().stdin(Stdio::null());
    for name in ["PATH", "SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("OMP_NUM_THREADS", "1")
        .env("OPENBLAS_NUM_THREADS", "1");
    command
}

const CUDA_UNAVAILABLE: &str = "CUDA was requested but Whisper could not initialize a CUDA backend. Use the CUDA image, reserve an NVIDIA GPU, and check the compatible host NVIDIA driver.";
const CUDA_LOG_LINE_BYTES: usize = 4096;

#[derive(Default)]
struct CudaEvidence {
    verified: AtomicBool,
    rejected: AtomicBool,
}

/// These anchored messages are specific to whisper.cpp commit
/// 2eeeba56e9edd762b4b38467bab96c2517163158, src/whisper.cpp:
/// whisper_backend_init_gpu logs selection before initialization (1204-1237),
/// and whisper_init_state logs KV allocation after backend initialization
/// (3103-3125). Selection alone is never evidence of successful CUDA use.
struct CudaLogParser {
    line: [u8; CUDA_LOG_LINE_BYTES],
    length: usize,
    discard: bool,
    selected: bool,
    evidence: Arc<CudaEvidence>,
}

impl CudaLogParser {
    fn new(evidence: Arc<CudaEvidence>) -> Self {
        Self {
            line: [0; CUDA_LOG_LINE_BYTES],
            length: 0,
            discard: false,
            selected: false,
            evidence,
        }
    }

    fn reject_prefix(&self) {
        let line = self.line.get(..self.length).unwrap_or_default();
        if line.starts_with(b"whisper_backend_init_gpu: no GPU found")
            || line.starts_with(b"whisper_backend_init_gpu: failed to initialize ")
        {
            self.evidence.rejected.store(true, Ordering::Release);
        }
    }

    fn complete_line(&mut self) {
        if !self.discard {
            let line = self.line.get(..self.length).unwrap_or_default();
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if let Some(device) = line
                .strip_prefix(b"whisper_backend_init_gpu: using CUDA")
                .and_then(|value| value.strip_suffix(b" backend"))
                && !device.is_empty()
                && device.iter().all(u8::is_ascii_digit)
            {
                self.selected = true;
            }
            if self.selected
                && let Some(allocation) = line.strip_prefix(b"whisper_init_state: kv self size")
                && let Ok(allocation) = std::str::from_utf8(allocation)
                && let Some(size) = allocation
                    .trim()
                    .strip_prefix('=')
                    .and_then(|value| value.trim().strip_suffix(" MB"))
                    .and_then(|value| value.trim().parse::<f64>().ok())
                && size.is_finite()
                && size > 0.0
            {
                self.evidence.verified.store(true, Ordering::Release);
            }
        }
        // No native paths, recognized text or unrecognized text survives a line.
        self.line.fill(0);
        self.length = 0;
        self.discard = false;
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.complete_line();
                continue;
            }
            if self.discard {
                continue;
            }
            if let Some(slot) = self.line.get_mut(self.length) {
                *slot = byte;
                self.length += 1;
                // Reject known fallback signals even before a newline arrives.
                self.reject_prefix();
            } else {
                self.line.fill(0);
                self.length = 0;
                self.discard = true;
            }
        }
    }

    fn finish(&mut self) {
        self.complete_line();
    }
}

fn drain_cuda_log(mut stderr: impl Read, evidence: Arc<CudaEvidence>) -> std::io::Result<()> {
    let mut parser = CudaLogParser::new(evidence);
    // A fixed read buffer plus a 4 KiB line buffer bounds memory independently
    // of process runtime and log length; overlong lines are discarded to newline.
    let mut bytes = [0; 1024];
    loop {
        let count = match stderr.read(&mut bytes) {
            Ok(0) => {
                parser.finish();
                return Ok(());
            }
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        parser.feed(bytes.get(..count).unwrap_or_default());
        bytes.fill(0);
    }
}

/// Require a successfully initialized CUDA backend for this Whisper process.
/// stdout is suppressed, stderr is drained without retaining or exposing logs.
/// A fallback signal, timeout or cancellation kills and reaps the child; every
/// path also joins the stderr reader before returning.
pub fn run_whisper_cuda(
    executable: &str,
    args: &[OsString],
    timeout: u64,
    cancel: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    cancel()?;
    let mut command = native_command(executable, args);
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = Reap(command.spawn().map_err(
        |_| "Unable to start CUDA Whisper helper; check the CUDA image and configured executable",
    )?);
    let stderr = child
        .0
        .stderr
        .take()
        .ok_or("CUDA Whisper stderr monitor is unavailable")?;
    let evidence = Arc::new(CudaEvidence::default());
    let shared = Arc::clone(&evidence);
    let reader = thread::Builder::new()
        .name("whisper-cuda-monitor".into())
        .spawn(move || drain_cuda_log(stderr, shared))
        .map_err(|_| "Unable to start CUDA Whisper monitor")?;
    let started = Instant::now();
    let outcome = loop {
        if let Err(error) = cancel() {
            break Err(error);
        }
        if evidence.rejected.load(Ordering::Acquire) {
            break Err(CUDA_UNAVAILABLE.to_owned());
        }
        if started.elapsed() >= Duration::from_secs(timeout.max(1)) {
            break Err("CUDA Whisper helper timed out".into());
        }
        match child.0.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(_) => break Err("Unable to wait for CUDA Whisper helper".into()),
        }
    };
    if outcome.is_err() {
        let _ = child.0.kill();
    }
    let _ = child.0.wait();
    let drained = reader.join();
    // Accepted cancellation and process timeout retain their precise reason.
    let process_success = outcome?;
    drained
        .map_err(|_| "CUDA Whisper monitor failed")?
        .map_err(|_| "Unable to monitor CUDA Whisper initialization")?;
    cancel()?;
    // Reading the final stderr bytes precedes status interpretation, so a fast
    // failing child cannot hide its CUDA rejection behind a generic exit error.
    if evidence.rejected.load(Ordering::Acquire) {
        return Err(CUDA_UNAVAILABLE.to_owned());
    }
    if !process_success {
        return Err("CUDA Whisper helper failed; check model compatibility, GPU memory and the host NVIDIA driver".into());
    }
    if !evidence.verified.load(Ordering::Acquire) {
        return Err(CUDA_UNAVAILABLE.to_owned());
    }
    Ok(())
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
    let mut command = native_command(executable, args);
    command.stderr(Stdio::null());
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
    const SELECTED_CUDA: &[u8] = b"whisper_backend_init_gpu: using CUDA0 backend\n";
    const ALLOCATED_CUDA: &[u8] = b"whisper_init_state: kv self size  =    3.14 MB\n";

    fn parse_cuda(chunks: &[&[u8]]) -> Arc<CudaEvidence> {
        let evidence = Arc::new(CudaEvidence::default());
        let mut parser = CudaLogParser::new(Arc::clone(&evidence));
        for chunk in chunks {
            parser.feed(chunk);
        }
        parser.finish();
        evidence
    }

    #[test]
    fn cuda_markers_can_span_every_read_boundary_but_require_completed_initialization() {
        let mut markers = Vec::new();
        markers.extend_from_slice(SELECTED_CUDA);
        markers.extend_from_slice(ALLOCATED_CUDA);
        for split in 0..=markers.len() {
            let first = markers.get(..split).expect("fixture prefix");
            let second = markers.get(split..).expect("fixture suffix");
            let evidence = parse_cuda(&[first, second]);
            assert!(evidence.verified.load(Ordering::Acquire), "split {split}");
            assert!(!evidence.rejected.load(Ordering::Acquire));
        }
        for chunks in [
            vec![SELECTED_CUDA],
            vec![ALLOCATED_CUDA],
            vec![ALLOCATED_CUDA, SELECTED_CUDA],
            vec![
                b"whisper_backend_init_gpu: using Metal backend\n".as_slice(),
                ALLOCATED_CUDA,
            ],
            vec![
                SELECTED_CUDA,
                b"whisper_init_state: kv self size  = nan MB\n".as_slice(),
            ],
        ] {
            assert!(!parse_cuda(&chunks).verified.load(Ordering::Acquire));
        }
    }

    #[test]
    fn cuda_failure_is_detected_without_waiting_for_newline_and_overrides_selection() {
        for failure in [
            b"whisper_backend_init_gpu: no GPU found".as_slice(),
            b"whisper_backend_init_gpu: failed to initialize CUDA0 backend".as_slice(),
        ] {
            let evidence = Arc::new(CudaEvidence::default());
            let mut parser = CudaLogParser::new(Arc::clone(&evidence));
            parser.feed(SELECTED_CUDA);
            parser.feed(ALLOCATED_CUDA);
            for byte in failure {
                parser.feed(std::slice::from_ref(byte));
            }
            assert!(evidence.rejected.load(Ordering::Acquire));
        }
    }

    #[test]
    fn cuda_monitor_discards_overlong_lines_and_private_contents_with_fixed_memory() {
        let evidence = Arc::new(CudaEvidence::default());
        let mut parser = CudaLogParser::new(Arc::clone(&evidence));
        parser.feed(b"PRIVATE_FIXTURE_PATH PRIVATE_FIXTURE_TRANSCRIPT\n");
        assert_eq!(parser.length, 0);
        assert!(parser.line.iter().all(|byte| *byte == 0));
        parser.feed(&vec![b'x'; CUDA_LOG_LINE_BYTES + 1]);
        assert!(parser.discard);
        assert_eq!(parser.length, 0);
        assert!(parser.line.iter().all(|byte| *byte == 0));
        parser.feed(SELECTED_CUDA);
        parser.feed(ALLOCATED_CUDA);
        assert!(
            !evidence.verified.load(Ordering::Acquire),
            "marker inside discarded long line cannot select CUDA"
        );
        parser.feed(SELECTED_CUDA);
        parser.feed(ALLOCATED_CUDA);
        assert!(evidence.verified.load(Ordering::Acquire));
        assert!(parser.line.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn cuda_monitor_ignores_unanchored_failure_text_and_invalid_utf8() {
        let evidence = parse_cuda(&[
            b"private message: whisper_backend_init_gpu: no GPU found\n\xff\xfe\n",
            SELECTED_CUDA,
            ALLOCATED_CUDA,
        ]);
        assert!(evidence.verified.load(Ordering::Acquire));
        assert!(!evidence.rejected.load(Ordering::Acquire));
    }

    fn cuda_fixture(
        name: &str,
        timeout: u64,
        cancel: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let executable = std::env::current_exe().expect("fixture executable");
        run_whisper_cuda(
            executable.to_str().expect("fixture executable path"),
            &[
                "--ignored".into(),
                "--exact".into(),
                format!("process::tests::{name}").into(),
                "--nocapture".into(),
            ],
            timeout,
            cancel,
        )
    }

    #[test]
    fn cuda_child_requires_positive_backend_allocation_and_drains_large_logs() {
        cuda_fixture("cuda_positive_fixture", 5, || Ok(()))
            .expect("synthetic CUDA initialization evidence");
        let error = cuda_fixture("cuda_selection_only_fixture", 5, || Ok(()))
            .expect_err("selection precedes initialization");
        assert_eq!(error, CUDA_UNAVAILABLE);
        assert!(!error.contains("PRIVATE_FIXTURE"));
    }

    #[test]
    fn cuda_fallback_kills_and_reaps_child_before_cpu_work_can_continue() {
        for fixture in ["cuda_missing_fixture", "cuda_failed_init_fixture"] {
            let started = Instant::now();
            let error =
                cuda_fixture(fixture, 10, || Ok(())).expect_err("fallback must be rejected");
            assert_eq!(error, CUDA_UNAVAILABLE);
            assert!(!error.contains("PRIVATE_FIXTURE"));
            assert!(started.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    fn cuda_timeout_and_cancellation_reap_and_join_monitor() {
        let started = Instant::now();
        let cancelled = cuda_fixture("waiting_fixture", 5, || {
            if started.elapsed() > Duration::from_millis(100) {
                Err("Cancelled by user".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(cancelled, Err("Cancelled by user".into()));
        assert!(started.elapsed() < Duration::from_secs(2));
        let started = Instant::now();
        assert!(
            cuda_fixture("waiting_fixture", 1, || Ok(()))
                .expect_err("timeout")
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(
            run_whisper_cuda("nonexistent-fixture", &[], 1, || Err(
                "Cancelled by user".into()
            )),
            Err("Cancelled by user".into())
        );
    }

    #[test]
    #[ignore = "subprocess fixture only"]
    fn cuda_positive_fixture() {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        for _ in 0..64 {
            stderr.write_all(&[b'x'; 4096]).expect("fixture noise");
        }
        stderr.write_all(b"\n").expect("fixture newline");
        stderr.write_all(SELECTED_CUDA).expect("fixture selection");
        stderr
            .write_all(ALLOCATED_CUDA)
            .expect("fixture allocation");
        stderr.flush().expect("fixture flush");
    }

    #[test]
    #[ignore = "subprocess fixture only"]
    fn cuda_selection_only_fixture() {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        stderr
            .write_all(b"PRIVATE_FIXTURE_PATH PRIVATE_FIXTURE_TRANSCRIPT\n")
            .expect("fixture private data");
        stderr.write_all(SELECTED_CUDA).expect("fixture selection");
    }

    #[test]
    #[ignore = "subprocess fixture only"]
    fn cuda_missing_fixture() {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        stderr
            .write_all(b"PRIVATE_FIXTURE_PATH\nwhisper_backend_init_gpu: no GPU found")
            .expect("fixture fallback");
        stderr.flush().expect("fixture flush");
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[ignore = "subprocess fixture only"]
    fn cuda_failed_init_fixture() {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        stderr.write_all(SELECTED_CUDA).expect("fixture selection");
        stderr
            .write_all(b"whisper_backend_init_gpu: failed to initialize CUDA0 backend")
            .expect("fixture failed initialization");
        stderr.flush().expect("fixture flush");
        thread::sleep(Duration::from_secs(30));
    }
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
