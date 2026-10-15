#![forbid(unsafe_code)]

use image::{DynamicImage, ImageReader, Limits};
use ocr_rs::{Backend, DetOptions, OcrEngine, OcrEngineConfig};
#[cfg(test)]
use serde_json::Value;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

const MODEL_DIR: &str = "/opt/ocr-models";
const MAX_IMAGES: usize = 10_000;
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4096;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_RESULT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CUDA_HEALTH_BYTES: usize = 128;
const DEFAULT_GPU_MIN_FREE_MIB: u32 = 2048;
const MAX_GPU_MIN_FREE_MIB: u32 = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelFamily {
    English,
    Latin,
    Arabic,
    ChineseJapanese,
    Cyrillic,
    Devanagari,
    Greek,
    Eslav,
    Korean,
    Tamil,
    Telugu,
    Thai,
}

#[derive(Debug)]
struct Cli {
    input: PathBuf,
    language: String,
    output: PathBuf,
}

fn error(message: &str) -> ! {
    eprintln!("subforge-ocr: {message}");
    std::process::exit(2)
}

fn language_family(value: &str) -> Result<ModelFamily, &'static str> {
    let language = value.trim().to_ascii_lowercase().replace('-', "_");
    if matches!(language.as_str(), "sr_latn" | "rs_latin") {
        return Ok(ModelFamily::Latin);
    }
    let base = language.split('_').next().unwrap_or(&language);
    if matches!(base, "en" | "eng") || language == "english" {
        return Ok(ModelFamily::English);
    }
    if matches!(
        base,
        "ar" | "ara"
            | "arabic"
            | "fa"
            | "fas"
            | "per"
            | "persian"
            | "ur"
            | "urd"
            | "ps"
            | "pus"
            | "ug"
            | "uig"
            | "sd"
            | "snd"
    ) {
        return Ok(ModelFamily::Arabic);
    }
    if matches!(base, "zh" | "chi" | "zho" | "ja" | "jpn") {
        return Ok(ModelFamily::ChineseJapanese);
    }
    if matches!(base, "ru" | "rus" | "be" | "bel" | "uk" | "ukr") {
        return Ok(ModelFamily::Eslav);
    }
    if matches!(
        base,
        "bg" | "bul" | "sr" | "srp" | "mk" | "mkd" | "kk" | "kaz" | "mn" | "mon" | "tg" | "tgk"
    ) {
        return Ok(ModelFamily::Cyrillic);
    }
    if matches!(
        base,
        "hi" | "hin" | "mr" | "mar" | "ne" | "nep" | "sa" | "san"
    ) {
        return Ok(ModelFamily::Devanagari);
    }
    if matches!(base, "el" | "ell" | "gre" | "greek") {
        return Ok(ModelFamily::Greek);
    }
    if matches!(base, "ko" | "kor" | "korean") {
        return Ok(ModelFamily::Korean);
    }
    if matches!(base, "ta" | "tam") {
        return Ok(ModelFamily::Tamil);
    }
    if matches!(base, "te" | "tel") {
        return Ok(ModelFamily::Telugu);
    }
    if matches!(base, "th" | "tha") {
        return Ok(ModelFamily::Thai);
    }
    if matches!(
        base,
        "af" | "az"
            | "bs"
            | "ca"
            | "cs"
            | "cy"
            | "da"
            | "de"
            | "deu"
            | "ger"
            | "es"
            | "spa"
            | "et"
            | "fi"
            | "ga"
            | "hr"
            | "hu"
            | "id"
            | "is"
            | "it"
            | "ku"
            | "la"
            | "lt"
            | "lv"
            | "mi"
            | "ms"
            | "mt"
            | "nl"
            | "no"
            | "oc"
            | "pl"
            | "pt"
            | "ro"
            | "rs_latin"
            | "sk"
            | "sl"
            | "sq"
            | "sv"
            | "sw"
            | "tl"
            | "tr"
            | "uz"
            | "vi"
            | "fr"
            | "fra"
            | "fre"
            | "french"
            | "latin"
    ) {
        return Ok(ModelFamily::Latin);
    }
    Err("unsupported OCR language; no verified offline model is configured for this code")
}

fn parse_cli(args: &[String]) -> Result<Cli, &'static str> {
    if args.first().map(String::as_str) != Some("ocr") {
        return Err("expected `ocr -i DIR --lang CODE --save_path DIR`");
    }
    let mut input = None;
    let mut language = None;
    let mut output = None;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args.get(index + 1).ok_or("missing flag value")?;
        match flag {
            "-i" if input.is_none() => input = Some(PathBuf::from(value)),
            "--lang" if language.is_none() => language = Some(value.clone()),
            "--save_path" if output.is_none() => output = Some(PathBuf::from(value)),
            "--use_angle_cls" if value == "false" => {}
            "--use_angle_cls" => return Err("only `--use_angle_cls false` is supported"),
            _ => return Err("unsupported OCR argument"),
        }
        index += 2;
    }
    let language = language.ok_or("missing --lang")?;
    language_family(&language)?;
    let input = input.ok_or("missing -i")?;
    let output = output.ok_or("missing --save_path")?;
    if !input.is_absolute() || !output.is_absolute() {
        return Err("input and output directories must be absolute paths inside TMPDIR");
    }
    Ok(Cli {
        input,
        language,
        output,
    })
}

