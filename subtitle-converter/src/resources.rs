//! Small, cached kernel telemetry. No subprocesses, media walks, or dependencies.
//! CPU usage covers the cgroup (including conversion children); RSS and thread
//! count cover only this server process. Unknown measurements remain `None`.

use serde::Serialize;
use std::time::{Duration, Instant};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(3);
#[cfg(any(target_os = "linux", test))]
const FILE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CpuSampleState {
    Available,
    Collecting,
    #[default]
    Unavailable,
}

#[derive(Clone, Default, Serialize)]
pub struct CpuUsage {
    source: Option<&'static str>,
    quota_cores: Option<f64>,
    limit_unlimited: Option<bool>,
    allowed_logical_cpus: Option<u32>,
    effective_cores: Option<f64>,
    usage_sample_state: CpuSampleState,
    /// 100% is one logical CPU, not 100% of the host or CPU allowance.
    usage_percent: Option<f64>,
    /// 100% is the reported effective CPU allowance.
    quota_usage_percent: Option<f64>,
}

#[derive(Clone, Default, Serialize)]
pub struct MemoryUsage {
    process_rss_bytes: Option<u64>,
    cgroup_current_bytes: Option<u64>,
    cgroup_limit_bytes: Option<u64>,
    cgroup_limit_unlimited: Option<bool>,
}

#[derive(Clone, Serialize)]
pub struct ProcessUsage {
    id: u32,
    threads: Option<u32>,
}

#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub sample_age_seconds: f64,
    sample_interval_seconds: u64,
    pub cpu: CpuUsage,
    memory: MemoryUsage,
    process: ProcessUsage,
}

impl Snapshot {
    #[cfg(test)]
    pub fn fixture(cpu: f64, memory: Option<u64>) -> Self {
        Self {
            sample_age_seconds: 0.0,
            sample_interval_seconds: 3,
            cpu: CpuUsage {
                effective_cores: Some(cpu),
                ..CpuUsage::default()
            },
            memory: MemoryUsage {
                cgroup_limit_bytes: memory,
                ..MemoryUsage::default()
            },
            process: ProcessUsage {
                id: std::process::id(),
                threads: None,
            },
        }
    }
    pub fn cpu_allowance(&self) -> Option<f64> {
        self.cpu.effective_cores
    }

    pub fn memory_limit(&self) -> Option<u64> {
        self.memory.cgroup_limit_bytes
    }
}

#[derive(Default)]
struct RawSample {
    cpu: CpuUsage,
    memory: MemoryUsage,
    threads: Option<u32>,
    cpu_usage_nanoseconds: Option<u64>,
}

#[derive(Default)]
pub struct Sampler {
    cached: Option<(Instant, Snapshot)>,
    previous_cpu: Option<(Instant, u64, Option<&'static str>, Option<f64>)>,
}

impl Sampler {
    #[cfg(test)]
    pub fn set_fixture(&mut self, sample: Snapshot) {
        self.cached = Some((Instant::now(), sample));
    }
    pub fn snapshot(&mut self) -> Snapshot {
        let now = Instant::now();
        if let Some((sampled, snapshot)) = &self.cached
            && now.duration_since(*sampled) < SAMPLE_INTERVAL
        {
            let mut snapshot = snapshot.clone();
            snapshot.sample_age_seconds = now.duration_since(*sampled).as_secs_f64();
            return snapshot;
        }
        self.update(now, read_kernel())
    }

