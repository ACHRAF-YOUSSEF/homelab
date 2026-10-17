use crate::task_schedule::CalendarSchedule;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::PathBuf;

const MAX_STATE_BYTES: usize = 64 * 1024;
const MAX_INTERVAL: u64 = 2_592_000;
const DEFAULTS_VERSION: u64 = 1;
const TASKS: [(&str, &str, &str, bool, u64, u64); 6] = [
    (
        "discovery",
        "Library discovery",
        "Find media files and subtitle tracks.",
        true,
        21_600,
        21_600,
    ),
    (
        "ocr",
        "Subtitle OCR",
        "Recognize image based subtitles for new or changed files.",
        true,
        86_400,
        21_600,
    ),
    (
        "inventory",
        "Media inventory",
        "Refresh the media inventory.",
        true,
        86_400,
        21_600,
    ),
    (
        "health",
        "Health check",
        "Check local application state and mounted paths.",
        true,
        21_600,
        3_600,
    ),
    (
        "cleanup",
        "Cache cleanup",
        "Remove stale entries from the metadata cache.",
        true,
        86_400,
        86_400,
    ),
    (
        "backup",
        "State backup",
        "Create a local application state backup.",
        true,
        604_800,
        86_400,
    ),
];

#[derive(Clone)]
struct Task {
    id: &'static str,
    enabled: bool,
    interval: u64,
    next_run: f64,
    last_run: Option<f64>,
    last_result: String,
    status: String,
    schedule: Option<CalendarSchedule>,
}

pub struct Scheduler {
    path: PathBuf,
    tasks: Vec<Task>,
    paused: bool,
    auto_scan: bool,
    ocr_available: bool,
}

