"""A small, private HTTP worker for the n8n MKV-to-English-SRT workflow."""

from __future__ import annotations

import json
import os
import queue
import re
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PureWindowsPath


MEDIA_ROOT = Path(os.getenv("SUBTITLE_MEDIA_ROOT", "/media")).resolve()
INPUT_DIR = MEDIA_ROOT / "input"
OUTPUT_DIR = MEDIA_ROOT / "output"
SOURCE_DIR = Path(os.getenv("SUBTITLE_SOURCE_ROOT", "/source")).resolve()
SOURCE_HOST_DIR = PureWindowsPath(os.getenv("SUBTITLE_SOURCE_HOST_DIR", "D:/subtitles/input"))
MIN_AGE_SECONDS = int(os.getenv("SUBTITLE_MIN_AGE_SECONDS", "90"))
WHISPER_MODEL = os.getenv("WHISPER_MODEL", "medium")
WHISPER_DEVICE = os.getenv("WHISPER_DEVICE", "cpu")
WHISPER_COMPUTE_TYPE = os.getenv("WHISPER_COMPUTE_TYPE", "int8")
WHISPER_LANGUAGE = os.getenv("WHISPER_LANGUAGE", "").strip() or None
WHISPER_LOCAL_FILES_ONLY = os.getenv("WHISPER_LOCAL_FILES_ONLY", "false").lower() in {"1", "true", "yes", "on"}
WHISPER_CACHE_DIR = os.getenv("HF_HUB_CACHE", "/models/hub")
AUDIO_TRACK = int(os.getenv("SUBTITLE_AUDIO_TRACK", "0"))
LLM_PROVIDER = os.getenv("SUBTITLE_LLM_PROVIDER", "lmstudio").lower()
LLM_BASE_URL = os.getenv("SUBTITLE_LLM_BASE_URL", "http://host.docker.internal:1234/v1").rstrip("/")
LLM_MODEL = os.getenv("SUBTITLE_LLM_MODEL", "").strip()
OPENAI_API_KEY = os.getenv("OPENAI_API_KEY", "")

jobs: dict[str, dict] = {}
seen: dict[str, tuple[int, int]] = {}
pending: queue.Queue[tuple[str, Path]] = queue.Queue()
lock = threading.Lock()
whisper_model = None


def update_job(job_id: str, stage: str, percent: int, **details) -> None:
    percent = max(0, min(100, int(percent)))
    with lock:
        job = jobs[job_id]
        previous_stage = job.get("stage")
        previous_percent = job.get("progress_percent", 0)
        job.update(stage=stage, progress_percent=max(previous_percent, percent), **details)
    if stage != previous_stage or percent >= previous_percent + 5:
        print(f"{job['filename']}: {stage} {percent}%", flush=True)


def resolve_source(raw_path: str) -> Path:
    """Accept a Windows host path under the configured source root or /source."""
    if not isinstance(raw_path, str) or not raw_path.strip() or "\x00" in raw_path:
        raise ValueError("file_path is required")
    raw_path = raw_path.strip()
    if re.match(r"^[A-Za-z]:[\\/]", raw_path):
        try:
            relative = PureWindowsPath(raw_path).relative_to(SOURCE_HOST_DIR)
        except ValueError as error:
            raise ValueError(f"Path must be inside {SOURCE_HOST_DIR}") from error
        source = SOURCE_DIR.joinpath(*relative.parts)
    else:
        source = Path(raw_path)
        if not source.is_absolute():
            source = SOURCE_DIR / source
    root = SOURCE_DIR.resolve()
    source = source.resolve()
    if not source.is_relative_to(root) or source == root:
        raise ValueError(f"Path must be inside {SOURCE_HOST_DIR} (container: {root})")
    if source.suffix.lower() != ".mkv" or not source.is_file():
        raise ValueError("Path must name an existing MKV file")
    return source


def stamp(seconds: float) -> str:
    milliseconds = max(0, round(seconds * 1000))
    hours, remainder = divmod(milliseconds, 3_600_000)
    minutes, remainder = divmod(remainder, 60_000)
    secs, millis = divmod(remainder, 1000)
    return f"{hours:02}:{minutes:02}:{secs:02},{millis:03}"


