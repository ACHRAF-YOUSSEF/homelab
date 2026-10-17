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
    result["telemetry"] = telemetry(job, at);
    result["result_text"] = json!(result_text(job));
    result
}

pub(crate) fn result_text(job: &Value) -> Option<String> {
    let empty = Value::Null;
    let result = job["results"]
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or(&empty);
    let summary = &job["summary"];
    let concrete = match job["kind"].as_str() {
        Some("backup") => result["count_bytes"]
            .as_u64()
            .or_else(|| summary["backup_bytes"].as_u64())
            .map(|bytes| {
                format!(
                    "Backup created · {:.1} MB · {} files",
                    bytes as f64 / 1_048_576.0,
                    result["files"]
                        .as_array()
                        .map(Vec::len)
                        .map(|count| count as u64)
                        .or_else(|| summary["backup_files"].as_u64())
                        .unwrap_or(0)
                )
            }),
        Some("cleanup") => {
            let source = if result["state_entries_removed"].is_u64() {
                result
            } else {
                summary
            };
            source["state_entries_removed"]
                .as_u64()
                .zip(source["metadata_entries_removed"].as_u64())
                .map(|(state, metadata)| {
                    format!(
                        "Removed {} state entries and {} metadata entries",
                        state, metadata
                    )
                })
        }
        Some("health") => {
            let checked = result["configured_mounts"]
                .as_u64()
                .or_else(|| summary["mounts_checked"].as_u64());
            let source = if result["unavailable_mounts"].is_u64() {
                result
            } else {
                summary
            };
            checked
                .zip(source["unavailable_mounts"].as_u64())
                .zip(source["state_directory_available"].as_bool())
                .map(|((checked, unavailable), available)| {
                    format!(
                        "{} mounts checked · {} unavailable · state storage {} · no network checks",
                        checked,
                        unavailable,
                        if available {
                            "available"
                        } else {
                            "unavailable"
                        }
                    )
                })
        }
        Some("refresh") => result["message"].as_str().map(str::to_owned),
        _ => None,
    };
    concrete.or_else(|| {
        (matches!(job["kind"].as_str(), Some("backup" | "cleanup" | "health"))
            && job["status"] == "completed")
            .then(|| "Completed · details unavailable for this older run".into())
    })
}

// Live widget polling must not clone potentially large per-file result histories.
fn telemetry(job: &Value, at: f64) -> Value {
    let started = timestamp(&job["started"]);
    let created = timestamp(&job["created"]);
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
    json!({
        "started_at":started,"queue_wait_seconds":seconds_since(started.unwrap_or(at), created),
        "elapsed_seconds":elapsed,"progress_percent":percentage,"eta_seconds":eta,
        "heartbeat_at":heartbeat_at,"heartbeat_age_seconds":heartbeat_age,
        "last_progress_at":last_progress_at,"idle_seconds":idle,
        "liveness":liveness,"wait_reason":job["wait_reason"],
        "active_files":job["active_files"],"pending_files":job["pending_files"],
        "stall_after_seconds":stall_after,
        "heartbeat_timeout_seconds":heartbeat_timeout,
        "eta_basis":"completed_files"
    })
}

