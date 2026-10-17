//! Persisted allocation is distinct from kernel and deployment ceilings.
//! Counts are file slots; OCR slots are a subset of CPU slots. Tools stay at
//! one native thread each. The coordinator alone publishes persisted state.

use crate::resources::Snapshot;
use crate::util::{read_json, write_private_pretty};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const HARD_CPU_LIMIT: usize = 32;
pub const HARD_TOTAL_LIMIT: usize = HARD_CPU_LIMIT + crate::gpu::HARD_GPU_LIMIT;
const HARD_OCR_LIMIT: usize = 16;
/// A persisted target is a scalar, never a thread or buffer allocation size.
pub const MAX_REQUESTED: usize = 1_000_000;
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
            max_cpu_workers: HARD_CPU_LIMIT,
            max_ocr_workers: HARD_OCR_LIMIT,
            max_gpu_workers: crate::gpu::HARD_GPU_LIMIT,
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
        let configuration = if existed {
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
        let gpu = crate::gpu::Capability::detect(ocr_allowed, configuration.max_gpu_workers);
        // Retain intent across resource pressure, image changes and restarts.
        // Capability and deployment limits apply only at admission time.
        let allocation = configuration.allocation();
        if !existed {
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

    pub fn update(&mut self, value: &Value) -> Result<(), String> {
        let allocation = parse_allocation(value)?;
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
        // CPU quotas limit runtime throughput, not how many single-threaded
        // jobs may wait for I/O. Explicit targets may oversubscribe that quota.
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
        self.admission_reason(sample, resource).is_none()
    }

    pub fn admission_reason(&self, sample: &Snapshot, resource: &str) -> Option<&'static str> {
        if self.allocation.paused {
            return Some("Worker dispatch is paused; active files finish normally");
        }
        if resource == "gpu" && !self.gpu.eligible {
            return Some(self.gpu.reason);
        }
        if resource == "cpu_ocr" && !self.ocr_allowed {
            return Some("OCR is disabled by deployment permissions");
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
            return Some(match resource {
                "gpu" if self.allocation.gpu_workers == 0 => "No GPU workers requested",
                "cpu_ocr" if self.allocation.ocr_workers == 0 => "No CPU OCR workers requested",
                "cpu" | "cpu_ocr" | "gpu" => {
                    "Effective worker slots are occupied or limited by deployment and memory reservations"
                }
                _ => "Unsupported worker resource",
            });
        }
        let reserve = match resource {
            "gpu" => GPU_RESERVE,
            "cpu_ocr" => OCR_RESERVE,
            _ => CPU_RESERVE,
        };
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
            .saturating_add(reserve);
        if sample.memory_limit().is_some_and(|limit| budget > limit) {
            return Some("Memory reservations leave insufficient space for another worker");
        }
        if let (Some(limit), Some(current)) = (sample.memory_limit(), sample.memory_current())
            && reserve > limit.saturating_sub(current)
        {
            return Some(
                "Current cgroup memory pressure leaves insufficient headroom for another worker",
            );
        }
        None
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
        let admission = |resource| {
            let reason = self.admission_reason(sample, resource);
            json!({"allowed":reason.is_none(),"reason":reason.unwrap_or("Ready for another worker; runtime checks still apply")})
        };
        json!({"edition":crate::edition::NAME,"allocation":self.allocation,"requested":self.allocation,"effective":effective,
            "metrics":{"active_file_workers":self.active.len(),"total_file_slots":effective.cpu_workers+effective.gpu_workers,"cpu_slots_including_ocr":effective.cpu_workers,"ocr_slots_within_cpu":effective.ocr_workers,"gpu_slots":effective.gpu_workers,"scope":"local_workers","cpu_and_gpu_slots_are_additive":true,"ocr_is_a_subset_of_cpu":true},
            "max_requested":MAX_REQUESTED,
            "admission":{"cpu":admission("cpu"),"cpu_ocr":admission("cpu_ocr"),"gpu":admission("gpu")},
            "configuration_path":"/data/workers.conf",
            "configuration":self.configuration,
            "deployment":{"max_cpu_workers":self.max_cpu,"max_ocr_workers":self.max_ocr,"max_gpu_workers":self.gpu.max_workers},
            "detected":{"cpu_allowance_cores":sample.cpu_allowance(),"gpu_available":self.gpu.eligible,"gpu_reason":self.gpu.reason,
                "gpu_verification":if self.gpu.verified.is_some() {"verified"} else if self.gpu.eligible {"pending"} else {"unavailable"},"gpu_health":self.gpu.verified},
            "cpu_limit_reason":if effective.cpu_workers < self.allocation.cpu_workers {"Deployment safety ceiling or memory reservations limit active workers; requested target is retained"} else {"Requested concurrency admitted; CPU quota controls throughput and may be oversubscribed"},
            "ocr_limit_reason":if !self.ocr_allowed {"OCR is disabled by deployment permissions"} else if effective.ocr_workers < self.allocation.ocr_workers {"OCR memory or CPU allowance limits active workers"} else {"OCR slots are a subset of CPU slots"},
            "gpu_limit_reason":if !self.gpu.eligible {self.gpu.reason} else if effective.gpu_workers < self.allocation.gpu_workers {"Deployment GPU safety ceiling or host memory reservations limit active workers; requested target is retained"} else {"GPU workers still require per-file CUDA, VRAM and offline-model verification"},
            "active":{"cpu_workers":self.active.len().saturating_sub(active_gpu),"ocr_workers":active_ocr,"gpu_workers":active_gpu},
            "threads_per_worker":1,"workers":self.active,
            "memory_reservations":{"kind":"soft_estimate","server_bytes":SERVER_RESERVE,"cpu_worker_bytes":CPU_RESERVE,"ocr_worker_bytes":OCR_RESERVE,"gpu_worker_bytes":GPU_RESERVE}})
    }
}