def clean_text(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def wrap_text(text: str, max_width: int = 42) -> str:
    words = text.split()
    if len(text) <= max_width or len(words) < 2:
        return text
    best = min(range(1, len(words)), key=lambda i: (
        max(len(" ".join(words[:i])), len(" ".join(words[i:]))),
        abs(len(" ".join(words[:i])) - len(" ".join(words[i:]))),
    ))
    return " ".join(words[:best]) + "\n" + " ".join(words[best:])


def split_segment(segment) -> list[dict]:
    """Split long model segments using their word timestamps."""
    text = clean_text(segment.text)
    if not text:
        return []
    start, end = float(segment.start), float(segment.end)
    if end <= start:
        return []
    words = [w for w in (segment.words or []) if clean_text(w.word)]
    if len(text) <= 84 and end - start <= 7:
        return [{"start": start, "end": end, "text": text}]
    if not words:
        return [{"start": start, "end": end, "text": text}]
    result = []
    group = []
    for word in words:
        candidate = clean_text(" ".join(clean_text(w.word) for w in group + [word]))
        duration = float(word.end) - float(group[0].start) if group else 0
        if group and (len(candidate) > 82 or duration > 6.5):
            result.append({
                "start": float(group[0].start),
                "end": float(group[-1].end),
                "text": clean_text(" ".join(clean_text(w.word) for w in group)),
            })
            group = []
        group.append(word)
    if group:
        result.append({
            "start": float(group[0].start),
            "end": float(group[-1].end),
            "text": clean_text(" ".join(clean_text(w.word) for w in group)),
        })
    return result


def normalize_cues(cues: list[dict]) -> list[dict]:
    """Remove invalid cues and prevent overlaps without moving speech starts."""
    ordered = sorted(cues, key=lambda cue: (cue["start"], cue["end"]))
    result = []
    for cue in ordered:
        start, end = max(0.0, cue["start"]), cue["end"]
        text = clean_text(cue["text"])
        if not text or end <= start:
            continue
        if result and start < result[-1]["end"]:
            if start > result[-1]["start"] + 0.1:
                result[-1]["end"] = start
            else:
                start = result[-1]["end"]
        if end <= start:
            continue
        result.append({"start": start, "end": end, "text": text})
    return result


def render_srt(cues: list[dict]) -> str:
    return "\n".join(
        f"{number}\n{stamp(cue['start'])} --> {stamp(cue['end'])}\n{wrap_text(cue['text'])}\n"
        for number, cue in enumerate(cues, 1)
    )


def run_command(args: list[str]) -> None:
    completed = subprocess.run(args, text=True, capture_output=True, check=False)
    if completed.returncode:
        raise RuntimeError(f"{args[0]} failed: {completed.stderr[-2000:]}")


def extract_audio(source: Path, target: Path) -> None:
    run_command([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-i", str(source), "-map", f"0:a:{AUDIO_TRACK}", "-vn", "-af",
        "aresample=async=1:first_pts=0", "-ac", "1", "-ar", "16000",
        "-c:a", "pcm_s16le", str(target),
    ])


def request_json(url: str, payload: dict | None = None, headers: dict | None = None) -> dict:
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8") if payload is not None else None,
        headers={"Content-Type": "application/json", **(headers or {})},
        method="POST" if payload is not None else "GET",
    )
    try:
        with urllib.request.urlopen(request, timeout=180) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"Language model HTTP {error.code}: {error.read(1000).decode('utf-8', 'replace')}") from error


def model_name() -> str:
    if LLM_MODEL:
        return LLM_MODEL
    if LLM_PROVIDER == "openai":
        return "gpt-4.1-mini"
    models = request_json(LLM_BASE_URL + "/models").get("data", [])
    if not models:
        raise RuntimeError("LM Studio has no loaded model; load one or set SUBTITLE_LLM_MODEL")
    return models[0]["id"]


