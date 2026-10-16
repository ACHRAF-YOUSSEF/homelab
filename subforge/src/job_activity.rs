//! Bounded job activity metadata. A live coordinator and advancing work are
//! separate signals: neither a heartbeat nor a quiet subprocess proves progress.

use serde_json::{Value, json};

const STALL_AFTER_SECONDS: u64 = 300;
const HEARTBEAT_TIMEOUT_SECONDS: u64 = 60;

fn timestamp(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|time| time.is_finite() && *time >= 0.0)
}

fn seconds_since(at: f64, since: Option<f64>) -> Option<u64> {
    since.map(|since| (at - since).max(0.0) as u64)
}

pub(crate) fn started(job: &mut Value, at: f64) {
    job["heartbeat_at"] = json!(at);
    job["last_progress_at"] = json!(at);
    job["wait_reason"] = Value::Null;
    job["phase"] = json!("preparing");
    job["current"] = json!({"status":"preparing"});
}

pub(crate) fn progress(job: &mut Value, event: &Value, at: f64) {
    job["heartbeat_at"] = json!(at);
    // Repeated identical remote progress reports do not hide stalled work.
    if job["current"] != *event {
        job["last_progress_at"] = json!(at);
    }
    job["wait_reason"] = Value::Null;
}

pub(crate) fn work_started(job: &mut Value, at: f64) {
    // Bazarr may count already-covered files before dispatch. These cheap
    // skips must not be used to predict the duration of actual conversion.
    job["work_started_at"] = json!(at);
    job["initial_processed"] = job["processed"].clone();
}

pub(crate) fn heartbeat(
    job: &mut Value,
    at: f64,
    active: usize,
    pending: usize,
    wait_reason: Option<&str>,
) {
    job["heartbeat_at"] = json!(at);
    job["active_files"] = json!(active);
    job["pending_files"] = json!(pending);
    job["wait_reason"] = wait_reason
        .map(|reason| json!(reason))
        .unwrap_or(Value::Null);
}

