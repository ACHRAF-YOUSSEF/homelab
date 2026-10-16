use serde_json::{Value, json};
use std::{env, fs, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub library_root: PathBuf,
    pub output_root: PathBuf,
    pub data_root: PathBuf,
    pub model_root: PathBuf,
    pub bind_address: String,
    pub port: u16,
    pub whisper_model: String,
    pub whisper_device: String,
    pub whisper_cli: String,
    pub local_only: bool,
    pub cpu_threads: usize,
    pub audio_chunk_seconds: u64,
    pub process_timeout: u64,
    pub llm_base: String,
    pub llm_model: String,
    pub review_model: String,
    pub llm_timeout: u64,
    pub max_json_bytes: u64,
    pub max_jobs: usize,
    pub max_queue: usize,
    pub max_clients: usize,
    pub max_streams: usize,
    pub allowed_hosts: Vec<String>,
}

fn setting(name: &str, fallback: &str) -> String {
    env::var(name)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback.into())
}

fn number(name: &str, fallback: u64, min: u64, max: u64) -> Result<u64, String> {
    let value = setting(name, &fallback.to_string())
        .parse::<u64>()
        .map_err(|_| format!("{name} must be an integer"))?;
    if !(min..=max).contains(&value) {
        return Err(format!("{name} must be between {min} and {max}"));
    }
    Ok(value)
}

pub fn validate_url(value: &str) -> Result<url::Url, String> {
    if value
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err("Invalid service URL".into());
    }
    let url = url::Url::parse(value).map_err(|_| "Invalid service URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
        || url.port() == Some(0)
    {
        return Err("Service URL must use HTTP(S), without credentials, query or fragment".into());
    }
    Ok(url)
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let llm_base = setting("SUBTITLE_LLM_BASE_URL", "http://127.0.0.1:1234/v1")
            .trim_end_matches('/')
            .to_string();
        validate_url(&llm_base)?;
        let whisper_device = setting("WHISPER_DEVICE", "cpu");
        if !matches!(whisper_device.as_str(), "cpu" | "cuda" | "metal") {
            return Err("WHISPER_DEVICE must be cpu, cuda or metal".into());
        }
        let local_only = match setting("WHISPER_LOCAL_FILES_ONLY", "true").as_str() {
            "true" => true,
            "false" => false,
            _ => return Err("WHISPER_LOCAL_FILES_ONLY must be true or false".into()),
        };
        let max_clients = number("SUBTITLE_MAX_CLIENTS", 16, 2, 128)? as usize;
        let max_streams = number("SUBTITLE_MAX_STREAMS", 8, 1, 64)? as usize;
        if max_streams >= max_clients {
            return Err("SUBTITLE_MAX_STREAMS must be smaller than SUBTITLE_MAX_CLIENTS".into());
        }
        Ok(Self {
            library_root: PathBuf::from(setting("SUBTITLE_LIBRARY_ROOT", "./library")),
            output_root: PathBuf::from(setting("SUBTITLE_OUTPUT_ROOT", "./output")),
            data_root: PathBuf::from(setting("SUBTITLE_DATA_ROOT", "./data")),
            model_root: PathBuf::from(setting("WHISPER_MODEL_ROOT", "./models")),
            bind_address: setting("SUBTITLE_BIND_ADDRESS", "127.0.0.1"),
            port: number("SUBTITLE_PORT", 8099, 1, 65535)? as u16,
            whisper_model: setting("WHISPER_MODEL", "small-q5_1"),
            whisper_device,
            whisper_cli: setting("WHISPER_CLI", "whisper-cli"),
            local_only,
            cpu_threads: number("SUBTITLE_CPU_THREADS", 2, 1, 64)? as usize,
            audio_chunk_seconds: number("SUBTITLE_AUDIO_CHUNK_SECONDS", 300, 30, 900)?,
            process_timeout: number("SUBTITLE_PROCESS_TIMEOUT", 3600, 1, 86400)?,
            llm_base,
            llm_model: setting("SUBTITLE_LLM_MODEL", ""),
            review_model: setting("SUBTITLE_REVIEW_MODEL", ""),
            llm_timeout: number("SUBTITLE_LLM_TIMEOUT", 600, 1, 3600)?,
            max_json_bytes: number(
                "SUBTITLE_MAX_JSON_BYTES",
                16 * 1024 * 1024,
                16384,
                64 * 1024 * 1024,
            )?,
            max_jobs: number("SUBTITLE_MAX_JOBS", 500, 1, 10000)? as usize,
            max_queue: number("SUBTITLE_MAX_QUEUE", 32, 1, 256)? as usize,
            max_clients,
            max_streams,
            allowed_hosts: setting(
                "SUBTITLE_ALLOWED_HOSTS",
                "localhost,127.0.0.1,::1,subtitle-worker",
            )
            .split(',')
            .map(|s| s.trim().trim_matches(['[', ']']).to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        })
    }

    pub fn prepare(&mut self) -> Result<(), String> {
        for path in [
            &self.library_root,
            &self.output_root,
            &self.data_root,
            &self.model_root,
        ] {
            fs::create_dir_all(path).map_err(|_| "Could not create application directory")?;
        }
        self.library_root =
            fs::canonicalize(&self.library_root).map_err(|_| "Invalid library root")?;
        self.output_root =
            fs::canonicalize(&self.output_root).map_err(|_| "Invalid output root")?;
        self.data_root = fs::canonicalize(&self.data_root).map_err(|_| "Invalid data root")?;
        self.model_root = fs::canonicalize(&self.model_root).map_err(|_| "Invalid model root")?;
        if !crate::media::supported_model(&self.whisper_model) {
            return Err("Unsupported Whisper model".into());
        }
        Ok(())
    }
}

pub fn languages() -> Value {
    json!({"en":"English","fr":"French","ar":"Arabic","es":"Spanish","de":"German","it":"Italian","pt":"Portuguese","ja":"Japanese","ko":"Korean","zh":"Chinese","ru":"Russian","tr":"Turkish","nl":"Dutch","pl":"Polish","hi":"Hindi","id":"Indonesian","uk":"Ukrainian","sv":"Swedish","he":"Hebrew"})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_service_urls() {
        for value in [
            "file:///etc/passwd",
            "https://name:password@localhost/v1",
            "https://localhost/#fragment",
            "https://localhost/\\evil",
            "http://localhost:0",
        ] {
            assert!(validate_url(value).is_err());
        }
        assert!(validate_url("http://localhost:1234/v1").is_ok());
        assert!(validate_url("https://example.com/v1").is_ok());
    }
}
