"""Local MKV subtitle studio. No cloud or n8n dependency."""

import json
import gc
import hashlib
import logging
import os
import queue
import re
import subprocess
import threading
import time
import uuid
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, quote, unquote, urlparse
from urllib.request import Request, urlopen


LIBRARY_ROOT = Path(os.getenv("SUBTITLE_LIBRARY_ROOT", "/library")).resolve()
OUTPUT_ROOT = Path(os.getenv("SUBTITLE_OUTPUT_ROOT", "/media/output")).resolve()
DATA_ROOT = Path(os.getenv("SUBTITLE_DATA_ROOT", "/data")).resolve()
WEB_ROOT = Path(__file__).parent / "web"
LLM_BASE = os.getenv("SUBTITLE_LLM_BASE_URL", "http://host.docker.internal:1234/v1").rstrip("/")
LLM_DEFAULT = os.getenv("SUBTITLE_LLM_MODEL", "").strip()
LLM_TIMEOUT = int(os.getenv("SUBTITLE_LLM_TIMEOUT", "600"))
WHISPER_NAME = os.getenv("WHISPER_MODEL", "medium").strip() or "medium"
WHISPER_CACHE_DIR = os.getenv("HF_HUB_CACHE", "/models/hub")
WHISPER_LOCAL_ONLY = os.getenv("WHISPER_LOCAL_FILES_ONLY", "false").lower() == "true"
LANGUAGES = {
    "en": "English", "fr": "French", "ar": "Arabic", "es": "Spanish",
    "de": "German", "it": "Italian", "pt": "Portuguese", "ja": "Japanese",
    "ko": "Korean", "zh": "Chinese", "ru": "Russian", "tr": "Turkish",
    "nl": "Dutch", "pl": "Polish", "hi": "Hindi", "id": "Indonesian",
    "uk": "Ukrainian", "sv": "Swedish", "he": "Hebrew",
}
VIDEO_SUFFIXES = {".mkv", ".mp4", ".m4v", ".webm"}
JOBS_FILE = DATA_ROOT / "jobs.json"
CACHE_VERSION = 1
jobs = {}
jobs_lock = threading.RLock()
job_queue = queue.Queue()
speech_model = None
speech_model_name = None
speech_lock = threading.Lock()


def now():
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def cache_digest(value):
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def media_fingerprint(media, relative):
    """A cheap identity check that also catches same-size replacements."""
    stat = media.stat()
    digest = hashlib.sha256()
    with media.open("rb") as source:
        digest.update(source.read(65536))
        if stat.st_size > 65536:
            source.seek(max(0, stat.st_size - 65536))
            digest.update(source.read(65536))
    return {"path": relative, "size": stat.st_size, "mtime_ns": stat.st_mtime_ns, "sample": digest.hexdigest()}


def transcript_cache_key(job, media):
    return cache_digest({
        "kind": "transcript", "version": CACHE_VERSION,
        "media": media_fingerprint(media, job["path"]),
        "audio_stream_index": job["audio_stream_index"],
        "audio_offset": job["audio_offset"],
        "source_language": job["source_language"],
        "whisper_model": job.get("whisper_model", WHISPER_NAME),
        "whisper_options": {"beam_size": 5, "vad_filter": True, "word_timestamps": True, "condition_on_previous_text": False},
    })


def translation_cache_key(transcript_key, source_code, cues, target_code, model):
    return cache_digest({
        "kind": "translation", "version": CACHE_VERSION,
        "transcript_key": transcript_key, "source_language": source_code,
        "source_cues": cues, "target_language": target_code, "model": model,
        "adapter": "translategemma-raw-v1" if is_translategemma(model) else "chat-json-v1",
    })


def cache_path(kind, key):
    return DATA_ROOT / "cache" / kind / f"{key}.json"


def read_cache(kind, key):
    try:
        payload = json.loads(cache_path(kind, key).read_text(encoding="utf-8"))
        return payload if isinstance(payload, dict) and payload.get("key") == key else None
    except (OSError, ValueError):
        return None


def write_cache(kind, key, payload):
    destination = cache_path(kind, key)
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(destination.name + f".{uuid.uuid4().hex}.tmp")
    try:
        temporary.write_text(json.dumps({"key": key, **payload}, ensure_ascii=False), encoding="utf-8")
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)


