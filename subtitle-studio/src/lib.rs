pub mod config;
pub mod llm;
pub mod media;
pub mod pipeline;
pub mod process;
pub mod server;
pub mod storage;
pub mod subtitle;

use config::Config;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Work {
    pub id: String,
    pub kind: String,
    pub step: Option<String>,
    pub token: String,
}

pub(crate) struct State {
    jobs: Vec<Value>,
    queue: VecDeque<Work>,
    revision: u64,
}
pub struct App {
    pub config: Config,
    pub(crate) state: Mutex<State>,
    pub(crate) changed: Condvar,
    pub(crate) epoch: String,
}

pub fn active(status: &Value) -> bool {
    matches!(status.as_str(), Some("queued" | "running" | "cancelling"))
}
pub fn busy(job: &Value) -> bool {
    active(&job["status"])
        || active(&job["review_task"]["status"])
        || active(&job["step_task"]["status"])
}
fn text<'a>(value: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or(fallback)
}
fn allowed_language(code: &str) -> bool {
    config::languages().get(code).is_some()
}

pub fn initial_steps(job: &Value) -> Value {
    let mut steps = json!({"extraction":{"label":"Extract audio","status":"pending","progress":0,"error":null},"transcription":{"label":"Transcribe audio","status":"pending","progress":0,"error":null}});
    if let Some(targets) = job["targets"].as_array() {
        for target in targets.iter().filter_map(Value::as_str) {
            if target
                == text(
                    job,
                    "detected_language",
                    text(job, "source_language", "auto"),
                )
            {
                continue;
            }
            let language = config::languages()[target]
                .as_str()
                .unwrap_or(target)
                .to_string();
            steps[format!("translation:{target}")] = json!({"label":format!("Translate to {language}"),"status":"pending","progress":0,"error":null});
            if job["review_enabled"].as_bool().unwrap_or(true) {
                steps[format!("review:{target}")] = json!({"label":format!("Review {language} translation"),"status":"pending","progress":0,"error":null});
            }
        }
    }
    steps
}

impl App {
    pub fn open(mut config: Config) -> Result<Self, String> {
        config.prepare()?;
        let file = config.data_root.join("jobs.json");
        let mut jobs = if file.exists() {
            storage::read_json(&file, config.max_json_bytes)?
                .as_array()
                .ok_or("Saved jobs must be an array")?
                .clone()
        } else {
            Vec::new()
        };
        for job in &mut jobs {
            if !job.is_object() || !valid_id(text(job, "id", "")) {
                return Err("Saved job record is invalid; restore a valid jobs.json backup".into());
            }
            if !job["steps"].is_object() {
                job["steps"] = initial_steps(job);
                if job["outputs"].as_object().is_some_and(|o| !o.is_empty()) {
                    for name in ["extraction", "transcription"] {
                        job["steps"][name]["status"] = json!("completed");
                        job["steps"][name]["progress"] = json!(100);
                    }
                    if let Some(outputs) = job["outputs"].as_object() {
                        for code in outputs.keys().cloned().collect::<Vec<_>>() {
                            if job["steps"].get(format!("translation:{code}")).is_some() {
                                job["steps"][format!("translation:{code}")]["status"] =
                                    json!("completed");
                            }
                        }
                    }
                }
            }
            let interrupted = busy(job);
            if active(&job["status"]) {
                job["status"] = json!("failed");
                job["stage"] = json!("Interrupted by service restart");
                job["error"] = json!("Service restarted before this job completed");
            }
            for task in ["review_task", "step_task"] {
                if active(&job[task]["status"]) {
                    job[task]["status"] = json!("failed");
                    job[task]["error"] = json!("Service restarted before this task completed");
                }
            }
            if interrupted {
                if let Some(steps) = job["steps"].as_object_mut() {
                    for step in steps.values_mut() {
                        if active(&step["status"]) || step["status"] == "pending" {
                            step["status"] = json!("blocked");
                            step["error"] = json!("Interrupted by service restart");
                        }
                    }
                }
                if let Some(reports) = job["quality"].as_object_mut() {
                    for report in reports.values_mut() {
                        if report["status"] == "running" {
                            report["status"] = json!("unavailable");
                            if let Some(report) = report.as_object_mut() {
                                report.remove("score");
                                report.remove("fluency");
                            }
                        }
                    }
                }
            }
        }
        let app = Self {
            config,
            state: Mutex::new(State {
                jobs,
                queue: VecDeque::new(),
                revision: 0,
            }),
            changed: Condvar::new(),
            epoch: storage::token()[..12].to_string(),
        };
        app.persist(&mut app.state.lock().unwrap())?;
        Ok(app)
    }

