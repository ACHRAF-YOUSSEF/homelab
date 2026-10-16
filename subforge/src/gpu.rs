//! Optional trusted GPU adapter boundary. Core builds never import GPU code.
//! Eligibility requires the compiled CUDA edition, grants and fixed files; only the adapter's
//! bounded per-file CUDA health check establishes runtime verification.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub const ADAPTER: &str = "/usr/local/bin/subforge-ocr";
pub const HARD_GPU_LIMIT: usize = 4;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Health {
    ok: bool,
    cuda: bool,
    device_count: u32,
    free_bytes: u64,
    total_bytes: u64,
    /// CUDA preflight does not import or validate OCR model artifacts.
    #[serde(default)]
    models_checked: bool,
}

pub struct Capability {
    pub max_workers: usize,
    pub eligible: bool,
    pub reason: &'static str,
    pub verified: Option<Health>,
}

impl Capability {
    pub fn detect(ocr_allowed: bool, granted: usize) -> Self {
        Self::detect_with(
            ocr_allowed,
            granted,
            || {
                std::env::var("CONVERTER_ENABLE_GPU")
                    .is_ok_and(|value| value == "true" || value == "1")
            },
            fixed_adapter_available,
            fixed_device_available,
        )
    }

    fn detect_with(
        ocr_allowed: bool,
        granted: usize,
        permitted: impl FnOnce() -> bool,
        adapter_available: impl FnOnce() -> bool,
        device_available: impl FnOnce() -> bool,
    ) -> Self {
        let granted = granted.min(HARD_GPU_LIMIT);
        let (eligible, reason) = if !crate::edition::CUDA_ENABLED {
            (false, "GPU OCR requires the CUDA image edition")
        } else if granted == 0 || !permitted() {
            (false, "GPU OCR is disabled by deployment permissions")
        } else if !ocr_allowed {
            (false, "OCR is disabled by deployment permissions")
        } else if !adapter_available() {
            (false, "The optional trusted GPU OCR adapter is unavailable")
        } else if !device_available() {
            (
                false,
                "An explicitly granted NVIDIA GPU device is unavailable",
            )
        } else {
            (
                true,
                "GPU runtime enabled; CUDA and offline models are checked before each file",
            )
        };
        Self {
            max_workers: if eligible { granted } else { 0 },
            eligible,
            reason,
            verified: None,
        }
    }

    pub fn failed(&mut self) {
        self.eligible = false;
        self.verified = None;
        self.reason =
            "GPU CUDA or offline model verification failed; restart after correcting deployment";
    }
}

fn fixed_adapter_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = Path::new(ADAPTER);
        if path.ancestors().any(|part| {
            std::fs::symlink_metadata(part).is_ok_and(|metadata| metadata.file_type().is_symlink())
        }) {
            return false;
        }
        std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn fixed_device_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::FileTypeExt;
        let nvidia = ["/dev/nvidia0", "/dev/nvidiactl"].iter().all(|path| {
            std::fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.file_type().is_char_device())
        });
        nvidia
            || std::fs::symlink_metadata("/dev/dxg")
                .is_ok_and(|metadata| metadata.file_type().is_char_device())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub fn probe(stop: &AtomicBool, minimum_free_mib: u32) -> Result<Health, &'static str> {
    if !crate::edition::CUDA_ENABLED {
        return Err("GPU OCR requires the CUDA image edition");
    }
    if !fixed_adapter_available() || !fixed_device_available() {
        return Err("The explicitly granted GPU runtime is unavailable");
    }
    let mut command = Command::new(Path::new(ADAPTER));
    command.arg("--health-json");
    let (success, bytes) = crate::util::bounded_gpu_process(
        &mut command,
        Duration::from_secs(30),
        stop,
        4096,
        minimum_free_mib,
    )
    .map_err(|_| "GPU CUDA verification failed or was cancelled")?;
    parse_health(success, &bytes, u64::from(minimum_free_mib) * 1024 * 1024)
}

fn parse_health(success: bool, bytes: &[u8], minimum_free: u64) -> Result<Health, &'static str> {
    let health: Health =
        serde_json::from_slice(bytes).map_err(|_| "GPU CUDA verification failed")?;
    if !success
        || !health.ok
        || !health.cuda
        || health.device_count != 1
        || health.free_bytes < minimum_free
        || health.free_bytes > health.total_bytes
        || health.total_bytes == 0
    {
        return Err("GPU CUDA verification failed");
    }
    Ok(health)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "cuda"))]
    #[test]
    fn cpu_edition_denies_gpu_before_permissions_or_runtime_detection() {
        let capability = Capability::detect_with(
            true,
            HARD_GPU_LIMIT,
            || panic!("CPU edition must not inspect GPU permissions"),
            || panic!("CPU edition must not inspect the GPU adapter"),
            || panic!("CPU edition must not inspect GPU devices"),
        );
        assert!(!capability.eligible);
        assert_eq!(capability.max_workers, 0);
        assert!(capability.verified.is_none());
        assert_eq!(capability.reason, "GPU OCR requires the CUDA image edition");
        assert!(probe(&AtomicBool::new(false), 0).is_err());
    }

    #[cfg(feature = "cuda")]
    #[test]
    fn cuda_edition_requires_all_grants_and_fixed_runtime_before_health() {
        for (ocr, granted, permitted, adapter, device) in [
            (false, 1, true, true, true),
            (true, 0, true, true, true),
            (true, 1, false, true, true),
            (true, 1, true, false, true),
            (true, 1, true, true, false),
        ] {
            let capability =
                Capability::detect_with(ocr, granted, || permitted, || adapter, || device);
            assert!(!capability.eligible);
            assert_eq!(capability.max_workers, 0);
            assert!(capability.verified.is_none());
        }
        let capability = Capability::detect_with(true, usize::MAX, || true, || true, || true);
        assert!(capability.eligible);
        assert_eq!(capability.max_workers, HARD_GPU_LIMIT);
        assert!(capability.verified.is_none());
    }

    #[test]
    fn health_requires_success_cuda_device_and_valid_memory() {
        let valid = br#"{"ok":true,"cuda":true,"device_count":1,"free_bytes":2147483648,"total_bytes":8589934592}"#;
        assert!(parse_health(true, valid, 2147483648).is_ok());
        assert!(parse_health(true, br#"{"ok":true,"cuda":true,"device_count":1,"free_bytes":2147483648,"total_bytes":8589934592,"models_checked":false}"#, 2147483648).is_ok());
        assert!(parse_health(false, valid, 2147483648).is_err());
        assert!(parse_health(true, valid, 4294967296).is_err());
        for invalid in [br#"{"ok":false,"error":"generic failure"}"#.as_slice(),
            br#"{"ok":true,"cuda":false,"device_count":1,"free_bytes":1,"total_bytes":2}"#,
            br#"{"ok":true,"cuda":true,"device_count":0,"free_bytes":1,"total_bytes":2}"#,
            br#"{"ok":true,"cuda":true,"device_count":1,"free_bytes":3,"total_bytes":2}"#,
            br#"{"ok":true,"cuda":true,"device_count":1,"free_bytes":1,"total_bytes":2,"command":"arbitrary"}"#] {
            assert!(parse_health(true, invalid, 1).is_err());
        }
    }
}