def polish(cues: list[dict], on_progress=None) -> None:
    if LLM_PROVIDER not in {"lmstudio", "openai"}:
        raise RuntimeError("SUBTITLE_LLM_PROVIDER must be lmstudio or openai")
    if LLM_PROVIDER == "openai" and not OPENAI_API_KEY:
        raise RuntimeError("OPENAI_API_KEY is required for the OpenAI language model")
    base_url = "https://api.openai.com/v1" if LLM_PROVIDER == "openai" else LLM_BASE_URL
    headers = {"Authorization": f"Bearer {OPENAI_API_KEY}"} if LLM_PROVIDER == "openai" else {}
    model = model_name()
    for offset in range(0, len(cues), 20):
        batch = cues[offset:offset + 20]
        source = [{"id": offset + index, "text": cue["text"]} for index, cue in enumerate(batch)]
        prompt = (
            "Edit these English subtitles for natural, grammatical English. Preserve meaning, "
            "names, numbers, tone, and every id. Keep each cue concise (ideally 84 characters or less). "
            "Do not add explanations or new events. Return only a JSON array of objects with id and text.\n"
            + json.dumps(source, ensure_ascii=False)
        )
        for attempt in range(2):
            response = request_json(base_url + "/chat/completions", {
                "model": model,
                "temperature": 0,
                "messages": [
                    {"role": "system", "content": "You are a careful subtitle editor. Output valid JSON only."},
                    {"role": "user", "content": prompt if not attempt else prompt + "\nReturn the exact JSON array format with every original id."},
                ],
            }, headers)
            content = response["choices"][0]["message"]["content"].strip()
            if content.startswith("```"):
                content = re.sub(r"^```(?:json)?\s*|\s*```$", "", content)
            try:
                edited = json.loads(content)
                if not isinstance(edited, list) or [item.get("id") for item in edited] != [item["id"] for item in source]:
                    raise ValueError("changed subtitle IDs or count")
                if any(not clean_text(item.get("text", "")) or len(clean_text(item["text"])) > 84 for item in edited):
                    raise ValueError("returned an empty or oversized cue")
                break
            except (ValueError, TypeError, AttributeError) as error:
                if attempt:
                    raise RuntimeError(f"Language model returned invalid subtitle JSON: {error}") from error
        for cue, item in zip(batch, edited):
            cue["text"] = clean_text(item["text"])
        if on_progress:
            on_progress(min(len(cues), offset + len(batch)), len(cues))


def transcribe(audio: Path, on_progress=None) -> list[dict]:
    from faster_whisper import WhisperModel

    global whisper_model
    if whisper_model is None:
        whisper_model = WhisperModel(
            WHISPER_MODEL, device=WHISPER_DEVICE, compute_type=WHISPER_COMPUTE_TYPE,
            download_root=WHISPER_CACHE_DIR, local_files_only=WHISPER_LOCAL_FILES_ONLY,
        )
    segments, _ = whisper_model.transcribe(
        str(audio), task="translate", language=WHISPER_LANGUAGE,
        beam_size=5, vad_filter=True, word_timestamps=True,
        condition_on_previous_text=False,
    )
    cues = []
    for segment in segments:
        cues.extend(split_segment(segment))
        if on_progress:
            on_progress(float(segment.end), len(cues))
    cues = normalize_cues(cues)
    if not cues:
        raise RuntimeError("No speech was detected in the selected audio track")
    return cues


def process_job(job_id: str, source: Path) -> None:
    output = OUTPUT_DIR / f"{source.stem}.en.srt"
    error_file = OUTPUT_DIR / f"{source.stem}.en.error.txt"
    with tempfile.TemporaryDirectory(prefix="subtitle-") as directory:
        audio = Path(directory) / "audio.wav"
        update_job(job_id, "extracting_audio", 2)
        extract_audio(source, audio)
        with wave.open(str(audio), "rb") as wave_file:
            duration = wave_file.getnframes() / wave_file.getframerate()
        update_job(job_id, "loading_speech_model", 10, duration_seconds=round(duration, 2))

        def transcription_progress(position: float, count: int) -> None:
            fraction = min(1.0, max(0.0, position / duration)) if duration else 0.0
            update_job(
                job_id, "transcribing", 10 + int(70 * fraction),
                media_position_seconds=round(position, 2), cue_count=count,
            )

        cues = transcribe(audio, transcription_progress)
        update_job(job_id, "proofreading", 80, cue_count=len(cues))
        polish(cues, lambda finished, total: update_job(job_id, "proofreading", 80 + int(18 * finished / total), edited_cues=finished))
        update_job(job_id, "writing_srt", 99)
        text = render_srt(cues)
        temporary_output = output.with_suffix(".srt.tmp")
        temporary_output.write_text(text, encoding="utf-8")
        temporary_output.replace(output)
    error_file.unlink(missing_ok=True)
    with lock:
        jobs[job_id].update(status="completed", stage="completed", progress_percent=100, output=str(output), cue_count=len(cues), finished_at=time.time())