    fn persist(&self, state: &mut State) -> Result<(), String> {
        let data = serde_json::to_vec(&state.jobs).map_err(|_| "Could not encode jobs")?;
        if data.len() as u64 > self.config.max_json_bytes {
            return Err("Job history reached the configured JSON size limit; archive saved jobs before adding work".into());
        }
        storage::write_bytes_for(
            &self.config.data_root.join("jobs.json"),
            &data,
            storage::StorageRoot::Data,
        )?;
        state.revision = state.revision.wrapping_add(1);
        self.changed.notify_all();
        Ok(())
    }

    pub fn job(&self, id: &str) -> Result<Value, String> {
        self.state
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|j| j["id"] == id)
            .cloned()
            .ok_or_else(|| "Job not found".into())
    }

    pub fn update(&self, id: &str, change: impl FnOnce(&mut Value)) -> Result<(), String> {
        self.mutate(id, None, |job| {
            change(job);
            Ok(())
        })
    }

    pub fn update_checked(
        &self,
        id: &str,
        step: &str,
        change: impl FnOnce(&mut Value) -> Result<(), String>,
    ) -> Result<(), String> {
        self.mutate(id, Some(step), change)
    }

    fn mutate(
        &self,
        id: &str,
        check: Option<&str>,
        change: impl FnOnce(&mut Value) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let index = state
            .jobs
            .iter()
            .position(|j| j["id"] == id)
            .ok_or("Job not found")?;
        let previous = state.jobs[index].clone();
        if check.is_some_and(|step| cancelled(&previous, step)) {
            return Err("Cancelled by user".into());
        }
        if let Err(error) = change(&mut state.jobs[index]) {
            state.jobs[index] = previous;
            return Err(error);
        }
        state.jobs[index]["updated_at"] = json!(storage::now());
        if let Err(error) = self.persist(&mut state) {
            state.jobs[index] = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn check_cancel(&self, id: &str, step: &str) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        let job = state
            .jobs
            .iter()
            .find(|j| j["id"] == id)
            .ok_or("Job not found")?;
        if cancelled(job, step) {
            Err("Cancelled by user".into())
        } else {
            Ok(())
        }
    }

    pub fn step(
        &self,
        id: &str,
        step: &str,
        status: &str,
        progress: u64,
        error: Option<&str>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let index = state
            .jobs
            .iter()
            .position(|j| j["id"] == id)
            .ok_or("Job not found")?;
        let job = &mut state.jobs[index];
        if matches!(status, "running" | "completed") && cancelled(job, step) {
            return Err("Cancelled by user".into());
        }
        let previous = job.clone();
        if !job["steps"].is_object() {
            job["steps"] = initial_steps(job);
        }
        if !job["steps"][step].is_object() {
            job["steps"][step] = json!({"label":step});
        }
        job["steps"][step]["status"] = json!(status);
        job["steps"][step]["progress"] = json!(progress.min(100));
        job["steps"][step]["error"] = json!(error);
        job["updated_at"] = json!(storage::now());
        if let Err(error) = self.persist(&mut state) {
            state.jobs[index] = previous;
            return Err(error);
        }
        Ok(())
    }

    fn enqueue(
        &self,
        id: &str,
        kind: &str,
        step: Option<String>,
        change: impl FnOnce(&mut Value),
    ) -> Result<Value, String> {
        let mut state = self.state.lock().unwrap();
        if state.queue.len() >= self.config.max_queue {
            return Err("Processing queue is full".into());
        }
        let index = state
            .jobs
            .iter()
            .position(|j| j["id"] == id)
            .ok_or("Job not found")?;
        if busy(&state.jobs[index]) {
            return Err("Conflict: This job already has work queued or running".into());
        }
        let previous = state.jobs[index].clone();
        change(&mut state.jobs[index]);
        state.jobs[index]["updated_at"] = json!(storage::now());
        let token = if kind == "generation" {
            text(&state.jobs[index], "generation_token", "")
        } else {
            text(&state.jobs[index][format!("{kind}_task")], "token", "")
        }
        .to_string();
        if let Err(error) = self.persist(&mut state) {
            state.jobs[index] = previous;
            return Err(error);
        }
        state.queue.push_back(Work {
            id: id.into(),
            kind: kind.into(),
            step,
            token,
        });
        self.changed.notify_all();
        Ok(state.jobs[index].clone())
    }

    pub fn create_job(&self, payload: &Value) -> Result<Value, String> {
        if !payload.is_object() {
            return Err("Expected a JSON object".into());
        }
        let relative = payload["path"].as_str().ok_or("Select a video")?;
        let path = media::resolve_media(&self.config, relative, true)?;
        let source = text(payload, "source_language", "auto");
        if source != "auto" && !allowed_language(source) {
            return Err("Unsupported source language".into());
        }
        let mut targets = Vec::new();
        for target in payload
            .get("targets")
            .unwrap_or(&json!([]))
            .as_array()
            .ok_or("Invalid output languages")?
        {
            let code = target
                .as_str()
                .filter(|s| allowed_language(s))
                .ok_or("Invalid output languages")?;
            if !targets.contains(&code.to_string()) {
                targets.push(code.to_string());
            }
        }
        let transcript = optional_bool(payload, "transcript", false)?;
        if !transcript && targets.is_empty() {
            return Err("Select transcription or at least one translation".into());
        }
        let model = model_setting(payload, "model")?;
        let review_model = model_setting(payload, "review_model")?;
        if review_model.to_lowercase().contains("translategemma") {
            return Err("Choose a general instruction model for review".into());
        }
        let review_enabled = optional_bool(payload, "review_enabled", true)?;
        let translation_thinking = thinking(payload, "translation_thinking")?;
        let review_thinking = thinking(payload, "review_thinking")?;
        let whisper = text(payload, "whisper_model", &self.config.whisper_model);
        if !media::supported_model(whisper) {
            return Err("Unsupported Whisper model".into());
        }
        let metadata = media::probe(&self.config, &path)?;
        let track_index = payload["audio_stream_index"]
            .as_u64()
            .ok_or("Select an audio track")?;
        let track = metadata["tracks"]
            .as_array()
            .and_then(|a| a.iter().find(|t| t["index"] == track_index))
            .ok_or("Select an audio track from this file")?;
        let id = storage::token()[..12].to_string();
        let token = storage::token();
        let now = storage::now();
        let mut job = json!({"id":id,"path":relative,"filename":path.file_name().unwrap_or_default().to_string_lossy(),"audio_stream_index":track_index,"source_language":source,"transcript":transcript,"targets":targets,"whisper_model":whisper,"review_model":review_model,"review_enabled":review_enabled,"quality":{},"translation_thinking":translation_thinking,"review_thinking":review_thinking,"model":model,"status":"queued","stage":"Waiting","progress":0,"duration":metadata["duration"],"position":0,"audio_offset":track["offset"],"detected_language":null,"media_fingerprint":media::fingerprint(&path,relative)?,"reused":[],"outputs":{},"error":null,"created_at":now,"updated_at":now,"cancelled_steps":[],"cancel_requested":false,"generation_token":token});
        job["steps"] = initial_steps(&job);
        job["steps"]["extraction"]["status"] = json!("queued");
        let mut state = self.state.lock().unwrap();
        if state.jobs.len() >= self.config.max_jobs {
            return Err("Job history is full; archive saved jobs before adding work".into());
        }
        if state.queue.len() >= self.config.max_queue {
            return Err("Processing queue is full".into());
        }
        state.jobs.push(job.clone());
        if let Err(error) = self.persist(&mut state) {
            state.jobs.pop();
            return Err(error);
        }
        state.queue.push_back(Work {
            id,
            kind: "generation".into(),
            step: None,
            token,
        });
        self.changed.notify_all();
        Ok(job)
    }

    pub fn queue_review(&self, id: &str, payload: &Value) -> Result<Value, String> {
        let model = model_setting(payload, "review_model")?;
        if model.to_lowercase().contains("translategemma") {
            return Err("Choose a general instruction model for review".into());
        }
        let force = optional_bool(payload, "force", false)?;
        let thinking = thinking(payload, "review_thinking")?;
        let job = self.job(id)?;
        if busy(&job) {
            return Err("Conflict: This job already has work queued or running".into());
        }
        let available = pipeline::usable_review_languages(&self.config, &job);
        let mut selected: Vec<String> = Vec::new();
        if let Some(requested) = payload.get("languages").filter(|v| !v.is_null()) {
            for code in requested
                .as_array()
                .filter(|v| !v.is_empty())
                .ok_or("Select valid languages")?
            {
                let code = code.as_str().ok_or("Select valid languages")?;
                if !available.contains(&code.to_string()) {
                    return Err(
                        "Review source or translated subtitle is unavailable or misaligned".into(),
                    );
                }
                if !selected.contains(&code.to_string()) {
                    selected.push(code.into());
                }
            }
        } else {
            selected = available
                .into_iter()
                .filter(|code| force || job["quality"][code]["status"] != "completed")
                .collect();
        }
        if selected.is_empty() {
            return Err(
                "No saved translations need review, or the source transcript is unavailable".into(),
            );
        }
        let token = storage::token();
        let now = storage::now();
        self.enqueue(id,"review",None,|job| {
            job["review_task"] = json!({"status":"queued","stage":"Waiting for translation review","progress":0,"languages":selected,"model":model,"thinking":thinking,"force":force,"error":null,"token":token,"created_at":now,"updated_at":now});
            job["cancel_requested"] = json!(false);
            for code in &selected { let name = format!("review:{code}"); clear_cancel(job,&name); job["steps"][&name] = json!({"label":format!("Review {} translation",config::languages()[code].as_str().unwrap_or(code)),"status":"queued","progress":0,"error":null}); }
        })
    }

    pub fn retry_step(&self, id: &str, step: &str) -> Result<Value, String> {
        let job = self.job(id)?;
        if busy(&job) {
            return Err("Conflict: This job already has work queued or running".into());
        }
        let public = self.public_job(job.clone());
        if public["steps"][step]["can_retry"] != true {
            return Err("This step is not retryable or its source is unavailable".into());
        }
        if let Some(code) = step.strip_prefix("review:") {
            return self.queue_review(id,&json!({"languages":[code],"review_model":job["review_task"]["model"].as_str().unwrap_or(text(&job,"review_model","")),"review_thinking":job["review_task"].get("thinking").unwrap_or(&job["review_thinking"])}));
        }
        if matches!(step, "extraction" | "transcription") {
            media::resolve_media(&self.config, text(&job, "path", ""), true)?;
        }
        if step.starts_with("translation:") && !pipeline::source_available(&self.config, &job) {
            return Err("Source transcript is missing or invalid".into());
        }
        let token = storage::token();
        let now = storage::now();
        self.enqueue(id,"step",Some(step.into()),|job| {job["step_task"] = json!({"status":"queued","step":step,"stage":"Waiting to retry processing step","progress":0,"error":null,"token":token,"created_at":now,"updated_at":now});job["cancel_requested"] = json!(false);clear_cancel(job,step);job["steps"][step]["status"] = json!("queued");job["steps"][step]["error"] = Value::Null;})
    }

    pub fn cancel(&self, id: &str, selected: Option<&str>) -> Result<Value, String> {
        let job = self.job(id)?;
        let public = self.public_job(job.clone());
        if !busy(&job) || job["cancel_requested"] == true {
            return Err("Conflict: This job has no cancellable work".into());
        }
        if let Some(step) = selected
            && public["steps"][step]["can_cancel"] != true
        {
            return Err("Conflict: This step is not cancellable".into());
        }
        self.mutate(id, None, |job| {
            if !busy(job) || job["cancel_requested"] == true {
                return Err("Conflict: This job has no cancellable work".into());
            }
            if selected.is_some_and(|name| {
                !matches!(
                    job["steps"][name]["status"].as_str(),
                    Some("pending" | "queued" | "running")
                )
            }) {
                return Err("Conflict: This step is not cancellable".into());
            }
            // The queued/running work token must still match the snapshot used to select steps.
            for key in ["generation_token", "step_task", "review_task"] {
                let old = if key == "generation_token" {
                    &job[key]
                } else {
                    &job[key]["token"]
                };
                let expected = if key == "generation_token" {
                    &public[key]
                } else {
                    &public[key]["token"]
                };
                if old != expected {
                    return Err("Conflict: Job work changed; refresh and retry cancellation".into());
                }
            }
            let names = job["steps"]
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for name in names {
                if selected.is_none_or(|s| s == name)
                    && public["steps"][&name]["can_cancel"] == true
                    && matches!(
                        job["steps"][&name]["status"].as_str(),
                        Some("pending" | "queued" | "running")
                    )
                {
                    let status = if job["steps"][&name]["status"] == "running" {
                        "cancelling"
                    } else {
                        "cancelled"
                    };
                    job["steps"][&name]["status"] = json!(status);
                    job["steps"][&name]["error"] = json!("Cancelled by user");
                    if !job["cancelled_steps"].is_array() {
                        job["cancelled_steps"] = json!([]);
                    }
                    if !job["cancelled_steps"]
                        .as_array()
                        .unwrap()
                        .contains(&json!(name))
                    {
                        job["cancelled_steps"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!(name));
                    }
                }
            }
            if selected.is_none() {
                job["cancel_requested"] = json!(true);
            }
            let core =
                selected.is_none() || matches!(selected, Some("extraction" | "transcription"));
            if core && job["status"] == "queued" {
                job["status"] = json!("cancelled");
                job["stage"] = json!("Cancelled");
                block_pending(job);
            }
            for task in ["review_task", "step_task"] {
                let affected = selected.is_none()
                    || (task == "step_task" && job[task]["step"].as_str() == selected)
                    || (task == "review_task"
                        && job[task]["languages"].as_array().is_some_and(|a| {
                            a.iter().all(|c| {
                                job["cancelled_steps"].as_array().unwrap().contains(&json!(
                                    format!("review:{}", c.as_str().unwrap_or(""))
                                ))
                            })
                        }));
                if affected && active(&job[task]["status"]) {
                    job[task]["status"] = json!(if job[task]["status"] == "queued" {
                        "cancelled"
                    } else {
                        "cancelling"
                    });
                }
            }
            Ok(())
        })?;
        self.job(id)
    }

    pub fn public_job(&self, mut job: Value) -> Value {
        let is_busy = busy(&job);
        // Snapshot enrichment uses file existence only. Read/parse/align subtitle
        // contents when accepting work, never once per SSE client and revision.
        let has_source = self.source_hint(&job);
        let review_languages = if !active(&job["status"]) {
            job["outputs"]
                .as_object()
                .into_iter()
                .flat_map(|o| o.keys())
                .filter(|code| {
                    config::languages().get(code.as_str()).is_some()
                        && job["detected_language"].as_str() != Some(code.as_str())
                        && has_source
                        && pipeline::output_path(&self.config, &job, code).is_ok()
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        job["review_available"] = json!({"languages":review_languages,"reason":if has_source {""}else{"Source transcript is unavailable"},"rejected":{}});
        job["can_cancel"] = json!(is_busy && job["cancel_requested"] != true);
        let generation = active(&job["status"]);
        let step_task = job["step_task"].clone();
        let review_task = job["review_task"].clone();
        let cancel_requested = job["cancel_requested"] == true;
        let outputs = job["outputs"].clone();
        let extraction_done = job["steps"]["extraction"]["status"] == "completed";
        if let Some(steps) = job["steps"].as_object_mut() {
            for (name, step) in steps {
                let selected = generation
                    || (active(&step_task["status"]) && step_task["step"] == *name)
                    || (active(&review_task["status"])
                        && name.strip_prefix("review:").is_some_and(|code| {
                            review_task["languages"]
                                .as_array()
                                .is_some_and(|a| a.contains(&json!(code)))
                        }));
                step["can_cancel"] = json!(
                    selected
                        && !cancel_requested
                        && matches!(
                            step["status"].as_str(),
                            Some("pending" | "queued" | "running")
                        )
                );
                let source_ok = if let Some(code) = name.strip_prefix("review:") {
                    has_source && outputs.get(code).is_some()
                } else if name.starts_with("translation:") {
                    has_source
                } else if name == "transcription" {
                    extraction_done
                } else {
                    true
                };
                step["can_retry"] = json!(
                    !is_busy
                        && source_ok
                        && matches!(
                            step["status"].as_str(),
                            Some("failed" | "blocked" | "cancelled")
                        )
                );
            }
        }
        job
    }

    fn source_hint(&self, job: &Value) -> bool {
        pipeline::source_available_hint(&self.config, job)
    }

    pub fn snapshot(&self, view: Option<&View>) -> Value {
        let items = {
            let state = self.state.lock().unwrap();
            let mut items: Vec<&Value> = state
                .jobs
                .iter()
                .filter(|j| view.is_none_or(|v| matches_filter(j, &v.status)))
                .collect();
            items.sort_by(|a, b| text(b, "created_at", "").cmp(text(a, "created_at", "")));
            let total = items.len();
            let total_all = state.jobs.len();
            if let Some(v) = view {
                let count = total.div_ceil(v.page_size).max(1);
                let page = v.page.min(count);
                let selected = items
                    .into_iter()
                    .skip((page - 1) * v.page_size)
                    .take(v.page_size)
                    .cloned()
                    .collect::<Vec<_>>();
                (selected, Some((page, v.page_size, total, total_all, count)))
            } else {
                (items.into_iter().take(100).cloned().collect(), None)
            }
        };
        let jobs: Vec<Value> = items.0.into_iter().map(|j| self.public_job(j)).collect();
        if let Some((page, size, total, total_all, count)) = items.1 {
            json!({"jobs":jobs,"page":page,"page_size":size,"total":total,"total_all":total_all,"page_count":count})
        } else {
            json!(jobs)
        }
    }

    pub fn worker(&self) {
        loop {
            let work = {
                let mut state = self.state.lock().unwrap();
                while state.queue.is_empty() {
                    state = self.changed.wait(state).unwrap();
                }
                state.queue.pop_front().unwrap()
            };
            if !self.claim(&work).unwrap_or(false) {
                continue;
            }
            if let Err(error) = pipeline::run(self, &work) {
                let cancelled = error == "Cancelled by user";
                let _ = self.update(&work.id, |job| {
                    let task = if work.kind == "generation" {
                        &mut *job
                    } else {
                        &mut job[format!("{}_task", work.kind)]
                    };
                    task["status"] = json!(if cancelled { "cancelled" } else { "failed" });
                    task["stage"] = json!(if cancelled { "Cancelled" } else { "Failed" });
                    task["error"] = if cancelled { Value::Null } else { json!(error) };
                    if let Some(steps) = job["steps"].as_object_mut() {
                        for step in steps.values_mut() {
                            if active(&step["status"]) {
                                step["status"] =
                                    json!(if cancelled { "cancelled" } else { "failed" });
                                step["error"] = json!(error);
                            }
                        }
                    }
                    if work.kind == "generation" {
                        block_pending(job);
                    }
                    if let Some(reports) = job["quality"].as_object_mut() {
                        for report in reports.values_mut() {
                            if report["status"] == "running" {
                                report["status"] = json!("unavailable");
                                report["error"] = json!(error);
                                if let Some(obj) = report.as_object_mut() {
                                    obj.remove("score");
                                    obj.remove("fluency");
                                }
                            }
                        }
                    }
                });
                eprintln!(
                    "Subtitle work {} {}",
                    work.id,
                    if cancelled { "cancelled" } else { "failed" }
                );
            }
        }
    }

    fn claim(&self, work: &Work) -> Result<bool, String> {
        let mut state = self.state.lock().unwrap();
        let Some(index) = state.jobs.iter().position(|j| j["id"] == work.id) else {
            return Ok(false);
        };
        let previous = state.jobs[index].clone();
        let task = if work.kind == "generation" {
            &mut state.jobs[index]
        } else {
            &mut state.jobs[index][format!("{}_task", work.kind)]
        };
        let expected = if work.kind == "generation" {
            text(task, "generation_token", "")
        } else {
            text(task, "token", "")
        };
        if task["status"] != "queued" || expected != work.token {
            return Ok(false);
        }
        task["status"] = json!("running");
        if let Err(error) = self.persist(&mut state) {
            state.jobs[index] = previous;
            return Err(error);
        }
        Ok(true)
    }

    pub(crate) fn wait_revision(&self, last: u64) -> u64 {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(15), |s| s.revision == last)
            .unwrap();
        state.revision
    }
}

fn cancelled(job: &Value, step: &str) -> bool {
    job["cancel_requested"] == true
        || job["cancelled_steps"]
            .as_array()
            .is_some_and(|v| v.contains(&json!(step)))
}
fn clear_cancel(job: &mut Value, step: &str) {
    if let Some(items) = job["cancelled_steps"].as_array_mut() {
        items.retain(|v| v != step);
    }
}
fn block_pending(job: &mut Value) {
    if let Some(steps) = job["steps"].as_object_mut() {
        for step in steps.values_mut() {
            if step["status"] == "pending" {
                step["status"] = json!("blocked");
                step["error"] = json!("An earlier step did not complete");
            }
        }
    }
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 12
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn optional_bool(p: &Value, key: &str, default: bool) -> Result<bool, String> {
    p.get(key).map_or(Ok(default), |v| {
        v.as_bool()
            .ok_or_else(|| format!("{key} must be a boolean"))
    })
}
fn model_setting(p: &Value, key: &str) -> Result<String, String> {
    let value = p
        .get(key)
        .map_or(Ok(""), |v| v.as_str().ok_or("Invalid model"))?;
    if value.len() > 300 || value.chars().any(char::is_control) {
        return Err("Invalid model".into());
    }
    Ok(value.trim().into())
}
fn thinking(p: &Value, key: &str) -> Result<Value, String> {
    let v = p.get(key).unwrap_or(&Value::Null);
    if v.is_null() || v.is_boolean() {
        Ok(v.clone())
    } else {
        Err(format!("{key} must be a boolean or null"))
    }
}

#[derive(Clone, Debug)]
pub struct View {
    pub page: usize,
    pub page_size: usize,
    pub status: String,
}
impl View {
    pub fn parse(query: &[(String, String)]) -> Result<Option<Self>, String> {
        if !query
            .iter()
            .any(|(k, _)| matches!(k.as_str(), "page" | "page_size" | "status"))
        {
            return Ok(None);
        }
        let get = |name: &str, default: &str| -> Result<String, String> {
            let items: Vec<_> = query.iter().filter(|(k, _)| k == name).collect();
            if items.len() > 1 {
                return Err(format!("Duplicate {name}"));
            }
            Ok(items.first().map_or(default, |(_, v)| v).into())
        };
        let parse = |name: &str, default: &str, max: usize| -> Result<usize, String> {
            let s = get(name, default)?;
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("Invalid {name}"));
            }
            s.parse::<usize>()
                .ok()
                .filter(|v| *v > 0 && *v <= max)
                .ok_or_else(|| format!("Invalid {name}"))
        };
        let status = get("status", "all")?;
        if !matches!(
            status.as_str(),
            "all" | "completed" | "failed" | "running" | "queued" | "cancelled"
        ) {
            return Err("Unsupported job status filter".into());
        }
        Ok(Some(Self {
            page: parse("page", "1", usize::MAX)?,
            page_size: parse("page_size", "5", 50)?,
            status,
        }))
    }
}
fn matches_filter(job: &Value, status: &str) -> bool {
    let states = [
        &job["status"],
        &job["review_task"]["status"],
        &job["step_task"]["status"],
    ];
    match status {
        "all" => true,
        "running" => states
            .iter()
            .any(|s| matches!(s.as_str(), Some("running" | "cancelling"))),
        "queued" | "cancelled" => states.iter().any(|s| s.as_str() == Some(status)),
        _ => job["status"].as_str() == Some(status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn queued_fixture() -> (tempfile::TempDir, App, Work) {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::from_env().unwrap();
        config.library_root = root.path().join("library");
        config.output_root = root.path().join("output");
        config.data_root = root.path().join("data");
        config.model_root = root.path().join("models");
        let app = App::open(config).unwrap();
        std::fs::write(app.config.library_root.join("video.mkv"), b"fixture").unwrap();
        let mut job = json!({"id":"abcdef123456","path":"video.mkv","source_language":"en","transcript":true,"targets":[],"status":"queued","generation_token":"first","cancelled_steps":[],"cancel_requested":false,"outputs":{},"created_at":"2026-01-01T00:00:00+00:00"});
        job["steps"] = initial_steps(&job);
        job["steps"]["extraction"]["status"] = json!("queued");
        app.state.lock().unwrap().jobs.push(job);
        (
            root,
            app,
            Work {
                id: "abcdef123456".into(),
                kind: "generation".into(),
                step: None,
                token: "first".into(),
            },
        )
    }
    #[test]
    fn claimed_work_remains_busy_until_cancellation_is_observed() {
        let (_root, app, work) = queued_fixture();
        assert!(app.claim(&work).unwrap());
        app.cancel(&work.id, None).unwrap();
        assert!(
            app.retry_step(&work.id, "extraction")
                .unwrap_err()
                .starts_with("Conflict:")
        );
        assert_eq!(
            app.check_cancel(&work.id, "extraction"),
            Err("Cancelled by user".into())
        );
    }
    #[test]
    fn cancelled_queued_generation_cannot_restart_after_a_step_retry() {
        let (_root, app, old) = queued_fixture();
        app.cancel(&old.id, None).unwrap();
        app.retry_step(&old.id, "extraction").unwrap();
        assert!(!app.claim(&old).unwrap());
        let retry = app.state.lock().unwrap().queue.pop_front().unwrap();
        assert!(app.claim(&retry).unwrap());
        assert_eq!(app.job(&old.id).unwrap()["status"], "cancelled");
    }
    #[test]
    fn paging_is_bounded() {
        for query in [
            "page=0",
            "page_size=51",
            "page=1&page=2",
            "status=unknown",
            "page=-1",
        ] {
            let pairs = url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect::<Vec<_>>();
            assert!(View::parse(&pairs).is_err(), "{query}");
        }
        assert!(View::parse(&[]).unwrap().is_none());
    }
    #[test]
    fn filters_include_independent_tasks() {
        let job = json!({"status":"completed","review_task":{"status":"running"}});
        assert!(matches_filter(&job, "running"));
        assert!(matches_filter(&job, "completed"));
    }
}