def valid_cues(cues):
    return (isinstance(cues, list) and bool(cues) and all(
        isinstance(cue, dict) and type(cue.get("start")) in (int, float)
        and type(cue.get("end")) in (int, float) and cue["end"] > cue["start"]
        and isinstance(cue.get("text"), str) and bool(cue["text"].strip())
        for cue in cues
    ))


def load_transcript_cache(key):
    payload = read_cache("transcripts", key)
    if payload and isinstance(payload.get("language"), str) and payload["language"] not in ("", "auto") and valid_cues(payload.get("cues")):
        return payload["language"], payload["cues"]
    return None


def load_translation_cache(key, expected_count):
    payload = read_cache("translations", key)
    texts = payload.get("texts") if payload else None
    if isinstance(texts, list) and len(texts) <= expected_count and all(isinstance(text, str) and text.strip() for text in texts):
        return texts
    return []


def within(path, root):
    return path == root or root in path.parents


def resolve_media(relative, file_required=False):
    if not isinstance(relative, str) or "\\" in relative or "\x00" in relative:
        raise ValueError("Invalid library path")
    parts = PurePosixPath(relative).parts
    if relative.startswith("/") or any(part in ("..", ".") for part in parts):
        raise ValueError("Path must stay inside the media library")
    resolved = LIBRARY_ROOT.joinpath(*parts).resolve()
    if not within(resolved, LIBRARY_ROOT):
        raise ValueError("Path must stay inside the media library")
    if not resolved.exists():
        raise ValueError("Media path does not exist")
    if file_required and (not resolved.is_file() or resolved.suffix.lower() not in VIDEO_SUFFIXES):
        raise ValueError("Select a supported video file")
    return resolved


def browse(relative):
    directory = resolve_media(relative)
    if not directory.is_dir():
        raise ValueError("Select a folder")
    entries = []
    try:
        children = directory.iterdir()
        for child in children:
            if child.name.startswith("."):
                continue
            try:
                is_folder = child.is_dir()
                if not is_folder and (child.suffix.lower() not in VIDEO_SUFFIXES or not child.is_file()):
                    continue
                target = child.resolve()
            except OSError:
                # Drive roots include protected files such as pagefile.sys.
                continue
            if not within(target, LIBRARY_ROOT):
                continue
            entries.append({"name": child.name, "path": target.relative_to(LIBRARY_ROOT).as_posix(), "type": "folder" if is_folder else "media"})
    except PermissionError as exc:
        raise ValueError("This folder cannot be read by Subtitle Studio") from exc
    entries.sort(key=lambda x: (x["type"] != "folder", x["name"].casefold()))
    return {"path": relative, "entries": entries}


def probe_media(path):
    result = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "stream=index,codec_type,codec_name,channels,start_time:stream_tags=language,title:format=duration,start_time", "-of", "json", str(path)],
        capture_output=True, text=True, timeout=45, check=True,
    )
    info = json.loads(result.stdout)
    tracks = []
    format_start = float(info.get("format", {}).get("start_time") or 0)
    for stream in info.get("streams", []):
        if stream.get("codec_type") == "audio":
            tags = stream.get("tags", {})
            tracks.append({"index": stream["index"], "language": tags.get("language", "und"), "title": tags.get("title", ""), "codec": stream.get("codec_name", ""), "channels": stream.get("channels", 0), "offset": max(0, float(stream.get("start_time") or 0) - format_start)})
    return {"tracks": tracks, "duration": float(info.get("format", {}).get("duration") or 0)}


def list_models():
    request = Request(LLM_BASE + "/models", headers={"Accept": "application/json"})
    with urlopen(request, timeout=8) as response:
        data = json.load(response)
    return [item["id"] for item in data.get("data", []) if isinstance(item.get("id"), str) and not any(term in item["id"].lower() for term in ("embedding", "embed-text", "rerank"))]


def model_for_job(preferred):
    if preferred:
        return preferred
    if LLM_DEFAULT:
        return LLM_DEFAULT
    models = list_models()
    if not models:
        raise RuntimeError("LM Studio has no loaded model. Load a multilingual instruction model in LM Studio.")
    return models[0]