fn parse_allocation(value: &Value) -> Result<Allocation, String> {
    let allocation: Allocation = serde_json::from_value(value.clone())
        .map_err(|_| "Invalid worker allocation; supply CPU, OCR, GPU counts and paused")?;
    if allocation.cpu_workers == 0
        || allocation.cpu_workers > MAX_REQUESTED
        || allocation.ocr_workers > MAX_REQUESTED
        || allocation.ocr_workers > allocation.cpu_workers
        || allocation.gpu_workers > MAX_REQUESTED
    {
        return Err("Worker targets must be between zero and 1000000; CPU must be positive and OCR cannot exceed CPU".into());
    }
    Ok(allocation)
}

fn memory_slots(limit: Option<u64>, reserve: u64) -> usize {
    limit.map_or(HARD_CPU_LIMIT, |limit| {
        usize::try_from(limit.saturating_sub(SERVER_RESERVE) / reserve)
            .unwrap_or(HARD_CPU_LIMIT)
            .min(HARD_CPU_LIMIT)
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
            json!({"cpu_workers":1,"ocr_workers":1,"gpu_workers":1000001,"paused":false}),
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
        workers
            .update(&json!({"cpu_workers":3,"ocr_workers":1,"gpu_workers":0,"paused":false}))
            .unwrap();
        assert_eq!(workers.allocation.cpu_workers, 3);
        assert_eq!(workers.limits(&Snapshot::fixture(1.0, None)).cpu_workers, 2);
        workers
            .update(&json!({"cpu_workers":2,"ocr_workers":1,"gpu_workers":0,"paused":false}))
            .unwrap();
        let saved = read_json(&path, 4096).unwrap();
        assert_eq!(saved["max_cpu_workers"], 2);
        assert_eq!(saved["gpu_min_free_mib"], 3072);
        assert_eq!(workers.gpu_min_free_mib(), 3072);
    }

    #[test]
    fn saved_targets_survive_reduced_deployment_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workers.conf");
        write_private(
            &dir.path().join("workers.json"),
            &json!({"cpu_workers":8,"ocr_workers":2,"gpu_workers":1,"paused":true}),
        )
        .unwrap();
        let workers = Workers::new(path.clone(), false).unwrap();
        assert_eq!(workers.allocation.cpu_workers, 8);
        assert_eq!(workers.allocation.ocr_workers, 2);
        assert_eq!(workers.allocation.gpu_workers, 1);
        assert_eq!(workers.limits(&Snapshot::fixture(4.0, None)).gpu_workers, 0);
        assert!(workers.allocation.paused);
        assert_eq!(read_json(&path, 4096).unwrap()["gpu_workers"], 1);
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
        assert_eq!(workers.limits(&Snapshot::fixture(0.5, None)).cpu_workers, 4);
        assert_eq!(
            workers
                .limits(&Snapshot::fixture(4.0, Some(512 * 1024 * 1024)))
                .ocr_workers,
            0
        );
    }

    #[test]
    fn large_targets_are_preserved_without_large_execution_allocations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workers.conf");
        let mut workers = Workers::new(path.clone(), false).unwrap();
        workers.update(&json!({"cpu_workers":MAX_REQUESTED,"ocr_workers":MAX_REQUESTED,"gpu_workers":MAX_REQUESTED,"paused":false})).unwrap();
        let sample = Snapshot::fixture(0.5, Some(1536 * 1024 * 1024));
        let public = workers.public(&sample);
        assert_eq!(public["requested"]["gpu_workers"], MAX_REQUESTED);
        assert_eq!(public["effective"]["cpu_workers"], 5);
        assert_eq!(public["effective"]["ocr_workers"], 0);
        assert_eq!(public["effective"]["gpu_workers"], 0);
        assert_eq!(public["admission"]["gpu"]["allowed"], false);
        assert!(workers.active.capacity() <= HARD_TOTAL_LIMIT);
        assert_eq!(
            Workers::new(path, false).unwrap().allocation.cpu_workers,
            MAX_REQUESTED
        );
    }

    #[test]
    fn live_memory_pressure_blocks_new_files_with_an_explicit_reason() {
        let dir = tempfile::tempdir().unwrap();
        let mut workers = Workers::new(dir.path().join("workers.conf"), true).unwrap();
        workers.gpu.eligible = true;
        workers.gpu.max_workers = 1;
        workers.allocation.gpu_workers = 1;
        let sample = Snapshot::fixture(4.0, Some(1536 * 1024 * 1024))
            .with_memory_current(1400 * 1024 * 1024);
        for resource in ["cpu", "cpu_ocr", "gpu"] {
            assert!(!workers.can_start_resource(&sample, resource));
            assert!(
                workers
                    .admission_reason(&sample, resource)
                    .unwrap()
                    .contains("memory pressure")
            );
        }
        assert_eq!(
            workers.public(&sample)["admission"]["gpu"]["allowed"],
            false
        );
        let recovered = sample.with_memory_current(128 * 1024 * 1024);
        assert!(workers.can_start_resource(&recovered, "gpu"));
    }
    #[test]
    fn public_file_capacity_adds_cpu_and_gpu_but_never_double_counts_ocr() {
        let dir = tempfile::tempdir().unwrap();
        let mut workers = Workers::new(dir.path().join("workers.conf"), true).unwrap();
        workers.allocation.cpu_workers = 2;
        workers.allocation.ocr_workers = 1;
        workers.allocation.gpu_workers = 1;
        workers.gpu.eligible = true;
        workers.gpu.max_workers = 1;
        let public = workers.public(&Snapshot::fixture(8.0, Some(8 * 1024 * 1024 * 1024)));
        assert_eq!(public["metrics"]["total_file_slots"], 3);
        assert_eq!(public["metrics"]["cpu_slots_including_ocr"], 2);
        assert_eq!(public["metrics"]["ocr_slots_within_cpu"], 1);
        assert_eq!(public["metrics"]["active_file_workers"], 0);
    }
}
