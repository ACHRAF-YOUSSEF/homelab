//! Persisted allocation is distinct from kernel and deployment ceilings.
//! Counts are file slots; OCR slots are a subset of CPU slots. Tools stay at
//! one native thread each. The coordinator alone publishes persisted state.

use crate::resources::Snapshot;
use crate::util::{read_json, write_private_pretty};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const HARD_CPU_LIMIT: usize = 8;
pub const HARD_TOTAL_LIMIT: usize = HARD_CPU_LIMIT + crate::gpu::HARD_GPU_LIMIT;
const HARD_OCR_LIMIT: usize = 2;
const SERVER_RESERVE: u64 = 128 * 1024 * 1024;
const CPU_RESERVE: u64 = 256 * 1024 * 1024;
const OCR_RESERVE: u64 = 1024 * 1024 * 1024;
const GPU_RESERVE: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub cpu_workers: usize,
    pub ocr_workers: usize,
    pub gpu_workers: usize,
    pub paused: bool,
}

impl Default for Allocation {
    fn default() -> Self {
        Self {
            cpu_workers: 1,
            ocr_workers: 1,
            gpu_workers: 0,
            paused: false,
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    cpu_workers: usize,
    ocr_workers: usize,
    gpu_workers: usize,
    paused: bool,
    max_cpu_workers: usize,
    max_ocr_workers: usize,
    max_gpu_workers: usize,
    gpu_min_free_mib: u32,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            cpu_workers: 1,
            ocr_workers: 1,
            gpu_workers: 0,
            paused: false,
            max_cpu_workers: 4,
            max_ocr_workers: 1,
            max_gpu_workers: 1,
            gpu_min_free_mib: 2048,
        }
    }
}

impl Configuration {
    fn allocation(self) -> Allocation {
        Allocation {
            cpu_workers: self.cpu_workers,
            ocr_workers: self.ocr_workers,
            gpu_workers: self.gpu_workers,
            paused: self.paused,
        }
    }
    fn set_allocation(&mut self, allocation: Allocation) {
        self.cpu_workers = allocation.cpu_workers;
        self.ocr_workers = allocation.ocr_workers;
        self.gpu_workers = allocation.gpu_workers;
        self.paused = allocation.paused;
    }
    fn validate(self) -> Result<Self, String> {
        parse_allocation(&json!(self.allocation()))?;
        if !(1..=HARD_CPU_LIMIT).contains(&self.max_cpu_workers)
            || self.max_ocr_workers > HARD_OCR_LIMIT
            || self.max_gpu_workers > crate::gpu::HARD_GPU_LIMIT
            || self.gpu_min_free_mib > 65536
        {
            return Err("Worker configuration exceeds hard safety limits".into());
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Serialize)]
pub struct Limits {
    pub cpu_workers: usize,
    pub ocr_workers: usize,
    pub gpu_workers: usize,
}

#[derive(Clone, Serialize)]
pub struct ActiveWorker {
    pub id: usize,
    pub resource: &'static str,
    pub path: PathBuf,
    pub started: f64,
    pub status: &'static str,
}

pub struct Workers {
    path: PathBuf,
    configuration: Configuration,
    pub allocation: Allocation,
    max_cpu: usize,
    max_ocr: usize,
    ocr_allowed: bool,
    pub gpu: crate::gpu::Capability,
    pub active: Vec<ActiveWorker>,
}

impl Workers {
    pub fn new(path: PathBuf, ocr_allowed: bool) -> Result<Self, String> {
        let existed = path.exists();
        let mut configuration = if existed {
            serde_json::from_value::<Configuration>(read_json(&path, 4096)?)
                .map_err(|_| "workers.conf contains invalid configuration")?
                .validate()?
        } else {
            let mut configuration = Configuration::default();
            let legacy = path.with_file_name("workers.json");
            if legacy.exists() && legacy != path {
                configuration.set_allocation(parse_allocation(&read_json(&legacy, 4096)?)?);
            }
            configuration
        };
        let stored = configuration.allocation();
        let gpu = crate::gpu::Capability::detect(ocr_allowed, configuration.max_gpu_workers);
        let allocation = Allocation {
            cpu_workers: stored.cpu_workers.min(configuration.max_cpu_workers),
            ocr_workers: stored
                .ocr_workers
                .min(configuration.max_ocr_workers)
                .min(stored.cpu_workers.min(configuration.max_cpu_workers)),
            gpu_workers: stored.gpu_workers.min(gpu.max_workers),
            paused: stored.paused,
        };
        configuration.set_allocation(allocation);
        if !existed || allocation != stored {
            write_private_pretty(&path, &json!(configuration))?;
        }
        let workers = Self {
            path,
            configuration,
            allocation,
            max_cpu: configuration.max_cpu_workers,
            max_ocr: configuration.max_ocr_workers,
            ocr_allowed,
            gpu,
            active: Vec::with_capacity(HARD_TOTAL_LIMIT),
        };
        Ok(workers)
    }