def persist_locked():
    DATA_ROOT.mkdir(parents=True, exist_ok=True)
    temporary = JOBS_FILE.with_suffix(".tmp")
    temporary.write_text(json.dumps(list(jobs.values()), ensure_ascii=False), encoding="utf-8")
    temporary.replace(JOBS_FILE)


def set_job(job_id, **changes):
    with jobs_lock:
        jobs[job_id].update(changes, updated_at=now())
        persist_locked()


def load_jobs():
    if not JOBS_FILE.exists():
        return
    try:
        for job in json.loads(JOBS_FILE.read_text(encoding="utf-8")):
            if job["status"] in ("queued", "running"):
                job.update(status="failed", stage="Interrupted by service restart", error="Service restarted before this job completed")
            jobs[job["id"]] = job
    except (OSError, ValueError, KeyError):
        logging.exception("Could not read previous jobs")


def supported_whisper_models():
    from faster_whisper.utils import available_models
    return list(dict.fromkeys([WHISPER_NAME, *available_models()]))


def whisper_downloaded(name):
    from faster_whisper.utils import download_model
    try:
        location = download_model(name, cache_dir=WHISPER_CACHE_DIR, local_files_only=True)
        return (Path(location) / "model.bin").is_file()
    except Exception:
        return False


def whisper_model_options():
    return [{"id": name, "downloaded": whisper_downloaded(name)} for name in supported_whisper_models()]


def create_job(payload):
    relative = payload.get("path")
    media = resolve_media(relative, file_required=True)
    source = payload.get("source_language", "auto")
    if source != "auto" and source not in LANGUAGES:
        raise ValueError("Unsupported source language")
    targets = payload.get("targets", [])
    if not isinstance(targets, list) or not all(isinstance(x, str) and x in LANGUAGES for x in targets):
        raise ValueError("Invalid output languages")
    targets = list(dict.fromkeys(targets))
    transcript = payload.get("transcript", False)
    if not isinstance(transcript, bool) or (not transcript and not targets):
        raise ValueError("Select transcription or at least one translation")
    model = payload.get("model", "")
    if not isinstance(model, str) or len(model) > 300:
        raise ValueError("Invalid LM Studio model")
    whisper_model = payload.get("whisper_model", WHISPER_NAME)
    if (not isinstance(whisper_model, str) or len(whisper_model) > 100
            or not whisper_model or (whisper_model != WHISPER_NAME and whisper_model not in supported_whisper_models())):
        raise ValueError("Unsupported Whisper model")
    metadata = probe_media(media)
    index = payload.get("audio_stream_index")
    if type(index) is not int or index not in [track["index"] for track in metadata["tracks"]]:
        raise ValueError("Select an audio track from this file")
    job_id = uuid.uuid4().hex[:12]
    selected_track = next(track for track in metadata["tracks"] if track["index"] == index)
    job = {
        "id": job_id, "path": relative, "filename": media.name, "audio_stream_index": index,
        "source_language": source, "transcript": transcript, "targets": targets,
        "whisper_model": whisper_model,
        "model": model.strip(), "status": "queued", "stage": "Waiting", "progress": 0,
        "duration": metadata["duration"], "position": 0, "audio_offset": selected_track.get("offset", 0), "detected_language": None,
        "media_fingerprint": media_fingerprint(media, relative), "reused": [],
        "outputs": {}, "error": None, "created_at": now(), "updated_at": now(),
    }
    with jobs_lock:
        jobs[job_id] = job
        persist_locked()
    job_queue.put(job_id)
    return job


def get_speech_model(name):
    global speech_model, speech_model_name
    with speech_lock:
        if speech_model is None or speech_model_name != name:
            if WHISPER_LOCAL_ONLY and not whisper_downloaded(name):
                raise RuntimeError(f"Whisper {name} is not downloaded. Run: docker compose run --rm subtitle-worker python download_model.py {name}")
            speech_model = None
            speech_model_name = None
            gc.collect()
            from faster_whisper import WhisperModel
            speech_model = WhisperModel(
                name, device=os.getenv("WHISPER_DEVICE", "cpu"),
                compute_type=os.getenv("WHISPER_COMPUTE_TYPE", "int8"),
                download_root=WHISPER_CACHE_DIR,
                local_files_only=WHISPER_LOCAL_ONLY,
            )
            speech_model_name = name
    return speech_model