fn private_scratch() -> Result<PathBuf, &'static str> {
    let path = env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    if !path.is_absolute() || has_symlink_ancestor(&path) {
        return Err("TMPDIR must be an absolute private directory without symlink ancestors");
    }
    let canonical = path.canonicalize().map_err(|_| "TMPDIR is unavailable")?;
    let metadata = fs::symlink_metadata(&canonical).map_err(|_| "TMPDIR is unavailable")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("TMPDIR must be a directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("TMPDIR permissions must be private (0700 or stricter)");
        }
    }
    Ok(canonical)
}

fn has_symlink_ancestor(path: &Path) -> bool {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(part) => current.push(part),
            Component::CurDir => {}
            Component::ParentDir => return true,
        }
        if fs::symlink_metadata(&current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return true;
        }
    }
    false
}

fn checked_dir(path: &Path, scratch: &Path) -> Result<PathBuf, &'static str> {
    if has_symlink_ancestor(path) {
        return Err("input/output paths cannot contain symlinks");
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "input/output directory is unavailable")?;
    let metadata =
        fs::symlink_metadata(&canonical).map_err(|_| "input/output directory is unavailable")?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || !canonical.starts_with(scratch)
        || canonical == scratch
    {
        return Err("input/output directories must be below TMPDIR");
    }
    Ok(canonical)
}

fn validate_paths(cli: &Cli, scratch: &Path) -> Result<(PathBuf, PathBuf), &'static str> {
    let input = checked_dir(&cli.input, scratch)?;
    let output = checked_dir(&cli.output, scratch)?;
    if input.starts_with(&output) || output.starts_with(&input) {
        return Err("input and output directories must not overlap");
    }
    Ok((input, output))
}

fn list_images(input: &Path) -> Result<Vec<PathBuf>, &'static str> {
    let mut files = Vec::new();
    for entry in fs::read_dir(input).map_err(|_| "cannot read OCR input directory")? {
        let entry = entry.map_err(|_| "cannot read OCR input directory")?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|_| "cannot inspect OCR input")?;
        if meta.file_type().is_symlink() {
            return Err("symlinked OCR inputs are not allowed");
        }
        if !meta.is_file() || !supported_extension(&path) {
            continue;
        }
        if meta.len() > MAX_INPUT_BYTES {
            return Err("OCR image exceeds the 64 MiB input limit");
        }
        files.push(path);
        if files.len() > MAX_IMAGES {
            return Err("OCR input contains more than 10,000 images");
        }
    }
    files.sort();
    Ok(files)
}

fn supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png"
                    | "bmp"
                    | "jpg"
                    | "jpeg"
                    | "webp"
                    | "gif"
                    | "tif"
                    | "tiff"
                    | "pnm"
                    | "ppm"
                    | "pgm"
                    | "pbm"
                    | "qoi"
                    | "tga"
                    | "ico"
            )
        })
}

fn model_files(family: ModelFamily) -> (&'static str, &'static str) {
    match family {
        ModelFamily::English => ("en_PP-OCRv5_mobile_rec_infer.mnn", "ppocr_keys_en.txt"),
        ModelFamily::Latin => (
            "latin_PP-OCRv5_mobile_rec_infer.mnn",
            "ppocr_keys_latin.txt",
        ),
        ModelFamily::Arabic => (
            "arabic_PP-OCRv5_mobile_rec_infer.mnn",
            "ppocr_keys_arabic.txt",
        ),
        ModelFamily::ChineseJapanese => ("PP-OCRv5_mobile_rec_fp16.mnn", "ppocr_keys_v5.txt"),
        ModelFamily::Cyrillic => (
            "cyrillic_PP-OCRv5_mobile_rec_infer.mnn",
            "ppocr_keys_cyrillic.txt",
        ),
        ModelFamily::Devanagari => (
            "devanagari_PP-OCRv5_mobile_rec_infer.mnn",
            "ppocr_keys_devanagari.txt",
        ),
        ModelFamily::Greek => ("el_PP-OCRv5_mobile_rec_infer.mnn", "ppocr_keys_el.txt"),
        ModelFamily::Eslav => (
            "eslav_PP-OCRv5_mobile_rec_infer.mnn",
            "ppocr_keys_eslav.txt",
        ),
        ModelFamily::Korean => (
            "korean_PP-OCRv5_mobile_rec_infer.mnn",
            "ppocr_keys_korean.txt",
        ),
        ModelFamily::Tamil => ("ta_PP-OCRv5_mobile_rec_infer.mnn", "ppocr_keys_ta.txt"),
        ModelFamily::Telugu => ("te_PP-OCRv5_mobile_rec_infer.mnn", "ppocr_keys_te.txt"),
        ModelFamily::Thai => ("th_PP-OCRv5_mobile_rec_infer.mnn", "ppocr_keys_th.txt"),
    }
}

