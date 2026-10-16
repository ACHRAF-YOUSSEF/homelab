use crate::storage::{self, Operation, StorageRoot};
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
    pub model_download_timeout: u64,
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
    parse_number(name, &setting(name, &fallback.to_string()), min, max)
}

fn parse_number(name: &str, input: &str, min: u64, max: u64) -> Result<u64, String> {
    let value = input
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
            model_download_timeout: number("SUBTITLE_MODEL_DOWNLOAD_TIMEOUT", 3600, 1, 86400)?,
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
        for (path, root) in [
            (&self.library_root, StorageRoot::Library),
            (&self.output_root, StorageRoot::Output),
            (&self.data_root, StorageRoot::Data),
            (&self.model_root, StorageRoot::Model),
        ] {
            fs::create_dir_all(path)
                .map_err(|error| storage::io_error(root, Operation::CreateDirectory, &error))?;
        }
        self.prepare_read_only()?;
        storage::verify_writable(&self.data_root, StorageRoot::Data)?;
        storage::verify_writable(&self.output_root, StorageRoot::Output)?;
        Ok(())
    }

    /// Resolve existing roots for read-only commands, without creating files or
    /// requiring permission to modify saved jobs, models, media or exports.
    pub fn prepare_read_only(&mut self) -> Result<(), String> {
        self.library_root = fs::canonicalize(&self.library_root).map_err(|error| {
            storage::io_error(StorageRoot::Library, Operation::ResolveDirectory, &error)
        })?;
        self.output_root = fs::canonicalize(&self.output_root).map_err(|error| {
            storage::io_error(StorageRoot::Output, Operation::ResolveDirectory, &error)
        })?;
        self.data_root = fs::canonicalize(&self.data_root).map_err(|error| {
            storage::io_error(StorageRoot::Data, Operation::ResolveDirectory, &error)
        })?;
        self.model_root = fs::canonicalize(&self.model_root).map_err(|error| {
            storage::io_error(StorageRoot::Model, Operation::ResolveDirectory, &error)
        })?;
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

    fn fixture_config(root: &std::path::Path) -> Config {
        Config {
            library_root: root.join("library"),
            output_root: root.join("output"),
            data_root: root.join("data"),
            model_root: root.join("models"),
            bind_address: "127.0.0.1".into(),
            port: 8099,
            whisper_model: "small-q5_1".into(),
            whisper_device: "cpu".into(),
            whisper_cli: "whisper-cli".into(),
            local_only: true,
            cpu_threads: 2,
            audio_chunk_seconds: 300,
            process_timeout: 5,
            llm_base: "http://127.0.0.1:1/v1".into(),
            llm_model: String::new(),
            review_model: String::new(),
            llm_timeout: 3,
            model_download_timeout: 3600,
            max_json_bytes: 1024 * 1024,
            max_jobs: 10,
            max_queue: 4,
            max_clients: 4,
            max_streams: 2,
            allowed_hosts: vec!["localhost".into()],
        }
    }

    #[test]
    fn preparation_creates_required_roots_without_leaving_write_probes() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let mut config = fixture_config(directory.path());
        config.prepare().expect("prepare writable roots");
        for root in [
            &config.library_root,
            &config.model_root,
            &config.data_root,
            &config.output_root,
        ] {
            assert!(root.is_absolute());
            assert!(root.is_dir());
            assert_eq!(fs::read_dir(root).expect("fixture listing").count(), 0);
        }
    }

    #[test]
    fn model_download_timeout_is_bounded_without_mutating_environment() {
        const NAME: &str = "SUBTITLE_MODEL_DOWNLOAD_TIMEOUT";
        assert_eq!(
            parse_number(NAME, "3600", 1, 86400).expect("default timeout"),
            3600
        );
        assert_eq!(
            parse_number(NAME, "1", 1, 86400).expect("minimum timeout"),
            1
        );
        assert_eq!(
            parse_number(NAME, "86400", 1, 86400).expect("maximum timeout"),
            86400
        );
        for value in ["0", "86401", "-1", "not-a-number", ""] {
            assert!(parse_number(NAME, value, 1, 86400).is_err());
        }
    }

    #[test]
    fn read_only_preparation_never_creates_missing_directories() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let mut config = fixture_config(directory.path());
        let error = config.prepare_read_only().expect_err("roots do not exist");
        assert!(error.contains("SUBTITLE_LIBRARY_ROOT"));
        assert!(error.contains("resolve directory"));
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("fixture listing")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    struct RestorePermissions(std::path::PathBuf, fs::Permissions);

    #[cfg(unix)]
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, self.1.clone());
        }
    }

    #[cfg(unix)]
    fn make_read_only(path: &std::path::Path) -> RestorePermissions {
        use std::os::unix::fs::PermissionsExt;
        let original = fs::metadata(path)
            .expect("fixture permissions")
            .permissions();
        fs::set_permissions(path, fs::Permissions::from_mode(0o555))
            .expect("make fixture read only");
        RestorePermissions(path.to_owned(), original)
    }

    #[cfg(unix)]
    #[test]
    fn unprivileged_data_root_without_write_permission_has_actionable_error() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let mut config = fixture_config(directory.path());
        config.prepare().expect("initialize fixture roots");
        let _restore = make_read_only(&config.data_root);
        // This fixture must run as an unprivileged user (the container runtime
        // UID 1000), so it tests actual OS denial rather than permission bits.
        let error = config
            .prepare()
            .expect_err("run this Unix permission fixture as an unprivileged user");
        assert!(error.contains("SUBTITLE_DATA_ROOT"), "{error}");
        assert!(error.contains("create temporary file"), "{error}");
        assert!(error.contains("PermissionDenied"), "{error}");
        assert!(error.contains("UID 1000"), "{error}");
        assert!(!error.contains(&directory.path().to_string_lossy().into_owned()));
        assert_eq!(
            fs::read_dir(&config.data_root)
                .expect("read-only fixture listing")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn preloaded_read_only_models_and_library_are_supported() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let mut config = fixture_config(directory.path());
        config.prepare().expect("initialize fixture roots");
        fs::write(
            config.model_root.join("ggml-small-q5_1.bin"),
            b"disposable preloaded-model fixture",
        )
        .expect("preloaded model fixture");
        fs::write(
            config.library_root.join("video.mkv"),
            b"disposable library fixture",
        )
        .expect("library fixture");
        let _model_restore = make_read_only(&config.model_root);
        let _library_restore = make_read_only(&config.library_root);
        config
            .prepare()
            .expect("read-only model and library roots need no writes");
        assert_eq!(
            fs::read_dir(&config.model_root)
                .expect("model listing")
                .count(),
            1
        );
        assert_eq!(
            fs::read_dir(&config.library_root)
                .expect("library listing")
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_only_commands_accept_read_only_data_and_output_roots() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let mut config = fixture_config(directory.path());
        config.prepare().expect("initialize fixture roots");
        let _data_restore = make_read_only(&config.data_root);
        let _output_restore = make_read_only(&config.output_root);
        config
            .prepare_read_only()
            .expect("validation needs no write permissions");
        assert_eq!(
            fs::read_dir(&config.data_root)
                .expect("data listing")
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(&config.output_root)
                .expect("output listing")
                .count(),
            0
        );
    }
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