/// Return a presentation snapshot without advancing the underlying heartbeat.
/// ETA uses completed-file throughput; per-track OCR reports are not falsely
/// presented as a percentage of a whole, possibly multi-track file.
pub(crate) fn public(job: &Value, at: f64) -> Value {
    let mut result = job.clone();
    let started = timestamp(&job["started"]);
    let finished = timestamp(&job["finished"]);
    let elapsed = seconds_since(finished.unwrap_or(at), started);
    let heartbeat_at = timestamp(&job["heartbeat_at"]).or(started);
    let last_progress_at = timestamp(&job["last_progress_at"]).or(started);
    let heartbeat_age = seconds_since(finished.unwrap_or(at), heartbeat_at);
    let idle = seconds_since(finished.unwrap_or(at), last_progress_at);
    let running = job["status"] == "running" || job["status"] == "cancelling";
    let waiting = job["wait_reason"].as_str().is_some();
    // These phases perform bounded I/O without intermediate callbacks. Allow
    // their documented deadline plus margin before flagging missing updates.
    let heartbeat_timeout = match job["phase"].as_str() {
        Some("syncing_bazarr") => 120,
        Some("notifying_bazarr") => 330,
        Some("discovering") => 300,
        _ => HEARTBEAT_TIMEOUT_SECONDS,
    };
    let stall_after = STALL_AFTER_SECONDS.max(heartbeat_timeout);
    let liveness = if job["status"] == "queued" {
        "queued"
    } else if !running {
        "finished"
    } else if heartbeat_age.is_some_and(|age| age >= heartbeat_timeout) {
        "unresponsive"
    } else if waiting {
        "waiting"
    } else if idle.is_some_and(|age| age >= stall_after) {
        "stalled"
    } else {
        "running"
    };
    let total = job["total"].as_u64().unwrap_or(0);
    let processed = job["processed"].as_u64().unwrap_or(0).min(total);
    let percentage = (total > 0).then(|| processed.saturating_mul(100) / total);
    let completed = processed.saturating_sub(job["initial_processed"].as_u64().unwrap_or(0));
    let work_elapsed = seconds_since(at, timestamp(&job["work_started_at"]).or(started));
    let eta = if liveness == "running" && completed > 0 && processed < total {
        work_elapsed
            .and_then(|elapsed| elapsed.checked_mul(total - processed))
            .map(|remaining| remaining / completed)
    } else {
        None
    };
    result["telemetry"] = json!({
        "elapsed_seconds":elapsed,"progress_percent":percentage,"eta_seconds":eta,
        "heartbeat_at":heartbeat_at,"heartbeat_age_seconds":heartbeat_age,
        "last_progress_at":last_progress_at,"idle_seconds":idle,
        "liveness":liveness,"wait_reason":job["wait_reason"],
        "active_files":job["active_files"],"pending_files":job["pending_files"],
        "stall_after_seconds":stall_after,
        "heartbeat_timeout_seconds":heartbeat_timeout,
        "eta_basis":"completed_files"
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Value {
        let mut job = json!({"status":"running","started":1000.0,"total":10,"processed":2});
        started(&mut job, 1000.0);
        job
    }

    #[test]
    fn progress_and_heartbeat_are_independent_and_eta_requires_advancing_work() {
        let mut job = job();
        heartbeat(&mut job, 1030.0, 1, 7, None);
        let result = public(&job, 1040.0);
        assert_eq!(result["telemetry"]["elapsed_seconds"], 40);
        assert_eq!(result["telemetry"]["eta_seconds"], 160);
        assert_eq!(result["telemetry"]["progress_percent"], 20);
        heartbeat(&mut job, 1300.0, 1, 7, None);
        let result = public(&job, 1301.0);
        assert_eq!(result["telemetry"]["liveness"], "stalled");
        assert!(result["telemetry"]["eta_seconds"].is_null());
        assert_eq!(
            public(&job, 1361.0)["telemetry"]["liveness"],
            "unresponsive"
        );
        let event = json!({"status":"ocr_progress","frames":5,"frames_total":10});
        progress(&mut job, &event, 1362.0);
        job["current"] = event.clone();
        progress(&mut job, &event, 1370.0);
        assert_eq!(job["last_progress_at"], 1362.0);
        assert_eq!(public(&job, 1371.0)["telemetry"]["liveness"], "running");
    }

    #[test]
    fn waiting_is_explicit_and_never_a_false_stall_or_eta() {
        let mut job = job();
        heartbeat(&mut job, 1500.0, 0, 8, Some("Worker allocation is paused"));
        let result = public(&job, 1501.0);
        assert_eq!(result["telemetry"]["liveness"], "waiting");
        assert_eq!(
            result["telemetry"]["wait_reason"],
            "Worker allocation is paused"
        );
        assert!(result["telemetry"]["eta_seconds"].is_null());
        assert_eq!(
            public(&job, 1561.0)["telemetry"]["liveness"],
            "unresponsive"
        );
    }

    #[test]
    fn initial_covered_skips_do_not_fabricate_conversion_throughput() {
        let mut job = job();
        job["total"] = json!(100);
        job["processed"] = json!(95);
        work_started(&mut job, 1020.0);
        heartbeat(&mut job, 1040.0, 1, 4, None);
        let result = public(&job, 1040.0);
        assert_eq!(result["telemetry"]["progress_percent"], 95);
        assert!(result["telemetry"]["eta_seconds"].is_null());
        job["processed"] = json!(96);
        assert_eq!(public(&job, 1040.0)["telemetry"]["eta_seconds"], 80);
    }

    #[test]
    fn bounded_inventory_notification_and_discovery_phases_have_deadline_grace() {
        for (phase, timeout) in [
            ("syncing_bazarr", 120),
            ("notifying_bazarr", 330),
            ("discovering", 300),
        ] {
            let mut job = job();
            job["phase"] = json!(phase);
            let before = public(&job, 1000.0 + timeout as f64 - 1.0);
            assert_eq!(before["telemetry"]["liveness"], "running");
            assert_eq!(before["telemetry"]["heartbeat_timeout_seconds"], timeout);
            assert_eq!(
                public(&job, 1000.0 + timeout as f64)["telemetry"]["liveness"],
                "unresponsive"
            );
        }
    }

    #[test]
    fn queued_legacy_and_finished_jobs_handle_unknown_totals_and_clock_changes() {
        let mut job = json!({"status":"queued","created":1000.0,"total":0,"processed":0});
        let result = public(&job, 1100.0);
        assert_eq!(result["telemetry"]["liveness"], "queued");
        assert!(result["telemetry"]["elapsed_seconds"].is_null());
        assert!(result["telemetry"]["progress_percent"].is_null());
        job = json!({"status":"completed","started":1000.0,"finished":1040.0,"processed":12,"total":10});
        let result = public(&job, 2000.0);
        assert_eq!(result["telemetry"]["liveness"], "finished");
        assert_eq!(result["telemetry"]["elapsed_seconds"], 40);
        assert_eq!(result["telemetry"]["progress_percent"], 100);
        assert!(result["telemetry"]["eta_seconds"].is_null());
        job["status"] = json!("running");
        job["finished"] = Value::Null;
        assert_eq!(public(&job, 900.0)["telemetry"]["elapsed_seconds"], 0);
        assert!(
            job["telemetry"].is_null(),
            "read snapshots must not mutate persisted state"
        );
    }
}