def extract_audio(media, index, destination):
    subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i", str(media),
         "-map", f"0:{index}", "-vn", "-af", "aresample=async=1",
         "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", str(destination)],
        check=True, timeout=3600,
    )


def timestamp(seconds):
    milliseconds = max(0, round(float(seconds) * 1000))
    hours, remainder = divmod(milliseconds, 3600000)
    minutes, remainder = divmod(remainder, 60000)
    secs, millis = divmod(remainder, 1000)
    return f"{hours:02}:{minutes:02}:{secs:02},{millis:03}"


def normalize_text(text):
    return re.sub(r"\s+", " ", str(text)).strip()


def make_cues(segments):
    cues = []
    for segment in segments:
        words = getattr(segment, "words", None) or []
        if words:
            group = []
            for word in words:
                group.append(word)
                text = normalize_text("".join(w.word for w in group))
                span = group[-1].end - group[0].start
                if len(text) >= 72 or span >= 5.5 or (len(text) >= 25 and text.endswith((".", "?", "!", "。", "？", "！"))):
                    cues.append({"start": max(0, group[0].start), "end": max(group[0].start + .1, group[-1].end), "text": text})
                    group = []
            if group:
                cues.append({"start": max(0, group[0].start), "end": max(group[0].start + .1, group[-1].end), "text": normalize_text("".join(w.word for w in group))})
        else:
            text = normalize_text(segment.text)
            if text:
                cues.append({"start": max(0, segment.start), "end": max(segment.start + .1, segment.end), "text": text})
    for i, cue in enumerate(cues):
        if i + 1 < len(cues):
            cue["end"] = max(cue["start"] + .1, min(cue["end"], cues[i + 1]["start"]))
    return [cue for cue in cues if cue["text"]]


def wrap_subtitle(text, width=42):
    words = normalize_text(text).split(" ")
    if len(words) == 1:
        return "\n".join(words[0][i:i + width] for i in range(0, len(words[0]), width))
    lines = []
    line = ""
    for word in words:
        proposal = (line + " " + word).strip()
        if line and len(proposal) > width and len(lines) < 1:
            lines.append(line)
            line = word
        else:
            line = proposal
    lines.append(line)
    return "\n".join(lines)


def render_srt(cues):
    return "\n\n".join(
        f"{i}\n{timestamp(cue['start'])} --> {timestamp(cue['end'])}\n{wrap_subtitle(cue['text'])}"
        for i, cue in enumerate(cues, 1)
    ) + ("\n" if cues else "")


def is_translategemma(model):
    return "translategemma" in model.lower()


def translategemma_prompt(source_code, target_code, text):
    """Render Google's strict text translation template for the raw completions API."""
    if source_code not in LANGUAGES or target_code not in LANGUAGES:
        raise ValueError("TranslateGemma needs supported two-letter source and target languages")
    source = LANGUAGES[source_code]
    target = LANGUAGES[target_code]
    return (
        f"<bos><start_of_turn>user\nYou are a professional {source} ({source_code}) to {target} ({target_code}) translator. "
        f"Your goal is to accurately convey the meaning and nuances of the original {source} text while adhering to "
        f"{target} grammar, vocabulary, and cultural sensitivities.\n"
        f"Produce only the {target} translation, without any additional explanations or commentary. "
        f"Please translate the following {source} text into {target}:\n\n\n"
        f"{text.strip()}<end_of_turn>\n<start_of_turn>model\n"
    )


def request_translategemma(model, source_code, target_code, cue):
    payload = {
        "model": model, "prompt": translategemma_prompt(source_code, target_code, cue["text"]),
        "temperature": 0, "max_tokens": 256, "stop": ["<end_of_turn>"], "stream": False,
    }
    request = Request(LLM_BASE + "/completions", data=json.dumps(payload, ensure_ascii=False).encode("utf-8"), headers={"Content-Type": "application/json"}, method="POST")
    last_error = None
    for attempt in range(3):
        try:
            with urlopen(request, timeout=LLM_TIMEOUT) as response:
                reply = json.load(response)["choices"][0]["text"]
            reply = normalize_text(reply.split("<end_of_turn>", 1)[0])
            if not reply:
                raise ValueError("TranslateGemma returned empty text")
            return reply
        except (TimeoutError, URLError, HTTPError, ValueError, KeyError, TypeError) as exc:
            last_error = exc
            logging.warning("TranslateGemma cue attempt %d failed: %s", attempt + 1, exc)
            if attempt < 2:
                time.sleep(2 * (attempt + 1))
    raise RuntimeError(f"TranslateGemma translation failed after 3 attempts: {last_error}")


