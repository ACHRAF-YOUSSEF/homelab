//! Manual history retention. Subtitle snapshots, shared exports and caches are
//! never removed. Active history is authoritative after a two-file interruption.
use crate::{App, State, busy, config::Config, storage, valid_id};
use serde_json::{Value, json};
use std::{collections::HashSet, fs};

pub const MAX_ARCHIVED_JOBS: usize = 10_000;
const FILE: &str = "archived-jobs.json";

pub(crate) fn load(config: &Config, active: &[Value]) -> Result<Vec<Value>, String> {
    let path = config.data_root.join(FILE);
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err("Archived history must be a regular file".into());
    }
    if !path.exists() {
        return Ok(Vec::new());
    }
    let value = storage::read_json(&path, config.max_json_bytes)?;
    let mut archived = value
        .as_array()
        .ok_or("Archived jobs must be an array")?
        .clone();
    if archived.len() > MAX_ARCHIVED_JOBS {
        return Err("Archived history exceeds the 10000 record limit".into());
    }
    let mut ids = HashSet::new();
    for job in &archived {
        let id = job["id"].as_str().unwrap_or("");
        if !job.is_object()
            || !valid_id(id)
            || !ids.insert(id)
            || !job["archived_at"].is_string()
            || busy(job)
        {
            return Err(
                "Archived job record is invalid; restore a valid archived-jobs.json backup".into(),
            );
        }
    }
    // Archive writes precede removal from jobs.json; restore writes precede
    // archive removal. Either interruption can leave a harmless duplicate.
    let active_ids: HashSet<&str> = active.iter().filter_map(|job| job["id"].as_str()).collect();
    archived.retain(|job| !active_ids.contains(job["id"].as_str().unwrap_or("")));
    Ok(archived)
}

impl App {
    fn persist_archived(&self, state: &State) -> Result<(), String> {
        if state.archived.len() > MAX_ARCHIVED_JOBS {
            return Err("Archive is full (10000 jobs); retain or back up archived-jobs.json before managing older archives offline".into());
        }
        let data =
            serde_json::to_vec(&state.archived).map_err(|_| "Could not encode archived jobs")?;
        if data.len() as u64 > self.config.max_json_bytes {
            return Err(
                "Archive reached the configured JSON size limit; no saved jobs were removed".into(),
            );
        }
        storage::write_bytes_for(
            &self.config.data_root.join(FILE),
            &data,
            storage::StorageRoot::Data,
        )
    }