impl Scheduler {
    pub fn new(path: PathBuf, now: f64, auto_scan: bool) -> Result<Self, String> {
        if !now.is_finite() || now < 0.0 {
            return Err("Invalid scheduler time".into());
        }
        let ocr_available = std::env::var("CONVERTER_ENABLE_OCR")
            .is_ok_and(|value| value == "true" || value == "1");
        let mut tasks: Vec<Task> = TASKS
            .iter()
            .map(|(id, _, _, enabled, interval, _)| Task {
                id,
                enabled: if *id == "ocr" {
                    ocr_available
                } else {
                    *enabled
                },
                interval: *interval,
                next_run: now + *interval as f64,
                last_run: None,
                last_result: String::new(),
                status: "idle".into(),
                schedule: None,
            })
            .collect();
        let mut paused = false;
        if path.exists() {
            let saved = crate::util::read_json(&path, MAX_STATE_BYTES)?;
            let migrate_inventory_default =
                saved["defaults_version"].as_u64().unwrap_or(0) < DEFAULTS_VERSION;
            if let Some(values) = saved.get("tasks").and_then(Value::as_array) {
                for value in values {
                    let Some(id) = value.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
                        continue;
                    };
                    let (_, _, _, _, _, min_interval) =
                        TASKS.iter().find(|spec| spec.0 == id).expect("fixed task");
                    if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                        // Older releases disabled inventory by default, without
                        // recording whether that was an explicit preference.
                        // Upgrade once; subsequent manual opt-outs persist.
                        task.enabled = (id == "inventory" && migrate_inventory_default) || enabled;
                    }
                    task.schedule = value
                        .get("schedule")
                        .filter(|value| !value.is_null())
                        .map(|value| {
                            serde_json::from_value::<CalendarSchedule>(value.clone())
                                .map_err(|_| "Invalid saved calendar schedule")
                        })
                        .transpose()?;
                    if let Some(schedule) = &task.schedule {
                        schedule.validate()?;
                    }
                    if let Some(interval) = value
                        .get("interval_seconds")
                        .and_then(Value::as_u64)
                        .filter(|v| *v >= *min_interval && *v <= MAX_INTERVAL)
                    {
                        task.interval = interval;
                    }
                    task.next_run = match value
                        .get("next_run")
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite() && *v >= 0.0)
                    {
                        Some(next) => next,
                        None => match &task.schedule {
                            Some(schedule) => schedule.next_after(now)?,
                            None => now + task.interval as f64,
                        },
                    };
                    task.last_run = value
                        .get("last_run")
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite() && *v >= 0.0);
                    task.last_result = value
                        .get("last_result")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .take(240)
                        .collect();
                    task.status = value
                        .get("status")
                        .and_then(Value::as_str)
                        .filter(|s| {
                            matches!(
                                *s,
                                "idle"
                                    | "completed"
                                    | "completed_with_errors"
                                    | "failed"
                                    | "cancelled"
                            )
                        })
                        .unwrap_or("idle")
                        .to_owned();
                }
            }
            paused = saved
                .get("paused")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        }
        let scheduler = Self {
            path,
            tasks,
            paused,
            auto_scan,
            ocr_available,
        };
        scheduler.persist(&scheduler.tasks, scheduler.paused)?;
        Ok(scheduler)
    }

    fn state_value(tasks: &[Task], paused: bool) -> Value {
        json!({"defaults_version":DEFAULTS_VERSION,"paused": paused, "tasks": tasks.iter().map(|t| json!({"id":t.id,"enabled":t.enabled,"interval_seconds":t.interval,"schedule":t.schedule,"next_run":t.next_run,"last_run":t.last_run,"last_result":t.last_result,"status":t.status})).collect::<Vec<_>>()})
    }

    fn persist(&self, tasks: &[Task], paused: bool) -> Result<(), String> {
        crate::util::write_private(&self.path, &Self::state_value(tasks, paused))
    }

    pub fn public(&self) -> Value {
        json!({"version": env!("CARGO_PKG_VERSION"), "edition": crate::edition::NAME, "paused": self.paused, "tasks": self.tasks.iter().map(|t| {
            let spec = TASKS.iter().find(|s| s.0 == t.id).expect("fixed task");
            let available = t.id != "ocr" || self.ocr_available;
            let mut item = json!({"id":t.id,"name":spec.1,"description":spec.2,"enabled":t.enabled,"interval_seconds":t.interval,"schedule":t.schedule,"schedule_timezone":"fixed_utc_offset","min_interval_seconds":spec.5,"next_run":t.next_run,"last_run":t.last_run,"last_result":t.last_result,"status":t.status,"available":available});
            if !available { item["blocked_reason"] = json!("OCR is disabled by deployment permissions"); }
            item
        }).collect::<Vec<_>>()})
    }

    pub fn update(&mut self, body: &Value, now: f64) -> Result<Value, String> {
        if !now.is_finite() || now < 0.0 {
            return Err("Invalid scheduler time".into());
        }
        let object = body.as_object().ok_or("Expected an object")?;
        if object.keys().any(|k| k != "paused" && k != "tasks") {
            return Err("Unknown scheduler setting".into());
        }
        let mut tasks = self.tasks.clone();
        let mut paused = self.paused;
        if let Some(v) = object.get("paused") {
            paused = v.as_bool().ok_or("paused must be a boolean")?;
        }
        if let Some(patches) = object.get("tasks") {
            let patches = patches.as_array().ok_or("tasks must be an array")?;
            if patches.len() > TASKS.len() {
                return Err("Too many task updates".into());
            }
            let mut seen = HashSet::new();
            for patch in patches {
                let patch = patch.as_object().ok_or("Task update must be an object")?;
                if patch.keys().any(|k| {
                    !["id", "enabled", "interval_seconds", "schedule"].contains(&k.as_str())
                }) {
                    return Err("Unknown task setting".into());
                }
                let id = patch
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Task id is required")?;
                if !seen.insert(id) {
                    return Err("Duplicate task id".into());
                }
                let index = tasks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or("Unknown task id")?;
                let was_enabled = tasks[index].enabled;
                let previous_interval = tasks[index].interval;
                let previous_schedule = tasks[index].schedule.clone();
                if let Some(value) = patch.get("schedule") {
                    tasks[index].schedule = if value.is_null() {
                        None
                    } else {
                        let schedule: CalendarSchedule = serde_json::from_value(value.clone())
                            .map_err(|_| "Invalid calendar schedule")?;
                        schedule.validate()?;
                        Some(schedule)
                    };
                }
                if let Some(v) = patch.get("enabled") {
                    tasks[index].enabled = v.as_bool().ok_or("enabled must be a boolean")?;
                }
                if let Some(v) = patch.get("interval_seconds") {
                    let interval = v.as_u64().ok_or("interval_seconds must be an integer")?;
                    let min = TASKS[index].5;
                    if interval < min || interval > MAX_INTERVAL {
                        return Err("Task interval is out of range".into());
                    }
                    tasks[index].interval = interval;
                }
                if tasks[index].interval != previous_interval
                    || tasks[index].schedule != previous_schedule
                    || (!was_enabled && tasks[index].enabled)
                {
                    tasks[index].next_run = match &tasks[index].schedule {
                        Some(schedule) => schedule.next_after(
                            tasks[index]
                                .last_run
                                .map(|last| last + TASKS[index].5 as f64 - 1.0)
                                .unwrap_or(now)
                                .max(now),
                        )?,
                        None => now + tasks[index].interval as f64,
                    };
                }
            }
        }
        self.persist(&tasks, paused)?;
        self.tasks = tasks;
        self.paused = paused;
        Ok(self.public())
    }

    pub fn due(&self, now: f64) -> Vec<String> {
        if self.paused || !self.auto_scan || !now.is_finite() {
            return Vec::new();
        }
        self.tasks
            .iter()
            .filter(|t| {
                t.enabled
                    && (t.id != "ocr" || self.ocr_available)
                    && t.status != "running"
                    && t.next_run <= now
            })
            .map(|t| t.id.to_owned())
            .collect()
    }

    pub fn start(&mut self, id: &str, now: f64) -> Result<(), String> {
        if !now.is_finite() || now < 0.0 {
            return Err("Invalid scheduler time".into());
        }
        let mut tasks = self.tasks.clone();
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or("Unknown task id")?;
        if task.status == "running" {
            return Err("Task is already running".into());
        }
        task.status = "running".into();
        task.last_run = Some(now);
        task.next_run = match &task.schedule {
            Some(schedule) => schedule.next_after(
                now + TASKS
                    .iter()
                    .find(|spec| spec.0 == id)
                    .expect("fixed task")
                    .5 as f64
                    - 1.0,
            )?,
            None => now + task.interval as f64,
        };
        self.persist(&tasks, self.paused)?;
        self.tasks = tasks;
        Ok(())
    }

    pub fn finish(
        &mut self,
        id: &str,
        _now: f64,
        status: &str,
        result: &str,
    ) -> Result<(), String> {
        if !matches!(
            status,
            "completed" | "completed_with_errors" | "failed" | "cancelled"
        ) {
            return Err("Invalid task status".into());
        }
        let result: String = result.chars().take(240).collect();
        let mut tasks = self.tasks.clone();
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or("Unknown task id")?;
        task.status = status.into();
        task.last_result = result;
        self.persist(&tasks, self.paused)?;
        self.tasks = tasks;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn scheduler() -> (tempfile::TempDir, Scheduler) {
        let dir = tempdir().unwrap();
        let scheduler = Scheduler::new(dir.path().join("tasks.json"), 100.0, true).unwrap();
        (dir, scheduler)
    }

    #[test]
    fn defaults_are_delayed_and_persisted() {
        let (dir, scheduler) = scheduler();
        assert!(scheduler.due(100.0).is_empty());
        let loaded = Scheduler::new(dir.path().join("tasks.json"), 101.0, true).unwrap();
        assert!(loaded.due(100.0 + 21_599.0).is_empty());
        assert_eq!(loaded.due(100.0 + 21_600.0), vec!["discovery", "health"]);
        assert_eq!(
            loaded.public()["tasks"][1]["enabled"].as_bool().unwrap(),
            loaded.ocr_available
        );
        assert_eq!(loaded.public()["tasks"][2]["enabled"], true);
    }

    #[test]
    fn legacy_inventory_defaults_upgrade_once_and_later_opt_out_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        crate::util::write_private(
            &path,
            &json!({"tasks":[{"id":"inventory","enabled":false,"interval_seconds":21600,"last_run":42.0}]}),
        ).unwrap();
        let mut migrated = Scheduler::new(path.clone(), 100.0, true).unwrap();
        assert_eq!(migrated.public()["tasks"][2]["enabled"], true);
        assert_eq!(migrated.public()["tasks"][2]["interval_seconds"], 21600);
        assert_eq!(migrated.public()["tasks"][2]["last_run"], 42.0);
        migrated
            .update(
                &json!({"tasks":[{"id":"inventory","enabled":false}]}),
                101.0,
            )
            .unwrap();
        let reloaded = Scheduler::new(path, 102.0, true).unwrap();
        assert_eq!(reloaded.public()["tasks"][2]["enabled"], false);
    }

    #[test]
    fn updates_validate_before_persisting_and_respect_minimums() {
        let (dir, mut scheduler) = scheduler();
        let original = std::fs::read(dir.path().join("tasks.json")).unwrap();
        assert!(
            scheduler
                .update(
                    &json!({"tasks":[{"id":"discovery","interval_seconds":2}]}),
                    101.0
                )
                .is_err()
        );
        assert!(
            scheduler
                .update(&json!({"tasks":[{"id":"bogus","enabled":true}]}), 101.0)
                .is_err()
        );
        assert_eq!(
            original,
            std::fs::read(dir.path().join("tasks.json")).unwrap()
        );
        scheduler
            .update(
                &json!({"tasks":[{"id":"discovery","interval_seconds":21600,"enabled":false}]}),
                101.0,
            )
            .unwrap();
        assert_eq!(scheduler.public()["tasks"][0]["interval_seconds"], 21600);
    }

    #[test]
    fn calendar_updates_persist_recompute_and_validate_transactionally() {
        let (dir, mut scheduler) = scheduler();
        scheduler.update(&json!({"tasks":[{"id":"inventory","schedule":{"time_of_day":"03:30","days_of_week":[4],"utc_offset_minutes":60}}]}),100.0).unwrap();
        assert_eq!(scheduler.public()["tasks"][2]["next_run"], 9000.0);
        let original = std::fs::read(dir.path().join("tasks.json")).unwrap();
        assert!(scheduler.update(&json!({"tasks":[{"id":"inventory","schedule":{"time_of_day":"24:00","days_of_week":[4],"utc_offset_minutes":60}}]}),100.0).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("tasks.json")).unwrap(),
            original
        );
        let loaded = Scheduler::new(dir.path().join("tasks.json"), 101.0, true).unwrap();
        assert_eq!(
            loaded.public()["tasks"][2]["schedule"]["time_of_day"],
            "03:30"
        );
        scheduler.start("inventory", 9000.0).unwrap();
        assert_eq!(scheduler.public()["tasks"][2]["next_run"], 613800.0);
        scheduler
            .update(
                &json!({"tasks":[{"id":"inventory","schedule":null}]}),
                10000.0,
            )
            .unwrap();
        assert!(scheduler.public()["tasks"][2]["schedule"].is_null());
    }

    #[test]
    fn pause_disables_scheduling_but_manual_start_works() {
        let (_dir, mut scheduler) = scheduler();
        scheduler.update(&json!({"paused":true}), 100.0).unwrap();
        assert!(scheduler.due(100_000.0).is_empty());
        scheduler.start("ocr", 101.0).unwrap();
        assert_eq!(scheduler.public()["tasks"][1]["status"], "running");
        assert!(scheduler.finish("ocr", 102.0, "completed", "done").is_ok());
    }

    #[test]
    fn interval_changes_and_reenabling_delay_runs_but_unchanged_updates_preserve_them() {
        let (_dir, mut scheduler) = scheduler();
        scheduler
            .update(
                &json!({"tasks":[{"id":"discovery","interval_seconds":86400}]}),
                200.0,
            )
            .unwrap();
        assert_eq!(scheduler.public()["tasks"][0]["next_run"], 86600.0);
        scheduler
            .update(
                &json!({"tasks":[{"id":"discovery","enabled":true,"interval_seconds":86400}]}),
                300.0,
            )
            .unwrap();
        assert_eq!(scheduler.public()["tasks"][0]["next_run"], 86600.0);
        scheduler
            .update(
                &json!({"tasks":[{"id":"discovery","enabled":false}]}),
                400.0,
            )
            .unwrap();
        scheduler
            .update(&json!({"tasks":[{"id":"discovery","enabled":true}]}), 500.0)
            .unwrap();
        assert_eq!(scheduler.public()["tasks"][0]["next_run"], 86900.0);
    }
}