def request_translation(model, source_code, target_code, batch):
    if is_translategemma(model):
        return [request_translategemma(model, source_code, target_code, cue) for cue in batch]
    numbered = [{"id": i, "text": cue["text"]} for i, cue in enumerate(batch, 1)]
    prompt = (
        f"Translate these subtitle cues from {LANGUAGES.get(source_code, source_code)} to {LANGUAGES[target_code]}. "
        "Write fluent, grammatically correct, concise subtitles. Preserve meaning, names, and the original cue boundaries. "
        "Return ONLY a JSON array with one object per input, each with the same integer id and a translated text string. "
        "No timestamps, markdown, notes, or extra objects.\n\n" + json.dumps(numbered, ensure_ascii=False)
    )
    body = json.dumps({"model": model, "messages": [{"role": "system", "content": "You are an expert audiovisual subtitle translator. Output valid JSON only."}, {"role": "user", "content": prompt}], "temperature": 0.1, "stream": False}, ensure_ascii=False).encode("utf-8")
    request = Request(LLM_BASE + "/chat/completions", data=body, headers={"Content-Type": "application/json"}, method="POST")
    last_error = None
    for attempt in range(3):
        try:
            with urlopen(request, timeout=LLM_TIMEOUT) as response:
                reply = json.load(response)["choices"][0]["message"]["content"].strip()
            if reply.startswith("```"):
                reply = re.sub(r"^```(?:json)?\s*|\s*```$", "", reply, flags=re.I).strip()
            translated = json.loads(reply)
            if not isinstance(translated, list) or len(translated) != len(batch):
                raise ValueError("LM Studio returned a different number of subtitle cues")
            by_id = {item["id"]: normalize_text(item["text"]) for item in translated if isinstance(item, dict)}
            if set(by_id) != set(range(1, len(batch) + 1)) or not all(by_id.values()):
                raise ValueError("LM Studio returned invalid subtitle cue IDs or empty text")
            return [by_id[i] for i in range(1, len(batch) + 1)]
        except (TimeoutError, URLError, HTTPError, ValueError, KeyError, TypeError) as exc:
            last_error = exc
            logging.warning("LM Studio batch attempt %d failed: %s", attempt + 1, exc)
            if attempt < 2:
                time.sleep(2 * (attempt + 1))
    raise RuntimeError(f"LM Studio translation failed after 3 attempts: {last_error}")


def output_path(job, language):
    relative = PurePosixPath(job["path"])
    folder = OUTPUT_ROOT.joinpath(*relative.parent.parts)
    folder.mkdir(parents=True, exist_ok=True)
    return folder / f"{relative.stem}.{language}.srt"


def write_output(job_id, language, cues):
    with jobs_lock:
        job = jobs[job_id].copy()
    destination = output_path(job, language)
    content = render_srt(cues)
    if not destination.exists() or destination.read_text(encoding="utf-8") != content:
        temporary = destination.with_suffix(".srt.tmp")
        temporary.write_text(content, encoding="utf-8")
        temporary.replace(destination)
    snapshot_dir = DATA_ROOT / "job_outputs" / job_id
    snapshot_dir.mkdir(parents=True, exist_ok=True)
    snapshot = snapshot_dir / destination.name
    snapshot_tmp = snapshot.with_suffix(".srt.tmp")
    snapshot_tmp.write_text(content, encoding="utf-8")
    snapshot_tmp.replace(snapshot)
    with jobs_lock:
        jobs[job_id]["outputs"][language] = {"path": str(snapshot), "export_path": str(destination), "name": destination.name, "url": f"/api/jobs/{job_id}/files/{language}"}
        jobs[job_id]["updated_at"] = now()
        persist_locked()