    fn validate(&self, value: &Value) -> Result<Allocation, String> {
        let allocation = parse_allocation(value)?;
        if allocation.cpu_workers == 0
            || allocation.cpu_workers > self.max_cpu
            || allocation.ocr_workers > self.max_ocr
            || allocation.ocr_workers > allocation.cpu_workers
            || allocation.gpu_workers > self.gpu.max_workers
            || (allocation.gpu_workers > 0 && !self.gpu.eligible)
        {
            return Err(
                "Worker allocation exceeds deployment limits; GPU OCR is unavailable".into(),
            );
        }
        Ok(allocation)
    }

    pub fn update(&mut self, value: &Value) -> Result<(), String> {
        let allocation = self.validate(value)?;
        let mut configuration = self.configuration;
        configuration.set_allocation(allocation);
        write_private_pretty(&self.path, &json!(configuration))?;
        self.configuration = configuration;
        self.allocation = allocation;
        Ok(())
    }

    pub fn gpu_min_free_mib(&self) -> u32 {
        self.configuration.gpu_min_free_mib
    }

    pub fn limits(&self, sample: &Snapshot) -> Limits {
        let detected = sample
            .cpu_allowance()
            .map(|cores| cores.floor().max(1.0) as usize)
            .or_else(|| std::thread::available_parallelism().ok().map(usize::from))
            .unwrap_or(1)
            .min(HARD_CPU_LIMIT);
        let cpu_memory = memory_slots(sample.memory_limit(), CPU_RESERVE);
        let ocr_memory = sample.memory_limit().map_or(HARD_OCR_LIMIT, |limit| {
            usize::try_from(limit.saturating_sub(SERVER_RESERVE) / OCR_RESERVE)
                .unwrap_or(HARD_OCR_LIMIT)
                .min(HARD_OCR_LIMIT)
        });
        let cpu_workers = self
            .allocation
            .cpu_workers
            .min(self.max_cpu)
            .min(detected)
            .min(cpu_memory);
        Limits {
            cpu_workers,
            ocr_workers: if self.ocr_allowed {
                self.allocation
                    .ocr_workers
                    .min(self.max_ocr)
                    .min(cpu_workers)
                    .min(ocr_memory)
            } else {
                0
            },
            gpu_workers: if self.gpu.eligible {
                self.allocation
                    .gpu_workers
                    .min(self.gpu.max_workers)
                    .min(ocr_memory)
            } else {
                0
            },
        }
    }

    #[cfg(test)]
    pub fn can_start(&self, sample: &Snapshot, ocr: bool) -> bool {
        self.can_start_resource(sample, if ocr { "cpu_ocr" } else { "cpu" })
    }

    pub fn can_start_resource(&self, sample: &Snapshot, resource: &str) -> bool {
        if self.allocation.paused {
            return false;
        }
        let limits = self.limits(sample);
        let active_ocr = self
            .active
            .iter()
            .filter(|worker| worker.resource == "cpu_ocr")
            .count();
        let active_gpu = self
            .active
            .iter()
            .filter(|worker| worker.resource == "gpu")
            .count();
        let active_cpu = self.active.len().saturating_sub(active_gpu);
        let blocked = match resource {
            "gpu" => active_gpu >= limits.gpu_workers,
            "cpu_ocr" => active_cpu >= limits.cpu_workers || active_ocr >= limits.ocr_workers,
            "cpu" => active_cpu >= limits.cpu_workers,
            _ => true,
        };
        if blocked || self.active.len() >= HARD_TOTAL_LIMIT {
            return false;
        }
        let budget = self
            .active
            .iter()
            .fold(SERVER_RESERVE, |used, worker| {
                used.saturating_add(if worker.resource == "gpu" {
                    GPU_RESERVE
                } else if worker.resource == "cpu_ocr" {
                    OCR_RESERVE
                } else {
                    CPU_RESERVE
                })
            })
            .saturating_add(match resource {
                "gpu" => GPU_RESERVE,
                "cpu_ocr" => OCR_RESERVE,
                _ => CPU_RESERVE,
            });
        sample
            .memory_limit()
            .is_none_or(|limit| budget <= limit || (resource == "cpu" && self.active.is_empty()))
    }