    fn update(&mut self, now: Instant, mut raw: RawSample) -> Snapshot {
        if let Some((previous_time, previous_usage, previous_source, previous_allowance)) =
            self.previous_cpu
            && previous_source == raw.cpu.source
            && previous_allowance == raw.cpu.effective_cores
            && let Some(current_usage) = raw.cpu_usage_nanoseconds
        {
            raw.cpu.usage_percent = cpu_percent(
                previous_usage,
                current_usage,
                now.saturating_duration_since(previous_time),
            );
            raw.cpu.quota_usage_percent = raw.cpu.usage_percent.and_then(|usage| {
                finite_positive(raw.cpu.effective_cores?).map(|allowance| usage / allowance)
            });
        }
        self.previous_cpu = raw
            .cpu_usage_nanoseconds
            .map(|usage| (now, usage, raw.cpu.source, raw.cpu.effective_cores));
        raw.cpu.usage_sample_state = if raw.cpu_usage_nanoseconds.is_none() {
            CpuSampleState::Unavailable
        } else if raw.cpu.usage_percent.is_some() {
            CpuSampleState::Available
        } else {
            CpuSampleState::Collecting
        };
        let snapshot = Snapshot {
            sample_age_seconds: 0.0,
            sample_interval_seconds: SAMPLE_INTERVAL.as_secs(),
            cpu: raw.cpu,
            memory: raw.memory,
            process: ProcessUsage {
                id: std::process::id(),
                threads: raw.threads,
            },
        };
        self.cached = Some((now, snapshot.clone()));
        snapshot
    }
}

fn finite_positive(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

fn cpu_percent(previous: u64, current: u64, elapsed: Duration) -> Option<f64> {
    let delta = current.checked_sub(previous)?;
    let seconds = finite_positive(elapsed.as_secs_f64())?;
    let value = delta as f64 / 1_000_000_000.0 / seconds * 100.0;
    (value.is_finite() && value >= 0.0).then_some(value)
}

#[cfg(target_os = "linux")]
fn read_kernel() -> RawSample {
    read_linux(&|path| read_bounded(std::path::Path::new(path)))
}

#[cfg(not(target_os = "linux"))]
fn read_kernel() -> RawSample {
    RawSample::default()
}

/// Every caller supplies an explicit kernel path; this does not follow names
/// obtained from requests, media, environment variables, or cgroup membership.
#[cfg(any(target_os = "linux", test))]
fn read_bounded(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    // /proc/<numeric server pid> avoids the /proc/self symlink. cgroup files
    // must be ordinary files, never user-controlled symlinks or devices.
    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take((FILE_LIMIT + 1) as u64)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= FILE_LIMIT).then_some(text)
}

#[cfg(any(target_os = "linux", test))]
fn read_linux(read: &impl Fn(&str) -> Option<String>) -> RawSample {
    let mut sample = RawSample::default();
    let status = read(&format!("/proc/{}/status", std::process::id()));
    if let Some(status) = status {
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("VmRSS:") {
                let mut parts = value.split_whitespace();
                sample.memory.process_rss_bytes = parts
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|_| parts.next() == Some("kB") && parts.next().is_none())
                    .and_then(|kb| kb.checked_mul(1024));
            } else if let Some(value) = line.strip_prefix("Threads:") {
                sample.threads = value.trim().parse().ok();
            } else if let Some(value) = line.strip_prefix("Cpus_allowed_list:") {
                sample.cpu.allowed_logical_cpus = cpu_list_count(value.trim());
            }
        }
    }
    // Fixed cgroup root files describe this process only when its namespace
    // membership is root. Avoid accidentally reporting host-wide measurements
    // on native Linux or a container sharing the host cgroup namespace. Never
    // turn a kernel membership string into a filesystem path.
    let membership = read(&format!("/proc/{}/cgroup", std::process::id()));
    let v2_root = membership
        .as_deref()
        .is_some_and(|value| value.lines().any(|line| line == "0::/"));
    let v1_root = membership.as_deref().is_some_and(|value| {
        !value.trim().is_empty()
            && value
                .lines()
                .all(|line| line.splitn(3, ':').nth(2) == Some("/"))
    });
    if !v2_root && !v1_root {
        return sample;
    }
    if let Some(max) = read("/sys/fs/cgroup/cpu.max") {
        sample.cpu.source = Some("cgroup_v2");
        if let Some((quota, unlimited)) = cpu_max(&max) {
            sample.cpu.quota_cores = quota;
            sample.cpu.limit_unlimited = Some(unlimited);
        }
        sample.cpu_usage_nanoseconds = read("/sys/fs/cgroup/cpu.stat")
            .and_then(|value| named_counter(&value, "usage_usec"))
            .and_then(|usage| usage.checked_mul(1000));
    } else if let Some(quota) = read("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
        .or_else(|| read("/sys/fs/cgroup/cpu,cpuacct/cpu.cfs_quota_us"))
    {
        sample.cpu.source = Some("cgroup_v1");
        let period = read("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
            .or_else(|| read("/sys/fs/cgroup/cpu,cpuacct/cpu.cfs_period_us"));
        if let Some((cores, unlimited)) = period
            .as_deref()
            .and_then(|period| cpu_v1_max(&quota, period))
        {
            sample.cpu.quota_cores = cores;
            sample.cpu.limit_unlimited = Some(unlimited);
        }
        sample.cpu_usage_nanoseconds = read("/sys/fs/cgroup/cpuacct/cpuacct.usage")
            .or_else(|| read("/sys/fs/cgroup/cpu,cpuacct/cpuacct.usage"))
            .and_then(|value| value.trim().parse().ok());
    }
    // The exposed cgroup mount must be the process namespace root, as in the
    // supplied Docker stack. A shared host mount cannot safely infer the
    // process's cgroup from these fixed paths; see deployment documentation.
    if let Some(current) = read("/sys/fs/cgroup/memory.current") {
        sample.memory.cgroup_current_bytes = current.trim().parse().ok();
        if let Some(limit) = read("/sys/fs/cgroup/memory.max").and_then(|value| memory_max(&value))
        {
            sample.memory.cgroup_limit_bytes = limit.0;
            sample.memory.cgroup_limit_unlimited = Some(limit.1);
        }
    } else if let Some(current) = read("/sys/fs/cgroup/memory/memory.usage_in_bytes") {
        sample.memory.cgroup_current_bytes = current.trim().parse().ok();
        if let Some(limit) = read("/sys/fs/cgroup/memory/memory.limit_in_bytes")
            .and_then(|value| value.trim().parse::<u64>().ok())
        {
            // v1 represents no limit with a page-rounded signed integer max.
            let unlimited = limit >= (1_u64 << 60);
            sample.memory.cgroup_limit_bytes = (!unlimited).then_some(limit);
            sample.memory.cgroup_limit_unlimited = Some(unlimited);
        }
    }
    let allowed = sample.cpu.allowed_logical_cpus.map(f64::from);
    sample.cpu.effective_cores =
        match (sample.cpu.quota_cores, allowed, sample.cpu.limit_unlimited) {
            (Some(quota), Some(allowed), _) => Some(quota.min(allowed)),
            (Some(quota), None, _) => Some(quota),
            (None, allowed, Some(true)) => allowed,
            _ => None,
        }
        .and_then(finite_positive);
    sample
}

#[cfg(any(target_os = "linux", test))]
fn cpu_max(value: &str) -> Option<(Option<f64>, bool)> {
    let mut parts = value.split_whitespace();
    let quota = parts.next()?;
    let period = parts.next()?.parse::<u64>().ok()?;
    if period == 0 || parts.next().is_some() {
        return None;
    }
    if quota == "max" {
        Some((None, true))
    } else {
        let quota = quota.parse::<u64>().ok()?;
        finite_positive(quota as f64 / period as f64).map(|cores| (Some(cores), false))
    }
}

#[cfg(any(target_os = "linux", test))]
fn cpu_v1_max(quota: &str, period: &str) -> Option<(Option<f64>, bool)> {
    let period = period.trim().parse::<u64>().ok()?;
    if period == 0 {
        return None;
    }
    if quota.trim() == "-1" {
        return Some((None, true));
    }
    let quota = quota.trim().parse::<u64>().ok()?;
    finite_positive(quota as f64 / period as f64).map(|cores| (Some(cores), false))
}

#[cfg(any(target_os = "linux", test))]
fn memory_max(value: &str) -> Option<(Option<u64>, bool)> {
    if value.trim() == "max" {
        Some((None, true))
    } else {
        value.trim().parse().ok().map(|limit| (Some(limit), false))
    }
}

#[cfg(any(target_os = "linux", test))]
fn named_counter(value: &str, name: &str) -> Option<u64> {
    value.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        if parts.next()? != name {
            return None;
        }
        let value = parts.next()?.parse().ok()?;
        parts.next().is_none().then_some(value)
    })
}