fn is_arabic_mark(character: char) -> bool {
    matches!(
        character as u32,
        0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06ED
            | 0x08D3..=0x08FF
    )
}

fn is_ltr_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, ' ' | ':' | '*' | '.' | '/' | '%' | '+' | '-')
}

fn postprocess_text(text: &str, family: ModelFamily) -> String {
    if family != ModelFamily::Arabic {
        return text.to_owned();
    }

    // MNN's Arabic recognizer returns visual-order characters. Reverse whole
    // Arabic graphemes to logical order, then restore the direction inside
    // contiguous Latin, numeric, and punctuation runs.
    let mut clusters: Vec<String> = Vec::new();
    for character in text.chars() {
        if is_arabic_mark(character) {
            if let Some(previous) = clusters.last_mut() {
                previous.push(character);
            } else {
                clusters.push(character.to_string());
            }
        } else {
            clusters.push(character.to_string());
        }
    }
    clusters.reverse();

    let mut index = 0;
    while index < clusters.len() {
        if clusters[index].chars().all(is_ltr_character) {
            let start = index;
            while index < clusters.len() && clusters[index].chars().all(is_ltr_character) {
                index += 1;
            }
            clusters[start..index].reverse();
        } else {
            index += 1;
        }
    }
    clusters.concat()
}

fn expected_digests() -> Result<BTreeMap<&'static str, &'static str>, &'static str> {
    let pinned = include_str!("../models.sha256");
    let mut entries = BTreeMap::new();
    for line in pinned.lines() {
        let mut parts = line.split_whitespace();
        let digest = parts.next().ok_or("invalid embedded OCR model manifest")?;
        let filename = parts.next().ok_or("invalid embedded OCR model manifest")?;
        if parts.next().is_some()
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid embedded OCR model manifest");
        }
        let known = match filename {
            "en_PP-OCRv5_mobile_rec_infer.mnn" => {
                "c5e747fb69275e9d99fbe54f97642c65681c3e4244383f825d1d7668e9aece81"
            }
            "ppocr_keys_en.txt" => {
                "e025a66d31f327ba0c232e03f407ae8d105e1e709e7ccb3f408aa778c24e70d6"
            }
            "latin_PP-OCRv5_mobile_rec_infer.mnn" => {
                "f3da21186ade13e0d3553867205a8983d9fda46e10b6a15e5f6308325e6e7a55"
            }
            "ppocr_keys_latin.txt" => {
                "ccbcc45730b3fbbd9050c5bc74db6a99067141ef1035e3d14889a84a6b9b1aff"
            }
            "arabic_PP-OCRv5_mobile_rec_infer.mnn" => {
                "6c5d0653cdcf78317321b3932b62f02814b8d3b8bd2a8760ffc7316f8727cf33"
            }
            "ppocr_keys_arabic.txt" => {
                "7f92f7dbb9b75a4787a83bfb4f6d14a8ab515525130c9d40a9036f61cf6999e9"
            }
            "PP-OCRv5_mobile_det_fp16.mnn" => {
                "617b5228b101275594f96ebb6ae7662fd1618bcf8e84b0ffde1cf3b48e754951"
            }
            "PP-OCRv5_mobile_rec_fp16.mnn" => {
                "ff03e4204260325eabe9f4eae0ec8cc6b79b8a97a8e38a5292ba69cf02a689fc"
            }
            "ppocr_keys_v5.txt" => {
                "f2ed6bb20a850ce4767fa9b4622d9b282985ab7f0ea8f8c11abd790ca6d2ff94"
            }
            "cyrillic_PP-OCRv5_mobile_rec_infer.mnn" => {
                "5fe37fb8382e9cd227afa2605ba99202466960bc7aa9e22ae6b1ce05ecedf2e1"
            }
            "ppocr_keys_cyrillic.txt" => {
                "db40aa52ceb112055be80c694afdf655d5d2c4f7873704524cc16a447ca913ba"
            }
            "devanagari_PP-OCRv5_mobile_rec_infer.mnn" => {
                "8faa2c1a31b4e4bb4cedb765511edcb47d3050a493ace7abd9670250f40e15cc"
            }
            "ppocr_keys_devanagari.txt" => {
                "09c7440bfc5477e5c41052304b6b185aff8c4a5e8b2b4c23c1c706f6fe1ee9fc"
            }
            "el_PP-OCRv5_mobile_rec_infer.mnn" => {
                "302097ec28112f9be70a1edd1147f9392755c2ea4d7831cdf95d04f1bc065e2b"
            }
            "ppocr_keys_el.txt" => {
                "31defc62c0c3ad3674a82da6192226a2ba98ef4ff014a7045cb88d59f9c3de31"
            }
            "eslav_PP-OCRv5_mobile_rec_infer.mnn" => {
                "6d8a4d28dffc32a91d3b04e224df528adac6924c0702c955f10bc84d19cbdade"
            }
            "ppocr_keys_eslav.txt" => {
                "3e95f1581557162870cacdba5af91a4c6be2890710d395b0c3c7578e7ee5e6eb"
            }
            "korean_PP-OCRv5_mobile_rec_infer.mnn" => {
                "806d9773f5f0b00c16eac2b13ba91d50de3485f28ca7669be2e7ea9a66a41e38"
            }
            "ppocr_keys_korean.txt" => {
                "a88071c68c01707489baa79ebe0405b7beb5cca229f4fc94cc3ef992328802d7"
            }
            "ta_PP-OCRv5_mobile_rec_infer.mnn" => {
                "ae554bdd6c87139829cf30455e8e3919e3b50d8e0c24b282131ef269efc3cda4"
            }
            "ppocr_keys_ta.txt" => {
                "85b541352ae18dc6ba6d47152d8bf8adff6b0266e605d2eef2990c1bf466117b"
            }
            "te_PP-OCRv5_mobile_rec_infer.mnn" => {
                "9af6053abc2ef87b99897f11bb5d1b2fa6407063ecc018e86a7fa91b486c3447"
            }
            "ppocr_keys_te.txt" => {
                "42f83f5d3fdb50778e4fa5b66c58d99a59ab7792151c5e74f34b8ffd7b61c9d6"
            }
            "th_PP-OCRv5_mobile_rec_infer.mnn" => {
                "da289707e95c01dcf1e927f2512525bd1260602aa12ae677534d8f36754322ff"
            }
            "ppocr_keys_th.txt" => {
                "57f5406f94bb6688fb7077f7be65f08bbd71cecf48c01ea26c522cb5c4836b7a"
            }
            _ => return Err("unknown OCR model in embedded manifest"),
        };
        if digest != known || entries.insert(filename, digest).is_some() {
            return Err("embedded OCR model manifest does not match pinned digests");
        }
    }
    if entries.len() != 25 {
        return Err("embedded OCR model manifest is incomplete");
    }
    Ok(entries)
}

