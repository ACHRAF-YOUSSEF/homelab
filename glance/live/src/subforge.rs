//! A second projection boundary for the separately authenticated Subforge widget.
//! Deserialize only public fields; never forward provider diagnostics or paths.
use crate::Error;
use serde::{
    Deserialize,
    de::{self, SeqAccess, Visitor},
};
use serde_json::{Value, json};
use std::{collections::HashSet, fmt, marker::PhantomData};

const PHASES: &[&str] = &[
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
const WAIT_REASONS: &[&str] = &[
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
    "Waiting for worker capacity",
];

struct List<T, const MAX: usize>(Vec<T>);
impl<T, const MAX: usize> Default for List<T, MAX> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for List<T, MAX> {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListVisitor<T, const MAX: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for ListVisitor<T, MAX> {
            type Value = List<T, MAX>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded widget list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX));
                while let Some(item) = seq.next_element()? {
                    if items.len() >= MAX {
                        return Err(de::Error::custom("widget entry limit exceeded"));
                    }
                    items.push(item);
                }
                Ok(List(items))
            }
        }
        deserializer.deserialize_seq(ListVisitor::<T, MAX>(PhantomData))
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Job {
    id: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    phase: Option<String>,
    processed: u64,
    total: u64,
    progress_percent: Option<u64>,
    elapsed_seconds: Option<u64>,
    eta_seconds: Option<u64>,
    liveness: Option<String>,
    wait_reason: Option<String>,
}

#[derive(Deserialize)]
struct Widget {
    converted_count: u64,
    queued_count: u64,
    configured_root_count: u64,
    recent_failures_count: u64,
    selected_languages: List<String, 64>,
    #[serde(default)]
    active_jobs: Presence<List<Job, 32>>,
    #[serde(default)]
    current_run: Presence<Job>,
}

// Distinguish an explicitly idle legacy response from a broken provider schema.
#[derive(Default)]
enum Presence<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Presence<T> {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

fn label<'a>(raw: Option<&str>, allowed: &[&'a str], fallback: &'a str) -> &'a str {
    allowed
        .iter()
        .copied()
        .find(|value| Some(*value) == raw)
        .unwrap_or(fallback)
}

fn identifier(raw: Option<String>, fallback: String) -> String {
    raw.filter(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    })
    .unwrap_or(fallback)
}