#[cfg(any(target_os = "linux", test))]
fn cpu_list_count(value: &str) -> Option<u32> {
    let mut count = 0_u32;
    let mut previous_end = None;
    // No iteration over CPU IDs: ranges are counted arithmetically.
    for segment in value.split(',') {
        let (start, end) = if let Some((start, end)) = segment.split_once('-') {
            (start.parse::<u32>().ok()?, end.parse::<u32>().ok()?)
        } else {
            let cpu = segment.parse::<u32>().ok()?;
            (cpu, cpu)
        };
        if start > end || previous_end.is_some_and(|previous| start <= previous) {
            return None;
        }
        count = count.checked_add(end.checked_sub(start)?.checked_add(1)?)?;
        previous_end = Some(end);
    }
    (count > 0).then_some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn quota_parsers_reject_invalid_and_preserve_fractional_limits() {
        assert_eq!(cpu_max("400000 100000\n"), Some((Some(4.0), false)));
        assert_eq!(cpu_max("50000 100000"), Some((Some(0.5), false)));
        assert_eq!(cpu_max("max 100000"), Some((None, true)));
        assert_eq!(cpu_v1_max("-1", "100000"), Some((None, true)));
        for invalid in [
            "0 100000",
            "1 0",
            "NaN 100",
            "inf 100",
            "-1 100",
            "2 100 extra",
            "max 0",
        ] {
            assert!(cpu_max(invalid).is_none(), "{invalid}");
        }
        assert!(cpu_v1_max("-2", "100").is_none());
        assert!(memory_max("-1").is_none());
        assert_eq!(memory_max("max"), Some((None, true)));
        assert!(finite_positive(f64::INFINITY).is_none());
        assert!(finite_positive(f64::NAN).is_none());
    }

    #[test]
    fn cpu_deltas_are_core_percent_and_reset_is_unknown() {
        assert_eq!(
            cpu_percent(0, 6_000_000_000, Duration::from_secs(3)),
            Some(200.0)
        );
        assert_eq!(cpu_percent(6, 3, Duration::from_secs(3)), None);
        assert_eq!(cpu_percent(0, 10, Duration::ZERO), None);
        let start = Instant::now();
        let raw = |usage| RawSample {
            cpu: CpuUsage {
                source: Some("cgroup_v2"),
                effective_cores: Some(4.0),
                ..CpuUsage::default()
            },
            cpu_usage_nanoseconds: Some(usage),
            ..RawSample::default()
        };
        let mut sampler = Sampler::default();
        assert_eq!(sampler.update(start, raw(0)).cpu.usage_percent, None);
        let result = sampler.update(start + Duration::from_secs(3), raw(6_000_000_000));
        assert_eq!(result.cpu.usage_percent, Some(200.0));
        assert_eq!(result.cpu.quota_usage_percent, Some(50.0));
        assert_eq!(
            sampler
                .update(start + Duration::from_secs(6), raw(1))
                .cpu
                .usage_percent,
            None
        );
    }

    #[test]
    fn cpu_ranges_are_bounded_and_reject_overlap() {
        assert_eq!(cpu_list_count("0-3,8,10-11"), Some(7));
        assert_eq!(cpu_list_count("0-4294967295"), None);
        for invalid in ["", "3-1", "0-3,2", "1,1", "0-1-2", "NaN"] {
            assert!(cpu_list_count(invalid).is_none());
        }
    }

    #[test]
    fn cpu_sample_state_distinguishes_missing_initial_valid_and_reset_counters() {
        let membership = format!("/proc/{}/cgroup", std::process::id());
        let missing_counter = || {
            read_linux(&|name| {
                if name == membership {
                    Some("0::/\n".into())
                } else if name == "/sys/fs/cgroup/cpu.max" {
                    Some("400000 100000\n".into())
                } else {
                    None
                }
            })
        };
        let counter = |usage| {
            let mut raw = missing_counter();
            raw.cpu_usage_nanoseconds = Some(usage);
            raw
        };
        let start = Instant::now();
        let mut sampler = Sampler::default();
        let missing = sampler.update(start, missing_counter());
        assert_eq!(missing.cpu.source, Some("cgroup_v2"));
        assert_eq!(missing.cpu.usage_sample_state, CpuSampleState::Unavailable);
        assert_eq!(
            serde_json::to_value(&missing.cpu).unwrap()["usage_sample_state"],
            "unavailable"
        );
        assert_eq!(
            sampler
                .update(start + Duration::from_secs(3), counter(0))
                .cpu
                .usage_sample_state,
            CpuSampleState::Collecting
        );
        assert_eq!(
            sampler
                .update(start + Duration::from_secs(6), counter(3_000_000_000))
                .cpu
                .usage_sample_state,
            CpuSampleState::Available
        );
        assert_eq!(
            sampler
                .update(start + Duration::from_secs(9), counter(1))
                .cpu
                .usage_sample_state,
            CpuSampleState::Collecting
        );
        let missing_again = sampler.update(start + Duration::from_secs(12), missing_counter());
        assert_eq!(
            missing_again.cpu.usage_sample_state,
            CpuSampleState::Unavailable
        );
        assert_eq!(missing_again.cpu.usage_percent, None);
        assert_eq!(
            CpuUsage::default().usage_sample_state,
            CpuSampleState::Unavailable
        );
    }

    #[test]
    fn fixed_path_fixtures_measure_process_and_v2_without_fabrication() {
        let dir = tempfile::tempdir().unwrap();
        let fixtures = BTreeMap::from([
            (
                format!("/proc/{}/status", std::process::id()),
                "VmRSS:\t123 kB\nThreads:\t5\nCpus_allowed_list:\t0-7\n",
            ),
            (format!("/proc/{}/cgroup", std::process::id()), "0::/\n"),
            ("/sys/fs/cgroup/cpu.max".into(), "400000 100000\n"),
            (
                "/sys/fs/cgroup/cpu.stat".into(),
                "usage_usec 5000000\nuser_usec 4\n",
            ),
            ("/sys/fs/cgroup/memory.current".into(), "123456\n"),
            ("/sys/fs/cgroup/memory.max".into(), "536870912\n"),
        ]);
        let paths: BTreeMap<_, _> = fixtures
            .into_iter()
            .enumerate()
            .map(|(index, (name, content))| {
                let path = dir.path().join(index.to_string());
                std::fs::write(&path, content).unwrap();
                (name, path)
            })
            .collect();
        let raw = read_linux(&|name| paths.get(name).and_then(|path| read_bounded(path)));
        assert_eq!(raw.cpu.quota_cores, Some(4.0));
        assert_eq!(raw.cpu.effective_cores, Some(4.0));
        assert_eq!(raw.cpu_usage_nanoseconds, Some(5_000_000_000));
        assert_eq!(raw.memory.process_rss_bytes, Some(123 * 1024));
        assert_eq!(raw.memory.cgroup_current_bytes, Some(123456));
        assert_eq!(raw.memory.cgroup_limit_bytes, Some(536870912));
        assert_eq!(raw.threads, Some(5));
        let unknown = read_linux(&|_| None);
        assert_eq!(unknown.cpu.effective_cores, None);
        assert_eq!(unknown.memory.process_rss_bytes, None);
        assert_eq!(unknown.threads, None);
    }

    #[test]
    fn v1_fallback_and_missing_memory_limits_are_optional() {
        let values = BTreeMap::from([
            ("/sys/fs/cgroup/cpu/cpu.cfs_quota_us", "50000"),
            ("/sys/fs/cgroup/cpu/cpu.cfs_period_us", "100000"),
            ("/sys/fs/cgroup/cpuacct/cpuacct.usage", "123456"),
            ("/sys/fs/cgroup/memory/memory.usage_in_bytes", "4096"),
            (
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                "9223372036854771712",
            ),
        ]);
        let membership = format!("/proc/{}/cgroup", std::process::id());
        let sample = read_linux(&|name| {
            if name == membership {
                Some("1:cpu,cpuacct:/\n2:memory:/\n".into())
            } else {
                values.get(name).map(|value| (*value).into())
            }
        });
        assert_eq!(sample.cpu.source, Some("cgroup_v1"));
        assert_eq!(sample.cpu.effective_cores, Some(0.5));
        assert_eq!(sample.cpu_usage_nanoseconds, Some(123456));
        assert_eq!(sample.memory.cgroup_limit_bytes, None);
        assert_eq!(sample.memory.cgroup_limit_unlimited, Some(true));
    }

    #[test]
    fn non_root_namespace_never_reads_host_cgroup_counters() {
        let membership = format!("/proc/{}/cgroup", std::process::id());
        let sample = read_linux(&|name| {
            assert!(!name.starts_with("/sys/"));
            (name == membership).then(|| "0::/system.slice/service.scope\n".into())
        });
        assert_eq!(sample.cpu.source, None);
        assert_eq!(sample.memory.cgroup_current_bytes, None);
    }

    #[test]
    fn kernel_reads_reject_oversized_and_invalid_utf8_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("value");
        std::fs::write(&path, vec![b'a'; FILE_LIMIT + 1]).unwrap();
        assert!(read_bounded(&path).is_none());
        std::fs::write(&path, [0xff]).unwrap();
        assert!(read_bounded(&path).is_none());
        assert!(read_bounded(dir.path()).is_none());
    }

    #[test]
    fn repeated_requests_reuse_a_sample() {
        let mut sampler = Sampler::default();
        let initial = sampler.snapshot();
        let sampled = sampler.cached.as_ref().unwrap().0;
        let current = sampler.snapshot();
        assert_eq!(sampler.cached.as_ref().unwrap().0, sampled);
        assert!(current.sample_age_seconds >= initial.sample_age_seconds);
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(current.cpu.source, None);
            assert_eq!(current.cpu.usage_percent, None);
            assert_eq!(current.memory.cgroup_current_bytes, None);
        }
    }
}