fn verify_model(path: &Path, expected: &str) -> Result<(), &'static str> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "required offline OCR model is missing")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_INPUT_BYTES
    {
        return Err("offline OCR model file failed validation");
    }
    let mut file = File::open(path).map_err(|_| "offline OCR model file is unreadable")?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "offline OCR model could not be hashed")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if format!("{:x}", digest.finalize()) != expected {
        return Err("offline OCR model checksum does not match the pinned digest");
    }
    Ok(())
}

fn validate_models(family: ModelFamily) -> Result<(PathBuf, PathBuf, PathBuf), &'static str> {
    let manifest = expected_digests()?;
    let (recognizer, charset) = model_files(family);
    let base = Path::new(MODEL_DIR);
    if has_symlink_ancestor(base) {
        return Err("offline OCR model path contains a symlink");
    }
    let base_meta = fs::symlink_metadata(base).map_err(|_| "offline OCR models are unavailable")?;
    if !base_meta.is_dir() || base_meta.file_type().is_symlink() {
        return Err("offline OCR models are unavailable");
    }
    let detector = base.join("PP-OCRv5_mobile_det_fp16.mnn");
    let recognizer = base.join(recognizer);
    let charset = base.join(charset);
    for path in [&detector, &recognizer, &charset] {
        if has_symlink_ancestor(path) {
            return Err("offline OCR model path contains a symlink");
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("invalid offline OCR model path")?;
        verify_model(
            path,
            manifest
                .get(name)
                .ok_or("offline OCR model is not pinned")?,
        )?;
    }
    Ok((detector, recognizer, charset))
}

fn backend_from_env() -> Result<Backend, &'static str> {
    match env::var("SUBFORGE_OCR_BACKEND").as_deref() {
        Ok("cpu") => Ok(Backend::CPU),
        Ok("cuda") => {
            #[cfg(feature = "cuda")]
            {
                Ok(Backend::CUDA)
            }
            #[cfg(not(feature = "cuda"))]
            {
                Err("CUDA OCR is not present in this build")
            }
        }
        _ => Err("SUBFORGE_OCR_BACKEND must be cpu or cuda"),
    }
}

fn engine(family: ModelFamily, backend: Backend) -> Result<OcrEngine, &'static str> {
    let (detector, recognizer, charset) = validate_models(family)?;
    if backend == Backend::CUDA {
        if !backend_available(backend) {
            return Err("CUDA backend is unavailable");
        }
        let (_, free, _) = nvidia_metrics().ok_or("CUDA device 0 is unavailable")?;
        if free < u64::from(gpu_min_free_mib()?) * 1024 * 1024 {
            return Err("CUDA device 0 has insufficient free VRAM");
        }
    }
    let config = OcrEngineConfig::new()
        .with_backend(backend)
        .with_threads(1)
        .with_parallel(false)
        .with_det_options(DetOptions::new().with_max_side_len(960));
    OcrEngine::new(&detector, &recognizer, &charset, Some(config))
        .map_err(|_| "OCR engine initialization failed")
}