    pub fn public(&self, sample: &Snapshot) -> Value {
        let effective = self.limits(sample);
        let active_ocr = self
            .active
            .iter()
            .filter(|worker| worker.resource == "cpu_ocr")
            .count();
        let active_gpu = self
            .active
            .iter()
            .filter(|worker| worker.resource == "gpu")
            .count();
        json!({"edition":crate::edition::NAME,"allocation":self.allocation,"effective":effective,
            "configuration_path":"/data/workers.conf",
            "configuration":self.configuration,
            "deployment":{"max_cpu_workers":self.max_cpu,"max_ocr_workers":self.max_ocr,"max_gpu_workers":self.gpu.max_workers},
            "detected":{"cpu_allowance_cores":sample.cpu_allowance(),"gpu_available":self.gpu.eligible,"gpu_reason":self.gpu.reason,
                "gpu_verification":if self.gpu.verified.is_some() {"verified"} else if self.gpu.eligible {"pending"} else {"unavailable"},"gpu_health":self.gpu.verified},
            "cpu_limit_reason":if effective.cpu_workers < self.allocation.cpu_workers {"Kernel CPU or memory allowance limits active workers"} else {"Allocation is within deployment and kernel allowances"},
            "ocr_limit_reason":if !self.ocr_allowed {"OCR is disabled by deployment permissions"} else if effective.ocr_workers < self.allocation.ocr_workers {"OCR memory or CPU allowance limits active workers"} else {"OCR slots are a subset of CPU slots"},
            "active":{"cpu_workers":self.active.len().saturating_sub(active_gpu),"ocr_workers":active_ocr,"gpu_workers":active_gpu},
            "threads_per_worker":1,"workers":self.active,
            "memory_reservations":{"kind":"soft_estimate","server_bytes":SERVER_RESERVE,"cpu_worker_bytes":CPU_RESERVE,"ocr_worker_bytes":OCR_RESERVE,"gpu_worker_bytes":GPU_RESERVE}})
    }
}

fn parse_allocation(value: &Value) -> Result<Allocation, String> {
    let allocation: Allocation = serde_json::from_value(value.clone())
        .map_err(|_| "Invalid worker allocation; supply CPU, OCR, GPU counts and paused")?;
    if allocation.cpu_workers == 0
        || allocation.cpu_workers > HARD_CPU_LIMIT
        || allocation.ocr_workers > HARD_OCR_LIMIT
        || allocation.ocr_workers > allocation.cpu_workers
        || allocation.gpu_workers > crate::gpu::HARD_GPU_LIMIT
    {
        return Err("Worker allocation exceeds hard safety limits".into());
    }
    Ok(allocation)
}

fn memory_slots(limit: Option<u64>, reserve: u64) -> usize {
    limit.map_or(HARD_CPU_LIMIT, |limit| {
        usize::try_from(limit.saturating_sub(SERVER_RESERVE) / reserve)
            .unwrap_or(HARD_CPU_LIMIT)
            .clamp(1, HARD_CPU_LIMIT)
    })
}

/// Two subtitle jobs whose stems are equal or dot-prefixed may publish the
/// same output (movie.mkv / movie.en.ass). Distinct families can run together.
pub fn conflicts(left: &Path, right: &Path) -> bool {
    if left.parent() != right.parent() {
        return false;
    }
    let left = left.file_stem().unwrap_or_default().to_string_lossy();
    let right = right.file_stem().unwrap_or_default().to_string_lossy();
    #[cfg(windows)]
    let (left, right) = (left.to_lowercase(), right.to_lowercase());
    left == right
        || left
            .strip_prefix(&*right)
            .is_some_and(|suffix| suffix.starts_with('.'))
        || right
            .strip_prefix(&*left)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::write_private;
    #[test]
    fn allocation_is_strict_transactional_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workers.conf");
        let mut workers = Workers::new(path.clone(), true).unwrap();
        let original = read_json(&path, 4096).unwrap();
        for value in [
            json!({"cpu_workers":0,"ocr_workers":1,"gpu_workers":0,"paused":false}),
            json!({"cpu_workers":1,"ocr_workers":1,"gpu_workers":1,"paused":false}),
            json!({"cpu_workers":1,"ocr_workers":1,"gpu_workers":0,"paused":false,"command":"anything"}),
            json!({"cpu_workers":1,"ocr_workers":1,"gpu_workers":0,"paused":"true"}),
        ] {
            assert!(workers.update(&value).is_err());
            assert_eq!(read_json(&path, 4096).unwrap(), original);
        }
        workers
            .update(&json!({"cpu_workers":1,"ocr_workers":0,"gpu_workers":0,"paused":true}))
            .unwrap();
        let loaded = Workers::new(path, true).unwrap();
        assert!(loaded.allocation.paused);
        assert_eq!(loaded.allocation.ocr_workers, 0);
    }

    #[test]
    fn configuration_caps_are_file_backed_and_put_retains_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workers.conf");
        let configuration = Configuration {
            max_cpu_workers: 2,
            gpu_min_free_mib: 3072,
            ..Configuration::default()
        };
        write_private_pretty(&path, &json!(configuration)).unwrap();
        let mut workers = Workers::new(path.clone(), true).unwrap();
        assert!(
            workers
                .update(&json!({"cpu_workers":3,"ocr_workers":1,"gpu_workers":0,"paused":false}))
                .is_err()
        );
        workers
            .update(&json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":false}))
            .unwrap();
        let saved = read_json(&path, 4096).unwrap();
        assert_eq!(saved["max_cpu_workers"], 2);
        assert_eq!(saved["gpu_min_free_mib"], 3072);
        assert_eq!(workers.gpu_min_free_mib(), 3072);
    }

