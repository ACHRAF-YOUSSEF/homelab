//! Opt-in loopback probe for a disposable runtime with public sample.mkv media.
use serde_json::{Value, json};
use std::time::{Duration, Instant};
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let model = match arguments.as_slice() {
        [] => "tiny-q5_1",
        [option, model]
            if option == "--model" && subtitle_studio::media::supported_model(model) =>
        {
            model.as_str()
        }
        _ => return Err("Usage: subtitle-studio-probe [--model MODEL]".into()),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();
    let base = "http://127.0.0.1:8099";
    let get = |path: &str| -> Result<Value, Box<dyn std::error::Error>> {
        Ok(agent
            .get(format!("{base}{path}"))
            .call()?
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_json::<Value>()?)
    };
    let initial = get("/api/resources")?;
    let media = get("/api/media?path=sample.mkv")?;
    let track = media["tracks"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or("Fixture has no audio")?["index"]
        .as_u64()
        .ok_or("Fixture has no stream index")?;
    let job=agent.post(format!("{base}/api/jobs")).send_json(json!({"path":"sample.mkv","audio_stream_index":track,"source_language":"en","transcript":true,"targets":[],"review_enabled":false,"whisper_model":model}))?.body_mut().with_config().limit(65536).read_json::<Value>()?;
    let id = job["id"].as_str().ok_or("Missing fixture job ID")?;
    let started = Instant::now();
    let mut observed = 0;
    let gpu_memory = |usage: &Value| {
        usage["gpu"].as_array().and_then(|devices| {
            devices
                .iter()
                .filter_map(|device| device["memory_used_mib"].as_u64())
                .max()
        })
    };
    let mut observed_gpu = gpu_memory(&initial);
    let final_job = loop {
        let current = get(&format!("/api/jobs/{id}"))?;
        let usage = get("/api/resources")?;
        observed = observed.max(usage["container_memory_bytes"].as_u64().unwrap_or(0));
        observed_gpu = observed_gpu.max(gpu_memory(&usage));
        match current["status"].as_str() {
            Some("completed") => break current,
            Some("failed" | "cancelled") => {
                return Err(format!("Fixture transcription failed: {}", current["error"]).into());
            }
            _ => {}
        }
        if started.elapsed() > Duration::from_secs(600) {
            return Err("Fixture transcription timed out".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let subtitle = agent
        .get(format!("{base}/api/jobs/{id}/files/en"))
        .call()?
        .body_mut()
        .with_config()
        .limit(65536)
        .read_to_string()?;
    if !subtitle.contains("-->") || !subtitle.to_lowercase().contains("country") {
        return Err("Fixture transcription did not contain the expected speech".into());
    }
    let second=agent.post(format!("{base}/api/jobs")).send_json(json!({"path":"sample.mkv","audio_stream_index":track,"source_language":"en","transcript":true,"targets":[],"review_enabled":false,"whisper_model":model}))?.body_mut().with_config().limit(65536).read_json::<Value>()?;
    let second_id = second["id"].as_str().ok_or("Missing cached job")?;
    let cache_started = Instant::now();
    loop {
        let current = get(&format!("/api/jobs/{second_id}"))?;
        if current["status"] == "completed" {
            if !current["reused"]
                .as_array()
                .is_some_and(|a| a.contains(&json!("transcription")))
            {
                return Err("Second fixture did not reuse transcript".into());
            }
            break;
        }
        if current["status"] == "failed" || cache_started.elapsed() > Duration::from_secs(10) {
            return Err("Cached fixture failed".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let final_usage = get("/api/resources")?;
    println!(
        "{}",
        json!({"result":"passed","whisper_model":model,"whisper_device":initial["whisper_device"],"source_cues":subtitle.matches("-->").count(),"transcription_seconds":started.elapsed().as_secs_f64(),"server_idle_rss_bytes":initial["server_rss_bytes"],"server_after_rss_bytes":final_usage["server_rss_bytes"],"observed_container_memory_bytes":observed,"container_memory_peak_bytes":final_usage["container_memory_peak_bytes"],"initial_gpu_memory_used_mib":gpu_memory(&initial),"observed_gpu_memory_used_mib":observed_gpu,"container_cpu_seconds":final_usage["container_cpu_seconds"],"cache_reused":true,"outputs":final_job["outputs"].as_object().map(|o|o.len())})
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Runtime probe failed: {error}");
        std::process::exit(1);
    }
}