fn backend_available(backend: Backend) -> bool {
    backend.is_available()
}

fn is_backend_check_request(args: &[String]) -> bool {
    args == ["--backend-check-cuda"]
}

fn cuda_backend_check_child_exit() -> bool {
    cfg!(feature = "cuda") && backend_available(Backend::CUDA)
}

fn cuda_backend_available_isolated() -> bool {
    if !cfg!(feature = "cuda") {
        return false;
    }
    let Ok(executable) = env::current_exe() else {
        return false;
    };
    let Ok(mut child) = Command::new(executable)
        .arg("--backend-check-cuda")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
        }
    }
}

fn gpu_min_free_mib() -> Result<u32, &'static str> {
    let value =
        env::var("GPU_MIN_FREE_MIB").unwrap_or_else(|_| DEFAULT_GPU_MIN_FREE_MIB.to_string());
    let parsed = value
        .parse::<u32>()
        .map_err(|_| "GPU_MIN_FREE_MIB must be an integer from 0 to 65536")?;
    if parsed > MAX_GPU_MIN_FREE_MIB {
        return Err("GPU_MIN_FREE_MIB must be an integer from 0 to 65536");
    }
    Ok(parsed)
}

fn nvidia_smi_path() -> Option<&'static Path> {
    use std::os::unix::fs::PermissionsExt;
    ["/usr/bin/nvidia-smi", "/usr/lib/wsl/lib/nvidia-smi"]
        .iter()
        .map(Path::new)
        .find(|path| {
            !has_symlink_ancestor(path)
                && fs::symlink_metadata(path)
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

fn nvidia_metrics() -> Option<(u32, u64, u64)> {
    let binary = nvidia_smi_path()?;
    let mut child = Command::new(binary)
        .args([
            "--id=0",
            "--query-gpu=index,memory.free,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::with_capacity(MAX_CUDA_HEALTH_BYTES + 1);
        let _ = stdout
            .take((MAX_CUDA_HEALTH_BYTES + 1) as u64)
            .read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let bytes = reader.join().ok()?;
            if !status.success() || bytes.len() > MAX_CUDA_HEALTH_BYTES {
                return None;
            }
            let text = std::str::from_utf8(&bytes).ok()?.trim();
            let fields: Vec<_> = text.split(',').map(str::trim).collect();
            if fields.len() != 3 || fields[0] != "0" {
                return None;
            }
            let free_mib: u64 = fields[1].parse().ok()?;
            let total_mib: u64 = fields[2].parse().ok()?;
            if total_mib == 0 || free_mib > total_mib {
                return None;
            }
            return Some((1, free_mib * 1024 * 1024, total_mib * 1024 * 1024));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn health() -> i32 {
    let cuda_backend = cuda_backend_available_isolated();
    let metrics = nvidia_metrics();
    let threshold = gpu_min_free_mib().unwrap_or(MAX_GPU_MIN_FREE_MIB + 1) as u64 * 1024 * 1024;
    let (devices, free, total) = metrics.unwrap_or((0, 0, 0));
    let cuda = cuda_backend && devices == 1 && total > 0 && free >= threshold;
    let ok = cfg!(feature = "cuda") && cuda;
    println!(
        "{}",
        json!({"ok":ok,"cuda":cuda,"device_count":devices,"free_bytes":free,"total_bytes":total,"models_checked":false})
    );
    if ok { 0 } else { 1 }
}

fn load_bounded(path: &Path) -> Result<DynamicImage, &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "cannot inspect OCR input")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_INPUT_BYTES
    {
        return Err("OCR input failed file or size validation");
    }
    let bytes = fs::read(path).map_err(|_| "cannot read OCR input")?;
    let reader = ImageReader::new(Cursor::new(bytes.as_slice()))
        .with_guessed_format()
        .map_err(|_| "unsupported or invalid OCR image")?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| "invalid OCR image dimensions")?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or("invalid OCR image dimensions")?;
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || pixels > MAX_IMAGE_PIXELS
    {
        return Err("OCR image dimensions exceed the 4096 edge or 16-megapixel limit");
    }
    let mut reader = ImageReader::new(Cursor::new(bytes.as_slice()))
        .with_guessed_format()
        .map_err(|_| "unsupported or invalid OCR image")?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(|_| "OCR image decoding failed")
}

fn result_json(
    path: &Path,
    image: &DynamicImage,
    engine: &OcrEngine,
    family: ModelFamily,
) -> Result<Vec<u8>, &'static str> {
    let results = engine
        .recognize(image)
        .map_err(|_| "OCR recognition failed")?;
    if results.len() > 20_000 {
        return Err("OCR generated too many text regions");
    }
    let mut texts = Vec::with_capacity(results.len());
    let mut scores = Vec::with_capacity(results.len());
    let mut polygons = Vec::with_capacity(results.len());
    let mut boxes = Vec::with_capacity(results.len());
    let mut total_text_bytes = 0usize;
    for result in results {
        total_text_bytes = total_text_bytes
            .checked_add(result.text.len())
            .ok_or("OCR result size overflow")?;
        if result.text.len() > 16_384
            || total_text_bytes > MAX_RESULT_BYTES / 2
            || !result.confidence.is_finite()
            || !(0.0..=1.0).contains(&result.confidence)
        {
            return Err("OCR result exceeded output limits");
        }
        let rect = result.bbox.rect;
        let left_f = rect.left().max(0) as f32;
        let top_f = rect.top().max(0) as f32;
        let right_f = left_f + rect.width() as f32;
        let bottom_f = top_f + rect.height() as f32;
        let raw_points: Vec<[f32; 2]> = result
            .bbox
            .points
            .as_ref()
            .map(|points| points.iter().map(|point| [point.x, point.y]).collect())
            .unwrap_or_else(|| {
                vec![
                    [left_f, top_f],
                    [right_f, top_f],
                    [right_f, bottom_f],
                    [left_f, bottom_f],
                ]
            });
        if raw_points.len() != 4 || raw_points.iter().flatten().any(|value| !value.is_finite()) {
            return Err("OCR returned invalid coordinates");
        }
        let width = image.width() as f32;
        let height = image.height() as f32;
        let points: Vec<[f32; 2]> = raw_points
            .into_iter()
            .map(|[x, y]| [x.clamp(0.0, width), y.clamp(0.0, height)])
            .collect();
        let left = rect.left().max(0) as u32;
        let top = rect.top().max(0) as u32;
        let right = (rect.left().max(0) as u32)
            .saturating_add(rect.width())
            .min(image.width());
        let bottom = (rect.top().max(0) as u32)
            .saturating_add(rect.height())
            .min(image.height());
        texts.push(postprocess_text(&result.text, family));
        scores.push(result.confidence);
        polygons.push(points);
        boxes.push([left, top, right, bottom]);
    }
    serialize_result(path, texts, scores, polygons, boxes)
}

fn serialize_result(
    path: &Path,
    texts: Vec<String>,
    scores: Vec<f32>,
    polygons: Vec<Vec<[f32; 2]>>,
    boxes: Vec<[u32; 4]>,
) -> Result<Vec<u8>, &'static str> {
    let count = texts.len();
    if scores.len() != count || polygons.len() != count || boxes.len() != count {
        return Err("OCR result arrays are not aligned");
    }
    let value = json!({
        "input_path": path.to_string_lossy(),
        "rec_texts": texts,
        "rec_scores": scores,
        "rec_polys": polygons.clone(),
        "dt_polys": polygons,
        "rec_boxes": boxes
    });
    let bytes = serde_json::to_vec(&value).map_err(|_| "OCR results could not be serialized")?;
    if bytes.len() > MAX_RESULT_BYTES {
        return Err("OCR result exceeded the 8 MiB output limit");
    }
    Ok(bytes)
}