    #[test]
    fn saved_allocation_normalizes_when_deployment_permissions_are_reduced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workers.conf");
        write_private(
            &dir.path().join("workers.json"),
            &json!({"cpu_workers":8,"ocr_workers":2,"gpu_workers":1,"paused":true}),
        )
        .unwrap();
        let workers = Workers::new(path.clone(), false).unwrap();
        assert_eq!(workers.allocation.cpu_workers, workers.max_cpu.min(8));
        assert_eq!(workers.allocation.ocr_workers, 1);
        assert_eq!(workers.allocation.gpu_workers, 0);
        assert!(workers.allocation.paused);
        assert_eq!(read_json(&path, 4096).unwrap()["gpu_workers"], 0);
    }

    #[test]
    fn gpu_slots_are_separate_but_share_the_memory_budget() {
        let dir = tempfile::tempdir().unwrap();
        let mut workers = Workers::new(dir.path().join("workers.conf"), true).unwrap();
        workers.max_cpu = 4;
        workers.allocation.cpu_workers = 2;
        workers.allocation.gpu_workers = 1;
        workers.gpu.max_workers = 1;
        workers.gpu.eligible = true;
        let sample = Snapshot::fixture(4.0, Some(3 * 1024 * 1024 * 1024));
        assert!(workers.can_start_resource(&sample, "gpu"));
        workers.active.push(ActiveWorker {
            id: 0,
            resource: "gpu",
            path: "one.sup".into(),
            started: 0.0,
            status: "extracting",
        });
        assert!(!workers.can_start_resource(&sample, "gpu"));
        assert!(workers.can_start_resource(&sample, "cpu_ocr"));
        assert_eq!(workers.public(&sample)["active"]["cpu_workers"], 0);
        assert_eq!(workers.public(&sample)["active"]["gpu_workers"], 1);
        workers.gpu.failed();
        assert!(!workers.can_start_resource(&sample, "gpu"));
        assert_eq!(
            workers.public(&sample)["detected"]["gpu_verification"],
            "unavailable"
        );
    }

    #[test]
    fn reservations_and_output_families_are_bounded() {
        assert_eq!(memory_slots(Some(512 * 1024 * 1024), CPU_RESERVE), 1);
        assert_eq!(memory_slots(Some(1024 * 1024 * 1024), CPU_RESERVE), 3);
        assert_eq!(memory_slots(Some(u64::MAX), CPU_RESERVE), HARD_CPU_LIMIT);
        assert!(conflicts(
            Path::new("/media/movie.mkv"),
            Path::new("/media/movie.en.ass")
        ));
        assert!(conflicts(
            Path::new("/media/movie.idx"),
            Path::new("/media/movie.sub")
        ));
        assert!(!conflicts(
            Path::new("/media/movie.1.mkv"),
            Path::new("/media/movie.2.mkv")
        ));
        assert!(!conflicts(
            Path::new("/a/movie.mkv"),
            Path::new("/b/movie.mkv")
        ));
    }

    #[test]
    fn admission_respects_cpu_memory_ocr_subset_and_pause() {
        let dir = tempfile::tempdir().unwrap();
        let mut workers = Workers::new(dir.path().join("workers.conf"), true).unwrap();
        workers.max_cpu = 4;
        workers.allocation.cpu_workers = 4;
        let sample = Snapshot::fixture(4.0, Some(1536 * 1024 * 1024));
        assert_eq!(workers.limits(&sample).cpu_workers, 4);
        assert!(workers.can_start(&sample, true));
        workers.active.push(ActiveWorker {
            id: 0,
            resource: "cpu_ocr",
            path: "one.sup".into(),
            started: 0.0,
            status: "processing",
        });
        assert!(!workers.can_start(&sample, true));
        assert!(workers.can_start(&sample, false));
        workers.active.push(ActiveWorker {
            id: 1,
            resource: "cpu",
            path: "two.ass".into(),
            started: 0.0,
            status: "processing",
        });
        assert!(!workers.can_start(&sample, false));
        workers.active.clear();
        workers.allocation.paused = true;
        assert!(!workers.can_start(&sample, false));
        workers.allocation.paused = false;
        assert_eq!(workers.limits(&Snapshot::fixture(0.5, None)).cpu_workers, 1);
        assert_eq!(
            workers
                .limits(&Snapshot::fixture(4.0, Some(512 * 1024 * 1024)))
                .ocr_workers,
            0
        );
    }
}
