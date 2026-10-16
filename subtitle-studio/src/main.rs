use std::{path::PathBuf, sync::Arc};
use subtitle_studio::{App, config::Config, media, server, storage, subtitle};

fn run() -> Result<bool, String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut config = Config::from_env()?;
    match args.first().map(String::as_str).unwrap_or("serve") {
        "--healthcheck" => {
            server::healthcheck(&config)?;
            Ok(true)
        }
        "serve" => {
            server::serve(Arc::new(App::open(config)?))?;
            Ok(true)
        }
        "download-model" => {
            let name = args.get(1).unwrap_or(&config.whisper_model);
            if args.len() > 2 {
                return Err("Usage: subtitle-studio download-model [MODEL]".into());
            }
            std::fs::create_dir_all(&config.model_root)
                .map_err(|_| "Could not create model directory")?;
            media::download_model(&config, name)?;
            println!("Model ready: {name}");
            Ok(true)
        }
        "validate-srt" => {
            let mut target = None;
            let mut source = None;
            let mut video = None;
            let mut job_id = None;
            let mut language = "en".to_string();
            let mut max_cps = 20.0;
            let mut i = 1;
            while i < args.len() {
                let arg = &args[i];
                if matches!(
                    arg.as_str(),
                    "--source" | "--video" | "--job-id" | "--target-language" | "--max-cps"
                ) {
                    i += 1;
                    let value = args.get(i).ok_or("Missing option value")?;
                    match arg.as_str() {
                        "--source" => source = Some(PathBuf::from(value)),
                        "--video" => video = Some(PathBuf::from(value)),
                        "--job-id" => job_id = Some(value.clone()),
                        "--target-language" => language = value.clone(),
                        _ => {
                            max_cps = value
                                .parse::<f64>()
                                .ok()
                                .filter(|v| v.is_finite() && *v > 0.0)
                                .ok_or("Invalid reading speed")?
                        }
                    }
                } else if arg.starts_with('-') || target.is_some() {
                    return Err("Invalid validate-srt argument".into());
                } else {
                    target = Some(PathBuf::from(arg));
                }
                i += 1;
            }
            if target.is_some() == job_id.is_some() {
                return Err("Usage: subtitle-studio validate-srt FILE [--source FILE] [--video FILE] [--max-cps 20], or --job-id ID --target-language en".into());
            }
            if let Some(id) = job_id {
                config.prepare_read_only()?;
                let jobs =
                    storage::read_json(&config.data_root.join("jobs.json"), config.max_json_bytes)?;
                let job = jobs
                    .as_array()
                    .and_then(|a| a.iter().find(|j| j["id"] == id))
                    .ok_or("Job not found")?;
                target = Some(subtitle_studio::pipeline::output_path(
                    &config, job, &language,
                )?);
                if source.is_none()
                    && let Some(code) = job["detected_language"].as_str()
                {
                    source = subtitle_studio::pipeline::output_path(&config, job, code).ok();
                }
                if video.is_none() {
                    video = job["path"]
                        .as_str()
                        .and_then(|p| media::resolve_media(&config, p, true).ok());
                }
            }
            let read = |path: &std::path::Path| -> Result<Vec<subtitle::Cue>, String> {
                use std::io::Read;
                let file = std::fs::File::open(path).map_err(|_| "SRT file unavailable")?;
                let mut text = String::new();
                file.take(config.max_json_bytes + 1)
                    .read_to_string(&mut text)
                    .map_err(|_| "Could not read SRT")?;
                if text.len() as u64 > config.max_json_bytes {
                    return Err("SRT exceeds size limit".into());
                }
                subtitle::parse_srt(&text)
            };
            let cues = read(&target.unwrap())?;
            let original = source.as_deref().map(read).transpose()?;
            let duration = video
                .as_deref()
                .map(|path| {
                    media::probe(&config, path).map(|v| v["duration"].as_f64().unwrap_or(0.0))
                })
                .transpose()?;
            let report = subtitle::inspect(&cues, original.as_deref(), duration, max_cps);
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|_| "Could not print validation")?
            );
            Ok(report["structural_issues"].as_u64() == Some(0))
        }
        "--help" | "-h" => {
            println!(
                "subtitle-studio [serve | --healthcheck | download-model [MODEL] | validate-srt FILE [--source FILE] [--video FILE] [--max-cps 20]]"
            );
            Ok(true)
        }
        _ => Err("Unknown command; use --help".into()),
    }
}
fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("Subtitle Studio: {error}");
            std::process::exit(1);
        }
    }
}