/// The separately authenticated widget must never expose raw job metadata or
/// diagnostics: persisted wait reasons can contain private paths or addresses.
pub(crate) fn widget(job: &Value, at: f64) -> Value {
    let telemetry = telemetry(job, at);
    let id = job["id"]
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
        .unwrap_or("legacy-current");
    let phases = [
        "preparing",
        "syncing_bazarr",
        "checking_bazarr_coverage",
        "waiting_for_worker",
        "discovering",
        "notifying_bazarr",
        "inspecting",
        "extracting",
        "ocr",
        "ocr_progress",
        "processing",
        "checking_gpu",
        "validating",
        "track_complete",
        "converted",
        "cached",
        "failed",
        "skipped",
        "pending",
        "dispatched",
        "refreshing",
        "remote_queued",
        "remote_retry",
        "inspected",
        "metadata_pending",
    ];
    let phase = job["current"]["status"]
        .as_str()
        .filter(|phase| phases.contains(phase))
        .or_else(|| job["phase"].as_str().filter(|phase| phases.contains(phase)))
        .unwrap_or("preparing");
    let wait_reason = telemetry["wait_reason"].as_str().map(|reason| {
        [
            "Waiting for Bazarr inventory sync",
            "Worker dispatch is paused; active files finish normally",
            "Worker allocation is paused",
            "Current cgroup memory pressure leaves insufficient headroom for another worker",
            "Memory reservations leave insufficient space for another worker",
            "Effective worker slots are occupied or limited by deployment and memory reservations",
            "OCR is disabled by deployment permissions",
            "No GPU workers requested",
            "No CPU OCR workers requested",
            "output family is active",
        ]
        .into_iter()
        .find(|message| reason.contains(message))
        .unwrap_or("Waiting for worker capacity")
    });
    json!({
        "id":id,"phase":phase,
        "kind":job["kind"],"status":job["status"],
        "processed":job["processed"].as_u64().unwrap_or(0),
        "total":job["total"].as_u64().unwrap_or(0),
        "progress_percent":telemetry["progress_percent"],
        "elapsed_seconds":telemetry["elapsed_seconds"],
        "eta_seconds":telemetry["eta_seconds"],
        "liveness":telemetry["liveness"],"wait_reason":wait_reason
    })
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
    fn widget_projects_timing_and_liveness_without_exposing_job_details() {
        let mut job = job();
        job["kind"] = json!("extract");
        job["paths"] = json!(["/private/movie.mkv"]);
        job["current"] = json!({"path":"/private/movie.mkv","api_key":"private-key"});
        heartbeat(&mut job, 1030.0, 1, 7, None);
        let snapshot = widget(&job, 1040.0);
        assert_eq!(snapshot["elapsed_seconds"], 40);
        assert_eq!(snapshot["eta_seconds"], 160);
        assert_eq!(snapshot["progress_percent"], 20);
        assert_eq!(snapshot["liveness"], "running");
        assert_eq!(snapshot.as_object().unwrap().len(), 11);
        assert!(!snapshot.to_string().contains("private"));
        assert_eq!(job["heartbeat_at"], 1030.0);

        for (at, expected) in [(1301.0, "stalled"), (1361.0, "unresponsive")] {
            heartbeat(&mut job, 1300.0, 1, 7, None);
            let snapshot = widget(&job, at);
            assert_eq!(snapshot["liveness"], expected);
            assert!(snapshot["eta_seconds"].is_null());
        }
    }

    #[test]
    fn widget_identity_and_phase_never_copy_paths_or_unrecognized_diagnostics() {
        let mut job = job();
        job["id"] = json!("/private/file.mkv");
        job["phase"] = json!("private diagnostics");
        job["current"] = json!({"status":"https://private.invalid","path":"/private/file.mkv"});
        let projected = widget(&job, 1040.0);
        assert_eq!(projected["id"], "legacy-current");
        assert_eq!(projected["phase"], "preparing");
        assert!(!projected.to_string().contains("private"));
        job["id"] = json!("ab12cd34");
        job["current"]["status"] = json!("ocr_progress");
        assert_eq!(widget(&job, 1040.0)["phase"], "ocr_progress");
        assert_eq!(widget(&job, 1040.0)["id"], "ab12cd34");
    }

    #[test]
    fn widget_wait_reasons_are_fixed_labels_and_unknown_timing_stays_null() {
        let mut job = job();
        for (reason, expected) in [
            (
                "Waiting for Bazarr inventory sync",
                "Waiting for Bazarr inventory sync",
            ),
            (
                "Dispatch waiting: Current cgroup memory pressure leaves insufficient headroom for another worker; pending files 1; GPU: /private/secret",
                "Current cgroup memory pressure leaves insufficient headroom for another worker",
            ),
            (
                "private-key at https://private.invalid/path",
                "Waiting for worker capacity",
            ),
        ] {
            heartbeat(&mut job, 1040.0, 0, 8, Some(reason));
            let snapshot = widget(&job, 1041.0);
            assert_eq!(snapshot["liveness"], "waiting");
            assert_eq!(snapshot["wait_reason"], expected);
            assert!(snapshot["eta_seconds"].is_null());
            assert!(!snapshot.to_string().contains("private"));
        }
        let queued = widget(&json!({"kind":"scan","status":"queued"}), 1040.0);
        assert_eq!(queued["liveness"], "queued");
        for key in [
            "elapsed_seconds",
            "eta_seconds",
            "progress_percent",
            "wait_reason",
        ] {
            assert!(queued[key].is_null(), "{key} must remain unknown");
        }
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
    #[test]
    fn queue_wait_is_distinct_from_run_time_and_maintenance_results_are_concrete() {
        let job = json!({"kind":"backup","created":100.0,"started":130.0,"finished":140.0,"status":"completed","results":[{"count_bytes":4_194_304,"files":["state.json","preferences.json"]}]});
        let snapshot = public(&job, 1000.0);
        assert_eq!(snapshot["telemetry"]["queue_wait_seconds"], 30);
        assert_eq!(snapshot["telemetry"]["elapsed_seconds"], 10);
        assert_eq!(snapshot["telemetry"]["started_at"], 130.0);
        assert_eq!(snapshot["result_text"], "Backup created · 4.0 MB · 2 files");
        assert_eq!(
            public(&json!({"status":"queued","created":100.0}), 145.0)["telemetry"]["queue_wait_seconds"],
            45
        );
        assert!(
            public(&json!({"status":"queued"}), 145.0)["telemetry"]["queue_wait_seconds"].is_null()
        );
    }

    #[test]
    fn legacy_maintenance_results_use_retained_facts_without_inventing_zero_counts() {
        let backup = json!({"kind":"backup","status":"completed","summary":{"backup_bytes":1048576,"backup_files":3}});
        assert_eq!(
            result_text(&backup).as_deref(),
            Some("Backup created · 1.0 MB · 3 files")
        );
        for kind in ["backup", "cleanup", "health"] {
            let legacy =
                json!({"kind":kind,"status":"completed","results":[{}],"summary":{"failed":0}});
            assert_eq!(
                result_text(&legacy).as_deref(),
                Some("Completed · details unavailable for this older run")
            );
            assert!(
                result_text(&json!({"kind":kind,"status":"failed","error":"Could not run"}))
                    .is_none()
            );
        }
    }
}