def run_jobs() -> None:
    while True:
        job_id, source = pending.get()
        with lock:
            jobs[job_id].update(status="running", started_at=time.time())
        try:
            print(f"Processing {source.name}", flush=True)
            process_job(job_id, source)
            print(f"Completed {source.name}", flush=True)
        except Exception as error:
            error_file = OUTPUT_DIR / f"{source.stem}.en.error.txt"
            try:
                error_file.write_text(str(error) + "\n", encoding="utf-8")
            except OSError:
                pass
            print(f"Failed {source.name}: {error}", flush=True)
            with lock:
                jobs[job_id].update(status="failed", stage="failed", error=str(error), finished_at=time.time())
        finally:
            pending.task_done()


def enqueue_source(source: Path) -> dict:
    stat = source.stat()
    signature = (stat.st_size, stat.st_mtime_ns)
    with lock:
        existing = next((job for job in jobs.values() if job["filename"] == source.name and job["signature"] == signature and job["status"] in {"queued", "running"}), None)
        if existing:
            return existing.copy()
        output = OUTPUT_DIR / f"{source.stem}.en.srt"
        if output.exists():
            raise FileExistsError(f"Output already exists: {output}. Remove it to reprocess.")
        job_id = uuid.uuid4().hex
        job = {
            "id": job_id,
            "filename": source.name,
            "input": str(source),
            "signature": signature,
            "status": "queued",
            "stage": "queued",
            "progress_percent": 0,
            "expected_output": str(output),
            "created_at": time.time(),
        }
        jobs[job_id] = job
    pending.put((job_id, source))
    return job.copy()


def scan() -> list[dict]:
    submitted = []
    for source in sorted(INPUT_DIR.iterdir(), key=lambda path: path.name.lower()):
        if source.suffix.lower() != ".mkv" or not source.is_file() or source.is_symlink():
            continue
        stat = source.stat()
        key = source.name
        signature = (stat.st_size, stat.st_mtime_ns)
        output = OUTPUT_DIR / f"{source.stem}.en.srt"
        error_file = OUTPUT_DIR / f"{source.stem}.en.error.txt"
        with lock:
            previous = seen.get(key)
            seen[key] = signature
            existing = next((job for job in jobs.values() if job["filename"] == key and job["signature"] == signature and job["status"] in {"queued", "running"}), None)
            if previous != signature or time.time() - stat.st_mtime < MIN_AGE_SECONDS or output.exists() or error_file.exists() or existing:
                continue
        try:
            job = enqueue_source(source)
            submitted.append({"id": job["id"], "filename": key})
        except FileExistsError:
            continue
    return submitted


class Handler(BaseHTTPRequestHandler):
    def reply(self, status: int, body: dict) -> None:
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        if self.path == "/health":
            return self.reply(200, {"status": "ok"})
        if self.path == "/jobs":
            with lock:
                recent = [job.copy() for job in list(jobs.values())[-100:]]
            return self.reply(200, {"jobs": recent})
        if self.path.startswith("/jobs/"):
            with lock:
                job = jobs.get(self.path[len("/jobs/"):])
                result = job.copy() if job else None
            return self.reply(200 if result else 404, result or {"error": "Job not found"})
        self.reply(404, {"error": "Not found"})

    def do_POST(self) -> None:
        if self.path == "/scan":
            return self.reply(200, {"submitted": scan()})
        if self.path != "/jobs":
            return self.reply(404, {"error": "Not found"})
        try:
            size = int(self.headers.get("Content-Length", "0"))
            if size <= 0 or size > 8192:
                raise ValueError("Expected a small JSON body with file_path")
            body = json.loads(self.rfile.read(size))
            source = resolve_source(body.get("file_path"))
            job = enqueue_source(source)
        except (ValueError, TypeError, json.JSONDecodeError) as error:
            return self.reply(400, {"error": str(error)})
        except FileExistsError as error:
            return self.reply(409, {"error": str(error)})
        self.reply(202, job)


def main() -> None:
    INPUT_DIR.mkdir(parents=True, exist_ok=True)
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    SOURCE_DIR.mkdir(parents=True, exist_ok=True)
    threading.Thread(target=run_jobs, daemon=True).start()
    ThreadingHTTPServer(("0.0.0.0", 8099), Handler).serve_forever()


if __name__ == "__main__":
    main()