def run_job(job_id):
    with jobs_lock:
        job = jobs[job_id].copy()
    media = resolve_media(job["path"], file_required=True)
    whisper_model = job.get("whisper_model", WHISPER_NAME)
    set_job(job_id, status="running", stage="Checking cached results", progress=1)
    if job.get("media_fingerprint") != media_fingerprint(media, job["path"]):
        raise RuntimeError("The media file changed after this job was queued. Submit it again.")
    transcript_key = transcript_cache_key(job, media)
    cached = load_transcript_cache(transcript_key)
    reused = []
    if cached:
        source_code, cues = cached
        reused.append("transcription")
        set_job(job_id, stage="Reusing cached transcription", progress=64,
                detected_language=source_code, position=job["duration"], reused=reused[:])
    else:
        import tempfile
        with tempfile.TemporaryDirectory(prefix="subtitle-") as directory:
            audio = Path(directory) / "audio.wav"
            set_job(job_id, stage="Extracting audio", progress=2)
            extract_audio(media, job["audio_stream_index"], audio)
            set_job(job_id, stage=f"Loading Whisper {whisper_model}", progress=8)
            model = get_speech_model(whisper_model)
            segments, info = model.transcribe(
                str(audio), language=None if job["source_language"] == "auto" else job["source_language"],
                task="transcribe", beam_size=5, vad_filter=True, word_timestamps=True,
                condition_on_previous_text=False,
            )
            source_code = info.language or job["source_language"]
            if source_code == "auto":
                raise RuntimeError("Whisper could not detect the audio language. Select it explicitly and retry.")
            set_job(job_id, detected_language=source_code, stage="Transcribing", progress=10)
            collected = []
            last_percent = 10
            duration = max(job["duration"], 1)
            for segment in segments:
                collected.append(segment)
                percent = min(62, 10 + int(52 * segment.end / duration))
                if percent >= last_percent + 2:
                    set_job(job_id, progress=percent, position=round(segment.end, 1))
                    last_percent = percent
            cues = make_cues(collected)
            for cue in cues:
                cue["start"] += job["audio_offset"]
                cue["end"] += job["audio_offset"]
            if not cues:
                raise RuntimeError("No speech was detected in the selected audio track")
            write_cache("transcripts", transcript_key, {"language": source_code, "cues": cues})
    set_job(job_id, stage="Preparing subtitles", progress=64)
    if job["transcript"] or source_code in job["targets"]:
        write_output(job_id, source_code, cues)
    targets = [language for language in job["targets"] if language != source_code]
    if targets:
        model_id = model_for_job(job["model"])
        set_job(job_id, model=model_id)
        batch_size = 1 if is_translategemma(model_id) else 8
        total_batches = sum((len(cues) + batch_size - 1) // batch_size for _ in targets)
        finished_batches = 0
        for language in targets:
            translation_key = translation_cache_key(transcript_key, source_code, cues, language, model_id)
            texts = load_translation_cache(translation_key, len(cues))
            finished_batches += (len(texts) + batch_size - 1) // batch_size
            if texts:
                reused.append(language if len(texts) == len(cues) else f"{language} ({len(texts)} cues)")
                set_job(job_id, stage=f"Reusing cached {LANGUAGES[language]} cues", reused=reused[:],
                        progress=64 + int(34 * finished_batches / total_batches))
            for start in range(len(texts), len(cues), batch_size):
                batch = cues[start:start + batch_size]
                if start % max(1, len(cues) // 100) == 0:
                    set_job(job_id, stage=f"Translating to {LANGUAGES[language]} ({start + 1}/{len(cues)} cues)")
                translated = request_translation(model_id, source_code, language, batch)
                texts.extend(translated)
                write_cache("translations", translation_key, {"texts": texts})
                finished_batches += 1
                percent = 64 + int(34 * finished_batches / total_batches)
                with jobs_lock:
                    prior_percent = jobs[job_id]["progress"]
                if percent != prior_percent:
                    set_job(job_id, progress=percent)
            write_output(job_id, language, [
                {"start": cue["start"], "end": cue["end"], "text": text}
                for cue, text in zip(cues, texts)
            ])
    set_job(job_id, status="completed", stage="Complete", progress=100, position=job["duration"])


def worker_loop():
    while True:
        job_id = job_queue.get()
        try:
            run_job(job_id)
        except Exception as exc:
            logging.exception("Subtitle job %s failed", job_id)
            set_job(job_id, status="failed", stage="Failed", error=str(exc))
        finally:
            job_queue.task_done()


class Handler(BaseHTTPRequestHandler):
    def send_bytes(self, data, content_type, status=200, download=None):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        if download:
            self.send_header("Content-Disposition", f"attachment; filename*=UTF-8''{quote(download)}")
        self.end_headers()
        self.wfile.write(data)

    def send_json(self, value, status=200):
        self.send_bytes(json.dumps(value, ensure_ascii=False).encode("utf-8"), "application/json; charset=utf-8", status)

    def route_get(self, path, query):
        if path in ("/", "/app.js"):
            filename = "index.html" if path == "/" else path[1:]
            content_type = "text/html; charset=utf-8" if filename.endswith("html") else "application/javascript; charset=utf-8"
            return self.send_bytes((WEB_ROOT / filename).read_bytes(), content_type)
        if path == "/api/config":
            try:
                models, model_error = list_models(), None
            except Exception as exc:
                models, model_error = [], str(exc)
            if LLM_DEFAULT and LLM_DEFAULT not in models:
                models.insert(0, LLM_DEFAULT)
            return self.send_json({"languages": LANGUAGES, "whisper_model": WHISPER_NAME,
                                   "whisper_models": whisper_model_options(), "whisper_local_only": WHISPER_LOCAL_ONLY,
                                   "models": models, "default_model": LLM_DEFAULT or (models[0] if models else ""), "model_error": model_error})
        if path == "/api/library":
            return self.send_json(browse(query.get("path", [""])[0]))
        if path == "/api/media":
            relative = query.get("path", [""])[0]
            return self.send_json({"path": relative, **probe_media(resolve_media(relative, file_required=True))})
        if path == "/api/jobs":
            with jobs_lock:
                items = sorted(jobs.values(), key=lambda x: x["created_at"], reverse=True)
                return self.send_json(items[:100])
        match = re.fullmatch(r"/api/jobs/([a-f0-9]{12})(?:/files/([a-z]{2}))?", path)
        if match:
            job_id, language = match.groups()
            with jobs_lock:
                job = jobs.get(job_id)
                if not job:
                    return self.send_json({"error": "Job not found"}, 404)
                job = json.loads(json.dumps(job))
            if language:
                output = job["outputs"].get(language)
                if not output:
                    return self.send_json({"error": "Subtitle not found"}, 404)
                path_on_disk = Path(output["path"]).resolve()
                if not (within(path_on_disk, OUTPUT_ROOT) or within(path_on_disk, DATA_ROOT / "job_outputs")) or not path_on_disk.is_file():
                    return self.send_json({"error": "Subtitle file missing"}, 404)
                return self.send_bytes(path_on_disk.read_bytes(), "application/x-subrip; charset=utf-8", download=output["name"])
            return self.send_json(job)
        return self.send_json({"error": "Not found"}, 404)

    def do_GET(self):
        parsed = urlparse(self.path)
        try:
            self.route_get(unquote(parsed.path), parse_qs(parsed.query))
        except (ValueError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
            self.send_json({"error": str(exc)}, 400)
        except Exception:
            logging.exception("GET failed")
            self.send_json({"error": "Server error"}, 500)

    def do_POST(self):
        try:
            if urlparse(self.path).path != "/api/jobs":
                return self.send_json({"error": "Not found"}, 404)
            length = int(self.headers.get("Content-Length", "0"))
            if length < 1 or length > 16384:
                return self.send_json({"error": "Invalid request size"}, 400)
            payload = json.loads(self.rfile.read(length))
            if not isinstance(payload, dict):
                raise ValueError("Expected a JSON object")
            self.send_json(create_job(payload), 202)
        except (ValueError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
            self.send_json({"error": str(exc)}, 400)
        except Exception:
            logging.exception("POST failed")
            self.send_json({"error": "Server error"}, 500)


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    load_jobs()
    threading.Thread(target=worker_loop, daemon=True).start()
    server = ThreadingHTTPServer(("0.0.0.0", 8099), Handler)
    logging.info("Subtitle Studio listening on port 8099")
    server.serve_forever()