pub(crate) fn normalize(body: &[u8]) -> Result<Value, Error> {
    let widget: Widget = serde_json::from_slice(body).map_err(|_| Error::InvalidData)?;
    let mut languages = Vec::with_capacity(widget.selected_languages.0.len());
    for language in widget.selected_languages.0 {
        if language.is_empty()
            || language.len() > 32
            || !language
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'*'))
        {
            return Err(Error::InvalidData);
        }
        languages.push(language);
    }
    // Missing active_jobs is the old API shape. An explicit empty array remains empty.
    let (raw_jobs, legacy) = match (widget.active_jobs, widget.current_run) {
        (Presence::Value(jobs), _) => (jobs.0, false),
        (Presence::Missing, Presence::Value(job)) => (vec![job], true),
        (Presence::Missing, Presence::Null) => (Vec::new(), true),
        _ => return Err(Error::InvalidData),
    };
    let mut jobs = Vec::with_capacity(raw_jobs.len());
    let mut ids = HashSet::with_capacity(raw_jobs.len());
    for (index, job) in raw_jobs.into_iter().enumerate() {
        let id = identifier(
            job.id,
            if legacy {
                "legacy-current".into()
            } else {
                format!("job-{index}")
            },
        );
        if !ids.insert(id.clone()) {
            return Err(Error::InvalidData);
        }
        let kind = label(
            job.kind.as_deref(),
            &[
                "scan", "inspect", "extract", "refresh", "ocr", "health", "cleanup", "backup",
            ],
            "unknown",
        );
        let status = label(
            job.status.as_deref(),
            &[
                "queued",
                "running",
                "cancelling",
                "cancelled",
                "completed",
                "completed_with_errors",
                "failed",
            ],
            "unknown",
        );
        let phase = label(job.phase.as_deref(), PHASES, "preparing");
        let liveness = label(
            job.liveness.as_deref(),
            &[
                "queued",
                "running",
                "waiting",
                "stalled",
                "unresponsive",
                "finished",
            ],
            "unknown",
        );
        let wait_reason = job
            .wait_reason
            .as_deref()
            .map(|raw| label(Some(raw), WAIT_REASONS, "Waiting for worker capacity"));
        jobs.push(json!({
            "id": id, "kind": kind, "status": status, "phase": phase,
            "processed":job.processed, "total":job.total,
            "progressPercent":job.progress_percent.map(|value| value.min(100)),
            "elapsedSeconds":job.elapsed_seconds, "etaSeconds":job.eta_seconds,
            "liveness":liveness,"waitReason":wait_reason,
        }));
    }
    Ok(json!({
        "convertedCount":widget.converted_count, "queuedCount":widget.queued_count,
        "configuredRootCount":widget.configured_root_count,
        "recentFailuresCount":widget.recent_failures_count,
        "selectedLanguages":languages, "activeJobCount":jobs.len(), "jobs":jobs,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Value {
        json!({"converted_count":5,"queued_count":2,"configured_root_count":1,"recent_failures_count":0,"selected_languages":["eng","pt-BR"],"active_jobs":[
            {"id":"0123456789abcdef","kind":"extract","status":"running","phase":"extracting","processed":1,"total":3,"progress_percent":33,"elapsed_seconds":10,"eta_seconds":20,"liveness":"running","wait_reason":null},
            {"id":"fedcba9876543210","kind":"ocr","status":"running","phase":"ocr_progress","processed":0,"total":1,"progress_percent":0,"elapsed_seconds":12,"eta_seconds":null,"liveness":"running","wait_reason":null}
        ],"active_job_count":99})
    }

    fn project(payload: &Value) -> Result<Value, Error> {
        normalize(&serde_json::to_vec(payload).unwrap())
    }

    #[test]
    fn multiple_jobs_preserve_nullable_timing_and_only_project_public_fields() {
        let mut raw = payload();
        raw["paths"] = json!(["/private/movie.mkv"]);
        raw["active_jobs"][0]["current"] =
            json!({"api_key":"private-key","error":"private-diagnostic"});
        raw["active_jobs"][1]["wait_reason"] =
            json!("private-diagnostic at https://private.invalid/path");
        raw["active_jobs"][1]["phase"] = json!("/private/path");
        let result = project(&raw).unwrap();
        assert_eq!(result["activeJobCount"], 2);
        assert_eq!(result["jobs"][0]["id"], "0123456789abcdef");
        assert_eq!(result["jobs"][0]["etaSeconds"], 20);
        assert!(result["jobs"][1]["etaSeconds"].is_null());
        assert_eq!(result["jobs"][1]["phase"], "preparing");
        assert_eq!(
            result["jobs"][1]["waitReason"],
            "Waiting for worker capacity"
        );
        assert!(!result.to_string().contains("private"));
    }

    #[test]
    fn legacy_current_run_is_supported_but_cannot_override_explicit_job_list() {
        let mut raw = payload();
        raw["current_run"] = raw["active_jobs"][0].clone();
        raw["current_run"].as_object_mut().unwrap().remove("id");
        raw.as_object_mut().unwrap().remove("active_jobs");
        let result = project(&raw).unwrap();
        assert_eq!(result["activeJobCount"], 1);
        assert_eq!(result["jobs"][0]["id"], "legacy-current");
        raw["active_jobs"] = json!([]);
        assert_eq!(project(&raw).unwrap()["activeJobCount"], 0);
        raw.as_object_mut().unwrap().remove("active_jobs");
        raw["current_run"] = Value::Null;
        assert_eq!(project(&raw).unwrap()["activeJobCount"], 0);
    }

    #[test]
    fn limits_and_types_fail_closed_without_forwarding_untrusted_metadata() {
        let raw = payload();
        for field in ["id", "kind", "status", "phase", "liveness", "wait_reason"] {
            let mut value = raw.clone();
            value["active_jobs"][0][field] = json!("/private/path");
            assert!(
                !project(&value).unwrap().to_string().contains("private"),
                "{field}"
            );
        }
        let mut value = raw.clone();
        value["active_jobs"] = json!(vec![raw["active_jobs"][0].clone(); 33]);
        assert!(project(&value).is_err());
        value = raw.clone();
        value["selected_languages"] = json!(vec!["eng"; 65]);
        assert!(project(&value).is_err());
        value = raw.clone();
        value["selected_languages"] = json!(["/private/path"]);
        assert!(project(&value).is_err());
        value = raw.clone();
        value["active_jobs"][0]["eta_seconds"] = json!(-1);
        assert!(project(&value).is_err());
        assert!(normalize(b"{}").is_err());
    }

    #[test]
    fn missing_or_null_job_list_is_not_a_healthy_idle_response() {
        let mut raw = payload();
        raw.as_object_mut().unwrap().remove("active_jobs");
        assert!(project(&raw).is_err());
        raw["active_jobs"] = Value::Null;
        assert!(project(&raw).is_err());
        raw["current_run"] = Value::Null;
        assert!(project(&raw).is_err());
        raw.as_object_mut().unwrap().remove("active_jobs");
        assert_eq!(project(&raw).unwrap()["activeJobCount"], 0);
    }
}