fn publish_no_clobber(output_dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or("OCR input has an invalid filename")?;
    let target = output_dir.join(format!("{stem}_res.json"));
    if target.exists() {
        return Err("OCR output already exists; refusing to overwrite");
    }
    let mut temporary =
        NamedTempFile::new_in(output_dir).map_err(|_| "cannot create private OCR output")?;
    temporary
        .write_all(bytes)
        .map_err(|_| "cannot write OCR output")?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| "cannot persist OCR output")?;
    temporary
        .persist_noclobber(&target)
        .map_err(|_| "cannot publish OCR output without overwriting")?;
    Ok(())
}

fn run(cli: Cli) -> Result<(), &'static str> {
    let scratch = private_scratch()?;
    let (input, output) = validate_paths(&cli, &scratch)?;
    let images = list_images(&input)?;
    let family = language_family(&cli.language)?;
    let backend = backend_from_env()?;
    if backend == Backend::CUDA {
        let threshold = gpu_min_free_mib()?;
        let (_, free, _) = nvidia_metrics().ok_or("CUDA device 0 is unavailable")?;
        if free < u64::from(threshold) * 1024 * 1024 {
            return Err("CUDA device 0 has insufficient free VRAM");
        }
    }
    let engine = engine(family, backend)?;
    let mut total = 0u64;
    for image_path in images {
        let image = load_bounded(&image_path)?;
        let bytes = result_json(&image_path, &image, &engine, family)?;
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or("OCR output size overflow")?;
        if total > MAX_TOTAL_RESULT_BYTES {
            return Err("OCR outputs exceed the 64 MiB total limit");
        }
        publish_no_clobber(&output, &image_path, &bytes)?;
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.as_slice() == ["--health-json"] {
        std::process::exit(health());
    }
    if is_backend_check_request(&args) {
        std::process::exit(if cuda_backend_check_child_exit() {
            0
        } else {
            1
        });
    }
    let cli = parse_cli(&args).unwrap_or_else(|message| error(message));
    run(cli).unwrap_or_else(|message| error(message));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn language_codes_select_only_supported_model_families() {
        assert_eq!(language_family("eng").unwrap(), ModelFamily::English);
        assert_eq!(language_family("en").unwrap(), ModelFamily::English);
        assert_eq!(language_family("en-US").unwrap(), ModelFamily::English);
        assert_eq!(language_family("fra").unwrap(), ModelFamily::Latin);
        assert_eq!(language_family("fr-FR").unwrap(), ModelFamily::Latin);
        assert_eq!(language_family("de").unwrap(), ModelFamily::Latin);
        assert_eq!(language_family("ara").unwrap(), ModelFamily::Arabic);
        assert_eq!(language_family("ur").unwrap(), ModelFamily::Arabic);
        assert_eq!(
            language_family("zh-TW").unwrap(),
            ModelFamily::ChineseJapanese
        );
        assert_eq!(language_family("ja").unwrap(), ModelFamily::ChineseJapanese);
        assert_eq!(language_family("ru").unwrap(), ModelFamily::Eslav);
        assert_eq!(language_family("sr-Cyrl").unwrap(), ModelFamily::Cyrillic);
        assert_eq!(language_family("hi").unwrap(), ModelFamily::Devanagari);
        assert_eq!(language_family("el").unwrap(), ModelFamily::Greek);
        assert_eq!(language_family("ko").unwrap(), ModelFamily::Korean);
        assert_eq!(language_family("ta").unwrap(), ModelFamily::Tamil);
        assert_eq!(language_family("te").unwrap(), ModelFamily::Telugu);
        assert_eq!(language_family("th").unwrap(), ModelFamily::Thai);
        assert!(language_family("xx").is_err());
    }

    #[test]
    fn cli_rejects_unrecognized_flags_and_requires_absolute_scratch_paths() {
        let args = [
            "ocr",
            "-i",
            "/tmp/in",
            "--lang",
            "en",
            "--save_path",
            "/tmp/out",
            "--device",
            "cuda",
        ]
        .map(str::to_owned);
        assert!(parse_cli(&args).is_err());
        let args = [
            "ocr",
            "-i",
            "relative",
            "--lang",
            "en",
            "--save_path",
            "/tmp/out",
        ]
        .map(str::to_owned);
        assert!(parse_cli(&args).is_err());
        let args = [
            "ocr",
            "-i",
            "/tmp/in",
            "--lang",
            "xx",
            "--save_path",
            "/tmp/out",
        ]
        .map(str::to_owned);
        assert!(parse_cli(&args).is_err());
    }

    #[test]
    fn directory_validation_rejects_overlap_and_paths_outside_private_scratch() {
        let scratch = tempfile::tempdir().unwrap();
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let input = scratch.path().join("in");
        let output = scratch.path().join("out");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&output).unwrap();
        let cli = Cli {
            input: input.clone(),
            language: "eng".into(),
            output: output.clone(),
        };
        assert!(validate_paths(&cli, scratch.path()).is_ok());
        let overlap = Cli {
            input: input.clone(),
            language: "eng".into(),
            output: input.clone(),
        };
        assert!(validate_paths(&overlap, scratch.path()).is_err());
        let outside = Cli {
            input: std::env::temp_dir(),
            language: "eng".into(),
            output,
        };
        assert!(validate_paths(&outside, scratch.path()).is_err());
    }

    #[test]
    fn symlinked_input_paths_and_non_private_tmp_are_rejected() {
        let scratch = tempfile::tempdir().unwrap();
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let real = scratch.path().join("real");
        let link = scratch.path().join("link");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(checked_dir(&link, scratch.path()).is_err());
        assert!(checked_dir(&real, scratch.path()).is_ok());
    }

    #[test]
    fn serializer_emits_empty_and_aligned_seconv_schema_and_rejects_mismatch() {
        let empty =
            serialize_result(Path::new("/tmp/a.png"), vec![], vec![], vec![], vec![]).unwrap();
        let empty: Value = serde_json::from_slice(&empty).unwrap();
        for key in [
            "rec_texts",
            "rec_scores",
            "rec_polys",
            "dt_polys",
            "rec_boxes",
        ] {
            assert!(empty[key].as_array().is_some_and(Vec::is_empty));
        }
        assert_eq!(empty["input_path"], "/tmp/a.png");

        let one = serialize_result(
            Path::new("/tmp/b.png"),
            vec!["hello".into()],
            vec![0.9],
            vec![vec![[1.0, 2.0], [3.0, 2.0], [3.0, 4.0], [1.0, 4.0]]],
            vec![[1, 2, 3, 4]],
        )
        .unwrap();
        let one: Value = serde_json::from_slice(&one).unwrap();
        assert_eq!(one["rec_texts"][0], "hello");
        assert!((one["rec_scores"][0].as_f64().unwrap() - 0.9).abs() < 1e-6);
        assert_eq!(one["rec_polys"][0], one["dt_polys"][0]);
        assert_eq!(one["rec_boxes"][0], json!([1, 2, 3, 4]));
        assert!(
            serialize_result(
                Path::new("/tmp/c.png"),
                vec!["x".into()],
                vec![],
                vec![],
                vec![]
            )
            .is_err()
        );
    }

    #[test]
    fn output_publication_never_replaces_an_existing_report() {
        let scratch = tempfile::tempdir().unwrap();
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let image = scratch.path().join("frame.png");
        let existing = scratch.path().join("frame_res.json");
        fs::write(&image, b"input").unwrap();
        fs::write(&existing, b"keep").unwrap();
        assert!(publish_no_clobber(scratch.path(), &image, b"replacement").is_err());
        assert_eq!(fs::read(existing).unwrap(), b"keep");
    }

    #[test]
    fn model_manifest_embeds_the_expected_verified_artifacts() {
        let manifest = expected_digests().unwrap();
        assert_eq!(manifest.len(), 25);
        for family in [
            ModelFamily::English,
            ModelFamily::Latin,
            ModelFamily::Arabic,
            ModelFamily::ChineseJapanese,
            ModelFamily::Cyrillic,
            ModelFamily::Devanagari,
            ModelFamily::Greek,
            ModelFamily::Eslav,
            ModelFamily::Korean,
            ModelFamily::Tamil,
            ModelFamily::Telugu,
            ModelFamily::Thai,
        ] {
            let (recognizer, charset) = model_files(family);
            assert!(manifest.contains_key(recognizer));
            assert!(manifest.contains_key(charset));
        }
        assert_eq!(
            manifest["PP-OCRv5_mobile_det_fp16.mnn"],
            "617b5228b101275594f96ebb6ae7662fd1618bcf8e84b0ffde1cf3b48e754951"
        );
        assert_eq!(
            manifest["PP-OCRv5_mobile_rec_fp16.mnn"],
            "ff03e4204260325eabe9f4eae0ec8cc6b79b8a97a8e38a5292ba69cf02a689fc"
        );
        assert_eq!(
            model_files(ModelFamily::ChineseJapanese),
            ("PP-OCRv5_mobile_rec_fp16.mnn", "ppocr_keys_v5.txt")
        );
    }

    #[test]
    fn primary_subtitle_languages_use_bundled_offline_models() {
        let manifest = expected_digests().unwrap();
        let cases = [
            (
                "en",
                ModelFamily::English,
                "en_PP-OCRv5_mobile_rec_infer.mnn",
                "ppocr_keys_en.txt",
            ),
            (
                "fr",
                ModelFamily::Latin,
                "latin_PP-OCRv5_mobile_rec_infer.mnn",
                "ppocr_keys_latin.txt",
            ),
            (
                "ar",
                ModelFamily::Arabic,
                "arabic_PP-OCRv5_mobile_rec_infer.mnn",
                "ppocr_keys_arabic.txt",
            ),
        ];
        for (language, family, recognizer, charset) in cases {
            assert_eq!(language_family(language), Ok(family), "language {language}");
            assert_eq!(model_files(family), (recognizer, charset));
            assert!(
                manifest.contains_key(recognizer),
                "missing model for {language}"
            );
            assert!(
                manifest.contains_key(charset),
                "missing charset for {language}"
            );
        }
        assert_eq!(language_family("fr-FR"), Ok(ModelFamily::Latin));
        assert_eq!(language_family("ar-SA"), Ok(ModelFamily::Arabic));
    }

    #[test]
    fn arabic_visual_order_is_restored_without_reversing_ltr_runs_or_marks() {
        assert_eq!(
            postprocess_text("ملاعلاب ابحرم", ModelFamily::Arabic),
            "مرحبا بالعالم"
        );
        assert_eq!(
            postprocess_text("ملاعلاب 123 ABC ابحرم", ModelFamily::Arabic),
            "مرحبا 123 ABC بالعالم"
        );
        assert_eq!(postprocess_text("ابًحَرْمَ", ModelFamily::Arabic), "مَرْحَبًا");
        assert_eq!(
            postprocess_text("L'ÉTÉ À PARIS", ModelFamily::Latin),
            "L'ÉTÉ À PARIS"
        );
        assert_eq!(
            postprocess_text("HELLO WORLD 123", ModelFamily::English),
            "HELLO WORLD 123"
        );
    }

    #[test]
    fn isolated_cuda_check_accepts_only_its_exact_internal_command() {
        assert!(is_backend_check_request(&["--backend-check-cuda".into()]));
        for args in [
            vec![],
            vec!["--backend-check-cuda".into(), "extra".into()],
            vec!["prefix--backend-check-cuda".into()],
            vec!["--health-json".into()],
            vec!["--backend-check-cpu".into()],
        ] {
            assert!(!is_backend_check_request(&args));
        }
    }

    #[cfg(not(feature = "cuda"))]
    #[test]
    fn cpu_edition_rejects_the_cuda_backend_probe_without_loading_it() {
        assert!(!cuda_backend_check_child_exit());
        assert!(!cuda_backend_available_isolated());
    }
}