    pub fn archive_jobs(&self, id: Option<&str>) -> Result<Value, String> {
        if id.is_some_and(|id| !valid_id(id)) {
            return Err("Invalid job id".into());
        }
        let mut state = self.state.lock().unwrap();
        if let Some(id) = id {
            let job = state
                .jobs
                .iter()
                .find(|job| job["id"] == id)
                .ok_or("Job not found")?;
            if busy(job) {
                return Err(
                    "Conflict: Wait for queued, running or cancelling work before archiving".into(),
                );
            }
        }
        // The same lock covers the final inactivity check and both writes; a
        // worker or HTTP request cannot queue work in between.
        let selected: Vec<usize> = state
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| !busy(job) && id.is_none_or(|id| job["id"] == id))
            .map(|(index, _)| index)
            .collect();
        if selected.is_empty() {
            return Ok(json!({"archived":0}));
        }
        if selected.len() > MAX_ARCHIVED_JOBS.saturating_sub(state.archived.len()) {
            return Err("Archive is full (10000 jobs); no saved jobs were removed".into());
        }
        let original_len = state.archived.len();
        let now = storage::now();
        for &index in &selected {
            let mut archived = state.jobs[index].clone();
            archived["archived_at"] = json!(now);
            state.archived.push(archived);
        }
        // Commit the complete archive before removing any active history.
        if let Err(error) = self.persist_archived(&state) {
            state.archived.truncate(original_len);
            return Err(error);
        }
        let mut removed = Vec::with_capacity(selected.len());
        for &index in selected.iter().rev() {
            removed.push((index, state.jobs.remove(index)));
        }
        if let Err(error) = self.persist(&mut state) {
            for (index, job) in removed.into_iter().rev() {
                state.jobs.insert(index, job);
            }
            state.archived.truncate(original_len);
            // If rollback fails, jobs.json still contains all original records;
            // load() ignores duplicate archive copies on the next restart.
            let _ = self.persist_archived(&state);
            return Err(error);
        }
        Ok(json!({"archived":selected.len()}))
    }

    pub fn restore_job(&self, id: &str) -> Result<Value, String> {
        let mut state = self.state.lock().unwrap();
        if state.jobs.len() >= self.config.max_jobs {
            return Err(
                "Conflict: Job history is full; archive inactive saved jobs before restoring"
                    .into(),
            );
        }
        let index = state
            .archived
            .iter()
            .position(|job| job["id"] == id)
            .ok_or("Job not found")?;
        let original = state.archived.remove(index);
        let mut restored = original.clone();
        restored
            .as_object_mut()
            .ok_or("Invalid archived job")?
            .remove("archived_at");
        state.jobs.push(restored.clone());
        if let Err(error) = self.persist(&mut state) {
            state.jobs.pop();
            state.archived.insert(index, original);
            return Err(error);
        }
        // Restore is committed in jobs.json. A failed archive cleanup cannot
        // undo it; startup gives this active record precedence over its copy.
        if self.persist_archived(&state).is_err() {
            eprintln!(
                "Restored job; archived history cleanup will be retried on the next archive change"
            );
        }
        drop(state);
        Ok(self.public_job(restored))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, App) {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::from_env().unwrap();
        config.library_root = root.path().join("library");
        config.output_root = root.path().join("output");
        config.data_root = root.path().join("data");
        config.model_root = root.path().join("models");
        config.max_jobs = 2;
        let app = App::open(config).unwrap();
        let mut state = app.state.lock().unwrap();
        state.jobs = vec![
            job("abcdef123456", "completed"),
            job("abcdef123457", "queued"),
        ];
        app.persist(&mut state).unwrap();
        drop(state);
        (root, app)
    }

    fn job(id: &str, status: &str) -> Value {
        json!({"id":id,"status":status,"source_language":"en","targets":[],"steps":{},"outputs":{},"created_at":"2026-01-01T00:00:00+00:00"})
    }

    #[test]
    fn archive_frees_capacity_preserves_files_and_restores_across_restarts() {
        let (_root, app) = fixture();
        let subtitle = app.config.output_root.join("keep.srt");
        let cache = app.config.data_root.join("keep-cache.json");
        fs::write(&subtitle, "saved subtitle").unwrap();
        fs::write(&cache, "saved cache").unwrap();
        assert_eq!(app.archive_jobs(None).unwrap()["archived"], 1);
        assert_eq!(app.state.lock().unwrap().jobs.len(), 1);
        assert_eq!(app.job("abcdef123456").unwrap()["status"], "completed");
        let view = crate::View {
            page: 1,
            page_size: 5,
            status: "archived".into(),
        };
        let snapshot = app.snapshot(Some(&view));
        assert_eq!(snapshot["total"], 1);
        assert_eq!(snapshot["jobs"][0]["can_restore"], true);
        assert_eq!(snapshot["retention"]["saved_limit"], 2);
        let reopened = App::open(app.config.clone()).unwrap();
        reopened.restore_job("abcdef123456").unwrap();
        assert_eq!(reopened.snapshot(Some(&view))["total"], 0);
        assert!(
            reopened
                .job("abcdef123456")
                .unwrap()
                .get("archived_at")
                .is_none()
        );
        let restarted = App::open(app.config.clone()).unwrap();
        assert_eq!(restarted.state.lock().unwrap().jobs.len(), 2);
        assert_eq!(fs::read_to_string(subtitle).unwrap(), "saved subtitle");
        assert_eq!(fs::read_to_string(cache).unwrap(), "saved cache");
    }

    #[test]
    fn active_generation_review_and_retry_cannot_be_archived() {
        let (_root, app) = fixture();
        assert!(
            app.archive_jobs(Some("abcdef123457"))
                .unwrap_err()
                .starts_with("Conflict:")
        );
        for task in ["review_task", "step_task"] {
            app.update("abcdef123456", |job| {
                job[task] = json!({"status":"running"})
            })
            .unwrap();
            assert!(
                app.archive_jobs(Some("abcdef123456"))
                    .unwrap_err()
                    .starts_with("Conflict:")
            );
            app.update("abcdef123456", |job| {
                job[task] = json!({"status":"completed"})
            })
            .unwrap();
        }
        assert_eq!(app.state.lock().unwrap().jobs.len(), 2);
    }

    #[test]
    fn archive_write_or_size_failure_keeps_active_history() {
        let (_root, mut app) = fixture();
        app.config.max_json_bytes = 1;
        assert!(
            app.archive_jobs(None)
                .unwrap_err()
                .contains("JSON size limit")
        );
        assert_eq!(app.state.lock().unwrap().jobs.len(), 2);
        assert_eq!(app.state.lock().unwrap().archived.len(), 0);
        app.config.max_json_bytes = 16384;
        fs::create_dir(app.config.data_root.join(FILE)).unwrap();
        assert!(app.archive_jobs(None).is_err());
        assert_eq!(app.state.lock().unwrap().jobs.len(), 2);
        assert_eq!(app.state.lock().unwrap().archived.len(), 0);
    }

    #[test]
    fn active_history_commit_failure_rolls_back_archive_and_memory() {
        let (_root, app) = fixture();
        let saved = app.config.data_root.join("jobs.json");
        fs::remove_file(&saved).unwrap();
        fs::create_dir(&saved).unwrap();
        assert!(app.archive_jobs(None).is_err());
        assert_eq!(app.state.lock().unwrap().jobs.len(), 2);
        assert_eq!(app.state.lock().unwrap().archived.len(), 0);
        assert_eq!(
            storage::read_json(&app.config.data_root.join(FILE), 16384).unwrap(),
            json!([])
        );
    }

    #[test]
    fn interrupted_archive_or_restore_prefers_committed_active_record() {
        let (_root, app) = fixture();
        let mut duplicate = app.job("abcdef123456").unwrap();
        duplicate["archived_at"] = json!(storage::now());
        storage::write_json(&app.config.data_root.join(FILE), &json!([duplicate])).unwrap();
        let reopened = App::open(app.config.clone()).unwrap();
        assert!(reopened.state.lock().unwrap().archived.is_empty());
        assert_eq!(reopened.state.lock().unwrap().jobs.len(), 2);
        assert_eq!(
            reopened.archive_jobs(Some("abcdef123456")).unwrap()["archived"],
            1
        );
        let restarted = App::open(app.config.clone()).unwrap();
        assert_eq!(restarted.state.lock().unwrap().archived.len(), 1);
        assert_eq!(restarted.state.lock().unwrap().jobs.len(), 1);
    }

    #[test]
    fn restore_capacity_or_commit_failure_preserves_archive() {
        let (_root, mut app) = fixture();
        app.archive_jobs(None).unwrap();
        app.config.max_jobs = 1;
        assert!(
            app.restore_job("abcdef123456")
                .unwrap_err()
                .contains("history is full")
        );
        app.config.max_jobs = 2;
        app.config.max_json_bytes = 1;
        assert!(app.restore_job("abcdef123456").is_err());
        assert_eq!(app.state.lock().unwrap().archived.len(), 1);
        assert_eq!(app.state.lock().unwrap().jobs.len(), 1);
    }

    #[test]
    fn archive_count_limit_preserves_saved_jobs() {
        let (_root, app) = fixture();
        app.state.lock().unwrap().archived =
            vec![job("abcdef123458", "completed"); MAX_ARCHIVED_JOBS];
        assert!(
            app.archive_jobs(None)
                .unwrap_err()
                .contains("Archive is full")
        );
        assert_eq!(app.state.lock().unwrap().jobs.len(), 2);
    }
}
